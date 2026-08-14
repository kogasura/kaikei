//! `list_pending_transactions` — 取り込んだ明細のうち、まだ仕訳になっていない
//! ものを返す（`docs/05-csv-import.md` §3・§6）。
//!
//! # これは仕訳ではない
//!
//! 返すのは銀行・カードの明細であって、仕訳ではない。借方も貸方も勘定科目も
//! 無い——あるのは「口座から見た入金か出金か」だけである（§1）。**どの科目に
//! 立てるかを決めるのがこの後の仕事**であり、それをここで先取りしない。
//!
//! # 0 件は正常。ただし2つの意味がある
//!
//! 未処理が0件なのは、全部片付いたのかもしれないし、そもそも1件も取り込んで
//! いないのかもしれない。**前者は喜ぶところだが、後者は CSV を流し忘れて
//! いる**ということで、確定申告の直前にこれを取り違えると帳簿に丸ごと抜けが
//! できる。状態ごとの件数を添えて区別できるようにする。
//!
//! # 金額は文字列で返す
//!
//! `search_documents` と同じ理由（数値にすると読み直しで誤差が出る）。

use crate::dispatch::{McpTool, ToolContext, ToolFailure, ToolSuccess};
use crate::error::ToolError;
use kaikei_app::error::codes;
use kaikei_app::ports::ImportedTxQuery;
use kaikei_app::view::{ImportedTxQuerySpec, ImportedTxView};
use kaikei_core::AccountingDate;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Map, Value};

/// 一度に返す上限。
///
/// 応答の量が帳簿の大きさに比例しないようにする（`docs/07-mcp-server.md` §5）。
const DEFAULT_LIMIT: u32 = 50;
const MAX_LIMIT: u32 = 200;

/// 状態の指定に使える値。
///
/// **`ImportedTxQuerySpec::status` は文字列を素通しする**ので、ここで語彙を
/// 検査する。検査しないと `Pending`（大文字始まり）のような惜しい指定が
/// 黙って0件になり、「未処理は無い」と読み違える。
const KNOWN_STATUSES: [&str; 3] = ["pending", "journalized", "ignored"];

/// `list_pending_transactions`。
pub struct ListPendingTransactions;

// ★この構造体の doc コメントは `tools/list` の応答に出る★
/// 取り込んだ明細の絞り込み条件。指定していないキーは受け付けません。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListPendingTransactionsInput {
    /// 取り込み元（mizuho_business など。完全一致）。
    #[serde(default)]
    pub source: Option<String>,
    /// 状態。pending / journalized / ignored。既定は pending。
    #[serde(default)]
    pub status: Option<String>,
    /// 取引年月日の下限（この日を含む）。YYYY-MM-DD。
    #[serde(default)]
    pub date_from: Option<String>,
    /// 取引年月日の上限（この日を含む）。YYYY-MM-DD。
    #[serde(default)]
    pub date_to: Option<String>,
    /// 取得件数。既定は 50、最大 200。
    #[serde(default)]
    pub limit: Option<u32>,
}

impl McpTool for ListPendingTransactions {
    type Input = ListPendingTransactionsInput;

    const NAME: &'static str = "list_pending_transactions";

    const DESCRIPTION: &'static str = "\
銀行・カードから取り込んだ明細のうち、まだ仕訳になっていないものを返します。\
既定では未処理（pending）だけを、取引年月日の古い順に返します。\
返すのは明細であって仕訳ではありません。借方・貸方・勘定科目は含まれません。\
あるのは入金か出金か（is_money_in）だけで、どの科目に立てるかはこれから決めます。\
金額は常に正の値で、文字列で返します（数値にすると読み直しで誤差が出るため）。\
条件に合う明細が無い場合は空の配列を返します。これは異常ではありません。\
状態ごとの件数を counts に添えるので、\
「全部片付いた」のか「まだ1件も取り込んでいない」のかを区別できます。\
counts の合計が 0 なら、まだ CSV を取り込んでいません（kaikei import を使います）。\
摘要（raw_description）は銀行の表記のままです。半角カナや略称が混ざります。";

    async fn run(ctx: &ToolContext<'_>, input: Self::Input) -> Result<ToolSuccess, ToolFailure> {
        let limit = input.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
        let status = validate_status(input.status)?;

        let date_from = parse_date(input.date_from.as_deref(), "date_from")?;
        let date_to = parse_date(input.date_to.as_deref(), "date_to")?;
        // **期間の逆指定を黙って 0 件にしない。** 指定を間違えたのか本当に
        // 無いのかを、AI が区別できるようにする。
        if let (Some(from), Some(to)) = (date_from, date_to) {
            if from > to {
                return Err(ToolError::new(
                    codes::INVALID_VALUE,
                    format!(
                        "date_from（{}）が date_to（{}）より後です。入れ替えるか、正しい期間を指定してください",
                        from.to_iso_string(),
                        to.to_iso_string()
                    ),
                )
                .into());
            }
        }

        let spec = ImportedTxQuerySpec {
            source: input.source.clone(),
            status: Some(status.clone()),
            date_from,
            date_to,
        };

        let query = ctx.imported_tx_query();
        let found = query
            .list_imported(&spec, limit)
            .await
            .map_err(|error| ToolFailure::from(ToolError::from_app_error(&error.into())))?;

        // 「全部片付いた」と「まだ取り込んでいない」を区別できるようにする。
        // **絞り込みには取り込み元だけを渡す**——期間や状態で絞った件数を
        // 返すと、それは一覧の件数と同じものになり、区別の役に立たない。
        let counts = query
            .import_status_counts(input.source.as_deref())
            .await
            .map_err(|error| ToolFailure::from(ToolError::from_app_error(&error.into())))?;

        let items: Vec<Value> = found.iter().map(to_json).collect();
        let mut body = Map::new();
        body.insert("transactions".to_string(), Value::Array(items));
        body.insert("count".to_string(), json!(found.len()));
        body.insert(
            "counts".to_string(),
            json!({
                "pending": counts.pending,
                "journalized": counts.journalized,
                "ignored": counts.ignored,
                "total": counts.total(),
            }),
        );
        body.insert("status".to_string(), json!(status));
        body.insert("limit".to_string(), json!(limit));
        Ok(ToolSuccess::new(body))
    }
}

/// 状態の語彙を検査する。
///
/// 惜しい指定（`Pending`）を黙って 0 件にすると、「未処理は無い」と
/// 読み違える。
fn validate_status(status: Option<String>) -> Result<String, ToolFailure> {
    let status = status.unwrap_or_else(|| "pending".to_string());
    if KNOWN_STATUSES.contains(&status.as_str()) {
        return Ok(status);
    }
    Err(ToolError::new(
        codes::INVALID_VALUE,
        format!(
            "status は {} のいずれかで指定してください（受け取った値: {status}）",
            KNOWN_STATUSES.join(" / ")
        ),
    )
    .into())
}

fn to_json(tx: &ImportedTxView) -> Value {
    json!({
        "id": tx.id,
        "source": tx.source,
        "occurred_on": tx.occurred_on.to_iso_string(),
        // **金額は文字列で、常に正。** 向きは is_money_in が表す。
        "amount_minor": tx.amount_minor.to_string(),
        "is_money_in": tx.is_money_in,
        "raw_description": tx.raw_description,
        // 残高の無い明細があるので null を許す（0 で埋めない）。
        "balance_after": tx.balance_after.map(|value| value.to_string()),
        "status": tx.status,
        "entry_id": tx.entry_id,
        "ignore_reason": tx.ignore_reason,
    })
}

fn parse_date(text: Option<&str>, field: &str) -> Result<Option<AccountingDate>, ToolFailure> {
    match text {
        None => Ok(None),
        Some(text) => AccountingDate::parse(text).map(Some).map_err(|error| {
            ToolError::new(
                codes::INVALID_VALUE,
                format!("{field} は YYYY-MM-DD の形式で指定してください（受け取った値: {text}。{error}）"),
            )
            .into()
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_status_is_pending() {
        assert_eq!(validate_status(None).unwrap(), "pending");
    }

    #[test]
    fn the_three_known_statuses_are_accepted() {
        for status in KNOWN_STATUSES {
            assert_eq!(validate_status(Some(status.to_string())).unwrap(), status);
        }
    }

    /// **本命。** 惜しい指定を黙って 0 件にしない。
    ///
    /// `Pending` が通ってしまうと、条件に合う明細が無いだけなのに
    /// 「未処理は無い」と読み違える。
    #[test]
    fn a_near_miss_status_is_rejected_instead_of_returning_nothing() {
        let failure = validate_status(Some("Pending".to_string())).expect_err("拒否されること");
        let message = format!("{failure:?}");
        assert!(message.contains("pending"), "{message}");
        assert!(message.contains("Pending"), "受け取った値を出す: {message}");
    }

    #[test]
    fn an_unknown_status_lists_what_is_allowed() {
        let failure = validate_status(Some("まだ".to_string())).expect_err("拒否されること");
        let message = format!("{failure:?}");
        for status in KNOWN_STATUSES {
            assert!(message.contains(status), "{status} が案内に無い: {message}");
        }
    }

    /// 説明に、これが仕訳ではないことが書いてある。
    ///
    /// 明細を仕訳と取り違えると、勘定科目が最初から決まっていると思って
    /// しまう。
    #[test]
    fn the_description_says_these_are_not_journal_entries() {
        let description = ListPendingTransactions::DESCRIPTION;
        assert!(description.contains("仕訳ではありません"), "{description}");
        assert!(description.contains("is_money_in"), "{description}");
    }

    /// 説明に、0件の読み方が書いてある。
    #[test]
    fn the_description_explains_how_to_read_an_empty_result() {
        let description = ListPendingTransactions::DESCRIPTION;
        assert!(description.contains("counts"), "{description}");
        assert!(description.contains("取り込んでいません"), "{description}");
    }
}
