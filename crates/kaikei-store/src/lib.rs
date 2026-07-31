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
//!
//! `query`（read model の SQL 集計）はまだここでは宣言しない。PR-6 が追加する。

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod chart;
pub mod convert;
pub mod error;
mod journal;
mod numbering;
mod period;
pub mod pool;
pub mod sqlstate;
mod store;
pub mod tags;
