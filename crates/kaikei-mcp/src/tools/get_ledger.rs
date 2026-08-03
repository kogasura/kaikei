//! `get_ledger` — 総勘定元帳（科目別の明細）を返す（`docs/07-mcp-server.md` §3）。
//!
//! # このファイルに監査ログの手順は無い
//!
//! 開始レコード → 操作 → 結果レコードは [`crate::dispatch::call`] が行う
//! （`DECISIONS.md` D-084）。
//!
//! # 合計はページではなく期間全体のもの
//!
//! `opening_balance` / `debit_total` / `credit_total` / `closing_balance` /
//! `total_lines` はページングと無関係に**指定期間の全明細**から求める。
//! ページの行を足しても `debit_total` にはならない。
//! 行ごとの `running_balance` は期首残高からの累計なので、2ページ目の
//! 先頭行の残高も正しい。
//!
//! # 件数の上限と続きの取り方（`DECISIONS.md` D-089）
//!
//! `total_lines` / `returned` / `has_more` / `next_cursor` /
//! `truncation_note` の5つで「切れたかどうか」が必ず分かる
//! （`search_entries` と同じ形。黙って切らない）。
//!
//! # 空と「見つからない」を区別する
//!
//! 期間に明細が1行も無い科目は**成功**（合計は 0、`rows` は空）。
//! 勘定科目マスタに無い科目コードは `not_found` の**エラー**である。
//! 前者は期間を広げる、後者はコードを調べ直すという別の次の手になる
//! （判定は read model が行う。`crates/kaikei-store/src/query/ledger.rs`）。

use kaikei_app::error::AppError;
use kaikei_app::id::entry_id_to_uuid_string;
use kaikei_app::usecase::ledger::{self, LedgerInput, DEFAULT_LIMIT, MAX_LIMIT};
use kaikei_app::view::{LedgerPageView, LedgerRowView};
use kaikei_app::wire::{account_type_code, ledger_cursor_from_string, ledger_cursor_to_string};
use kaikei_core::AccountCode;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::dispatch::{McpTool, ToolContext, ToolFailure, ToolSuccess};
use crate::error::ToolError;
use crate::tools::{core_error, in_field, parse_date};
use crate::wire::{reversal_ref_to_json, tag_set_to_json, AmountStr};

/// `get_ledger`。
pub struct GetLedger;

// ★この構造体の doc コメントは `tools/list` の応答に出る★
// 内部設計書への参照・crate 名・Markdown の強調記法を書かないこと。
/// 総勘定元帳の取得条件。指定していないキーは受け付けません。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetLedgerInput {
    /// 勘定科目コード。帳簿に登録されている科目コードを指定します
    /// （例: "600"）。登録されていないコードはエラーになります。
    pub account: String,

    /// 集計期間の開始日（取引日。YYYY-MM-DD、この日を含む）。必須です。
    pub from: String,

    /// 集計期間の終了日（取引日。YYYY-MM-DD、この日を含む）。必須です。
    pub to: String,

    /// 1回に返す行数。省略すると 100 行です。上限を超える値は丸めずに
    /// エラーになります。
    #[serde(default)]
    pub limit: Option<u32>,

    /// 続きを読むときに、直前の応答の next_cursor の値をそのまま指定します。
    /// 先頭から読む場合は指定しません。
    #[serde(default)]
    pub cursor: Option<String>,
}

impl McpTool for GetLedger {
    type Input = GetLedgerInput;

    const NAME: &'static str = "get_ledger";

    const DESCRIPTION: &'static str = "\
指定した勘定科目の総勘定元帳（明細と残高の推移）を返します。帳簿は変更しません。\
from と to は取引日（YYYY-MM-DD）で両端を含み、どちらも必須です。\
金額はすべて文字列で返します（例: \"110000\"）。\
opening_balance は from より前のすべての明細から求めた残高で、会計年度の期首ではありません。\
debit_total / credit_total / closing_balance / total_lines は期間全体の値で、\
返した行だけを合計した値ではありません。行ごとの running_balance は期首残高からの累計です。\
残高の符号は科目の種類に従います（資産と費用は借方が正、負債・純資産・収益は貸方が正）。\
1回に返す行数には上限があり、has_more と next_cursor で続きの有無が分かります。\
指定した期間に明細が無い場合は空の rows を返し、勘定科目マスタに無い科目コードはエラーになります。\
帳簿は追記のみなので、赤伝（逆仕訳）で取り消された仕訳の明細も残高も元帳に残ります。\
取り消された仕訳の行には reversed_by が付き、赤伝の行には reverses と reverse_reason が付きます。";

    async fn run(ctx: &ToolContext<'_>, input: Self::Input) -> Result<ToolSuccess, ToolFailure> {
        let account = AccountCode::parse(&input.account)
            .map_err(|error| in_field("account", core_error(error)))?;
        let from = parse_date("from", &input.from)?;
        let to = parse_date("to", &input.to)?;
        let cursor = input
            .cursor
            .as_deref()
            .map(|text| {
                ledger_cursor_from_string(text).map_err(|error| ToolError::from_app_error(&error))
            })
            .transpose()?;

        let settings = ctx.book_settings();
        let limit = input.limit.unwrap_or(DEFAULT_LIMIT);
        let page = ledger::execute(
            ctx.ledger_query(),
            &settings,
            LedgerInput {
                account,
                from,
                to,
                cursor,
                limit,
            },
        )
        .await
        .map_err(|error: AppError| ToolError::from_app_error(&error))?;

        let body = success_body(&page, &input.from, &input.to, limit);
        let summary = audit_summary(&body);
        Ok(ToolSuccess::new(body).with_audit_summary(summary))
    }
}

/// 成功応答（`docs/07-mcp-server.md` §3）。
///
/// 金額はすべて**区切り無しの文字列**（§5）。件数・仕訳番号は JSON number。
///
/// `limit` は**呼び出し元に適用された値**（省略時は [`DEFAULT_LIMIT`]）。
/// 切れた理由が「自分が指定した行数」なのか「サーバの上限」なのかで
/// 次の手が違うので、`truncation_note` はどちらで切れたかを述べる
/// （PR-H レビュー D-4）。
fn success_body(page: &LedgerPageView, from: &str, to: &str, limit: u32) -> Map<String, Value> {
    let mut body = Map::new();
    body.insert("account".to_string(), json!(page.account.as_str()));
    body.insert("account_name".to_string(), json!(page.account_name));
    body.insert(
        "account_type".to_string(),
        json!(account_type_code(page.account_type)),
    );
    body.insert("from".to_string(), json!(from));
    body.insert("to".to_string(), json!(to));
    // 0行でも通貨を名乗る（帳簿通貨。`LedgerParams::book_currency`）。
    body.insert(
        "currency".to_string(),
        json!(page.opening_balance.currency().code()),
    );
    body.insert(
        "opening_balance".to_string(),
        json!(AmountStr::from_money(&page.opening_balance).as_str()),
    );
    body.insert(
        "debit_total".to_string(),
        json!(AmountStr::from_money(&page.debit_total).as_str()),
    );
    body.insert(
        "credit_total".to_string(),
        json!(AmountStr::from_money(&page.credit_total).as_str()),
    );
    body.insert(
        "closing_balance".to_string(),
        json!(AmountStr::from_money(&page.closing_balance).as_str()),
    );
    body.insert("total_lines".to_string(), json!(page.total_lines));
    body.insert("returned".to_string(), json!(page.rows.len()));
    body.insert("has_more".to_string(), json!(page.next_cursor.is_some()));
    body.insert(
        "rows".to_string(),
        Value::Array(page.rows.iter().map(row_to_json).collect()),
    );

    if let Some(cursor) = page.next_cursor.as_ref() {
        body.insert(
            "next_cursor".to_string(),
            json!(ledger_cursor_to_string(cursor)),
        );
        body.insert(
            "truncation_note".to_string(),
            json!(format!(
                "この期間の明細 {total} 行のうち {returned} 行を返しました\
                 （{cause}）。\
                 続きは cursor に next_cursor の値を渡して取得してください。\
                 期間を狭めて取り直すこともできます。\
                 なお opening_balance / debit_total / credit_total / \
                 closing_balance は期間全体の値であり、返した行だけの合計では\
                 ありません",
                total = page.total_lines,
                returned = page.rows.len(),
                cause = truncation_cause(limit),
            )),
        );
    }

    body
}

/// 何が行数を決めたのかを述べる（呼び出し元の `limit` かサーバの上限か）。
fn truncation_cause(limit: u32) -> String {
    if limit < MAX_LIMIT {
        format!(
            "この呼び出しで指定された limit={limit} で切りました。\
             1回に返せる上限は {MAX_LIMIT} 行なので、limit を上げれば\
             1回で受け取れる行数を増やせます"
        )
    } else {
        format!("1回に返す上限の {MAX_LIMIT} 行で切りました")
    }
}

/// `audit_log.output` に残す要約（`DECISIONS.md` D-089 の決定6）。
///
/// 応答本文から**明細（`rows`）と説明文（`truncation_note`）を落とした
/// 残り**である。問い合わせ条件（`account` / `from` / `to`）・期間全体の
/// 合計・件数・読み終わった位置（`next_cursor`）は残るので、
/// 「誰がいつ何を、どこまで読んだか」は監査ログだけで追える
/// （理由は `search_entries` 側の同名関数と `DECISIONS.md` D-089 の決定6）。
fn audit_summary(body: &Map<String, Value>) -> Map<String, Value> {
    let mut summary = body.clone();
    summary.remove("rows");
    summary.remove("truncation_note");
    summary
}

/// 元帳の1行。
///
/// `reverses` / `reverse_reason` / `reversed_by` は該当するときだけ出す
/// （`null` を置かない）。`reverse_reason` を出すのは、赤伝の行を見た
/// 読み手が「なぜ取り消されたか」を `search_entries` へ引き直さずに
/// 読めるようにするためである（D-088 の表。PR-H レビュー D-2）。
fn row_to_json(row: &LedgerRowView) -> Value {
    let mut object = Map::new();
    object.insert(
        "entry_id".to_string(),
        json!(entry_id_to_uuid_string(row.entry_id)),
    );
    object.insert("entry_no".to_string(), json!(row.entry_no.as_u32()));
    object.insert(
        "entry_date".to_string(),
        json!(row.entry_date.to_iso_string()),
    );
    object.insert("line_no".to_string(), json!(row.line_no));
    object.insert("description".to_string(), json!(row.description));
    object.insert(
        "side".to_string(),
        json!(kaikei_app::wire::side_code(row.side)),
    );
    object.insert(
        "amount".to_string(),
        json!(AmountStr::from_money(&row.amount).as_str()),
    );
    object.insert("currency".to_string(), json!(row.amount.currency().code()));
    object.insert(
        "running_balance".to_string(),
        json!(AmountStr::from_money(&row.running_balance).as_str()),
    );
    object.insert(
        "counter_accounts".to_string(),
        Value::Array(
            row.counter_accounts
                .iter()
                .map(|code| json!(code.as_str()))
                .collect(),
        ),
    );
    object.insert("tags".to_string(), tag_set_to_json(&row.tags));
    if let Some(memo) = row.memo.as_ref() {
        object.insert("memo".to_string(), json!(memo));
    }
    if let Some(reverses) = row.reverses {
        object.insert(
            "reverses".to_string(),
            json!(entry_id_to_uuid_string(reverses)),
        );
    }
    if let Some(reason) = row.reverse_reason.as_ref() {
        object.insert("reverse_reason".to_string(), json!(reason));
    }
    if let Some(reversal) = row.reversed_by.as_ref() {
        object.insert("reversed_by".to_string(), reversal_ref_to_json(reversal));
    }
    Value::Object(object)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_app::view::{EntryCursor, LedgerCursor, ReversalRef};
    use kaikei_core::{
        AccountType, AccountingDate, Currency, EntryId, EntryNumber, Money, Side, TagSet,
    };

    fn parse(json: &str) -> Result<GetLedgerInput, serde_json::Error> {
        serde_json::from_str(json)
    }

    // 設計書の例がそのまま受理される。
    #[test]
    fn the_documented_request_shape_is_accepted() {
        let input =
            parse(r#"{"account": "600", "from": "2026-01-01", "to": "2026-12-31", "limit": 50}"#)
                .expect("設計書の例は受理されるはず");
        assert_eq!(input.account, "600");
        assert_eq!(input.limit, Some(50));
        assert!(input.cursor.is_none());
    }

    // 期間は必須（省略はデシリアライズで弾かれる）。
    #[test]
    fn omitting_the_period_is_rejected_by_deserialization() {
        let err = parse(r#"{"account": "600"}"#).expect_err("期間の省略は受理しない");
        assert!(err.to_string().contains("from"), "{err}");
    }

    // 知らないキーを黙って捨てない。
    #[test]
    fn an_unknown_field_is_rejected_instead_of_being_dropped() {
        let err = parse(r#"{"account":"600","from":"2026-01-01","to":"2026-12-31","group_by":[]}"#)
            .expect_err("未知のキーは受理しない");
        assert!(err.to_string().contains("group_by"), "{err}");
    }

    fn row(line_no: u16, side: Side, amount: i128, running: i128) -> LedgerRowView {
        LedgerRowView {
            entry_id: EntryId::new(1),
            entry_no: EntryNumber::new(1),
            entry_date: AccountingDate::new(2026, 4, 15).unwrap(),
            line_no,
            description: "A社への請求".to_string(),
            side,
            amount: Money::from_minor(amount, Currency::JPY),
            tags: TagSet::new(),
            memo: None,
            counter_accounts: vec![AccountCode::parse("100").unwrap()],
            running_balance: Money::from_minor(running, Currency::JPY),
            reverses: None,
            reverse_reason: None,
            reversed_by: None,
        }
    }

    fn page(rows: Vec<LedgerRowView>, total_lines: u64) -> LedgerPageView {
        LedgerPageView {
            account: AccountCode::parse("600").unwrap(),
            account_name: "消耗品費".to_string(),
            account_type: AccountType::Expense,
            opening_balance: Money::from_minor(0, Currency::JPY),
            debit_total: Money::from_minor(3_000, Currency::JPY),
            credit_total: Money::from_minor(0, Currency::JPY),
            closing_balance: Money::from_minor(3_000, Currency::JPY),
            total_lines,
            rows,
            next_cursor: None,
        }
    }

    // 0行でも成功として通貨と合計を名乗る（切れた旨は出さない）。
    #[test]
    fn an_empty_period_is_a_success_that_still_names_the_currency() {
        let mut empty = page(Vec::new(), 0);
        empty.debit_total = Money::from_minor(0, Currency::JPY);
        empty.closing_balance = Money::from_minor(0, Currency::JPY);

        let body = success_body(&empty, "2026-01-01", "2026-12-31", DEFAULT_LIMIT);

        assert_eq!(body["rows"], json!([]));
        assert_eq!(body["currency"], json!("JPY"));
        assert_eq!(body["opening_balance"], json!("0"));
        assert_eq!(body["total_lines"], json!(0));
        assert_eq!(body["has_more"], json!(false));
        assert!(body.get("next_cursor").is_none());
        assert!(body.get("truncation_note").is_none());
    }

    // ★上限で切ったことが応答から分かる★
    #[test]
    fn a_truncated_page_says_so_and_carries_the_cursor_for_the_rest() {
        let mut truncated = page(vec![row(1, Side::Debit, 1_000, 1_000)], 132);
        truncated.next_cursor = Some(LedgerCursor {
            entry: EntryCursor {
                entry_date: AccountingDate::new(2026, 4, 15).unwrap(),
                entry_no: EntryNumber::new(1),
                entry_id: EntryId::new(1),
            },
            line_no: 1,
        });

        let body = success_body(&truncated, "2026-01-01", "2026-12-31", DEFAULT_LIMIT);

        assert_eq!(body["total_lines"], json!(132));
        assert_eq!(body["returned"], json!(1));
        assert_eq!(body["has_more"], json!(true));
        assert!(body["next_cursor"].is_string());
        let note = body["truncation_note"].as_str().expect("切れた旨の説明");
        assert!(note.contains("132"), "{note}");
        assert!(note.contains("next_cursor"), "{note}");
        // ページの行を足しても合計にならないことを言う。
        assert!(note.contains("期間全体"), "{note}");
    }

    // 出力の金額は全て文字列（MC-27）。負の残高も文字列で出る。
    #[test]
    fn every_amount_is_a_json_string() {
        let mut negative = page(vec![row(1, Side::Credit, 500, -500)], 1);
        negative.closing_balance = Money::from_minor(-500, Currency::JPY);

        let body = success_body(&negative, "2026-01-01", "2026-12-31", DEFAULT_LIMIT);

        for key in [
            "opening_balance",
            "debit_total",
            "credit_total",
            "closing_balance",
        ] {
            assert!(body[key].is_string(), "{key} が文字列ではない");
        }
        assert_eq!(body["closing_balance"], json!("-500"));
        assert!(body["rows"][0]["amount"].is_string());
        assert_eq!(body["rows"][0]["running_balance"], json!("-500"));
        // 件数・行番号・仕訳番号は金額ではないので number のままでよい。
        assert!(body["total_lines"].is_number());
        assert!(body["rows"][0]["line_no"].is_number());
    }

    // 取り消された仕訳の行はそれと分かる（D-088）。
    #[test]
    fn a_row_of_a_reversed_entry_is_marked() {
        let mut reversed = row(1, Side::Debit, 1_000, 1_000);
        reversed.reversed_by = Some(ReversalRef {
            entry_id: EntryId::new(2),
            entry_no: EntryNumber::new(2),
            entry_date: AccountingDate::new(2026, 5, 1).unwrap(),
        });
        let body = success_body(
            &page(vec![reversed], 1),
            "2026-01-01",
            "2026-12-31",
            DEFAULT_LIMIT,
        );

        assert_eq!(body["rows"][0]["reversed_by"]["entry_no"], json!(2));
        assert!(body["rows"][0].get("reverses").is_none());
        assert!(body["rows"][0].get("reverse_reason").is_none());

        // 取り消されていない行にはどれも出ない。
        let body = success_body(
            &page(vec![row(1, Side::Debit, 1_000, 1_000)], 1),
            "2026-01-01",
            "2026-12-31",
            DEFAULT_LIMIT,
        );
        assert!(body["rows"][0].get("reversed_by").is_none());
        assert!(body["rows"][0].get("reverses").is_none());
        assert!(body["rows"][0].get("reverse_reason").is_none());
    }

    // 赤伝の行だけを見て「なぜ取り消されたか」が読める
    // （D-088 の表。PR-H レビュー D-2）。
    #[test]
    fn a_row_of_a_reversal_carries_the_reason_it_was_made() {
        let mut red = row(1, Side::Credit, 1_000, 0);
        red.reverses = Some(EntryId::new(1));
        red.reverse_reason = Some("数量の誤り".to_string());

        let body = success_body(
            &page(vec![red], 1),
            "2026-01-01",
            "2026-12-31",
            DEFAULT_LIMIT,
        );

        assert_eq!(
            body["rows"][0]["reverses"],
            json!(entry_id_to_uuid_string(EntryId::new(1)))
        );
        assert_eq!(body["rows"][0]["reverse_reason"], json!("数量の誤り"));
        assert!(body["rows"][0].get("reversed_by").is_none());
    }

    // ★切れた理由が「自分の limit」か「サーバの上限」か分かる★
    // （PR-H レビュー D-4）
    #[test]
    fn the_truncation_note_says_whether_the_callers_limit_or_the_maximum_cut_it() {
        let caller = truncation_cause(1);
        assert!(caller.contains("limit=1"), "{caller}");
        assert!(caller.contains(&MAX_LIMIT.to_string()), "{caller}");

        let maximum = truncation_cause(MAX_LIMIT);
        assert!(!maximum.contains("limit="), "{maximum}");
        assert!(maximum.contains(&MAX_LIMIT.to_string()), "{maximum}");

        // 応答にもその文言が載る。
        let mut truncated = page(vec![row(1, Side::Debit, 1_000, 1_000)], 132);
        truncated.next_cursor = Some(LedgerCursor {
            entry: EntryCursor {
                entry_date: AccountingDate::new(2026, 4, 15).unwrap(),
                entry_no: EntryNumber::new(1),
                entry_id: EntryId::new(1),
            },
            line_no: 1,
        });
        let body = success_body(&truncated, "2026-01-01", "2026-12-31", 1);
        let note = body["truncation_note"].as_str().unwrap();
        assert!(note.contains("limit=1"), "{note}");
    }

    // ★監査ログに残すのは要約★（D-089 の決定6。PR-H レビュー C-5）
    //
    // 明細（`rows`）と説明文は落とすが、条件・合計・件数・続きの位置は残る。
    // 要約の大きさは**行数に依らない**。
    #[test]
    fn the_audit_summary_drops_the_rows_but_keeps_the_conditions_and_totals() {
        let rows: Vec<LedgerRowView> = (1..=MAX_LIMIT as u16)
            .map(|line_no| row(line_no, Side::Debit, 1_000, i128::from(line_no) * 1_000))
            .collect();
        let mut full = page(rows, u64::from(MAX_LIMIT) * 3);
        full.next_cursor = Some(LedgerCursor {
            entry: EntryCursor {
                entry_date: AccountingDate::new(2026, 4, 15).unwrap(),
                entry_no: EntryNumber::new(1),
                entry_id: EntryId::new(1),
            },
            line_no: 1,
        });
        let body = success_body(&full, "2026-01-01", "2026-12-31", MAX_LIMIT);
        let summary = audit_summary(&body);

        assert!(summary.get("rows").is_none(), "明細が要約に残っている");
        assert!(summary.get("truncation_note").is_none());
        // 「何を・どこまで読んだか」は残る。
        for key in [
            "account",
            "from",
            "to",
            "opening_balance",
            "debit_total",
            "credit_total",
            "closing_balance",
            "total_lines",
            "returned",
            "has_more",
            "next_cursor",
        ] {
            assert!(summary.contains_key(key), "{key} が要約から落ちている");
        }
        // 値は応答本文と同じ（別に組み立てていない）。
        assert_eq!(summary["closing_balance"], body["closing_balance"]);

        // 上限まで返しても要約は 1KB を超えない（本文は行数に比例して伸びる）。
        let summary_len = Value::Object(summary).to_string().len();
        let body_len = Value::Object(body).to_string().len();
        assert!(summary_len < 1_024, "要約が {summary_len} バイト");
        assert!(
            body_len > summary_len * 10,
            "本文 {body_len} / 要約 {summary_len}"
        );
    }

    // 相手科目が入る（元帳としての可読性）。
    #[test]
    fn each_row_carries_its_counter_accounts() {
        let body = success_body(
            &page(vec![row(1, Side::Debit, 1_000, 1_000)], 1),
            "2026-01-01",
            "2026-12-31",
            DEFAULT_LIMIT,
        );
        assert_eq!(body["rows"][0]["counter_accounts"], json!(["100"]));
    }

    // 説明文が `CLAUDE.md` §10 の禁止表現を含まず、§11 の「次の手」を含む。
    #[test]
    fn the_description_avoids_forbidden_claims_and_states_the_next_step() {
        let description = GetLedger::DESCRIPTION;
        for forbidden in ["準拠", "法令対応", "JIIMA"] {
            assert!(!description.contains(forbidden), "{forbidden}");
        }
        assert!(description.contains("文字列"));
        assert!(description.contains("next_cursor"));
        assert!(description.contains("reversed_by"));
        // 合計の意味（ページの合計ではない）を必ず述べる。
        assert!(description.contains("期間全体"));
    }
}
