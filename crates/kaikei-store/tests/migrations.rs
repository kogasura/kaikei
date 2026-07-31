//! マイグレーションが正しく適用され、期待するテーブル集合になっていることを検証する。
//!
//! `#[sqlx::test]` はテスト関数の実行前に `crates/kaikei-store/migrations/` を
//! 自動適用する（`CARGO_MANIFEST_DIR` 直下の `migrations/` を既定で検出する）。
//! 適用そのものに失敗すればテストのセットアップ自体が panic するため、
//! ここでは「適用できること」を前提に、適用結果を検証する。

#![cfg(feature = "pg-tests")]

mod common;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

#[sqlx::test]
async fn all_eight_migrations_are_recorded(pool_opts: PgPoolOptions, conn_opts: PgConnectOptions) {
    let roles = common::roles(pool_opts, conn_opts).await;

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(&roles.migrator)
        .await
        .unwrap();

    // 0008_distinct_error_codes.sql（append-onlyトリガと貸借不一致トリガの
    // ERRCODEを分離。DECISIONS.md D-038）を含めて8ファイル。
    assert_eq!(count, 8, "migrations/ の8ファイルが全て適用されていること");
}

#[sqlx::test]
async fn expected_tables_exist_and_documents_tables_do_not(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;

    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT table_name FROM information_schema.tables \
         WHERE table_schema = 'public' AND table_type = 'BASE TABLE' \
         ORDER BY table_name",
    )
    .fetch_all(&roles.migrator)
    .await
    .unwrap();

    let expected = vec![
        "_sqlx_migrations".to_string(),
        "accounts".to_string(),
        "counterparties".to_string(),
        "entry_counters".to_string(),
        "journal_entries".to_string(),
        "journal_lines".to_string(),
        "period_snapshots".to_string(),
    ];
    assert_eq!(tables, expected);

    // F-1（人間承認済み）: documents / entry_documents は Phase 1 では作らない
    // （Phase 4 の kaikei-blob と同時に作る）。
    assert!(!tables.iter().any(|t| t == "documents"));
    assert!(!tables.iter().any(|t| t == "entry_documents"));
}

#[sqlx::test]
async fn journal_tables_are_owned_by_migrator(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;

    for table in ["journal_entries", "journal_lines"] {
        let owner: String =
            sqlx::query_scalar("SELECT pg_get_userbyid(relowner) FROM pg_class WHERE relname = $1")
                .bind(table)
                .fetch_one(&roles.migrator)
                .await
                .unwrap();
        // R5: kaikei_migrator（所有者）であることが、append-only の防御が
        // トリガ頼みになる（= REVOKE をバイパスされうる）前提そのもの。
        assert_eq!(owner, "kaikei_migrator", "table {table} の所有者");
    }
}
