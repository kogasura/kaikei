//! ユースケース（`post_entry` / `reverse_entry` / `report`）。
//!
//! 各ユースケースは「1ファイル = 1関数」の原則に従う（`CLAUDE.md` §6）。
//! `AccountingService` のような巨大構造体は作らない。
//!
//! いずれも `begin` / `commit` を呼ばない（トランザクション境界は呼び出し側 =
//! [`crate::tx::with_tx`]）。[`post_entry`] / [`reverse_entry`] は
//! `execute<Tx: TxOps>(tx: &mut Tx, ...)` の形を取り、[`report`] は `Tx` を
//! 通さず read model（[`crate::ports::TrialBalanceQuery`]）に直行する。

pub mod post_entry;
pub mod report;
pub mod reverse_entry;
