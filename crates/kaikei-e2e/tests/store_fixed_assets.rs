//! 固定資産台帳の読み書き（`0012_fixed_assets.sql`）。
//!
//! **実 DB で確かめる。** 制約（定額法には耐用年数が要る／他の方法には
//! 入れさせない）は SQL の CHECK で書いてあり、Rust 側では効かない。

#![cfg(feature = "pg-tests")]

mod common;

use kaikei_app::ports::{FixedAssetRepo, FixedAssetRow};
use kaikei_app::tx::with_tx_err;
use kaikei_core::{AccountCode, AccountingDate, Currency, Money};
use kaikei_store::pool::PgStore;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::PgPool;

async fn seed_account(pool: &PgPool, code: &str, name: &str, account_type: i16) {
    sqlx::query(
        "INSERT INTO accounts (code, name, account_type, postable) VALUES ($1, $2, $3, TRUE)",
    )
    .bind(code)
    .bind(name)
    .bind(account_type)
    .execute(pool)
    .await
    .expect("科目");
}

fn asset(id: &str, method: i16, life: Option<i16>) -> FixedAssetRow {
    FixedAssetRow {
        id: id.to_string(),
        name: "テスト資産".to_string(),
        account: AccountCode::parse("210").unwrap(),
        acquired_on: AccountingDate::new(2025, 7, 24).unwrap(),
        acquisition_cost: Money::from_minor(280_717, Currency::JPY),
        method,
        useful_life_years: life,
        business_ratio: None,
        disposed_on: None,
        note: None,
    }
}

async fn insert(pool: &PgPool, list: Vec<FixedAssetRow>) -> Result<usize, String> {
    let store = PgStore::new(pool.clone());
    with_tx_err(&store, move |tx| {
        let list = list.clone();
        Box::pin(async move { tx.insert_fixed_assets(&list).await })
    })
    .await
    .map_err(|e: kaikei_app::error::RepoError| e.to_string())
}

async fn list(pool: &PgPool) -> Vec<FixedAssetRow> {
    let store = PgStore::new(pool.clone());
    with_tx_err(&store, |tx| {
        Box::pin(async move { tx.list_fixed_assets().await })
    })
    .await
    .expect("台帳を読めること")
}

/// **本命。** 入れたものがそのまま読める。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_fixed_asset_round_trips(pool_opts: PgPoolOptions, conn_opts: PgConnectOptions) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_account(&app, "210", "工具器具備品", 1).await;

    let mut a = asset("11111111-1111-1111-1111-111111111111", 1, Some(4));
    a.name = "パソコン・周辺機器".to_string();
    a.business_ratio = Some("0.8".to_string());
    a.note = Some("Amazon で購入".to_string());

    assert_eq!(insert(&app, vec![a.clone()]).await.unwrap(), 1);

    let rows = list(&app).await;
    assert_eq!(rows.len(), 1);
    let got = &rows[0];
    assert_eq!(got.name, "パソコン・周辺機器");
    assert_eq!(got.account.as_str(), "210");
    assert_eq!(got.acquired_on, a.acquired_on);
    assert_eq!(got.acquisition_cost.minor(), 280_717);
    assert_eq!(got.method, 1);
    assert_eq!(got.useful_life_years, Some(4));
    assert_eq!(
        got.business_ratio.as_deref(),
        Some("0.8000"),
        "NUMERIC(5,4) なので桁が揃う。Ratio::parse_fraction はこれを解釈できる"
    );
    assert_eq!(got.note.as_deref(), Some("Amazon で購入"));
    assert!(got.disposed_on.is_none());
}

/// **本命。** 定額法なのに耐用年数が無ければ入らない。
///
/// 無いまま入れると償却額の計算時に落ちる。**入口で止める。**
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn straight_line_without_a_useful_life_is_rejected(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_account(&app, "210", "工具器具備品", 1).await;

    let error = insert(
        &app,
        vec![asset("11111111-1111-1111-1111-111111111111", 1, None)],
    )
    .await
    .unwrap_err();

    assert!(
        error.contains("straight_line_needs_life") || error.contains("制約"),
        "どの制約に触れたか分かること: {error}"
    );
    assert!(list(&app).await.is_empty());
}

/// **本命。** 一括償却・少額特例に耐用年数を入れさせない。
///
/// これらは耐用年数を使わない。入っていると、入れた本人は効くと思っている。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn other_methods_reject_a_useful_life(pool_opts: PgPoolOptions, conn_opts: PgConnectOptions) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_account(&app, "210", "工具器具備品", 1).await;

    for method in [2i16, 3] {
        let error = insert(
            &app,
            vec![asset(
                "11111111-1111-1111-1111-111111111111",
                method,
                Some(3),
            )],
        )
        .await
        .unwrap_err();
        assert!(
            error.contains("takes_no_life") || error.contains("制約"),
            "method={method}: {error}"
        );
    }
    assert!(list(&app).await.is_empty());
}

/// 一括償却・少額特例は耐用年数なしで入る。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn other_methods_go_in_without_a_useful_life(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_account(&app, "210", "工具器具備品", 1).await;

    insert(
        &app,
        vec![
            asset("11111111-1111-1111-1111-111111111111", 2, None),
            asset("22222222-2222-2222-2222-222222222222", 3, None),
        ],
    )
    .await
    .unwrap();

    assert_eq!(list(&app).await.len(), 2);
}

/// 同じIDを2回入れても増えない。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn inserting_the_same_id_twice_does_not_duplicate(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_account(&app, "210", "工具器具備品", 1).await;

    let a = asset("11111111-1111-1111-1111-111111111111", 2, None);
    assert_eq!(insert(&app, vec![a.clone()]).await.unwrap(), 1);
    assert_eq!(
        insert(&app, vec![a]).await.unwrap(),
        0,
        "2回目は入らない（既存を書き換えもしない）"
    );
    assert_eq!(list(&app).await.len(), 1);
}

/// **本命。** 台帳から行を消せない。
///
/// 資産を帳簿から外すのは除却であって、台帳から消すことではない。
/// 消せると、過去の年度の償却費がどの資産のものだったか辿れなくなる。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn the_app_role_cannot_delete_from_the_ledger(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_account(&app, "210", "工具器具備品", 1).await;
    insert(
        &app,
        vec![asset("11111111-1111-1111-1111-111111111111", 2, None)],
    )
    .await
    .unwrap();

    let error = sqlx::query("DELETE FROM fixed_assets")
        .execute(&app)
        .await
        .expect_err("DELETE は拒否されること");

    assert!(
        error.to_string().contains("permission denied"),
        "権限で止まること: {error}"
    );
    assert_eq!(list(&app).await.len(), 1, "行は残っている");
}

/// 取得価額は正でなければ入らない。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_zero_cost_is_rejected(pool_opts: PgPoolOptions, conn_opts: PgConnectOptions) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_account(&app, "210", "工具器具備品", 1).await;

    let mut a = asset("11111111-1111-1111-1111-111111111111", 2, None);
    a.acquisition_cost = Money::from_minor(0, Currency::JPY);

    assert!(insert(&app, vec![a]).await.is_err());
}

/// 除却日が取得日より前にはならない。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn disposing_before_acquiring_is_rejected(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_account(&app, "210", "工具器具備品", 1).await;

    let mut a = asset("11111111-1111-1111-1111-111111111111", 2, None);
    a.disposed_on = Some(AccountingDate::new(2024, 1, 1).unwrap());

    assert!(insert(&app, vec![a]).await.is_err());
}

/// 知らない科目コードは入らない（外部キー）。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn an_unknown_account_is_rejected(pool_opts: PgPoolOptions, conn_opts: PgConnectOptions) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    // 科目を作らないまま入れる。
    assert!(insert(
        &app,
        vec![asset("11111111-1111-1111-1111-111111111111", 2, None)]
    )
    .await
    .is_err());
}
