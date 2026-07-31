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
//!   トリガ（reject_mutation）が最後の砦として拒否する → P0001

#![cfg(feature = "pg-tests")]

mod common;

use common::sqlstate;
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
/// トリガ（reject_mutation）が P0001 で拒否する。行トリガのため対象行が必要。
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
    assert_eq!(sqlstate(&err).as_deref(), Some("P0001"));
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

/// kaikei_migrator（所有者）による TRUNCATE も STATEMENT トリガが P0001 で拒否する
/// （TRUNCATE は行トリガを起動しないため、STATEMENT トリガが無いと素通りしてしまう。
/// phase1計画 R6）。
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
    assert_eq!(sqlstate(&err).as_deref(), Some("P0001"));
}
