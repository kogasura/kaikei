//! `get_entry` — 仕訳1件の詳細（`docs/07-mcp-server.md` §2 / §10 MC-14）。
//!
//! # read model を新設せず [`kaikei_app::ports::JournalRepo::find_entry`] を使う
//!
//! `docs/07-mcp-server.md` §2 は当初 `query/entry_detail.rs`（read model）の
//! 新設を挙げていたが、PR-G ではそれを作らない（`DECISIONS.md` D-086）。
//! 理由は3つある。
//!
//! 1. `CLAUDE.md` §6 が read model を分離する対象は「**SQL 集計**」であり、
//!    `get_entry` は集計ではなく**集約1件の取得**である
//! 2. `find_entry` は既に `JournalEntry`（明細・タグ・逆仕訳の関係を含む
//!    集約そのもの）を返す。`D-031` が read model を要求したのは
//!    `TrialBalance` / `BalanceRow` が **core の外から構築できない**ためで、
//!    `JournalEntry` にその制約は無い（`rehydrate` があり、`kaikei-store` が
//!    実際にそれを使って組み立てている）
//! 3. 同じ「仕訳1件を読む」ことに SQL 経路をもう1本作ると、`reverse_journal_entry`
//!    が読む姿とこのツールが返す姿が**別々に育つ**。訂正の可否
//!    （`reverses` / `reverse_reason`）が2つの実装で食い違うと、AI は
//!    どちらが帳簿の事実か判断できない
//!
//! # 「空の結果」と「見つからない」を区別する
//!
//! 存在しない仕訳IDは**エラー**（`not_found`）にする。空の成功
//! （`{"entry": null}` の類）にすると、AI は「IDが違う」のか「その仕訳の
//! 明細が0件」なのかを応答から判断できない。
//! `reason` には**UUID の正準表記**を入れる（10進表記にしない。§3）。

use kaikei_app::amount::money_to_plain_string;
use kaikei_app::error::{AppError, RepoError};
use kaikei_app::id::{entry_id_from_uuid_string, entry_id_to_uuid_string};
use kaikei_app::ports::JournalRepo;
use kaikei_app::tx::with_tx;
use kaikei_core::{EntryId, JournalEntry};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::dispatch::{McpTool, ToolContext, ToolFailure, ToolSuccess};
use crate::error::ToolError;
use crate::tools::in_field;
use crate::wire::lines_to_json;

/// `get_entry`。
pub struct GetEntry;

// ★この構造体の doc コメントは `tools/list` の応答に出る★
// 内部設計書への参照・crate 名・Markdown の強調記法を書かないこと。
/// 取得する仕訳の指定。指定していないキーは受け付けません。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetEntryInput {
    /// 仕訳ID。post_journal_entry や reverse_journal_entry が返した entry_id
    /// （ハイフン付き36文字の UUID）をそのまま指定します。
    pub entry_id: String,
}

impl McpTool for GetEntry {
    type Input = GetEntryInput;

    const NAME: &'static str = "get_entry";

    const DESCRIPTION: &'static str = "\
仕訳1件の詳細（取引日・摘要・明細・タグ・借方合計・貸方合計）を仕訳IDで取得します。\
金額はすべて文字列で返します（例: \"110000\"）。\
その仕訳が逆仕訳（赤伝）である場合は、訂正対象の仕訳ID（reverses）と\
訂正理由（reverse_reason）も返します。\
指定した仕訳IDの仕訳が存在しない場合はエラーになります（空の結果は返しません）。\
帳簿は追記のみで、このツールで取得した仕訳を書き換えることはできません。\
訂正は reverse_journal_entry（逆仕訳）で行います。";

    async fn run(ctx: &ToolContext<'_>, input: Self::Input) -> Result<ToolSuccess, ToolFailure> {
        // UUID のパースは `kaikei-app` の入口を通す（`uuid` を自前で持たない。
        // `docs/07-mcp-server.md` §3 の表）。
        let entry_id = entry_id_from_uuid_string(&input.entry_id)
            .map_err(|error| in_field("entry_id", ToolError::from_app_error(&error)))?;

        let entry = with_tx(ctx.store(), move |tx| {
            Box::pin(async move {
                tx.find_entry(entry_id)
                    .await?
                    .ok_or_else(|| not_found(entry_id))
            })
        })
        .await
        .map_err(|error| ToolError::from_app_error(&error))?;

        Ok(ToolSuccess::new(success_body(&entry)))
    }
}

/// 存在しない仕訳IDに対するエラー。
///
/// **仕訳IDは UUID の正準表記**で示す（`entry_id_to_uuid_string`。
/// `EntryId::as_u128()` の10進表記（最大39桁）で組み立てると、呼び出し元が
/// 送った文字列と突き合わせられない。`docs/07-mcp-server.md` §3 / MC-14）。
/// 文言は `reverse_journal_entry` の同じ状況（`reverse_entry::execute` が
/// 組み立てる `RepoError::NotFound`）と揃える。
fn not_found(entry_id: EntryId) -> AppError {
    AppError::Repo(RepoError::NotFound {
        reason: format!(
            "仕訳が見つかりません（仕訳ID: {}）。\
             仕訳IDが正しいか確認してください",
            entry_id_to_uuid_string(entry_id)
        ),
    })
}

/// 成功応答の本文。
///
/// **確定後の明細をそのまま返す**（`post_journal_entry` の成功応答と同じ形。
/// AI が同じ読み方をできるようにする）。
/// `reverses` / `reverse_reason` は逆仕訳のときだけ現れる——`null` を置くと
/// 「逆仕訳ではない」と「訂正理由が空」の区別が応答から消える
/// （`PROGRESS.md` Phase 1 の教訓3。`lines[].memo` と同じ扱い）。
fn success_body(entry: &JournalEntry) -> Map<String, Value> {
    let mut body = Map::new();
    body.insert(
        "entry_id".to_string(),
        json!(entry_id_to_uuid_string(entry.id())),
    );
    // 仕訳番号・会計年度は金額ではないので JSON number のままでよい（§5）。
    body.insert("entry_no".to_string(), json!(entry.entry_no().as_u32()));
    body.insert("fiscal_year".to_string(), json!(entry.fiscal_year()));
    body.insert(
        "entry_date".to_string(),
        json!(entry.entry_date().to_iso_string()),
    );
    body.insert("description".to_string(), json!(entry.description()));
    body.insert("lines".to_string(), lines_to_json(entry.lines()));
    body.insert(
        "debit_total".to_string(),
        json!(money_to_plain_string(&entry.debit_total())),
    );
    body.insert(
        "credit_total".to_string(),
        json!(money_to_plain_string(&entry.credit_total())),
    );
    if let Some(reverses) = entry.reverses() {
        body.insert(
            "reverses".to_string(),
            json!(entry_id_to_uuid_string(reverses)),
        );
    }
    if let Some(reason) = entry.reverse_reason() {
        body.insert("reverse_reason".to_string(), json!(reason));
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_app::error::codes;
    use kaikei_app::period_guard::ClosedPeriodGuard;
    use kaikei_core::{
        AccountCode, AccountDef, AccountType, AccountingDate, ChartOfAccounts, Clock, Currency,
        EntryNumber, FiscalYear, JournalLine, Money, NewEntry, Side, TagSchema, TagSet, Timestamp,
    };

    struct FixedClock;

    impl Clock for FixedClock {
        fn now(&self) -> Timestamp {
            Timestamp::from_unix_nanos(1_776_000_000_000_000_000)
        }
    }

    fn chart() -> ChartOfAccounts {
        ChartOfAccounts::new(vec![
            AccountDef {
                code: AccountCode::parse("135").unwrap(),
                name: "未収入金".to_string(),
                account_type: AccountType::Asset,
                parent: None,
                postable: true,
            },
            AccountDef {
                code: AccountCode::parse("500").unwrap(),
                name: "売上高".to_string(),
                account_type: AccountType::Revenue,
                parent: None,
                postable: true,
            },
        ])
        .unwrap()
    }

    fn line(account: &str, side: Side, amount: i128) -> JournalLine {
        JournalLine::new(
            AccountCode::parse(account).unwrap(),
            side,
            Money::from_minor(amount, Currency::JPY),
            TagSet::new(),
            None,
        )
        .unwrap()
    }

    fn entry() -> JournalEntry {
        JournalEntry::new(
            NewEntry {
                id: EntryId::new(0x0192_a7b3_1234_7abc_8def_0123_4567_89ab),
                entry_no: EntryNumber::new(42),
                entry_date: AccountingDate::new(2026, 4, 15).unwrap(),
                description: "A社への請求".to_string(),
                lines: vec![
                    line("135", Side::Debit, 1_000),
                    line("500", Side::Credit, 1_000),
                ],
                document_refs: Vec::new(),
            },
            &FiscalYear::calendar_year(2026),
            &chart(),
            &TagSchema::empty(),
            &ClosedPeriodGuard::all_open(),
            &FixedClock,
        )
        .unwrap()
    }

    // MC-14: 明細とタグを含む詳細が返り、金額は文字列。
    #[test]
    fn an_existing_entry_is_returned_with_its_lines_and_totals() {
        let body = Value::Object(success_body(&entry()));

        assert_eq!(
            body["entry_id"],
            json!("0192a7b3-1234-7abc-8def-0123456789ab")
        );
        assert_eq!(body["entry_no"], json!(42));
        assert_eq!(body["fiscal_year"], json!(2026));
        assert_eq!(body["entry_date"], json!("2026-04-15"));
        assert_eq!(body["description"], json!("A社への請求"));
        assert_eq!(body["lines"].as_array().unwrap().len(), 2);
        assert_eq!(body["lines"][0]["account"], json!("135"));
        assert_eq!(body["lines"][0]["tags"], json!({}));
        // MC-27: 金額は文字列（number にしない）。
        assert_eq!(body["debit_total"], json!("1000"));
        assert!(body["credit_total"].is_string());
        assert!(body["lines"][0]["amount"].is_string());
    }

    // 逆仕訳でない仕訳には `reverses` / `reverse_reason` を**出さない**
    // （`null` を置くと「訂正理由が空」と区別できない）。
    #[test]
    fn a_plain_entry_omits_the_reversal_keys() {
        let body = Value::Object(success_body(&entry()));
        assert!(body.get("reverses").is_none(), "{body}");
        assert!(body.get("reverse_reason").is_none(), "{body}");
    }

    // 逆仕訳は訂正対象の仕訳IDと理由を返す（UUID の正準表記）。
    #[test]
    fn a_reversal_entry_points_at_the_original_and_carries_the_reason() {
        let original = entry();
        let reversal = original
            .reverse(
                EntryId::new(0x0192_b1c4_1234_7abc_8def_0123_4567_89ab),
                EntryNumber::new(43),
                AccountingDate::new(2026, 5, 1).unwrap(),
                "請求金額の誤り（税率の適用誤り）".to_string(),
                &FiscalYear::calendar_year(2026),
                &chart(),
                &TagSchema::empty(),
                &ClosedPeriodGuard::all_open(),
                &FixedClock,
            )
            .unwrap();

        let body = Value::Object(success_body(&reversal));
        assert_eq!(
            body["reverses"],
            json!("0192a7b3-1234-7abc-8def-0123456789ab")
        );
        assert_eq!(
            body["reverse_reason"],
            json!("請求金額の誤り（税率の適用誤り）")
        );
        // 10進表記（最大39桁）で漏れていないこと。
        assert!(!body
            .to_string()
            .contains(&original.id().as_u128().to_string()));
    }

    // MC-14: 存在しない ID は**空の成功にしない**。仕訳IDは UUID の正準表記。
    #[test]
    fn a_missing_entry_is_reported_as_not_found_with_the_canonical_uuid() {
        let entry_id = EntryId::new(0x0192_a7b3_1234_7abc_8def_0123_4567_89ab);
        let error = ToolError::from_app_error(&not_found(entry_id));

        assert_eq!(error.code(), codes::NOT_FOUND);
        let message = error.message();
        assert!(
            message.contains("0192a7b3-1234-7abc-8def-0123456789ab"),
            "{message}"
        );
        assert!(
            !message.contains(&entry_id.as_u128().to_string()),
            "{message}"
        );
        // 次の手が分かる文言（`CLAUDE.md` §11）。
        assert!(message.contains("確認してください"), "{message}");
    }

    // 説明文が `CLAUDE.md` §10 の禁止表現を含まず、§11 の「次の手」を含む。
    #[test]
    fn the_description_avoids_forbidden_claims_and_states_the_next_step() {
        let description = GetEntry::DESCRIPTION;
        for forbidden in ["準拠", "法令対応", "JIIMA"] {
            assert!(!description.contains(forbidden), "{forbidden}");
        }
        assert!(description.contains("文字列"));
        assert!(description.contains("reverse_journal_entry"));
    }
}
