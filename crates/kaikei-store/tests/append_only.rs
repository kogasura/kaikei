//! append-only の実効性を、実際に UPDATE/TRUNCATE 文を発行して検証する（最小限）。
//!
//! 権限マトリクスそのもの（各テーブル・各操作の可否）は `has_table_privilege` で
//! 検査する `tests/privileges.rs` に置き、実際に文を発行して SQLSTATE を確認する
//! テストはここに最小限だけ集める（C-2: architecture.yml の
//! 「帳簿への UPDATE / DELETE が書かれていない」grep との共存のため。
//! 除外マーカー `ci-allow: append-only-probe` は UPDATE を書く行にのみ付ける。
//! 文字列連結でこの検知をすり抜ける書き方はしない）。
//!
//! 確認する経路は2つ（phase1計画 R5）:
//! - kaikei_app（アプリロール）: ロール権限（REVOKE）で拒否される → 42501
//! - kaikei_migrator（テーブル所有者）: REVOKE をバイパスできるため、
//!   トリガ（reject_mutation）が最後の砦として拒否する → P0010
//!   （`migrations/0008_distinct_error_codes.sql` で `P0001` から分離。
//!   `DECISIONS.md` D-038）

#![cfg(feature = "pg-tests")]

mod common;

use common::sqlstate;
use kaikei_app::error::RepoError;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use uuid::Uuid;

/// kaikei_app による UPDATE は権限不足で拒否される。UPDATE の権限チェックは
/// Postgres が文の実行前に行うため、対象行が無くても（`WHERE false`）検証できる。
#[sqlx::test]
async fn kaikei_app_update_journal_entries_is_denied_with_42501(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;

    let result = sqlx::query("UPDATE journal_entries SET description = description WHERE false") // ci-allow: append-only-probe
        .execute(&roles.app)
        .await;

    let err = result.expect_err("kaikei_app による UPDATE は成功してはいけません");
    assert_eq!(sqlstate(&err).as_deref(), Some("42501"));
}

/// kaikei_migrator（所有者）は REVOKE をバイパスできるため権限では止まらないが、
/// トリガ（reject_mutation）が P0010 で拒否する。行トリガのため対象行が必要。
///
/// SQLSTATE が `RepoError::AppendOnlyViolation` に正しく写像されることも
/// あわせて検証する（`kaikei-store::sqlstate::map_sqlstate` は DB 無しの
/// 純関数テストで検証済みだが、実際の `sqlx::Error` から
/// `kaikei-store::error::from_sqlx_error` を通す経路は PostgreSQL が必要な
/// ためここで確認する）。
#[sqlx::test]
async fn kaikei_migrator_update_journal_entries_is_rejected_by_trigger(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    let id = Uuid::now_v7();
    common::insert_balanced_entry(&roles.migrator, id, 2026, 1)
        .await
        .expect("下準備の仕訳INSERTに失敗しました");

    let result = sqlx::query("UPDATE journal_entries SET description = description WHERE id = $1") // ci-allow: append-only-probe
        .bind(id)
        .execute(&roles.migrator)
        .await;

    let err = result.expect_err("kaikei_migrator であっても UPDATE は成功してはいけません");
    assert_eq!(sqlstate(&err).as_deref(), Some("P0010"));
    assert!(matches!(
        kaikei_store::error::from_sqlx_error(err),
        RepoError::AppendOnlyViolation { .. }
    ));
}

/// kaikei_app による TRUNCATE も権限不足で拒否される（phase1計画 R6）。
#[sqlx::test]
async fn kaikei_app_truncate_journal_tables_is_denied_with_42501(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;

    let result = sqlx::query("TRUNCATE journal_entries, journal_lines")
        .execute(&roles.app)
        .await;

    let err = result.expect_err("kaikei_app による TRUNCATE は成功してはいけません");
    assert_eq!(sqlstate(&err).as_deref(), Some("42501"));
}

/// 監査ログ（audit_log）に対する kaikei_app の UPDATE も 42501 で拒否される。
/// 帳簿本体と同じ4点セットで守る（`docs/07-mcp-server.md` §9・MC-23）。
#[sqlx::test]
async fn kaikei_app_update_audit_log_is_denied_with_42501(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;

    let result = sqlx::query("UPDATE audit_log SET tool = tool WHERE false")
        .execute(&roles.app)
        .await;

    let err = result.expect_err("kaikei_app による監査ログの UPDATE は成功してはいけません");
    assert_eq!(sqlstate(&err).as_deref(), Some("42501"));
}

/// kaikei_migrator（所有者）は REVOKE をバイパスできるが、監査ログ**専用**の
/// トリガ（reject_audit_log_mutation）が P0012 で拒否する。
///
/// **P0010（帳簿の reject_mutation）と別のコードであることが要点。**
/// 同じコードに寄せると、監査ログの変更が拒否されたときに
/// 「訂正は逆仕訳で行ってください」という的外れな案内が出る
/// （`DECISIONS.md` D-038 と同じ誤診クラス。D-075）。
#[sqlx::test]
async fn kaikei_migrator_update_audit_log_is_rejected_by_a_dedicated_trigger(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    let request_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO audit_log (request_id, occurred_at, actor, tool, status) \
         VALUES ($1, now(), 'mcp', 'post_journal_entry', 'started')",
    )
    .bind(request_id)
    .execute(&roles.migrator)
    .await
    .expect("下準備の監査ログ INSERT に失敗しました");

    let result = sqlx::query("UPDATE audit_log SET tool = 'x' WHERE request_id = $1")
        .bind(request_id)
        .execute(&roles.migrator)
        .await;

    let err =
        result.expect_err("kaikei_migrator であっても監査ログの UPDATE は成功してはいけません");
    assert_eq!(sqlstate(&err).as_deref(), Some("P0012"));
    assert_ne!(
        sqlstate(&err).as_deref(),
        Some("P0010"),
        "帳簿の append-only 違反と同じコードにしてはいけない（誤った案内になる）"
    );

    // 写像先も AppendOnlyViolation ではない（「逆仕訳で」と案内しない）。
    let mapped = kaikei_store::error::from_sqlx_error(err);
    assert!(!matches!(mapped, RepoError::AppendOnlyViolation { .. }));
    assert!(!mapped.public_message().contains("逆仕訳"));
}

/// 監査ログの TRUNCATE も STATEMENT トリガで拒否される（行トリガは
/// TRUNCATE を捕まえない）。
#[sqlx::test]
async fn kaikei_migrator_truncate_audit_log_is_rejected_by_a_dedicated_trigger(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;

    let result = sqlx::query("TRUNCATE audit_log")
        .execute(&roles.migrator)
        .await;

    let err = result.expect_err("kaikei_migrator であっても TRUNCATE は成功してはいけません");
    assert_eq!(sqlstate(&err).as_deref(), Some("P0012"));
}

/// kaikei_migrator（所有者）による TRUNCATE も STATEMENT トリガが P0010 で拒否する
/// （TRUNCATE は行トリガを起動しないため、STATEMENT トリガが無いと素通りしてしまう。
/// phase1計画 R6。`no_truncate_*` トリガも `reject_mutation()` を使うため、
/// UPDATE と同じ P0010 になる）。
#[sqlx::test]
async fn kaikei_migrator_truncate_journal_tables_is_rejected_by_trigger(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;

    let result = sqlx::query("TRUNCATE journal_entries, journal_lines")
        .execute(&roles.migrator)
        .await;

    let err = result.expect_err("kaikei_migrator であっても TRUNCATE は成功してはいけません");
    assert_eq!(sqlstate(&err).as_deref(), Some("P0010"));
}
