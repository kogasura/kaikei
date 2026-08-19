//! `suggest_tax_category` — 税区分の**候補と根拠**（`docs/07-mcp-server.md`
//! §2 / §10 MC-08）。
//!
//! # ★このツールは断定しない★（`CLAUDE.md` §10）
//!
//! §10 は「提案系の機能は候補と根拠を返し、確定は人間に残す」と定めている。
//! したがってこのツールは:
//!
//! - **1件に絞らない**（順位も信頼度も付けない）
//! - **摘要（`description`）から区分を推論しない**
//! - 各候補について「**なぜ候補に挙がったか**」を、マスタに書かれている事実
//!   （適用期間・向き・税率・適格請求書の要否・注記）として返す
//!
//! ## なぜ摘要から推論しないのか（`DECISIONS.md` D-087）
//!
//! 1. 摘要の語から税区分を決める規則は、このリポジトリのどの層にも存在しない
//!    （仕訳化ルールは `kaikei-import`＝Phase 4 以降）。**無い規則を
//!    presentation 層で発明するのは D-072 が禁じている「業務判断を MCP 層に
//!    書く」ことそのもの**である
//! 2. 語の一致だけを根拠に1件を返すと、AI はそれを確定として使う。
//!    税区分の取り違えは税額計算を丸ごと変える（`CLAUDE.md` の冒頭
//!    「会計データは間違うと実害が出る」）
//! 3. 「根拠が空でない」（MC-08）は、推論の説明ではなく**マスタの記載事項**で
//!    満たせる。こちらは検証可能で、言い換えによる断定も混じらない
//!
//! 受け取った `description` は応答にそのまま echo し、**サーバがそれを判断に
//! 使っていないこと**を明示する。使っていないものを黙って受け取ると、
//! 呼び出し元は「摘要を書けば絞り込まれる」と誤解する。
//!
//! # 根拠に**帳簿の設定**も並べる（PR-G レビュー C-1）
//!
//! 初版の `reason` は「税率は 0.10 です」と述べ、候補には `tax_account`
//! （仮受消費税等の科目）も載っていた。ところが免税事業者
//! （`is_taxable_business: false`）や簡易課税の帳簿では、**その区分で実際に
//! 記帳しても税額行は1行も生成されない**。にもかかわらず応答は課税事業者の
//! 帳簿と完全に同一だった。`disclaimer` が否定していたのは「文面からの推論」
//! だけで、「帳簿の設定は考慮していない」とはどこにも書いていない。
//!
//! そこで [`kaikei_jp::tax::JpSettings`] の `tax_mode` /
//! `is_taxable_business` / `simplified_taxation` を `filtered_by` に並べ、
//! `disclaimer` に「帳簿の設定によっては税額行が生成されないことがある」を
//! 足す。**業務判断は書かない**（どの設定でどの区分が使えるかを決めるのは
//! `kaikei-jp` の policy であって MCP 層ではない。`DECISIONS.md` D-072）。
//! 並べているのは `get_settings` が返すのと同じ値であり、**事実の提示で
//! あって判断ではない**。
//!
//! # 帳簿を一切変更しない
//!
//! `Tx` を開かず、DB にも触れない（同 §4 の経路 (c)）。合成ルートが保持する
//! [`kaikei_jp::tax::TaxRuleSets`] を引くだけである。

use kaikei_jp::tax::{JpSettings, TaxCategoryTable, TaxDirection};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::dispatch::{McpTool, ToolContext, ToolFailure, ToolSuccess};
use crate::error::ToolError;
use crate::tools::list_tax_categories::{tax_category_to_json, tax_table_to_json};
use crate::tools::{in_field, parse_date};

/// `suggest_tax_category`。
pub struct SuggestTaxCategory;

// ★この構造体の doc コメントは `tools/list` の応答に出る★
// 内部設計書への参照・crate 名・Markdown の強調記法を書かないこと。
/// 税区分の候補を求める条件。指定していないキーは受け付けません。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SuggestTaxCategoryInput {
    /// 取引日。YYYY-MM-DD の形式で指定します。候補は取引日の時点で有効な
    /// 消費税区分マスタから挙げます。
    pub date: String,

    /// 売上側なら sales、仕入側なら purchase、税額計算をしない区分に絞るなら
    /// none を指定します。省略するとすべての向きの区分を返します。
    #[serde(default)]
    pub direction: Option<String>,

    /// 取引内容の説明（任意）。応答にそのまま含めて返しますが、
    /// サーバはこの文から税区分を推論しません。候補の絞り込みに使われるのは
    /// date と direction だけです。
    #[serde(default)]
    pub description: Option<String>,
}

impl McpTool for SuggestTaxCategory {
    type Input = SuggestTaxCategoryInput;

    const NAME: &'static str = "suggest_tax_category";

    const DESCRIPTION: &'static str = "\
指定した取引日に使える消費税区分の候補を、根拠つきで返します。\
このツールは候補と根拠だけを返し、どの区分が正しいかは決めません。\
候補は取引日の時点で有効な消費税区分マスタに登録されている区分で、\
direction を指定した場合はその向き（売上・仕入・対象外）に絞ります。\
各候補の根拠は、マスタに書かれている事実（適用期間・向き・税率・\
適格請求書の保存が必要かどうか・注記）です。\
候補はこの帳簿の設定（経理方式・課税事業者かどうか・簡易課税かどうか）では絞っていません。\
その設定は filtered_by に返すので、税額行が生成されるかどうかの判断に使ってください\
（設定によっては、候補の区分で記帳しても税額の行は生成されません）。\
摘要から区分を推論することはしません（description を渡しても絞り込みには使いません）。\
帳簿は一切変更しません。記帳するには、選んだ区分コードを post_journal_entry の\
明細の tags の tax_category に指定して別途呼び出します。";

    async fn run(ctx: &ToolContext<'_>, input: Self::Input) -> Result<ToolSuccess, ToolFailure> {
        let date = parse_date("date", &input.date)?;
        let direction = match &input.direction {
            Some(code) => Some(
                TaxDirection::from_code(code)
                    .map_err(|error| in_field("direction", ToolError::from_jp_error(&error)))?,
            ),
            None => None,
        };

        let composition = ctx.composition();
        // 該当なしをエラーにする入口を通す（有効期間つきの文言は `kaikei-jp`
        // が組み立てる。`DECISIONS.md` D-072）。
        let table = composition
            .tax_policy
            .rule_sets()
            .require_for_date(date)
            .map_err(|error| ToolError::from_jp_error(&error))?;

        Ok(ToolSuccess::new(success_body(
            &input.date,
            direction,
            input.description.as_deref(),
            table,
            composition.tax_policy.settings(),
        )))
    }
}

/// 応答の冒頭に置く「このサーバーが何をして何をしていないか」。
///
/// **確定は人間（呼び出し元）に残す**（`CLAUDE.md` §10）。ここで
/// 「この取引は SALES_10 です」に相当する文を書かないこと。
/// **帳簿の設定を考慮していないことも明示する**（PR-G レビュー C-1）。
/// 「文面からの推論をしていない」だけを書くと、「帳簿の設定は見たうえで
/// 候補を出した」と読めてしまう。実際には見ていない。
const DISCLAIMER: &str = "\
候補と根拠のみを返しています。どの区分を使うかの判断はこのサーバーでは行いません。\
候補は、指定された取引日の時点で有効な消費税区分マスタに登録されている区分です。\
取引内容の文面からの推論は行っていません。\
この帳簿の設定（filtered_by の tax_mode / is_taxable_business / simplified_taxation）でも\
候補を絞っていません。帳簿の設定によっては、候補の区分で記帳しても\
税額の行が生成されないことがあります。";

/// 候補1件の根拠（**マスタに書かれている事実だけ**を述べる）。
///
/// 推論の説明ではないので、断定にも言い換えにもならない。
/// 注記（`note`）はマスタの文言をそのまま運ぶ（`CLAUDE.md` §10）。
fn reason_for(
    category: &kaikei_jp::tax::TaxCategory,
    table: &TaxCategoryTable,
    date: &str,
) -> String {
    let mut reason = format!(
        "{date} 時点で有効なマスタ「{label}」（{range}）に、\
         {direction}の区分「{name}」として登録されています",
        label = table.label(),
        range = table.range_display(),
        direction = direction_label(category.direction),
        name = category.label,
    );
    match category.rate {
        Some(rate) => reason.push_str(&format!("。税率は {} です", rate.as_decimal())),
        None => reason.push_str("。税率は登録されていません（税額計算をしない区分です）"),
    }
    if category.requires_qualified_invoice {
        reason.push_str("。適格請求書の保存が必要と登録されています");
    }
    if let Some(note) = &category.note {
        reason.push_str(&format!("。マスタの注記: {note}"));
    }
    reason
}

/// 根拠の文中で使う向きの表記。
///
/// 機械可読名（`direction`）は [`TaxDirection::as_code`] を使い、こちらは
/// **人間・AI が読む文の一部**にしか使わない（分岐に使わせない。
/// `docs/07-mcp-server.md` §3 の `account_type` と同じ扱い）。
fn direction_label(direction: TaxDirection) -> &'static str {
    match direction {
        TaxDirection::Sales => "売上側",
        TaxDirection::Purchase => "仕入側",
        TaxDirection::None => "税額計算をしない",
    }
}

/// 成功応答の本文。
///
/// `settings` は**この帳簿の設定**（[`crate::tools::get_settings`] が返すのと
/// 同じ値）。候補の絞り込みには使わず、`filtered_by` に事実として並べる
/// （モジュール doc「根拠に帳簿の設定も並べる」）。綴りは `kaikei-jp` の
/// 入口（`TaxMode::as_code`）を通し、この層で表を作らない（D-072）。
fn success_body(
    date: &str,
    direction: Option<TaxDirection>,
    description: Option<&str>,
    table: &TaxCategoryTable,
    settings: JpSettings,
) -> Map<String, Value> {
    let candidates: Vec<Value> = table
        .categories()
        .filter(|category| direction.is_none_or(|wanted| category.direction == wanted))
        .map(|category| {
            let mut candidate = tax_category_to_json(category)
                .as_object()
                .cloned()
                .expect("tax_category_to_json はオブジェクトを返す");
            candidate.insert(
                "reason".to_string(),
                json!(reason_for(category, table, date)),
            );
            Value::Object(candidate)
        })
        .collect();

    let mut body = Map::new();
    body.insert("date".to_string(), json!(date));
    body.insert("table".to_string(), tax_table_to_json(table));
    // 何で絞ったかを必ず返す（0件だったときに「その日に区分が無い」のか
    // 「その向きの区分が無い」のかを応答だけで判断できるようにする）。
    //
    // 帳簿の設定は**絞り込みに使っていない**が、税額行が生成されるかどうかを
    // 決めるのはこの3つである。値を出さずに「税率は 0.10 です」だけを述べると、
    // 免税事業者の帳簿でも課税事業者と同一の応答になる（PR-G レビュー C-1）。
    body.insert(
        "filtered_by".to_string(),
        json!({
            "direction": direction.map(|d| d.as_code()),
            "description_used_for_filtering": false,
            "tax_mode": settings.tax_mode.as_code(),
            "is_taxable_business": settings.is_taxable_business,
            "simplified_taxation": settings.simplified_taxation,
            "book_settings_used_for_filtering": false,
        }),
    );
    if let Some(description) = description {
        body.insert("description".to_string(), json!(description));
    }
    body.insert("count".to_string(), json!(candidates.len()));
    body.insert("candidates".to_string(), Value::Array(candidates));
    body.insert("disclaimer".to_string(), json!(DISCLAIMER));
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_core::{AccountingDate, RoundMode};
    use kaikei_jp::tax::{RoundingUnit, TaxMode, TaxRuleSets};

    /// 課税事業者・税抜経理の帳簿（`get_settings` の既定の検証と同じ値）。
    fn taxable_settings() -> JpSettings {
        JpSettings {
            tax_mode: TaxMode::Exclusive,
            rounding: RoundMode::Floor,
            rounding_unit: RoundingUnit::Line,
            is_taxable_business: true,
            simplified_taxation: false,
        }
    }

    /// 免税事業者の帳簿（この設定では税額行が1行も生成されない）。
    fn tax_exempt_settings() -> JpSettings {
        JpSettings {
            is_taxable_business: false,
            ..taxable_settings()
        }
    }

    fn body_with(
        direction: Option<TaxDirection>,
        description: Option<&str>,
        settings: JpSettings,
    ) -> Value {
        let rule_sets = TaxRuleSets::from_embedded().expect("同梱マスタは読める");
        let date = AccountingDate::new(2026, 4, 15).unwrap();
        let table = rule_sets
            .require_for_date(date)
            .expect("同梱されている期間");
        Value::Object(success_body(
            &date.to_iso_string(),
            direction,
            description,
            table,
            settings,
        ))
    }

    fn body_for(direction: Option<TaxDirection>, description: Option<&str>) -> Value {
        body_with(direction, description, taxable_settings())
    }

    fn candidates(body: &Value) -> &Vec<Value> {
        body["candidates"].as_array().expect("配列")
    }

    // MC-08 (1): **根拠が空でない**。しかも候補ごとに付く。
    #[test]
    fn every_candidate_carries_a_non_empty_reason() {
        let body = body_for(None, None);
        let candidates = candidates(&body);
        assert!(!candidates.is_empty(), "{body}");
        for candidate in candidates {
            let reason = candidate["reason"].as_str().expect("reason は文字列");
            assert!(!reason.trim().is_empty(), "{candidate}");
            // 根拠は「どのマスタの、どういう登録か」という検証可能な事実である。
            assert!(reason.contains("2026-04-15"), "{reason}");
            assert!(reason.contains("登録されています"), "{reason}");
        }
    }

    // ★断定しない★ 1件に絞らず、順位も信頼度も付けない。
    #[test]
    fn the_response_does_not_single_out_one_category_or_rank_them() {
        let body = body_for(None, None);

        assert!(
            candidates(&body).len() > 1,
            "候補が1件に絞られています（提案系は候補を並べる）: {body}"
        );
        // 順位・信頼度・推奨に相当するキーを持たない。
        for forbidden in [
            "recommended",
            "confidence",
            "best",
            "rank",
            "score",
            "selected",
        ] {
            assert!(body.get(forbidden).is_none(), "{forbidden}: {body}");
            for candidate in candidates(&body) {
                assert!(
                    candidate.get(forbidden).is_none(),
                    "{forbidden}: {candidate}"
                );
            }
        }
        // 確定を呼び出し元に残す文言が入っている（`CLAUDE.md` §10）。
        let disclaimer = body["disclaimer"].as_str().unwrap();
        assert!(
            disclaimer.contains("判断はこのサーバーでは行いません"),
            "{disclaimer}"
        );
    }

    // 摘要は絞り込みに使わない（使っていないことを応答で明示する）。
    #[test]
    fn the_description_is_echoed_but_never_used_to_filter() {
        let with_text = body_for(None, Some("ｶ)ｻﾝﾌﾟﾙ ｼﾖｳｼﾞ"));
        let without_text = body_for(None, None);

        assert_eq!(with_text["description"], json!("ｶ)ｻﾝﾌﾟﾙ ｼﾖｳｼﾞ"));
        assert_eq!(
            with_text["candidates"], without_text["candidates"],
            "摘要で候補が変わっています（推論を書いてしまっています）"
        );
        assert_eq!(
            with_text["filtered_by"]["description_used_for_filtering"],
            json!(false)
        );
        assert!(without_text.get("description").is_none());
    }

    // 向きで絞れる。何で絞ったかが応答に残る。
    #[test]
    fn the_direction_filter_narrows_the_candidates_and_is_reported_back() {
        let sales = body_for(Some(TaxDirection::Sales), None);

        assert_eq!(sales["filtered_by"]["direction"], json!("sales"));
        assert!(!candidates(&sales).is_empty());
        for candidate in candidates(&sales) {
            assert_eq!(candidate["direction"], json!("sales"), "{candidate}");
        }
        assert!(
            candidates(&sales).len() < candidates(&body_for(None, None)).len(),
            "絞り込みが効いていない"
        );
    }

    // 絞り込まなければ全ての向きが混ざる（上の検査が自明に緑でないこと）。
    #[test]
    fn without_a_direction_filter_all_directions_appear() {
        let body = body_for(None, None);
        let directions: Vec<&str> = candidates(&body)
            .iter()
            .map(|c| c["direction"].as_str().unwrap())
            .collect();
        assert!(directions.contains(&"sales"), "{directions:?}");
        assert!(directions.contains(&"purchase"), "{directions:?}");
    }

    // 向きの綴りが違えば、有効な値を列挙して拒否する（既定に落とさない）。
    #[test]
    fn an_unknown_direction_is_rejected_listing_the_valid_values() {
        let error = TaxDirection::from_code("うりあげ")
            .map_err(|e| in_field("direction", ToolError::from_jp_error(&e)))
            .expect_err("未知の向きは拒否される");
        assert_eq!(error.code(), "invalid_setting_code");
        let message = error.message();
        assert!(message.starts_with("direction: "), "{message}");
        assert!(message.contains("sales"), "{message}");
    }

    // ★PR-G レビュー C-1★ 帳簿の設定が応答に出る。
    //
    // 免税事業者の帳簿では、候補の区分で記帳しても税額行は1行も生成されない。
    // その事実を応答から読めるようにする（候補そのものは絞らない——どの区分を
    // 使うかの判断はこのサーバーが行わないため）。
    #[test]
    fn the_book_settings_that_decide_whether_tax_lines_appear_are_reported_back() {
        let taxable = body_for(None, None);
        let exempt = body_with(None, None, tax_exempt_settings());

        assert_eq!(taxable["filtered_by"]["tax_mode"], json!("exclusive"));
        assert_eq!(taxable["filtered_by"]["is_taxable_business"], json!(true));
        assert_eq!(taxable["filtered_by"]["simplified_taxation"], json!(false));

        // ★免税事業者の帳簿の応答が課税事業者と同一にならない★
        assert_ne!(
            taxable["filtered_by"], exempt["filtered_by"],
            "帳簿の設定が応答に出ていません: {exempt}"
        );
        assert_eq!(exempt["filtered_by"]["is_taxable_business"], json!(false));

        // ただし**候補は絞らない**（1件に絞らない・順位を付けないのと同じ理由）。
        assert_eq!(
            taxable["candidates"], exempt["candidates"],
            "帳簿の設定で候補を絞っています（業務判断を MCP 層に書いています）"
        );
        assert_eq!(
            exempt["filtered_by"]["book_settings_used_for_filtering"],
            json!(false)
        );

        // 「設定は見ていない」ことが `disclaimer` にも書いてある
        // （否定しているのが「文面からの推論」だけ、という状態にしない）。
        let disclaimer = exempt["disclaimer"].as_str().unwrap();
        assert!(disclaimer.contains("is_taxable_business"), "{disclaimer}");
        assert!(
            disclaimer.contains("税額の行が生成されないことがあります"),
            "{disclaimer}"
        );
    }

    // 綴りは `kaikei-jp` の入口と一致する（この層で表を作っていない）。
    #[test]
    fn the_tax_mode_code_comes_from_the_frozen_vocabulary() {
        let inclusive = JpSettings {
            tax_mode: TaxMode::Inclusive,
            ..taxable_settings()
        };
        let body = body_with(None, None, inclusive);
        assert_eq!(
            body["filtered_by"]["tax_mode"],
            json!(TaxMode::Inclusive.as_code())
        );
    }

    // 説明文が `CLAUDE.md` §10 の禁止表現を含まず、断定もしない。
    #[test]
    fn the_description_avoids_forbidden_claims_and_leaves_the_decision_to_the_caller() {
        let description = SuggestTaxCategory::DESCRIPTION;
        for forbidden in ["準拠", "法令対応", "JIIMA"] {
            assert!(!description.contains(forbidden), "{forbidden}");
        }
        assert!(description.contains("候補と根拠"));
        assert!(description.contains("決めません"));
        assert!(description.contains("帳簿は一切変更しません"));
        // 帳簿の設定で絞っていないことを説明文でも述べる（C-1）。
        assert!(description.contains("filtered_by"), "{description}");
        assert!(description.contains("簡易課税"), "{description}");
    }
}
