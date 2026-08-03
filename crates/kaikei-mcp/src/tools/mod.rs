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

pub mod post_journal_entry;
pub mod reverse_journal_entry;

use kaikei_app::error::AppError;
use kaikei_core::{AccountingDate, CoreError};

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
