//! スキーマの CHECK/UNIQUE 制約と、貸借一致を検証する遅延制約トリガの実効性を検証する。
//!
//! いずれも INSERT を発行して SQLSTATE を確認する（帳簿への UPDATE/DELETE では
//! ないため C-2 の除外マーカーは不要）。

#![cfg(feature = "pg-tests")]

mod common;

use common::sqlstate;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use uuid::Uuid;

/// B-1: `currency_minor_unit` は 0〜18 の範囲外なら拒否される
/// （上限は kaikei-core の `Currency::MAX_MINOR_UNIT`、DECISIONS.md D-020 と一致）。
#[sqlx::test]
async fn journal_lines_currency_minor_unit_out_of_range_is_rejected(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    let id = Uuid::now_v7();
    insert_entry_only(&roles.migrator, id, 2026, 1).await;

    let result = sqlx::query(
        "INSERT INTO journal_lines \
         (entry_id, line_no, account_code, side, amount_minor, currency, currency_minor_unit) \
         VALUES ($1, 1, '100', 1, 1000, 'JPY', 19)",
    )
    .bind(id)
    .execute(&roles.migrator)
    .await;

    let err = result.expect_err("currency_minor_unit=19 は拒否されるべき");
    assert_eq!(sqlstate(&err).as_deref(), Some("23514"));
}

/// B-1（人間承認済み決定の核心）: `currency_minor_unit` に DEFAULT が無いこと。
/// 列を省略したら「既定で0になって金額が桁ズレする」のではなく、
/// NOT NULL 違反として明示的に拒否されなければならない。
#[sqlx::test]
async fn journal_lines_currency_minor_unit_has_no_default(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    let id = Uuid::now_v7();
    insert_entry_only(&roles.migrator, id, 2026, 1).await;

    // currency_minor_unit を意図的に列挙から外す。
    let result = sqlx::query(
        "INSERT INTO journal_lines \
         (entry_id, line_no, account_code, side, amount_minor, currency) \
         VALUES ($1, 1, '100', 1, 1000, 'JPY')",
    )
    .bind(id)
    .execute(&roles.migrator)
    .await;

    let err = result.expect_err("currency_minor_unit省略はNOT NULL違反で拒否されるべき");
    assert_eq!(sqlstate(&err).as_deref(), Some("23502"));
}

/// `amount_minor` は正の値でなければならない（0や負値は拒否）。
#[sqlx::test]
async fn journal_lines_non_positive_amount_is_rejected(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    let id = Uuid::now_v7();
    insert_entry_only(&roles.migrator, id, 2026, 1).await;

    let result = sqlx::query(
        "INSERT INTO journal_lines \
         (entry_id, line_no, account_code, side, amount_minor, currency, currency_minor_unit) \
         VALUES ($1, 1, '100', 1, 0, 'JPY', 0)",
    )
    .bind(id)
    .execute(&roles.migrator)
    .await;

    let err = result.expect_err("amount_minor=0 は拒否されるべき");
    assert_eq!(sqlstate(&err).as_deref(), Some("23514"));
}

/// `side` は 1（借方）/2（貸方）以外を拒否する。
#[sqlx::test]
async fn journal_lines_side_out_of_range_is_rejected(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    let id = Uuid::now_v7();
    insert_entry_only(&roles.migrator, id, 2026, 1).await;

    let result = sqlx::query(
        "INSERT INTO journal_lines \
         (entry_id, line_no, account_code, side, amount_minor, currency, currency_minor_unit) \
         VALUES ($1, 1, '100', 3, 1000, 'JPY', 0)",
    )
    .bind(id)
    .execute(&roles.migrator)
    .await;

    let err = result.expect_err("side=3 は拒否されるべき");
    assert_eq!(sqlstate(&err).as_deref(), Some("23514"));
}

/// 摘要が空白のみの場合は拒否される（`btrim(description) <> ''`）。
#[sqlx::test]
async fn journal_entries_blank_description_is_rejected(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;

    let result = sqlx::query(
        "INSERT INTO journal_entries \
         (id, fiscal_year, entry_no, entry_date, description, recorded_at) \
         VALUES ($1, 2026, 1, '2026-04-01', '   ', now())",
    )
    .bind(Uuid::now_v7())
    .execute(&roles.migrator)
    .await;

    let err = result.expect_err("空白のみの摘要は拒否されるべき");
    assert_eq!(sqlstate(&err).as_deref(), Some("23514"));
}

/// `reverses` と `reverse_reason` は「両方 NULL」か「両方 NOT NULL」でなければならない
/// （片方だけの指定を拒否する）。
#[sqlx::test]
async fn journal_entries_reverses_and_reverse_reason_must_pair(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    let original_id = Uuid::now_v7();
    insert_entry_only(&roles.migrator, original_id, 2026, 1).await;

    // reverses はあるが reverse_reason が無い。
    let result = sqlx::query(
        "INSERT INTO journal_entries \
         (id, fiscal_year, entry_no, entry_date, description, reverses, recorded_at) \
         VALUES ($1, 2026, 2, '2026-04-02', '訂正', $2, now())",
    )
    .bind(Uuid::now_v7())
    .bind(original_id)
    .execute(&roles.migrator)
    .await;

    let err = result.expect_err("reverse_reason を伴わない reverses は拒否されるべき");
    assert_eq!(sqlstate(&err).as_deref(), Some("23514"));
}

/// `(fiscal_year, entry_no)` は一意でなければならない。
#[sqlx::test]
async fn journal_entries_fiscal_year_entry_no_must_be_unique(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    insert_entry_only(&roles.migrator, Uuid::now_v7(), 2026, 1).await;

    let result = sqlx::query(
        "INSERT INTO journal_entries \
         (id, fiscal_year, entry_no, entry_date, description, recorded_at) \
         VALUES ($1, 2026, 1, '2026-04-02', '重複番号', now())",
    )
    .bind(Uuid::now_v7())
    .execute(&roles.migrator)
    .await;

    let err = result.expect_err("同一年度・同一番号の重複は拒否されるべき");
    assert_eq!(sqlstate(&err).as_deref(), Some("23505"));
}

/// 貸借が一致しない仕訳は、コミット時に遅延制約トリガが拒否する（phase1計画 R4）。
#[sqlx::test]
async fn unbalanced_entry_is_rejected_at_commit_by_deferred_trigger(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    let id = Uuid::now_v7();

    let mut tx = roles.migrator.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO journal_entries \
         (id, fiscal_year, entry_no, entry_date, description, recorded_at) \
         VALUES ($1, 2026, 1, '2026-04-01', '不均衡', now())",
    )
    .bind(id)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO journal_lines \
         (entry_id, line_no, account_code, side, amount_minor, currency, currency_minor_unit) \
         VALUES \
         ($1, 1, '100', 1, 1000, 'JPY', 0), \
         ($1, 2, '500', 2, 500, 'JPY', 0)",
    )
    .bind(id)
    .execute(&mut *tx)
    .await
    .unwrap();

    let result = tx.commit().await;
    let err = result.expect_err("借方1000/貸方500の不均衡な仕訳はコミットで拒否されるべき");
    assert_eq!(sqlstate(&err).as_deref(), Some("P0001"));
}

/// 貸借が一致する仕訳は正常にコミットできる（上のテストの陽性対照）。
#[sqlx::test]
async fn balanced_entry_commits_successfully(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    let id = Uuid::now_v7();

    common::insert_balanced_entry(&roles.migrator, id, 2026, 1)
        .await
        .expect("貸借が一致した仕訳のコミットは成功するべき");

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM journal_lines WHERE entry_id = $1")
        .bind(id)
        .fetch_one(&roles.migrator)
        .await
        .unwrap();
    assert_eq!(count, 2);
}

/// B-1: `period_snapshots.currency_minor_unit` にも DEFAULT が無いこと。
#[sqlx::test]
async fn period_snapshots_currency_minor_unit_has_no_default(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;

    let result = sqlx::query(
        "INSERT INTO period_snapshots \
         (fiscal_year, period_end, closed_at, balances, currency, entry_count, last_entry_no, checksum) \
         VALUES (2026, '2027-03-31', now(), '{}', 'JPY', 0, 0, 'deadbeef')",
    )
    .execute(&roles.migrator)
    .await;

    let err = result.expect_err("currency_minor_unit省略はNOT NULL違反で拒否されるべき");
    assert_eq!(sqlstate(&err).as_deref(), Some("23502"));
}

/// `entry_counters` は会計年度ごとに1行しか持てない。
#[sqlx::test]
async fn entry_counters_fiscal_year_must_be_unique(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;

    sqlx::query("INSERT INTO entry_counters (fiscal_year, next_no) VALUES (2026, 1)")
        .execute(&roles.migrator)
        .await
        .unwrap();

    let result = sqlx::query("INSERT INTO entry_counters (fiscal_year, next_no) VALUES (2026, 2)")
        .execute(&roles.migrator)
        .await;

    let err = result.expect_err("同一会計年度の重複行は拒否されるべき");
    assert_eq!(sqlstate(&err).as_deref(), Some("23505"));
}

/// 明細を伴わない最小限の仕訳ヘッダのみを INSERT する
/// （journal_entries 自体の CHECK 制約を検証するための下準備。
/// journal_entries には明細の存在を強制する制約は無い＝journal_lines 側からの
/// FK のみのため、ヘッダ単体の INSERT が成立する）。
async fn insert_entry_only(pool: &sqlx::PgPool, id: Uuid, fiscal_year: i32, entry_no: i32) {
    sqlx::query(
        "INSERT INTO journal_entries \
         (id, fiscal_year, entry_no, entry_date, description, recorded_at) \
         VALUES ($1, $2, $3, '2026-04-01', 'テスト仕訳', now())",
    )
    .bind(id)
    .bind(fiscal_year)
    .bind(entry_no)
    .execute(pool)
    .await
    .expect("下準備の journal_entries INSERT に失敗しました");
}
