//! ユースケース（`post_entry` / `reverse_entry` / `report` / `search_entries` /
//! `ledger` / `import_chart`）。
//!
//! 各ユースケースは「1ファイル = 1関数」の原則に従う（`CLAUDE.md` §6）。
//! `AccountingService` のような巨大構造体は作らない。
//!
//! いずれも `begin` / `commit` を呼ばない（トランザクション境界は呼び出し側 =
//! [`crate::tx::with_tx`]）。[`post_entry`] / [`reverse_entry`] は
//! `execute<Tx: TxOps>(tx: &mut Tx, ...)` の形を取り、[`report`] /
//! [`search_entries`] / [`ledger`] は `Tx` を通さず read model
//! （[`crate::ports::TrialBalanceQuery`] / [`crate::ports::SearchEntriesQuery`] /
//! [`crate::ports::LedgerQuery`]）に直行する。
//!
//! [`import_chart`] は勘定科目マスタの投入（合成ルートが起動時に呼ぶ。
//! MCP ツールではない。`DECISIONS.md` D-070 / D-081）で、`TxOps` ではなく
//! `Tx: ChartRepo + ChartWriteRepo` を要求する——記帳の経路にマスタ書き込みの
//! 能力を持ち込まないため（[`crate::ports::ChartWriteRepo`] の doc を参照）。

pub mod closing;
pub mod import_chart;
pub mod ledger;
pub mod post_entry;
pub mod report;
pub mod reverse_entry;
pub mod search_entries;
pub mod statements;
pub mod verify;
