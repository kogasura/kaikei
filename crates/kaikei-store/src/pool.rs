//! [`PgStore`]（[`kaikei_app::ports::Store`] の PostgreSQL 実装）と、
//! アプリ実行用ロール／マイグレーション実行用ロールでの接続確立ヘルパ。

use crate::error::from_sqlx_error;
use kaikei_app::error::RepoError;
use sqlx::postgres::{PgPool, PgPoolOptions};

/// アプリ実行用プールの最大接続数。
///
/// アプリ経路は axum のハンドラ等から並行に呼ばれることを前提とするため、
/// 複数の同時接続を許容する。
const APP_POOL_MAX_CONNECTIONS: u32 = 10;

/// マイグレーション実行用プールの最大接続数。
///
/// `bin/kaikei-migrate.rs` はマイグレーション適用のためだけに単発で使う接続
/// であり、並行アクセスは想定しない。
const MIGRATOR_POOL_MAX_CONNECTIONS: u32 = 1;

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
        .max_connections(APP_POOL_MAX_CONNECTIONS)
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
        .max_connections(MIGRATOR_POOL_MAX_CONNECTIONS)
        .connect(database_url)
        .await
}

/// 接続中のロールが帳簿本体に対して持っている権限。
///
/// [`inspect_journal_privileges`] が返す。**判断（起動を中止するか）は
/// 呼び出し側（合成ルート）が行う**——「起動時に何を致命的とするか」は
/// presentation 層の方針であり、永続化層が決めることではない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalPrivileges {
    /// 接続中のロール名（`current_user`）。
    pub role: String,
    /// `journal_entries` に対する `UPDATE` 権限を持っているか。
    pub can_update: bool,
    /// `journal_entries` に対する `DELETE` 権限を持っているか。
    pub can_delete: bool,
}

impl JournalPrivileges {
    /// append-only が DB 権限の層でも効いている（`UPDATE` も `DELETE` も
    /// 持っていない）か。
    pub fn is_append_only(&self) -> bool {
        !self.can_update && !self.can_delete
    }
}

/// 接続中のロールが帳簿本体（`journal_entries`）に対して持っている権限を調べる。
///
/// # なぜ起動時にこれを見るのか
///
/// `docs/07-mcp-server.md` §8 のとおり、`kaikei-mcp` は認証機構を持たない
/// （stdio でソケットを開かない）ため、**実効的な権限境界は DB ロールだけ**に
/// なる。接続先を `APP_DATABASE_URL`（`kaikei_app`）ではなく
/// `MIGRATOR_DATABASE_URL`（`kaikei_migrator` = テーブル所有者）に
/// 取り違えると、`0003_journal.sql` の `REVOKE` を所有者権限でバイパスし、
/// append-only の防御4層（`docs/07-mcp-server.md` §1）のうち DB 権限の層が
/// 丸ごと消える。**環境変数を1つ間違えるだけで起きる。**
///
/// ロール名の文字列比較（`current_user = 'kaikei_migrator'`）ではなく
/// `has_table_privilege` を見るのは、**守りたい性質そのもの**を検査するため。
/// ロールの名前を変えた環境や、`kaikei_app` に誤って `GRANT UPDATE` して
/// しまった環境でも検出できる。
///
/// # Errors
///
/// `journal_entries` が存在しない場合（マイグレーション未適用）を含め、
/// 問い合わせに失敗したら [`RepoError`]。
pub async fn inspect_journal_privileges(pool: &PgPool) -> Result<JournalPrivileges, RepoError> {
    let (role, can_update, can_delete): (String, bool, bool) = sqlx::query_as(
        "SELECT current_user::text, \
                has_table_privilege('journal_entries', 'UPDATE'), \
                has_table_privilege('journal_entries', 'DELETE')",
    )
    .fetch_one(pool)
    .await
    .map_err(from_sqlx_error)?;

    Ok(JournalPrivileges {
        role,
        can_update,
        can_delete,
    })
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
