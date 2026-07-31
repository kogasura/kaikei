//! [`PgStore`]（[`kaikei_app::ports::Store`] の PostgreSQL 実装）と、
//! アプリ実行用ロール／マイグレーション実行用ロールでの接続確立ヘルパ。

use sqlx::postgres::{PgPool, PgPoolOptions};

/// アプリ実行用ロール（`kaikei_app`）で PostgreSQL に接続するプールを作る。
///
/// `docs/03-database.md` §1 のとおり、帳簿本体（`journal_entries`/
/// `journal_lines`）への `UPDATE`/`DELETE`/`TRUNCATE` 権限を持たないロールでの
/// 接続を想定する（append-only を DB 権限で強制する。`CLAUDE.md` §2）。
///
/// # Errors
///
/// 接続に失敗した場合は `sqlx::Error` を返す。
pub async fn connect_app(database_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(10)
        .connect(database_url)
        .await
}

/// マイグレーション実行用ロール（`kaikei_migrator`）で PostgreSQL に接続する
/// プールを作る。
///
/// `bin/kaikei-migrate.rs` から使う。テーブル/スキーマ所有者としての接続で
/// あり、通常のアプリ経路（[`connect_app`]）とは別ロールになる。所有者は
/// REVOKE をバイパスできるため（phase1計画 R5）、この接続は原則としてマイグレーション
/// 適用時のみ使うこと。
///
/// # Errors
///
/// 接続に失敗した場合は `sqlx::Error` を返す。
pub async fn connect_migrator(database_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(1)
        .connect(database_url)
        .await
}

/// [`kaikei_app::ports::Store`] の PostgreSQL 実装。
///
/// `Arc<PgStore>` として axum の `State` 等に積む設計を想定する
/// （`kaikei_app::ports` モジュール doc、`DECISIONS.md` D-029 を参照）。
#[derive(Debug, Clone)]
pub struct PgStore {
    pool: PgPool,
}

impl PgStore {
    /// 接続済みの `PgPool`（[`connect_app`] 等で作成したもの）から store を作る。
    pub fn new(pool: PgPool) -> Self {
        PgStore { pool }
    }

    /// 内部のプールへの参照を返す。`crate::store` が `begin` の実装に使う。
    pub(crate) fn pool(&self) -> &PgPool {
        &self.pool
    }
}
