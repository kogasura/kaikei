//! Phase 1: 永続化層。PostgreSQL に対する Data Mapper と read model。
//!
//! # この crate が持つもの
//!
//! - [`sqlstate`]: PostgreSQL の SQLSTATE から `kaikei_app::error::RepoError`
//!   への写像（DB接続なしでテスト可能な純関数）
//! - [`error`]: `sqlx::Error` から `RepoError` への変換の入口。SQLSTATE の
//!   判別自体は重複させず [`sqlstate`] に委譲する
//! - [`tags`]: `kaikei_core::TagSet` と JSONB（`journal_lines.tags` 列）の
//!   相互変換
//! - [`convert`]: core の値オブジェクト（`Money` / `AccountingDate` /
//!   `Timestamp` / `Side` / `AccountType` / `EntryNumber`）と DB 表現の
//!   相互変換
//! - [`pool`]: [`pool::PgStore`]（[`kaikei_app::ports::Store`] の実装）と
//!   接続確立ヘルパ（[`pool::connect_app`] / [`pool::connect_migrator`]）
//! - `store` / `journal` / `chart` / `period` / `numbering`: `Store`/`PgTx`
//!   の実装本体と、`JournalRepo`/`ChartRepo`/`PeriodRepo`/`NumberingRepo` の
//!   各 PostgreSQL 実装（`PgTx` 自体は crate 内部の実装詳細であり公開しない。
//!   利用側は [`pool::PgStore`] と `kaikei_app::tx::with_tx` を経由する）
//! - [`audit`][]: `AuditSink`（監査ログ）の実装（[`audit::PgAuditSink`]）。
//!   **帳簿とは別のコネクション**で書く（`PgTx` を経由しない）。
//!   `with_tx` の rollback で監査ログが消えないことが設計の要点
//!   （`DECISIONS.md` D-070 / D-075）
//! - [`query`]: `TrialBalanceQuery`（試算表）の SQL 集計による実装
//!   （`kaikei_app::ports::TrialBalanceQuery` を実装する
//!   `query::PgTrialBalanceQuery`）。書き込みと違い `Store`/`PgTx` を
//!   経由せず、SQL から DTO へ直行する（`CLAUDE.md` §6「read model は
//!   物理的に分離する」）

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod audit;
mod chart;
pub mod convert;
pub mod documents;
pub mod error;
pub mod imported;
mod journal;
mod numbering;
mod period;
pub mod pool;
pub mod query;
pub mod sqlstate;
mod store;
pub mod tags;
