//! `list_tax_categories` — 指定日時点で有効な消費税区分の一覧
//! （`docs/07-mcp-server.md` §2 / §10 MC-18）。
//!
//! # `kaikei-app` を経由しない（経路 (c)）
//!
//! 税区分マスタを保持しているのは `kaikei-jp` であり、帳簿にも DB にも
//! 触らない問い合わせである（同 §4 の経路表）。合成ルートが起動時に組み立てた
//! [`kaikei_jp::tax::TaxRuleSets`] をそのまま引く。
//!
//! # 空配列と「未収録」は意味が違う
//!
//! どのマスタの適用期間にも入らない取引日は**エラー**にする
//! （`no_applicable_rule_set`）。空配列を返すと、AI は「この日は税区分が
//! 1つも無い」と誤解して税区分なしで記帳しようとする（同 §2）。
//!
//! **文言を MCP 層で書き起こさない。** 有効期間を含むエラーは
//! [`kaikei_jp::tax::TaxRuleSets::require_for_date`] が組み立てる
//! （`for_date` は `Option` のまま。`DECISIONS.md` D-055 / D-072）。

use kaikei_jp::tax::{TaxCategory, TaxCategoryTable};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::dispatch::{McpTool, ToolContext, ToolFailure, ToolSuccess};
use crate::error::ToolError;
use crate::tools::parse_date;

/// `list_tax_categories`。
pub struct ListTaxCategories;

// ★この構造体の doc コメントは `tools/list` の応答に出る★
// 内部設計書への参照・crate 名・Markdown の強調記法を書かないこと。
/// 税区分一覧の取得条件。指定していないキーは受け付けません。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListTaxCategoriesInput {
    /// 取引日。YYYY-MM-DD の形式で指定します。適用される税区分は取引日で
    /// 決まるため必須です。記帳した日ではありません。
    pub date: String,
}

impl McpTool for ListTaxCategories {
    type Input = ListTaxCategoriesInput;

    const NAME: &'static str = "list_tax_categories";

    const DESCRIPTION: &'static str = "\
指定した取引日の時点で有効な消費税区分の一覧を返します。\
区分コード（code）は仕訳明細の tags の tax_category にそのまま指定できます。\
各区分について、名称・向き（direction: sales / purchase / none）・税率（rate）・\
適格請求書の保存が必要かどうか・税額の計上先科目・マスタの注記を返します。\
適用される区分は取引日で切り替わるため、date には記帳した日ではなく取引日を指定します。\
その日を含むマスタが同梱されていない場合は、同梱している期間を示すエラーを返します。\
どの区分を使うかの判断はこのサーバーでは行いません。";

    async fn run(ctx: &ToolContext<'_>, input: Self::Input) -> Result<ToolSuccess, ToolFailure> {
        let date = parse_date("date", &input.date)?;
        let composition = ctx.composition();

        // 該当なしをエラーにする入口を通す（文言はここで書き起こさない）。
        let table = composition
            .tax_policy
            .rule_sets()
            .require_for_date(date)
            .map_err(|error| ToolError::from_jp_error(&error))?;

        Ok(ToolSuccess::new(success_body(&input.date, table)))
    }
}

/// 税区分1件を線上の JSON にする。
///
/// **マスタに書かれている値をそのまま運ぶ**（`CLAUDE.md` §10。税率も控除割合も
/// 注記もこの層では解釈しない）。値が無い項目は**キーごと出さない**——
/// `deductible: null` と `deductible: false` は意味が違い、`null` を置くと
/// 呼び出し元が「控除できない」と読みうる。
/// `direction` は [`kaikei_jp::tax::TaxDirection::as_code`]（綴りをこの層で
/// 作らない。`DECISIONS.md` D-072）。
pub(crate) fn tax_category_to_json(category: &TaxCategory) -> Value {
    let mut object = Map::new();
    object.insert("code".to_string(), json!(category.code));
    object.insert("label".to_string(), json!(category.label));
    object.insert("direction".to_string(), json!(category.direction.as_code()));
    if let Some(rate) = category.rate {
        // 税率は number にしない（金額と同じく倍精度に丸めさせない。§5）。
        object.insert("rate".to_string(), json!(rate.as_decimal().to_string()));
    }
    if let Some(deductible) = category.deductible {
        object.insert("deductible".to_string(), json!(deductible));
    }
    if let Some(ratio) = category.deduction_ratio {
        object.insert(
            "deduction_ratio".to_string(),
            json!(ratio.as_decimal().to_string()),
        );
    }
    object.insert(
        "requires_qualified_invoice".to_string(),
        json!(category.requires_qualified_invoice),
    );
    if let Some(account) = &category.tax_account {
        object.insert("tax_account".to_string(), json!(account.as_str()));
    }
    if let Some(note) = &category.note {
        object.insert("note".to_string(), json!(note));
    }
    Value::Object(object)
}

/// マスタ（適用期間1つぶん）の素性。
///
/// **どのマスタを見た結果なのかを必ず返す。** 取引日で切り替わる以上、
/// 「いつ時点の一覧か」が応答から読めないと、AI は別の年度の区分を
/// そのまま使い回してしまう。
pub(crate) fn tax_table_to_json(table: &TaxCategoryTable) -> Value {
    let mut object = Map::new();
    object.insert("label".to_string(), json!(table.label()));
    object.insert(
        "applies_from".to_string(),
        json!(table.applies_from().to_iso_string()),
    );
    if let Some(applies_to) = table.applies_to() {
        object.insert("applies_to".to_string(), json!(applies_to.to_iso_string()));
    }
    object.insert("range".to_string(), json!(table.range_display()));
    Value::Object(object)
}

/// 成功応答の本文。
fn success_body(date: &str, table: &TaxCategoryTable) -> Map<String, Value> {
    let categories: Vec<Value> = table.categories().map(tax_category_to_json).collect();

    let mut body = Map::new();
    body.insert("date".to_string(), json!(date));
    body.insert("table".to_string(), tax_table_to_json(table));
    body.insert("count".to_string(), json!(categories.len()));
    body.insert("categories".to_string(), Value::Array(categories));
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_core::AccountingDate;
    use kaikei_jp::tax::TaxRuleSets;

    fn embedded() -> TaxRuleSets {
        TaxRuleSets::from_embedded().expect("同梱マスタは読める")
    }

    fn body_at(year: i32, month: u8, day: u8) -> Value {
        let rule_sets = embedded();
        let date = AccountingDate::new(year, month, day).unwrap();
        let table = rule_sets
            .require_for_date(date)
            .expect("同梱されている期間");
        Value::Object(success_body(&date.to_iso_string(), table))
    }

    // MC-18: 指定日時点で有効な区分だけを、どのマスタ由来かとあわせて返す。
    #[test]
    fn the_categories_valid_on_the_given_date_are_returned_with_their_source_table() {
        let body = body_at(2026, 4, 15);

        assert_eq!(body["date"], json!("2026-04-15"));
        assert!(!body["table"]["label"].as_str().unwrap().is_empty());
        assert!(body["table"]["applies_from"].is_string());
        assert!(!body["table"]["range"].as_str().unwrap().is_empty());

        let categories = body["categories"].as_array().unwrap();
        assert!(!categories.is_empty(), "同梱マスタに区分が無い: {body}");
        assert_eq!(body["count"], json!(categories.len()));
        for category in categories {
            assert!(category["code"].is_string(), "{category}");
            assert!(category["label"].is_string(), "{category}");
            // 綴りは `kaikei-jp` の語彙（この層で作っていない）。
            let direction = category["direction"].as_str().unwrap();
            assert!(
                ["sales", "purchase", "none"].contains(&direction),
                "{category}"
            );
            assert!(
                category["requires_qualified_invoice"].is_boolean(),
                "{category}"
            );
            // 税率は number にしない（§5）。
            if let Some(rate) = category.get("rate") {
                assert!(rate.is_string(), "{category}");
            }
        }
    }

    // 値が無い項目はキーごと出さない（`null` を「控除できない」と読ませない）。
    #[test]
    fn absent_optional_fields_are_omitted_rather_than_null() {
        let body = body_at(2026, 4, 15);
        for category in body["categories"].as_array().unwrap() {
            for key in [
                "rate",
                "deductible",
                "deduction_ratio",
                "tax_account",
                "note",
            ] {
                assert!(
                    !category[key].is_null() || category.get(key).is_none(),
                    "{key} に null が入っています: {category}"
                );
            }
        }
        // 対照実験: 少なくとも1件は省略された項目を持つ（全件が全項目を
        // 埋めていると、この検査は何も見ていないことになる）。
        let omitted = body["categories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|category| category.get("rate").is_none() || category.get("note").is_none());
        assert!(omitted, "省略された項目が1つも無い: {body}");
    }

    // 同梱していない日付は**空配列ではなくエラー**で、有効期間を示す。
    #[test]
    fn a_date_outside_the_embedded_masters_is_an_error_that_shows_the_available_range() {
        let rule_sets = embedded();
        // 同梱マスタは 2026-01-01 開始（`applies_to` は未指定なので**未来側は
        // 開いている**）。収録外になるのは開始日より前の日付である。
        let before_the_masters = AccountingDate::new(2000, 1, 1).unwrap();
        let error = rule_sets
            .require_for_date(before_the_masters)
            .map(|_| ())
            .expect_err("同梱していない日付はエラー");
        let tool_error = ToolError::from_jp_error(&error);

        assert_eq!(
            tool_error.code(),
            kaikei_app::error::codes::NO_APPLICABLE_RULE_SET
        );
        let message = tool_error.message();
        assert!(message.contains("2000-01-01"), "{message}");
        // 有効期間（次の手）が本文に含まれる（`CLAUDE.md` §11）。
        assert!(message.contains("2026"), "{message}");
    }

    // 取引日で切り替わることが応答から読める（`table` が日付で変わる）。
    //
    // 同梱マスタが1件だった頃、このテストは「ラベルが**同じ**」ことを見ていた。
    // 名前が言う「切り替わる」を実際には確かめていなかったわけで、マスタが
    // 2件以上ある今は本来の意図どおり**違う**ことを見る。2026年は経過措置の
    // 控除割合が 10/1 に変わるため、暦年の前半と後半で別のマスタが引かれる
    // （`DECISIONS.md` D-092）。
    #[test]
    fn the_source_table_is_selected_by_the_transaction_date() {
        let early = body_at(2026, 1, 1);
        let late = body_at(2026, 12, 31);
        assert_ne!(
            early["table"]["label"], late["table"]["label"],
            "取引日が違えば別のマスタが引かれるはず: {early} / {late}"
        );
        assert_eq!(early["date"], json!("2026-01-01"));
        assert_eq!(late["date"], json!("2026-12-31"));
        // 応答の適用期間も、引かれたマスタのものに揃っている。
        assert_eq!(early["table"]["applies_to"], json!("2026-09-30"));
        assert_eq!(late["table"]["applies_from"], json!("2026-10-01"));
    }

    // 説明文が `CLAUDE.md` §10 の禁止表現を含まず、断定もしない。
    #[test]
    fn the_description_avoids_forbidden_claims_and_leaves_the_choice_to_the_caller() {
        let description = ListTaxCategories::DESCRIPTION;
        for forbidden in ["準拠", "法令対応", "JIIMA"] {
            assert!(!description.contains(forbidden), "{forbidden}");
        }
        assert!(description.contains("取引日"));
        assert!(description.contains("判断はこのサーバーでは行いません"));
    }
}
