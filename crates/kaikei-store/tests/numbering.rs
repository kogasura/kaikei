//! `NumberingRepo::next_entry_no` の検証。
//!
//! `DECISIONS.md` D-040（採番は `RETURNING next_no - 1` の1文upsert）が
//! 実際に「同一年度内で連番を払い出す」こと、そして
//! `migrations/0006_entry_counters.sql` のコメントと `DECISIONS.md`
//! が謳う「採番とINSERTを同一トランザクションにするため欠番は原理的に
//! 発生しない」という会計データの根幹をなす保証を、実際にロールバックを
//! 発生させて裏付ける。

#![cfg(feature = "pg-tests")]

mod common;

use kaikei_app::ports::NumberingRepo;
use kaikei_app::tx::with_tx;
use kaikei_core::EntryNumber;
use kaikei_store::pool::PgStore;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

async fn store_for(pool_opts: PgPoolOptions, conn_opts: PgConnectOptions) -> PgStore {
    let roles = common::roles(pool_opts, conn_opts).await;
    PgStore::new(roles.app)
}

/// 同一年度で連続して採番すると 1, 2 と連番になる（最も基本的な性質）。
#[sqlx::test]
async fn next_entry_no_increments_within_the_same_fiscal_year(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let store = store_for(pool_opts, conn_opts).await;

    let (first, second): (EntryNumber, EntryNumber) = with_tx(&store, |tx| {
        Box::pin(async move {
            let first = tx.next_entry_no(2026).await?;
            let second = tx.next_entry_no(2026).await?;
            Ok((first, second))
        })
    })
    .await
    .unwrap();

    assert_eq!(first.as_u32(), 1);
    assert_eq!(second.as_u32(), 2);
}

/// D-023/D-028の核心: 採番したトランザクションがロールバックされた場合、
/// その番号は「使用済み」として残らない。次のトランザクションで同じ年度を
/// 再度採番すると、2 ではなく 1 が返る（欠番が原理的に発生しないことの
/// 直接証明）。
#[sqlx::test]
async fn next_entry_no_does_not_skip_numbers_after_rollback(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let store = store_for(pool_opts, conn_opts).await;

    // 1回目: 採番した直後に意図的に失敗させ、with_tx にロールバックさせる
    // （`sqlx::Transaction` の ROLLBACK が実際に発行される）。
    let first_attempt: Result<(), kaikei_app::error::AppError> = with_tx(&store, |tx| {
        Box::pin(async move {
            tx.next_entry_no(2026).await?;
            Err(kaikei_app::error::AppError::Rejected {
                reason: "テスト用の意図的な失敗（rollbackを発生させる）".to_string(),
            })
        })
    })
    .await;
    assert!(first_attempt.is_err());

    // 2回目: 別のトランザクションで同じ年度を採番する。
    let reissued: EntryNumber = with_tx(&store, |tx| {
        Box::pin(async move { Ok(tx.next_entry_no(2026).await?) })
    })
    .await
    .unwrap();

    assert_eq!(
        reissued.as_u32(),
        1,
        "ロールバックされた採番は使用済みとして残ってはいけない（欠番防止の核心）"
    );
}

/// 会計年度が異なれば採番は独立する（年度をまたいだ連番の混線が無いこと）。
#[sqlx::test]
async fn next_entry_no_is_independent_per_fiscal_year(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let store = store_for(pool_opts, conn_opts).await;

    let (fy2026_first, fy2027_first, fy2026_second): (EntryNumber, EntryNumber, EntryNumber) =
        with_tx(&store, |tx| {
            Box::pin(async move {
                let fy2026_first = tx.next_entry_no(2026).await?;
                let fy2027_first = tx.next_entry_no(2027).await?;
                let fy2026_second = tx.next_entry_no(2026).await?;
                Ok((fy2026_first, fy2027_first, fy2026_second))
            })
        })
        .await
        .unwrap();

    assert_eq!(fy2026_first.as_u32(), 1);
    assert_eq!(fy2027_first.as_u32(), 1);
    assert_eq!(fy2026_second.as_u32(), 2);
}
