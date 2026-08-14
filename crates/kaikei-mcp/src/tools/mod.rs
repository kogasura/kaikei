//! ツールの実装。**1ツール1ファイル**で、ファイル名は MCP のツール名と
//! 1対1にする（`docs/07-mcp-server.md` §4）。
//!
//! `post_entry.rs` のような名前にすると
//! `kaikei-app/src/usecase/post_entry.rs` と同名になり、grep でどちらの層の
//! 話か区別できなくなる。
//!
//! # ここに書かないもの
//!
//! - **監査ログの手順**。各ツールは [`crate::dispatch::McpTool`] を実装する
//!   だけで、開始・結果レコードの記録は [`crate::dispatch::call`] が行う。
//!   ツールに渡る [`crate::dispatch::ToolContext`] は
//!   [`kaikei_app::ports::AuditSink`] を露出しないので、**ツール側で監査ログを
//!   書く/書き忘れるという状態そのものが作れない**（`DECISIONS.md` D-084）。
//! - **線上表現の語彙**。金額の文字列化・`side` / `account_type` の綴り・
//!   仕訳IDの表記・タグ値の変換・エラーコードの写像は `kaikei-app` /
//!   `kaikei-jp` にある（同 D-072、`crate::wire` / [`crate::error`]）。
//! - **業務判断**。税額行を足すかどうか、締まっているかどうかの判定は
//!   `kaikei-app` のユースケースと `kaikei-jp` の policy が行う。

pub mod get_entry;
pub mod get_ledger;
pub mod get_settings;
pub mod get_statements;
pub mod get_trial_balance;
pub mod journalize_transaction;
pub mod list_accounts;
pub mod list_pending_transactions;
pub mod list_tax_categories;
pub mod post_journal_entry;
pub mod propose_closing_entries;
pub mod reverse_journal_entry;
pub mod search_documents;
pub mod search_entries;
pub mod suggest_tax_category;
pub mod validate_invoice_number;

use kaikei_app::error::AppError;
use kaikei_core::{AccountingDate, CoreError, TagKey};
use kaikei_jp::compose::Composition;
use std::collections::BTreeMap;

use crate::error::ToolError;

/// `kaikei_core::CoreError` をツール結果エラーにする。
///
/// **写像表を書かない。** `AppError::Core` に包んでから
/// [`AppError::code`] / [`AppError::public_message`] を使う
/// （`docs/07-mcp-server.md` §6。`AppError::Core` は中身へ委譲する）。
pub(crate) fn core_error(error: CoreError) -> ToolError {
    ToolError::from_app_error(&AppError::Core(error))
}

/// 「どの入力欄の値か」を本文の先頭に添える。
///
/// 下位層の文言（`kaikei-core` / `kaikei-jp` が AI 向けに書いたもの）は
/// **言い換えず**、位置情報だけを足す（`CLAUDE.md` §10 / §11）。
pub(crate) fn in_field(field: &str, error: ToolError) -> ToolError {
    ToolError::new(error.code(), format!("{field}: {}", error.message()))
}

/// 日付欄（`entry_date` / `reverse_date`）を [`AccountingDate`] にする。
///
/// **取引日**であって記帳日ではない（`CLAUDE.md` §7）。
pub(crate) fn parse_date(field: &str, text: &str) -> Result<AccountingDate, ToolError> {
    AccountingDate::parse(text).map_err(|error| in_field(field, core_error(error)))
}

/// 線上の `tags`（文字列マップ）を**絞り込み条件**に変換する。
///
/// 記帳側（`post_journal_entry`）が `TagCatalog::parse_tag_set` で
/// `TagSet` を作るのと同じ入口（`TagCatalog::parse_value`）を通し、
/// 正準化済みの値文字列に揃える。**値の書き方を MCP 層で決めない**
/// （`0.30` と `0.3` のどちらで保存されているかを知っているのは
/// タグスキーマを読む層である。`DECISIONS.md` D-072）。
///
/// 検索条件では `TagSet` にせず `(TagKey, String)` の並びにする。
/// `TagSet` はキーごとに1つの値しか持てない袋であり、将来「同じキーの
/// 複数値」を条件にしたくなったときに形が合わなくなるためである。
///
/// # Errors
///
/// 未登録のキー・型に合わない値は `kaikei-jp` の文言のまま返す
/// （有効なキー一覧や期待する書式を含む。`CLAUDE.md` §11）。
pub(crate) fn parse_tag_filters(
    composition: &Composition,
    tags: &BTreeMap<String, String>,
) -> Result<Vec<(TagKey, String)>, ToolError> {
    tags.iter()
        .map(|(key, value)| {
            let (key, parsed) = composition
                .tag_catalog
                .parse_value(key, value)
                .map_err(|error| ToolError::from_jp_error(&error))?;
            Ok((key, kaikei_jp::tags::tag_value_to_string(&parsed)))
        })
        .collect()
}
