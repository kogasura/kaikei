//! append-only を DB 権限で強制するための前提条件（R5）と、権限マトリクスを検証する。
//!
//! ここでは `has_table_privilege` 等のカタログ関数のみを使い、実際に
//! UPDATE/DELETE 文を発行しない（C-2: architecture.yml の
//! 「帳簿への UPDATE / DELETE が書かれていない」grep に引っかからないようにする
//! ための設計。実際に文を発行して SQLSTATE を確認する最小限のテストは
//! `tests/append_only.rs` に分離する）。

#![cfg(feature = "pg-tests")]

mod common;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

/// R5: kaikei_app が特権属性を一切持たないこと。
/// これが崩れると REVOKE による append-only の強制が意味を失う
/// （0001_baseline_privileges.sql の DO ブロックと同じ検証を、
/// マイグレーション適用後の実データに対しても独立に確認する）。
#[sqlx::test]
async fn kaikei_app_lacks_dangerous_role_attributes(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;

    let row: (bool, bool, bool, bool) = sqlx::query_as(
        "SELECT rolsuper, rolbypassrls, rolcreatedb, rolcreaterole \
         FROM pg_catalog.pg_roles WHERE rolname = 'kaikei_app'",
    )
    .fetch_one(&roles.migrator)
    .await
    .unwrap();

    let (is_super, bypasses_rls, can_createdb, can_createrole) = row;
    assert!(!is_super, "kaikei_app が SUPERUSER であってはならない");
    assert!(!bypasses_rls, "kaikei_app が BYPASSRLS であってはならない");
    assert!(!can_createdb, "kaikei_app が CREATEDB であってはならない");
    assert!(
        !can_createrole,
        "kaikei_app が CREATEROLE であってはならない"
    );
}

/// R5: kaikei_app が kaikei_migrator のメンバーでないこと
/// （メンバーだとテーブル所有者の権限を継承でき、REVOKE が無効化される）。
#[sqlx::test]
async fn kaikei_app_is_not_member_of_migrator(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;

    let is_member: bool =
        sqlx::query_scalar("SELECT pg_has_role('kaikei_app', 'kaikei_migrator', 'USAGE')")
            .fetch_one(&roles.migrator)
            .await
            .unwrap();

    assert!(
        !is_member,
        "kaikei_app は kaikei_migrator のメンバーであってはならない"
    );
}

/// kaikei_app は public スキーマに新しいオブジェクト（テーブル等）を作成できない。
#[sqlx::test]
async fn kaikei_app_cannot_create_in_public_schema(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;

    let can_create: bool =
        sqlx::query_scalar("SELECT has_schema_privilege('kaikei_app', 'public', 'CREATE')")
            .fetch_one(&roles.migrator)
            .await
            .unwrap();

    assert!(
        !can_create,
        "kaikei_app は public スキーマに CREATE 権限を持ってはならない"
    );
}

/// 帳簿本体（journal_entries/journal_lines）: SELECT/INSERT のみ許可。
/// UPDATE/DELETE/TRUNCATE は禁止（append-only の核心。CLAUDE.md §2）。
#[sqlx::test]
async fn kaikei_app_journal_tables_are_append_only(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;

    for table in ["journal_entries", "journal_lines"] {
        assert_privilege(&roles.migrator, table, "SELECT", true).await;
        assert_privilege(&roles.migrator, table, "INSERT", true).await;
        assert_privilege(&roles.migrator, table, "UPDATE", false).await;
        assert_privilege(&roles.migrator, table, "DELETE", false).await;
        assert_privilege(&roles.migrator, table, "TRUNCATE", false).await;
    }
}

/// マスタ（可変）: SELECT/INSERT/UPDATE を許可。DELETE は許可しない
/// （docs/03-database.md §1。物理削除せず `active` フラグで無効化する）。
#[sqlx::test]
async fn kaikei_app_master_tables_are_updatable_but_not_deletable(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;

    for table in ["accounts", "counterparties", "entry_counters"] {
        assert_privilege(&roles.migrator, table, "SELECT", true).await;
        assert_privilege(&roles.migrator, table, "INSERT", true).await;
        assert_privilege(&roles.migrator, table, "UPDATE", true).await;
        assert_privilege(&roles.migrator, table, "DELETE", false).await;
    }
}

/// 締めスナップショット: SELECT/INSERT のみ許可（docs/03-database.md §1）。
#[sqlx::test]
async fn kaikei_app_period_snapshots_are_insert_only(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;

    assert_privilege(&roles.migrator, "period_snapshots", "SELECT", true).await;
    assert_privilege(&roles.migrator, "period_snapshots", "INSERT", true).await;
    assert_privilege(&roles.migrator, "period_snapshots", "UPDATE", false).await;
    assert_privilege(&roles.migrator, "period_snapshots", "DELETE", false).await;
}

/// 監査ログ: SELECT/INSERT のみ許可（`docs/07-mcp-server.md` §9・MC-23）。
///
/// 帳簿本体と同じ扱いにする。記録の訂正は新しい行の追加で行うのであって、
/// 既存の行を書き換えることではない。
#[sqlx::test]
async fn kaikei_app_audit_log_is_append_only(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;

    assert_privilege(&roles.migrator, "audit_log", "SELECT", true).await;
    assert_privilege(&roles.migrator, "audit_log", "INSERT", true).await;
    assert_privilege(&roles.migrator, "audit_log", "UPDATE", false).await;
    assert_privilege(&roles.migrator, "audit_log", "DELETE", false).await;
    assert_privilege(&roles.migrator, "audit_log", "TRUNCATE", false).await;
}

async fn assert_privilege(pool: &sqlx::PgPool, table: &str, privilege: &str, expected: bool) {
    let actual: bool = sqlx::query_scalar("SELECT has_table_privilege('kaikei_app', $1, $2)")
        .bind(table)
        .bind(privilege)
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(
        actual, expected,
        "kaikei_app の {table} に対する {privilege} 権限が期待と異なります"
    );
}
