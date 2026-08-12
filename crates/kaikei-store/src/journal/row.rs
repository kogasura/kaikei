//! `journal_entries` / `journal_lines` の生 DB 行表現と、両者を束ねる [`EntryRows`]。
//!
//! # なぜ `EntryRows` が必要か（phase1計画 §0-3 / R3。実測で確認済み）
//!
//! `impl TryFrom<(JournalEntryRow, Vec<JournalLineRow>)> for JournalEntry` は
//! **孤児則（E0117）によりコンパイルできない**。`Self`（`JournalEntry`）も
//! タプル構築子も外部型（`kaikei-core` の型）であり、「最初のローカル型」が
//! 引数リストに現れないため。この crate 内で定義するローカル包み型
//! [`EntryRows`] を介することで、`impl TryFrom<EntryRows> for JournalEntry`
//! （最初の型引数がローカル型 `EntryRows`）として実装できる（`mapper.rs`）。
//!
//! いずれの型も `pub(crate)`。DB 行の生表現は永続化層の内部実装詳細であり、
//! `kaikei-app` を含む他 crate には公開しない。

use serde_json::Value;
use uuid::Uuid;

/// `journal_entries` の1行の生表現。
#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct JournalEntryRow {
    pub id: Uuid,
    pub fiscal_year: i32,
    pub entry_no: i32,
    pub entry_date: chrono::NaiveDate,
    pub description: String,
    pub reverses: Option<Uuid>,
    pub reverse_reason: Option<String>,
    pub recorded_at: chrono::DateTime<chrono::Utc>,
}

/// `journal_lines` の1行の生表現。
///
/// `entry_id` を行ごとに保持する。仕訳1件を引く経路
/// （`WHERE entry_id = $1`）だけなら呼び出し側が既に把握しているので不要
/// だったが、**期間で複数件をまとめて引く経路**
/// （[`crate::ports`] の `JournalRepo::list_entries_in_period`）では、
/// 取得した明細をどの仕訳に束ねるかをこの列でしか決められない。
/// 明細を仕訳ごとに引き直すと件数ぶんのクエリになる（決算で数千件を読む）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct JournalLineRow {
    pub entry_id: Uuid,
    pub line_no: i16,
    pub account_code: String,
    pub side: i16,
    pub amount_minor: i64,
    pub currency: String,
    pub currency_minor_unit: i16,
    pub tags: Value,
    pub memo: Option<String>,
}

/// [`JournalEntryRow`] とその明細一覧を束ねるローカル型。
///
/// `mapper.rs` の `impl TryFrom<EntryRows> for JournalEntry` のためだけに
/// 存在する（孤児則の回避。上記モジュール doc を参照）。
pub(crate) struct EntryRows {
    pub entry: JournalEntryRow,
    pub lines: Vec<JournalLineRow>,
}
