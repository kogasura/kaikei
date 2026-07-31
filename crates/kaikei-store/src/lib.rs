//! Phase 1: 永続化層。PostgreSQL に対する Data Mapper と read model。
//!
//! # この先行コミットで用意するもの（`DECISIONS.md` D-034）
//!
//! PR-5 本体（`Store`/`PgTx` の実装、書き込み側のリポジトリ）と PR-6
//! （read model の SQL 集計）の両方が参照する共有基盤だけを、先にこの小さな
//! コミットで固める。
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
//!
//! `pool` / `store` / `journal` / `chart` / `period` / `numbering` / `query`
//! （`Store`/`PgTx` の実装本体、read model の SQL 集計）はまだここでは
//! 宣言しない。後続の PR-5 本体・PR-6 が追加する。

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod convert;
pub mod error;
pub mod sqlstate;
pub mod tags;
