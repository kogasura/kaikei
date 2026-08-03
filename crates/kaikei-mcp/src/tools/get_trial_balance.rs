//! `get_trial_balance` — 試算表（`docs/07-mcp-server.md` §3 / §10 MC-15）。
//!
//! # read model に直行する
//!
//! `CLAUDE.md` §6「read model は物理的に分離する。Repository を通さず SQL から
//! DTO へ直行する」。このツールは [`kaikei_app::usecase::report::execute`] を
//! 呼ぶだけで、`Tx`（[`kaikei_app::ports::JournalRepo`]）を一切開かない
//! （`docs/07-mcp-server.md` §4 の経路 (b)）。集計そのものは
//! `kaikei-store` の `query::PgTrialBalanceQuery`（SQL の `SUM`）が行う。
//!
//! # ここに書かない判定
//!
//! | 判定 | どこが行うか |
//! |---|---|
//! | `from > to` の拒否 | `report::execute`（`AppError::Rejected`。空の試算表として静かに成功させない） |
//! | `group_by` の `aggregatable` 検証と重複除去 | 同上（SQL に到達する前。`CLAUDE.md` §4） |
//! | 借方合計＝貸方合計の検算 | 同上（食い違えば `inconsistent`） |
//! | 0行でも通貨を名乗ること | `kaikei_app::view::TrialBalanceView`（帳簿通貨を明示的に保持する。D-074） |
//!
//! MCP 層がするのは日付とタグキーのパース、および `TrialBalanceView` の
//! 詰め替えだけである。
//!
//! # ★「未登録のキー」と「集計軸に使えないキー」を区別する★（PR-G レビュー C-2）
//!
//! `TagSchema::is_aggregatable` は**未登録のキーにも `false` を返す**。
//! したがって `report::execute` が返す `CoreError::NotAggregatable` の文言
//! 「集計軸に使えないタグキーです: {key}（aggregatable = false）」は、
//! 未登録のキーに対しては**成立していない事実**を述べる（`aggregatable` の
//! 宣言そのものが存在しない）。しかも Phase 3 の11ツールには有効なタグキーを
//! 一覧できるツールが無いので、AI はこのエラーを踏んだあと正しいキーに
//! 辿り着く手段が無い（`CLAUDE.md` §11）。
//!
//! 発生源（`kaikei-core` の `CoreError`）は凍結層なので触らない。代わりに
//! この層で次の3つを行う。
//!
//! 1. **登録されているかどうか**を先に見る（[`parse_group_by`]）。未登録なら
//!    `unknown_tag_key` として拒否する。**`aggregatable` の判定ではない**ので
//!    `report::execute` の検証と重複しない（登録済みのキーはそのまま素通しし、
//!    集計軸として妥当かは従来どおり `report::execute` が決める）
//! 2. どちらの拒否にも **`aggregatable: true` のキーの一覧**を添える
//!    （[`aggregatable_keys`]）
//! 3. [`GetTrialBalance::DESCRIPTION`] にそのキーを列挙する
//!    （`the_description_lists_exactly_the_aggregatable_keys` が同梱スキーマと
//!    突き合わせるので、`tags.yaml` を変えると落ちる）

use kaikei_app::amount::money_to_plain_string;
use kaikei_app::error::codes;
use kaikei_app::usecase::report::{self, ReportInput};
use kaikei_app::view::TrialBalanceView;
use kaikei_core::TagKey;
use kaikei_jp::tags::TagCatalog;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::dispatch::{McpTool, ToolContext, ToolFailure, ToolSuccess};
use crate::error::ToolError;
use crate::tools::{core_error, in_field, parse_date};

/// `get_trial_balance`。
pub struct GetTrialBalance;

// ★この構造体の doc コメントは `tools/list` の応答に出る★
// 内部設計書への参照・crate 名・Markdown の強調記法を書かないこと。
/// 試算表の集計条件。指定していないキーは受け付けません。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetTrialBalanceInput {
    /// 集計期間の開始日（取引日、この日を含む）。YYYY-MM-DD の形式で
    /// 指定します。必須です。
    pub from: String,

    /// 集計期間の終了日（取引日、この日を含む）。YYYY-MM-DD の形式で
    /// 指定します。必須です。開始日より前の日付を指定した場合はエラーに
    /// なります（0件の試算表としては扱いません）。
    pub to: String,

    /// 科目に加えて集計軸にするタグキー（例: ["counterparty"]）。
    /// 省略すると科目のみで集計します。指定できるのは集計軸として宣言されて
    /// いるタグキーだけで、その一覧はこのツールの説明文にあります。
    #[serde(default)]
    pub group_by: Vec<String>,
}

impl McpTool for GetTrialBalance {
    type Input = GetTrialBalanceInput;

    const NAME: &'static str = "get_trial_balance";

    const DESCRIPTION: &'static str = "\
指定した期間の試算表（科目別の借方合計・貸方合計・残高）を返します。\
from と to は取引日で、両端を含みます。どちらも必須です。\
記帳日ではなく取引日で絞り込みます。\
金額はすべて文字列で返します（例: \"110000\"。桁区切りは入りません）。\
balance は科目の借方・貸方のどちらが正常かに従った符号付きの残高で、\
負の値になることもあります。\
group_by に指定できるタグキーは counterparty, project, tax_category の3つです。\
仕訳に付けられるタグキーはこれ以外にもありますが、集計軸としては宣言されていません。\
帳簿に登録されていないタグキーを指定した場合と、登録されているが集計軸として\
宣言されていないタグキーを指定した場合とでは、返るエラーが異なります。\
どちらの場合も、指定できるタグキーの一覧が aggregatable_group_by_keys に返ります。\
該当する仕訳が1件も無い期間は、エラーではなく空の rows を返します。";

    async fn run(ctx: &ToolContext<'_>, input: Self::Input) -> Result<ToolSuccess, ToolFailure> {
        let from = parse_date("from", &input.from)?;
        let to = parse_date("to", &input.to)?;

        let composition = ctx.composition();
        let settings = ctx.book_settings();
        let group_by = parse_group_by(&composition.tag_catalog, &input.group_by)?;

        // read model に直行する（`Tx` を開かない。`CLAUDE.md` §6）。
        let view = report::execute(
            ctx.trial_balance_query(),
            composition.tag_catalog.schema(),
            &settings,
            ReportInput { from, to, group_by },
        )
        .await
        .map_err(|error| describe_failure(&composition.tag_catalog, &error))?;

        Ok(ToolSuccess::new(success_body(
            &input.from,
            &input.to,
            &view,
        )?))
    }
}

/// `aggregatable: true` が宣言されているタグキー（昇順）。
///
/// 一覧をこの層に**書き写さない**。同梱スキーマ（`tags.yaml` を読んだ
/// [`TagCatalog`]）から毎回導出する（`PROGRESS.md` Phase 1 の教訓6
/// 「手で維持する一覧は必ず腐る」）。
fn aggregatable_keys(catalog: &TagCatalog) -> Vec<&str> {
    let mut keys: Vec<&str> = catalog
        .defs()
        .iter()
        .filter(|(_, def)| def.aggregatable)
        .map(|(key, _)| key.as_str())
        .collect();
    keys.sort_unstable();
    keys
}

/// 線上のタグキーを [`TagKey`] にし、**帳簿に登録されているか**を確かめる。
///
/// # ここで見るのは「登録されているか」だけである
///
/// **集計軸として妥当かどうか（`aggregatable`）はここで見ない**
/// （`report::execute` が SQL 到達前に弾く。同じ検証を2箇所に置かない）。
/// 登録済みのキーはこの関数を素通りし、集計軸として使えるかは従来どおり
/// `report::execute` が決める。
///
/// 登録の有無を見るのは、`TagSchema::is_aggregatable` が両者を1つの `false` に
/// 潰してしまい、未登録のキーに対して「（aggregatable = false）」という
/// **成立していない事実**が返るためである（モジュール doc）。
/// この判定に使えるのは `TagCatalog`（`kaikei-jp`）だけで、`TagSchema` の
/// 公開 API では「登録されているか」を引き戻せない。
///
/// # Errors
///
/// - タグキーとして解釈できない文字列（`CoreError` をそのまま写す）
/// - 帳簿に登録されていないキー → [`codes::UNKNOWN_TAG_KEY`]
fn parse_group_by(catalog: &TagCatalog, keys: &[String]) -> Result<Vec<TagKey>, ToolError> {
    keys.iter()
        .map(|key| {
            let parsed =
                TagKey::parse(key).map_err(|error| in_field("group_by", core_error(error)))?;
            if catalog.def(&parsed).is_none() {
                return Err(unregistered_key(catalog, parsed.as_str()));
            }
            Ok(parsed)
        })
        .collect()
}

/// 帳簿に登録されていないタグキーを指定された（`aggregatable` 以前の話）。
///
/// **「集計軸に使えない」とは言わない。** そのキーには `aggregatable` の宣言
/// そのものが無く、次の手も違う（前者は綴りを直すか `tags.yaml` に登録する、
/// 後者は別の集計軸を選ぶ）。
fn unregistered_key(catalog: &TagCatalog, key: &str) -> ToolError {
    in_field(
        "group_by",
        ToolError::new(
            codes::UNKNOWN_TAG_KEY,
            format!(
                "タグキー \"{key}\" はこの帳簿に登録されていません\
                 （登録されているキー: {registered}）。\
                 集計軸に指定できるのは、そのうち集計軸として宣言されている\
                 キーだけです: {aggregatable}",
                registered = catalog.registered_keys_display(),
                aggregatable = aggregatable_keys(catalog).join(", "),
            ),
        ),
    )
    .with_detail(
        "aggregatable_group_by_keys",
        json!(aggregatable_keys(catalog)),
    )
}

/// `report::execute` が返した失敗を応答にする。
///
/// 集計軸として使えないキーだった場合（`CoreError::NotAggregatable`）は、
/// **文言を言い換えずに**選べるキーの一覧を添える。Phase 3 の11ツールには
/// 有効なタグキーを一覧できるツールが無いので、これが無いと AI は次の手を
/// 持てない（`CLAUDE.md` §11）。
fn describe_failure(catalog: &TagCatalog, error: &kaikei_app::error::AppError) -> ToolError {
    let tool_error = ToolError::from_app_error(error);
    if tool_error.code() == codes::NOT_AGGREGATABLE {
        return in_field("group_by", tool_error).with_detail(
            "aggregatable_group_by_keys",
            json!(aggregatable_keys(catalog)),
        );
    }
    tool_error
}

/// 成功応答の本文（`docs/07-mcp-server.md` §3）。
///
/// 金額はすべて**区切り無しの文字列**（同 §5。整形は
/// [`money_to_plain_string`] に委ね、ここで `format!` を書かない）。
///
/// `from` / `to` は**呼び出し元が送った文字列をそのまま**返す。
/// [`kaikei_core::AccountingDate::parse`] を通った時点で ISO 表記であることは
/// 確定しており、`to_iso_string` で往復させても同じ値になる。入力を素直に
/// 写すことで、「集計期間を取り違えていないか」を呼び出し元が自分の要求と
/// 突き合わせて確認できる。
///
/// # Errors
///
/// 合計の算出に失敗した場合（帳簿通貨と異なる通貨の行が混ざっている等）。
/// `report::execute` が既に検算済みなので通常は起こらない。
fn success_body(
    from: &str,
    to: &str,
    view: &TrialBalanceView,
) -> Result<Map<String, Value>, ToolError> {
    let (debit_total, credit_total) = view.totals().map_err(core_error)?;

    let rows: Vec<Value> = view
        .rows()
        .iter()
        .map(|row| {
            let mut group = Map::new();
            for (key, value) in &row.group {
                group.insert(key.clone(), json!(value));
            }
            json!({
                "account": row.account.as_str(),
                "account_type": kaikei_app::wire::account_type_code(row.account_type),
                "group": Value::Object(group),
                "debit_total": money_to_plain_string(&row.debit_total),
                "credit_total": money_to_plain_string(&row.credit_total),
                "balance": money_to_plain_string(&row.balance),
            })
        })
        .collect();

    let mut body = Map::new();
    body.insert("from".to_string(), json!(from));
    body.insert("to".to_string(), json!(to));
    // ★0行でも通貨を必ず返す★（`TrialBalanceView` は帳簿通貨を保持しており、
    // 行から推論していない。`docs/07-mcp-server.md` §3）。
    body.insert("currency".to_string(), json!(view.currency().code()));
    body.insert(
        "debit_total".to_string(),
        json!(money_to_plain_string(&debit_total)),
    );
    body.insert(
        "credit_total".to_string(),
        json!(money_to_plain_string(&credit_total)),
    );
    body.insert("rows".to_string(), Value::Array(rows));
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_app::view::{BalanceRowView, GroupKeyView};
    use kaikei_core::{AccountCode, AccountType, Currency, Money};

    fn row(
        account: &str,
        account_type: AccountType,
        debit: i128,
        credit: i128,
        group: GroupKeyView,
    ) -> BalanceRowView {
        let debit_total = Money::from_minor(debit, Currency::JPY);
        let credit_total = Money::from_minor(credit, Currency::JPY);
        let balance = if account_type.is_debit_normal() {
            debit_total.sub(&credit_total).unwrap()
        } else {
            credit_total.sub(&debit_total).unwrap()
        };
        BalanceRowView {
            account: AccountCode::parse(account).unwrap(),
            account_type,
            group,
            debit_total,
            credit_total,
            balance,
        }
    }

    fn body_of(view: &TrialBalanceView) -> Value {
        Value::Object(success_body("2026-01-01", "2026-12-31", view).unwrap())
    }

    // 設計書 §3 の応答の形（金額は区切り無しの**文字列**）。
    #[test]
    fn the_documented_response_shape_uses_plain_amount_strings() {
        let mut group = GroupKeyView::new();
        group.insert("counterparty".to_string(), "CP0001".to_string());
        let view = TrialBalanceView::new(
            vec![
                row("100", AccountType::Asset, 1_100, 0, group.clone()),
                row("500", AccountType::Revenue, 0, 1_000, group.clone()),
                row("330", AccountType::Liability, 0, 100, group),
            ],
            Currency::JPY,
        );

        let body = body_of(&view);
        assert_eq!(body["from"], json!("2026-01-01"));
        assert_eq!(body["to"], json!("2026-12-31"));
        assert_eq!(body["currency"], json!("JPY"));
        assert_eq!(body["debit_total"], json!("1100"));
        assert_eq!(body["credit_total"], json!("1100"));

        let rows = body["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["account"], json!("100"));
        assert_eq!(rows[0]["account_type"], json!("asset"));
        assert_eq!(rows[0]["group"]["counterparty"], json!("CP0001"));
        assert_eq!(rows[0]["balance"], json!("1100"));
        // MC-27: 出力の金額は全て JSON 文字列（number にしない）。
        for row in rows {
            for key in ["debit_total", "credit_total", "balance"] {
                assert!(row[key].is_string(), "{key} が文字列ではない: {row}");
            }
        }
        assert!(body["debit_total"].is_string());
    }

    // 残高は負にもなりうる（符号付きで返す）。
    #[test]
    fn a_negative_balance_is_returned_with_its_sign() {
        let view = TrialBalanceView::new(
            vec![row(
                "100",
                AccountType::Asset,
                0,
                1_100,
                GroupKeyView::new(),
            )],
            Currency::JPY,
        );
        assert_eq!(body_of(&view)["rows"][0]["balance"], json!("-1100"));
    }

    // 0行の期間でも**通貨を名乗り**、合計は "0"（空の成功であってエラーではない）。
    #[test]
    fn an_empty_period_still_names_the_currency_and_totals_zero() {
        let body = body_of(&TrialBalanceView::new(Vec::new(), Currency::JPY));
        assert_eq!(body["rows"], json!([]));
        assert_eq!(body["currency"], json!("JPY"));
        assert_eq!(body["debit_total"], json!("0"));
        assert_eq!(body["credit_total"], json!("0"));
    }

    // `group_by` を指定しなければ `group` は空オブジェクト（`null` にしない）。
    #[test]
    fn a_row_without_grouping_carries_an_empty_group_object() {
        let view = TrialBalanceView::new(
            vec![row("100", AccountType::Asset, 1, 0, GroupKeyView::new())],
            Currency::JPY,
        );
        assert_eq!(body_of(&view)["rows"][0]["group"], json!({}));
    }

    /// 同梱スキーマ（`kaikei-jp-data/tags.yaml`）。合成ルートが
    /// `Composition::tag_catalog` に載せるのと同じ値である
    /// （この層は `kaikei-jp-data` を直接依存に持てないので `bundled` を通す）。
    fn catalog() -> TagCatalog {
        TagCatalog::bundled().expect("同梱スキーマは読める")
    }

    // タグキーとして解釈できない文字列は、どの欄が悪いかを添えて拒否する。
    #[test]
    fn an_unparsable_group_by_key_names_the_field() {
        let error = parse_group_by(&catalog(), &["".to_string()]).unwrap_err();
        assert!(
            error.message().starts_with("group_by: "),
            "{}",
            error.message()
        );
    }

    // 集計軸の妥当性検証をこの層に持たない（**登録済みの**キーは素通しする）。
    // `business_ratio` は登録済みだが `aggregatable: false` であり、
    // ここでは弾かれず `report::execute` が弾く。
    #[test]
    fn registered_keys_pass_through_without_aggregatable_checks_here() {
        let keys = parse_group_by(
            &catalog(),
            &[
                "counterparty".to_string(),
                "project".to_string(),
                "business_ratio".to_string(),
            ],
        )
        .unwrap();
        assert_eq!(
            keys.iter().map(TagKey::as_str).collect::<Vec<_>>(),
            vec!["counterparty", "project", "business_ratio"]
        );
    }

    // ★PR-G レビュー C-2★ **未登録のキー**と
    // **登録済みだが集計軸に使えないキー**が別のエラーになる。
    //
    // `TagSchema::is_aggregatable` はどちらにも `false` を返すので、
    // 素直に `report::execute` へ流すと、未登録のキーに対して
    // 「（aggregatable = false）」という成立していない事実を述べてしまう。
    #[test]
    fn an_unregistered_key_is_not_reported_as_a_non_aggregatable_one() {
        let catalog = catalog();
        let error = parse_group_by(&catalog, &["memo".to_string()]).unwrap_err();

        assert_eq!(error.code(), kaikei_app::error::codes::UNKNOWN_TAG_KEY);
        let message = error.message();
        assert!(message.starts_with("group_by: "), "{message}");
        assert!(message.contains("登録されていません"), "{message}");
        // ★成立していない事実を述べない★
        assert!(!message.contains("aggregatable = false"), "{message}");
        // 次の手（`CLAUDE.md` §11）。有効なキーに辿り着ける。
        assert!(message.contains("counterparty"), "{message}");
        assert_eq!(
            error.to_json()["aggregatable_group_by_keys"],
            json!(["counterparty", "project", "tax_category"])
        );
    }

    // 登録済みだが集計軸に使えないキーは、`kaikei-core` の文言のまま
    // （言い換えない）返り、選べるキーの一覧が添う。
    #[test]
    fn a_registered_but_non_aggregatable_key_keeps_the_core_wording_and_gains_candidates() {
        let catalog = catalog();
        let error = describe_failure(
            &catalog,
            &kaikei_app::error::AppError::Core(kaikei_core::CoreError::NotAggregatable {
                key: "business_ratio".to_string(),
            }),
        );

        assert_eq!(error.code(), kaikei_app::error::codes::NOT_AGGREGATABLE);
        let message = error.message();
        assert!(message.starts_with("group_by: "), "{message}");
        // 下位層の文言はそのまま（ここでは事実として成立している）。
        assert!(message.contains("aggregatable = false"), "{message}");
        assert_eq!(
            error.to_json()["aggregatable_group_by_keys"],
            json!(["counterparty", "project", "tax_category"])
        );
    }

    // 集計軸に使えるキーの一覧を同梱スキーマから導出している
    // （この層に書き写していない）。
    #[test]
    fn the_aggregatable_keys_are_derived_from_the_bundled_schema() {
        let catalog = catalog();
        let keys = aggregatable_keys(&catalog);
        assert_eq!(keys, vec!["counterparty", "project", "tax_category"]);
        for (key, def) in catalog.defs() {
            assert_eq!(
                keys.contains(&key.as_str()),
                def.aggregatable,
                "{}",
                key.as_str()
            );
        }
    }

    // ★説明文に有効なキーを列挙する★（C-2 の (c)）
    //
    // Phase 3 の11ツールには有効なタグキーを一覧できるツールが無いので、
    // AI が最初に読む面に書いておく。`tags.yaml` を変えるとここが落ちる。
    #[test]
    fn the_description_lists_exactly_the_aggregatable_keys() {
        let catalog = catalog();
        let description = GetTrialBalance::DESCRIPTION;
        for (key, def) in catalog.defs() {
            assert_eq!(
                description.contains(key.as_str()),
                def.aggregatable,
                "説明文と tags.yaml が食い違っています（{}）: {description}",
                key.as_str()
            );
        }
    }

    // 既定は「科目のみで集計」。
    #[test]
    fn group_by_defaults_to_empty() {
        let input: GetTrialBalanceInput =
            serde_json::from_str(r#"{"from":"2026-01-01","to":"2026-12-31"}"#).unwrap();
        assert!(input.group_by.is_empty());
    }

    // 説明文が `CLAUDE.md` §10 の禁止表現を含まず、§5 / §11 の要点を含む。
    #[test]
    fn the_description_avoids_forbidden_claims_and_states_the_next_step() {
        let description = GetTrialBalance::DESCRIPTION;
        for forbidden in ["準拠", "法令対応", "JIIMA"] {
            assert!(!description.contains(forbidden), "{forbidden}");
        }
        assert!(description.contains("文字列"));
        assert!(description.contains("取引日"));
    }
}
