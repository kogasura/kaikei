//! マイグレーションが正しく適用され、期待するテーブル集合になっていることを検証する。
//!
//! `#[sqlx::test]` はテスト関数の実行前に `crates/kaikei-store/migrations/` を
//! 自動適用する（`CARGO_MANIFEST_DIR` 直下の `migrations/` を既定で検出する）。
//! 適用そのものに失敗すればテストのセットアップ自体が panic するため、
//! ここでは「適用できること」を前提に、適用結果を検証する。

#![cfg(feature = "pg-tests")]

mod common;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

/// 適用されたマイグレーションの**一覧**（バージョンと description）が
/// 期待どおりであること。
///
/// # なぜ「件数を数える」形にしないのか
///
/// `#[sqlx::test]` は実行前に `migrations/` の `.sql` を全件適用する。
/// したがって「`migrations/` の `.sql` の数」と「`_sqlx_migrations` の
/// 行数」を突き合わせる形は**原理的に常に一致し、赤になる経路が無い**
/// （ファイルを足せば期待値も一緒に増える）。旧版のリテラル `8` が持って
/// いた「マイグレーションが増減したことに人間が気づく」検出力すら失う。
/// 加えて `0010_x.down.sql` を置くと、sqlx は up/down を1件と数えるのに
/// ディレクトリを数える側は2件と数え、**誤検出で赤になる**。
///
/// ここではバージョン番号と description をリテラルの一覧として持つ。
/// description は sqlx がファイル名から作る（`0002_accounts.sql` →
/// version 2 / description `"accounts"`。`_` は空白に置換される）。
/// この一覧は「手で維持する一覧」だが、**維持を忘れると赤になる**点が
/// 前の形と決定的に違う（`PROGRESS.md` Phase 1 の教訓6が禁じているのは
/// 「腐っても誰も気づかない一覧」である）。
///
/// マイグレーションを足したら、この一覧に1行足すこと。
#[sqlx::test]
async fn applied_migrations_match_the_expected_list(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;

    let applied: Vec<(i64, String)> = sqlx::query_as(
        "SELECT version, description FROM _sqlx_migrations WHERE success ORDER BY version",
    )
    .fetch_all(&roles.migrator)
    .await
    .unwrap();

    let expected: Vec<(i64, String)> = [
        (1, "baseline privileges"),
        (2, "accounts"),
        (3, "journal"),
        (4, "append only triggers"),
        (5, "counterparties"),
        (6, "entry counters"),
        (7, "period snapshots"),
        (8, "distinct error codes"),
        // Phase 3 PR-C: 監査ログ（docs/07-mcp-server.md §9・D-075）。
        (9, "audit log"),
    ]
    .into_iter()
    .map(|(version, description)| (version, description.to_string()))
    .collect();

    assert_eq!(
        applied, expected,
        "適用されたマイグレーションの一覧が期待と違う。\
         migrations/ にファイルを追加・改名したなら、この一覧にも反映すること"
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
