//! `search_entries` — 仕訳を検索する（`docs/07-mcp-server.md` §3）。
//!
//! # このファイルに監査ログの手順は無い
//!
//! 開始レコード → 操作 → 結果レコードは [`crate::dispatch::call`] が行う
//! （`DECISIONS.md` D-084）。
//!
//! # 件数の上限と続きの取り方（`DECISIONS.md` D-089）
//!
//! 1回の応答で返す仕訳は最大
//! [`kaikei_app::usecase::search_entries::MAX_LIMIT`] 件で、**切ったことは
//! 応答から必ず読み取れる**:
//!
//! | 欄 | 意味 |
//! |---|---|
//! | `total_matches` | 条件に一致した**総件数**（このページの件数ではない） |
//! | `returned` | このページに入っている件数 |
//! | `has_more` | 続きがあるか |
//! | `next_cursor` | 続きを取るために `cursor` に渡す値（`has_more` が真のときだけ現れる） |
//! | `truncation_note` | 上と同じことを日本語で述べたもの（同上） |
//!
//! 黙って切らない（`PROGRESS.md`「無言の truncation は『全部見た』と
//! 読める」）。上限を超える `limit` は丸めずに拒否する——丸めて成功させると
//! 「要求した件数だけ返ってきた」と読めてしまうため（判断は
//! `kaikei-app` のユースケースが持つ。MCP 層は既定値を詰めるだけ）。
//!
//! # 取り消された仕訳の見え方（`DECISIONS.md` D-088）
//!
//! 赤伝で訂正された仕訳も検索に出る（帳簿は追記のみで、消えない）。
//! 取り消し済みの仕訳には `reversed_by` が付き、赤伝そのものには
//! `reverses` と `reverse_reason` が付く。**この2つが無いと、AI は
//! 取り消し済みの仕訳をもう一度訂正しようとする。**

use kaikei_app::error::AppError;
use kaikei_app::id::entry_id_to_uuid_string;
use kaikei_app::ports::SearchEntriesParams;
use kaikei_app::usecase::search_entries::{self, DEFAULT_LIMIT, MAX_LIMIT};
use kaikei_app::view::{EntrySearchPageView, EntrySummaryView};
use kaikei_app::wire::{entry_cursor_from_string, entry_cursor_to_string};
use kaikei_core::AccountCode;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

use crate::dispatch::{McpTool, ToolContext, ToolFailure, ToolSuccess};
use crate::error::ToolError;
use crate::tools::{core_error, in_field, parse_date, parse_tag_filters};
use crate::wire::{lines_to_json, reversal_ref_to_json, AmountStr};

/// `search_entries`。
pub struct SearchEntries;

// ★この構造体の doc コメントは `tools/list` の応答に出る★
// 内部設計書への参照・crate 名・Markdown の強調記法を書かないこと
// （PR-F レビュー D-2。`server.rs` の
// `every_input_schema_description_is_written_for_the_caller` が検査する）。
/// 仕訳の検索条件。指定していないキーは受け付けません。
/// 条件を複数指定した場合は、そのすべてを満たす仕訳だけが返ります。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchEntriesInput {
    /// 取引日の下限（YYYY-MM-DD、この日を含む）。省略すると下限なしです。
    #[serde(default)]
    pub from: Option<String>,

    /// 取引日の上限（YYYY-MM-DD、この日を含む）。省略すると上限なしです。
    #[serde(default)]
    pub to: Option<String>,

    /// この勘定科目コードの明細を含む仕訳だけに絞ります。
    #[serde(default)]
    pub account: Option<String>,

    /// 摘要にこの文字列を含む仕訳だけに絞ります（部分一致。英字の
    /// 大文字と小文字は区別しません）。空文字は受け付けません。
    #[serde(default)]
    pub description: Option<String>,

    /// 明細1行の金額がこの額以上である仕訳だけに絞ります。
    /// 文字列で指定します（例: "1000"）。
    #[serde(default)]
    pub min_amount: Option<AmountStr>,

    /// 明細1行の金額がこの額以下である仕訳だけに絞ります。
    /// 文字列で指定します（例: "5000"）。
    #[serde(default)]
    pub max_amount: Option<AmountStr>,

    /// タグでの絞り込み。キーも値も文字列で指定します
    /// （例: {"counterparty": "CP0001"}）。複数指定した場合はすべてを
    /// 満たす仕訳だけが返ります。集計軸として登録されているタグキー
    /// （get_trial_balance の group_by に使えるもの）だけが指定できます。
    #[serde(default)]
    pub tags: BTreeMap<String, String>,

    /// 1回に返す件数。省略すると 20 件です。上限を超える値は丸めずに
    /// エラーになります。
    #[serde(default)]
    pub limit: Option<u32>,

    /// 続きを読むときに、直前の応答の next_cursor の値をそのまま指定します。
    /// 先頭から読む場合は指定しません。
    #[serde(default)]
    pub cursor: Option<String>,
}

impl McpTool for SearchEntries {
    type Input = SearchEntriesInput;

    const NAME: &'static str = "search_entries";

    const DESCRIPTION: &'static str = "\
記帳済みの仕訳を、取引日・勘定科目・金額・摘要・タグで検索します。帳簿は変更しません。\
日付は取引日（YYYY-MM-DD）で絞り込みます（記帳した日ではありません）。\
金額は文字列で指定します（例: \"1000\"。JSON の number は受け付けません）。\
条件に一致する仕訳が無い場合はエラーではなく空の一覧を返します。\
1回に返す件数には上限があり、応答の total_matches（一致した総件数）・returned（返した件数）・\
has_more（続きの有無）で切れたかどうかが分かります。続きは next_cursor を cursor に渡して取得します。\
帳簿は追記のみなので、赤伝（逆仕訳）で取り消された仕訳も検索結果に残ります。\
取り消し済みの仕訳には reversed_by が付くので、それをもう一度訂正しないでください。\
赤伝そのものには reverses（訂正対象の仕訳ID）と reverse_reason が付きます。";

    async fn run(ctx: &ToolContext<'_>, input: Self::Input) -> Result<ToolSuccess, ToolFailure> {
        let composition = ctx.composition();
        let settings = ctx.book_settings();

        let from = input
            .from
            .as_deref()
            .map(|text| parse_date("from", text))
            .transpose()?;
        let to = input
            .to
            .as_deref()
            .map(|text| parse_date("to", text))
            .transpose()?;
        let account = input
            .account
            .as_deref()
            .map(|code| {
                AccountCode::parse(code).map_err(|error| in_field("account", core_error(error)))
            })
            .transpose()?;
        let min_amount = input
            .min_amount
            .as_ref()
            .map(|amount| {
                amount
                    .to_money(settings.book_currency)
                    .map_err(|error| in_field("min_amount", core_error(error)))
            })
            .transpose()?;
        let max_amount = input
            .max_amount
            .as_ref()
            .map(|amount| {
                amount
                    .to_money(settings.book_currency)
                    .map_err(|error| in_field("max_amount", core_error(error)))
            })
            .transpose()?;
        // タグの型付けと未登録キーの判定は `kaikei-jp` が持つ（D-072）。
        let tags = parse_tag_filters(&composition, &input.tags)?;
        let cursor = input
            .cursor
            .as_deref()
            .map(|text| {
                entry_cursor_from_string(text).map_err(|error| ToolError::from_app_error(&error))
            })
            .transpose()?;

        let params = SearchEntriesParams {
            from,
            to,
            account,
            description_contains: input.description,
            min_amount,
            max_amount,
            tags,
            cursor,
            limit: input.limit.unwrap_or(DEFAULT_LIMIT),
        };

        let page = search_entries::execute(
            ctx.search_entries_query(),
            composition.tag_catalog.schema(),
            params,
        )
        .await
        .map_err(|error: AppError| ToolError::from_app_error(&error))?;

        Ok(ToolSuccess::new(success_body(&page)))
    }
}

/// 成功応答（`docs/07-mcp-server.md` §3）。
///
/// **0件は成功**（空の一覧を返し、エラーにしない）。「条件に合う仕訳が
/// 無い」ことは、検索が失敗したことではない。
fn success_body(page: &EntrySearchPageView) -> Map<String, Value> {
    let mut body = Map::new();
    body.insert(
        "entries".to_string(),
        Value::Array(page.entries.iter().map(entry_to_json).collect()),
    );
    // 件数は金額ではないので JSON number のままでよい（§5）。
    body.insert("total_matches".to_string(), json!(page.total_matches));
    body.insert("returned".to_string(), json!(page.entries.len()));
    body.insert("has_more".to_string(), json!(page.next_cursor.is_some()));
    if let Some(cursor) = page.next_cursor.as_ref() {
        body.insert(
            "next_cursor".to_string(),
            json!(entry_cursor_to_string(cursor)),
        );
        body.insert(
            "truncation_note".to_string(),
            json!(format!(
                "条件に一致した {total} 件のうち {returned} 件を返しました。\
                 続きは cursor に next_cursor の値を渡して取得してください\
                 （1回に返す上限は {MAX_LIMIT} 件です）。\
                 件数を絞りたい場合は期間や科目の条件を追加してください",
                total = page.total_matches,
                returned = page.entries.len(),
            )),
        );
    }

    body
}

/// 検索結果の仕訳1件。
///
/// `reverses` / `reverse_reason` / `reversed_by` は**該当するときだけ**出す
/// （`null` を置くと「無い」と「取り消されていない」の区別が曖昧になる）。
fn entry_to_json(entry: &EntrySummaryView) -> Value {
    let mut object = Map::new();
    object.insert(
        "entry_id".to_string(),
        json!(entry_id_to_uuid_string(entry.entry_id)),
    );
    object.insert("entry_no".to_string(), json!(entry.entry_no.as_u32()));
    object.insert("fiscal_year".to_string(), json!(entry.fiscal_year));
    object.insert(
        "entry_date".to_string(),
        json!(entry.entry_date.to_iso_string()),
    );
    object.insert("description".to_string(), json!(entry.description));
    object.insert("lines".to_string(), lines_to_json(&entry.lines));
    if let Some(reverses) = entry.reverses {
        object.insert(
            "reverses".to_string(),
            json!(entry_id_to_uuid_string(reverses)),
        );
    }
    if let Some(reason) = entry.reverse_reason.as_ref() {
        object.insert("reverse_reason".to_string(), json!(reason));
    }
    if let Some(reversal) = entry.reversed_by.as_ref() {
        object.insert("reversed_by".to_string(), reversal_ref_to_json(reversal));
    }
    Value::Object(object)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_app::view::{EntryCursor, ReversalRef};
    use kaikei_core::{
        AccountingDate, Currency, EntryId, EntryNumber, JournalLine, Money, Side, TagSet,
    };

    fn parse(json: &str) -> Result<SearchEntriesInput, serde_json::Error> {
        serde_json::from_str(json)
    }

    // 条件を全部指定した要求がそのまま受理される。
    #[test]
    fn the_documented_request_shape_is_accepted() {
        let input = parse(
            r#"{
                "from": "2026-01-01",
                "to": "2026-12-31",
                "account": "600",
                "description": "A社",
                "min_amount": "1000",
                "max_amount": "5000",
                "tags": { "counterparty": "CP0001" },
                "limit": 10,
                "cursor": "2026-04-15:42:0192a7b3-1234-7abc-8def-0123456789ab"
            }"#,
        )
        .expect("設計書の例は受理されるはず");

        assert_eq!(input.from.as_deref(), Some("2026-01-01"));
        assert_eq!(input.min_amount.as_ref().unwrap().as_str(), "1000");
        assert_eq!(input.limit, Some(10));
        assert_eq!(input.tags.len(), 1);
    }

    // 条件なし（全件検索）も受理される。
    #[test]
    fn an_empty_request_is_accepted() {
        let input = parse("{}").expect("条件なしも受理する");
        assert!(input.from.is_none());
        assert!(input.tags.is_empty());
        assert!(input.limit.is_none());
    }

    // MC-09 (1): 金額を JSON number で渡すと日本語のエラーになる。
    #[test]
    fn an_amount_given_as_a_json_number_is_rejected_in_japanese() {
        let err = parse(r#"{"min_amount": 1000}"#).expect_err("number は受理しない");
        let message = err.to_string();
        assert!(
            message.contains("金額は文字列で渡してください"),
            "{message}"
        );
        assert!(!message.contains("invalid type"), "{message}");
    }

    // 知らないキーを黙って捨てない（絞り込んだつもりで全件が返るのを防ぐ）。
    #[test]
    fn an_unknown_field_is_rejected_instead_of_being_dropped() {
        let err = parse(r#"{"counterparty": "CP0001"}"#).expect_err("未知のキーは受理しない");
        assert!(err.to_string().contains("counterparty"), "{err}");
    }

    fn line(amount: i128) -> JournalLine {
        JournalLine::new(
            AccountCode::parse("600").unwrap(),
            Side::Debit,
            Money::from_minor(amount, Currency::JPY),
            TagSet::new(),
            None,
        )
        .unwrap()
    }

    fn entry(id: u128, entry_no: u32) -> EntrySummaryView {
        EntrySummaryView {
            entry_id: EntryId::new(id),
            entry_no: EntryNumber::new(entry_no),
            fiscal_year: 2026,
            entry_date: AccountingDate::new(2026, 4, 15).unwrap(),
            description: "A社への請求".to_string(),
            lines: vec![line(1_000)],
            reverses: None,
            reverse_reason: None,
            reversed_by: None,
        }
    }

    // 0件でも成功として空の一覧を返す（`has_more` は偽で、切れた旨は出さない）。
    #[test]
    fn an_empty_result_is_a_success_without_a_truncation_note() {
        let body = success_body(&EntrySearchPageView {
            entries: Vec::new(),
            total_matches: 0,
            next_cursor: None,
        });

        assert_eq!(body["entries"], json!([]));
        assert_eq!(body["total_matches"], json!(0));
        assert_eq!(body["returned"], json!(0));
        assert_eq!(body["has_more"], json!(false));
        assert!(body.get("next_cursor").is_none());
        assert!(body.get("truncation_note").is_none());
    }

    // ★上限で切ったことが応答から分かる★
    #[test]
    fn a_truncated_page_says_so_and_carries_the_cursor_for_the_rest() {
        let body = success_body(&EntrySearchPageView {
            entries: vec![entry(1, 1)],
            total_matches: 47,
            next_cursor: Some(EntryCursor {
                entry_date: AccountingDate::new(2026, 4, 15).unwrap(),
                entry_no: EntryNumber::new(1),
                entry_id: EntryId::new(1),
            }),
        });

        assert_eq!(body["total_matches"], json!(47));
        assert_eq!(body["returned"], json!(1));
        assert_eq!(body["has_more"], json!(true));
        assert!(body["next_cursor"].is_string());
        let note = body["truncation_note"].as_str().expect("切れた旨の説明");
        assert!(note.contains("47"), "{note}");
        assert!(note.contains("next_cursor"), "{note}");
    }

    // 取り消された仕訳・赤伝はそれと分かる形で出る（D-088）。
    #[test]
    fn a_reversed_entry_and_its_reversal_are_both_marked() {
        let mut original = entry(1, 1);
        original.reversed_by = Some(ReversalRef {
            entry_id: EntryId::new(2),
            entry_no: EntryNumber::new(2),
            entry_date: AccountingDate::new(2026, 5, 1).unwrap(),
        });
        let mut reversal = entry(2, 2);
        reversal.reverses = Some(EntryId::new(1));
        reversal.reverse_reason = Some("請求金額の誤り".to_string());

        let json = entry_to_json(&original);
        assert_eq!(
            json["reversed_by"]["entry_id"],
            json!(entry_id_to_uuid_string(EntryId::new(2)))
        );
        assert_eq!(json["reversed_by"]["entry_no"], json!(2));
        // 取り消されていない仕訳と混同できる形にしない。
        assert!(json.get("reverses").is_none());

        let json = entry_to_json(&reversal);
        assert_eq!(
            json["reverses"],
            json!(entry_id_to_uuid_string(EntryId::new(1)))
        );
        assert_eq!(json["reverse_reason"], json!("請求金額の誤り"));
        assert!(json.get("reversed_by").is_none());

        // 取り消しに関わらない仕訳にはどちらの欄も出ない。
        let json = entry_to_json(&entry(3, 3));
        assert!(json.get("reverses").is_none());
        assert!(json.get("reversed_by").is_none());
    }

    // 出力の金額は全て文字列（MC-27）。仕訳IDは UUID の正準表記。
    #[test]
    fn amounts_are_strings_and_ids_are_canonical_uuids() {
        let json = entry_to_json(&entry(0x0192_a7b3_1234_7abc_8def_0123_4567_89ab, 1));
        assert!(json["lines"][0]["amount"].is_string());
        assert_eq!(json["lines"][0]["amount"], json!("1000"));
        let id = json["entry_id"].as_str().unwrap();
        assert_eq!(id.len(), 36, "{id}");
        assert!(id.contains('-'), "{id}");
    }

    // 説明文が `CLAUDE.md` §10 の禁止表現を含まず、§11 の「次の手」を含む。
    #[test]
    fn the_description_avoids_forbidden_claims_and_states_the_next_step() {
        let description = SearchEntries::DESCRIPTION;
        for forbidden in ["準拠", "法令対応", "JIIMA"] {
            assert!(!description.contains(forbidden), "{forbidden}");
        }
        assert!(description.contains("文字列"));
        assert!(description.contains("next_cursor"));
        assert!(description.contains("reversed_by"));
    }
}
