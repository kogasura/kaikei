//! `ChartWriteRepo`（勘定科目マスタの投入）の PostgreSQL 実装の検証。
//!
//! 冪等性の**方針**（追加のみ・既存は残す）そのものは
//! `kaikei_app::usecase::import_chart` の単体テストが持つ。ここで実 DB に
//! 対して確かめるのは、その方針が**この実装で実際に成り立つか**である。
//!
//! - `ON CONFLICT (code) DO NOTHING` が本当に既存行を書き換えないこと
//! - 自己参照 FK（`accounts.parent_code`）を持つ科目表が、子を親より先に
//!   並べても1文で投入できること（`crates/kaikei-store/src/chart.rs` の
//!   「親子関係を2パスに分けない」の根拠）
//! - `kaikei_app` ロール（append-only を強制される側）で投入できること
//!   ——合成ルートは `APP_DATABASE_URL` で繋ぐので、migrator でしか
//!   投入できない実装では本番で使えない（`DECISIONS.md` D-081）

#![cfg(feature = "pg-tests")]

mod common;

use kaikei_app::error::AppError;
use kaikei_app::ports::{ChartRepo, ChartWriteRepo};
use kaikei_app::tx::with_tx;
use kaikei_app::usecase::import_chart;
use kaikei_core::{AccountCode, AccountDef, AccountType, ChartOfAccounts};
use kaikei_store::pool::{inspect_journal_privileges, PgStore};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

fn def(code: &str, name: &str, account_type: AccountType, parent: Option<&str>) -> AccountDef {
    AccountDef {
        code: AccountCode::parse(code).unwrap(),
        name: name.to_string(),
        account_type,
        parent: parent.map(|p| AccountCode::parse(p).unwrap()),
        postable: true,
    }
}

async fn app_store(pool_opts: PgPoolOptions, conn_opts: PgConnectOptions) -> PgStore {
    let roles = common::roles(pool_opts, conn_opts).await;
    PgStore::new(roles.app)
}

async fn load(store: &PgStore) -> ChartOfAccounts {
    with_tx(store, |tx| {
        Box::pin(async move { Ok::<_, AppError>(tx.load_chart().await?) })
    })
    .await
    .unwrap()
}

/// 子（`110`）を親（`100`）より先に並べても、1文の `UNNEST` INSERT なら
/// 自己参照 FK を通る。
///
/// PostgreSQL の参照整合性チェックは AFTER ROW トリガとして**文の終わりに**
/// 発火するため、同一文で挿入された行同士は順序を問わない。この性質に
/// 実装が依存しているので、実 DB で直接確かめる（依存が壊れたらここが落ちる）。
#[sqlx::test]
async fn insert_accounts_accepts_a_child_listed_before_its_parent(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let store = app_store(pool_opts, conn_opts).await;

    let inserted = with_tx(&store, |tx| {
        Box::pin(async move {
            let defs = vec![
                def("110", "現金", AccountType::Asset, Some("100")),
                def("100", "流動資産", AccountType::Asset, None),
            ];
            Ok::<_, AppError>(tx.insert_accounts(&defs).await?)
        })
    })
    .await
    .unwrap();

    assert_eq!(inserted, 2);

    let chart = load(&store).await;
    let child = chart.get(&AccountCode::parse("110").unwrap()).unwrap();
    assert_eq!(child.parent, Some(AccountCode::parse("100").unwrap()));
}

/// 既存のコードを含む投入は、その行を**書き換えずに読み飛ばす**。
#[sqlx::test]
async fn insert_accounts_never_overwrites_an_existing_row(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let store = app_store(pool_opts, conn_opts).await;

    with_tx(&store, |tx| {
        Box::pin(async move {
            let defs = vec![def("100", "現金", AccountType::Asset, None)];
            Ok::<_, AppError>(tx.insert_accounts(&defs).await?)
        })
    })
    .await
    .unwrap();

    // 同じコードで、名称も種別も違う定義を投入しようとする。
    let inserted = with_tx(&store, |tx| {
        Box::pin(async move {
            let defs = vec![
                def("100", "現金（書き換え）", AccountType::Expense, None),
                def("500", "売上高", AccountType::Revenue, None),
            ];
            Ok::<_, AppError>(tx.insert_accounts(&defs).await?)
        })
    })
    .await
    .unwrap();

    assert_eq!(inserted, 1, "新規の 500 だけが入るはず");

    let chart = load(&store).await;
    let cash = chart.get(&AccountCode::parse("100").unwrap()).unwrap();
    assert_eq!(cash.name, "現金", "既存の名称が保たれていること");
    assert_eq!(
        cash.account_type,
        AccountType::Asset,
        "既存の科目種別が保たれていること（過去の仕訳の意味を後から変えない）"
    );
}

/// ユースケースを実 DB に対して2回流しても、2回目は1行も入らない（冪等）。
///
/// あわせて、この投入が **`kaikei_app` ロール**（帳簿への UPDATE / DELETE を
/// 持たないロール）でできることを、権限そのものを見て裏付ける。
#[sqlx::test]
async fn import_chart_is_idempotent_against_a_real_database(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;

    let privileges = inspect_journal_privileges(&roles.app).await.unwrap();
    assert!(
        privileges.is_append_only(),
        "投入に使うロール（{}）が帳簿への UPDATE/DELETE を持っている",
        privileges.role
    );
    let migrator_privileges = inspect_journal_privileges(&roles.migrator).await.unwrap();
    assert!(
        !migrator_privileges.is_append_only(),
        "所有者ロール（{}）は REVOKE をバイパスするので、この検査は\
         「取り違えを検出できる」ものでなければならない",
        migrator_privileges.role
    );

    let store = PgStore::new(roles.app);

    let template = ChartOfAccounts::new(vec![
        def("100", "現金", AccountType::Asset, None),
        def("500", "売上高", AccountType::Revenue, None),
    ])
    .unwrap();

    let first = with_tx(&store, |tx| {
        let template = template.clone();
        Box::pin(async move { import_chart::execute(tx, &template).await })
    })
    .await
    .unwrap();
    assert_eq!(first.inserted_rows, 2);
    assert_eq!(first.unchanged, 0);

    let second = with_tx(&store, |tx| {
        let template = template.clone();
        Box::pin(async move { import_chart::execute(tx, &template).await })
    })
    .await
    .unwrap();
    assert_eq!(second.inserted_rows, 0, "2回目に追加が起きてはいけない");
    assert_eq!(second.unchanged, 2);
    assert!(second.kept_existing.is_empty());

    assert_eq!(load(&store).await.iter().count(), 2);
}

/// 投入がロールバックされたら1件も残らない（`with_tx` の外側で採番等と
/// 同じ規律に乗っていることの確認）。
#[sqlx::test]
async fn insert_accounts_is_rolled_back_with_the_transaction(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let store = app_store(pool_opts, conn_opts).await;

    let result: Result<(), AppError> = with_tx(&store, |tx| {
        Box::pin(async move {
            let defs = vec![def("100", "現金", AccountType::Asset, None)];
            tx.insert_accounts(&defs).await?;
            Err(AppError::Rejected {
                reason: "テストのために意図的に失敗させる".to_string(),
            })
        })
    })
    .await;
    assert!(result.is_err());

    assert_eq!(load(&store).await.iter().count(), 0);
}

/// 存在しない親を指す科目は DB が拒否する（黙って NULL にしない）。
#[sqlx::test]
async fn insert_accounts_rejects_a_parent_that_does_not_exist(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let store = app_store(pool_opts, conn_opts).await;

    let result: Result<usize, AppError> = with_tx(&store, |tx| {
        Box::pin(async move {
            let defs = vec![def("110", "現金", AccountType::Asset, Some("999"))];
            Ok(tx.insert_accounts(&defs).await?)
        })
    })
    .await;

    assert!(result.is_err(), "存在しない親を指す投入は失敗するはず");
    assert_eq!(load(&store).await.iter().count(), 0);
}
