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

use kaikei_app::amount::money_to_plain_string;
use kaikei_app::usecase::report::{self, ReportInput};
use kaikei_app::view::TrialBalanceView;
use kaikei_core::TagKey;
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
    /// 省略すると科目のみで集計します。集計軸に使えるタグキーだけを
    /// 指定できます。
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
group_by には集計軸に使えるタグキーだけを指定できます。\
該当する仕訳が1件も無い期間は、エラーではなく空の rows を返します。";

    async fn run(ctx: &ToolContext<'_>, input: Self::Input) -> Result<ToolSuccess, ToolFailure> {
        let from = parse_date("from", &input.from)?;
        let to = parse_date("to", &input.to)?;
        let group_by = parse_group_by(&input.group_by)?;

        let composition = ctx.composition();
        let settings = ctx.book_settings();

        // read model に直行する（`Tx` を開かない。`CLAUDE.md` §6）。
        let view = report::execute(
            ctx.trial_balance_query(),
            composition.tag_catalog.schema(),
            &settings,
            ReportInput { from, to, group_by },
        )
        .await
        .map_err(|error| ToolError::from_app_error(&error))?;

        Ok(ToolSuccess::new(success_body(
            &input.from,
            &input.to,
            &view,
        )?))
    }
}

/// 線上のタグキーを [`TagKey`] にする。
///
/// **集計軸として妥当かどうか（`aggregatable`）はここで見ない**
/// （`report::execute` が SQL 到達前に弾く。同じ検証を2箇所に置かない）。
/// ここが返すのは「そもそもタグキーとして解釈できない文字列」だけである。
fn parse_group_by(keys: &[String]) -> Result<Vec<TagKey>, ToolError> {
    keys.iter()
        .map(|key| TagKey::parse(key).map_err(|error| in_field("group_by", core_error(error))))
        .collect()
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

    // タグキーとして解釈できない文字列は、どの欄が悪いかを添えて拒否する。
    #[test]
    fn an_unparsable_group_by_key_names_the_field() {
        let error = parse_group_by(&["".to_string()]).unwrap_err();
        assert!(
            error.message().starts_with("group_by: "),
            "{}",
            error.message()
        );
    }

    // 集計軸の妥当性検証をこの層に持たない（解釈できるキーは素通しする）。
    #[test]
    fn parsable_keys_pass_through_without_aggregatable_checks_here() {
        let keys = parse_group_by(&["counterparty".to_string(), "project".to_string()]).unwrap();
        assert_eq!(
            keys.iter().map(TagKey::as_str).collect::<Vec<_>>(),
            vec!["counterparty", "project"]
        );
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
