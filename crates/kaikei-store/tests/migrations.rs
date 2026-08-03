//! マイグレーションが正しく適用され、期待するテーブル集合になっていることを検証する。
//!
//! `#[sqlx::test]` はテスト関数の実行前に `crates/kaikei-store/migrations/` を
//! 自動適用する（`CARGO_MANIFEST_DIR` 直下の `migrations/` を既定で検出する）。
//! 適用そのものに失敗すればテストのセットアップ自体が panic するため、
//! ここでは「適用できること」を前提に、適用結果を検証する。

#![cfg(feature = "pg-tests")]

mod common;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

/// `migrations/` にある `.sql` ファイルが**全て**適用されていること。
///
/// 期待値をリテラル（かつては `8`）で書くと、マイグレーションを追加する
/// たびに数字を手で直すことになり、直し忘れれば「適用されていない」のか
/// 「数え間違い」なのか区別できない緑/赤になる（`PROGRESS.md` Phase 1 の
/// 教訓6「手で維持する一覧は必ず腐る。構造で閉じる」）。
/// ディレクトリを数えれば、ファイルを足すだけで期待値が追随する。
#[sqlx::test]
async fn every_migration_file_is_recorded(pool_opts: PgPoolOptions, conn_opts: PgConnectOptions) {
    let roles = common::roles(pool_opts, conn_opts).await;

    let expected = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
        .expect("migrations/ を読めること")
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "sql"))
        .count() as i64;
    assert!(expected > 0, "migrations/ に .sql が1つも無い");

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(&roles.migrator)
        .await
        .unwrap();

    assert_eq!(
        count, expected,
        "migrations/ の .sql ファイルが全て適用されていること"
    );
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
        // Phase 3 PR-C: 監査ログ（docs/07-mcp-server.md §9・D-075）。
        "audit_log".to_string(),
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
