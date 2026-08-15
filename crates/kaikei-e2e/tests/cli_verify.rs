//! `kaikei verify` を**実行ファイルとして**通す。
//!
//! # なぜ実バイナリで見るのか
//!
//! `verify` が出す指摘は、`main.rs` の中で関数を呼んでいるだけである。
//! 呼び出しを消しても、使い方の文言を検査するテストは通ってしまう
//! （実際に変異を入れて確かめたところ、1件も落ちなかった）。
//! **指摘が本当に出ることは、動かしてみないと分からない。**
//!
//! # ここで見るもの
//!
//! | 見るもの | なぜ |
//! |---|---|
//! | 固定資産があるのに減価償却費が0なら指摘する | 貸借は一致したままなので決算書を見ても分からない |
//! | 資産がマイナス残高なら指摘する | 同上。実際に4年間気づかれなかった誤りがある |
//! | 指摘があっても終了コードは 0 | 誤りと決まったわけではない。失敗させると償却額が決まるまで検査が通らない |
//! | 正常な帳簿では指摘が出ない | 正しい帳簿で毎回出る指摘は、当たり前になって本当の異常を覆い隠す |

#![cfg(feature = "pg-tests")]

mod common;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::PgPool;
use std::path::{Path, PathBuf};
use std::process::Command;

/// `kaikei` の実行ファイル。
///
/// テスト実行ファイルは `<target>/<profile>/deps/` に置かれるので、その2つ上
/// が `cargo build` の成果物ディレクトリである。
fn cli_binary() -> PathBuf {
    let test_exe = std::env::current_exe().expect("テスト実行ファイルの場所を取れること");
    let profile_dir = test_exe
        .parent()
        .and_then(Path::parent)
        .expect("<target>/<profile>/deps/ の2つ上を取れること");
    let binary = profile_dir.join(format!("kaikei{}", std::env::consts::EXE_SUFFIX));
    assert!(
        binary.is_file(),
        "kaikei の実行ファイルがありません: {}\n\
         この検査は**実バイナリ**を起動します。先に\n\
         \x20 cargo build -p kaikei-cli\n\
         を実行してください。",
        binary.display()
    );
    binary
}

/// 使い捨てDBへの接続文字列（`kaikei_app` ロール）。
fn app_url(pool: &PgPool) -> String {
    let options = pool.connect_options();
    let database = options.get_database().expect("DB名があること");
    format!(
        "postgres://kaikei_app:{}@{}:{}/{}",
        std::env::var("KAIKEI_APP_PASSWORD").expect("KAIKEI_APP_PASSWORD が要ります"),
        options.get_host(),
        options.get_port(),
        database
    )
}

/// `kaikei verify` を走らせて、標準出力と標準エラーを返す。
fn run_verify(pool: &PgPool) -> (String, String, bool) {
    let output = Command::new(cli_binary())
        .args(["verify", "--year", "2026"])
        .env("APP_DATABASE_URL", app_url(pool))
        .env("KAIKEI_BOOK_CURRENCY", "JPY")
        .env("KAIKEI_FISCAL_YEAR_RULE", "calendar_year")
        .output()
        .expect("kaikei verify を起動できること");
    (
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
        output.status.success(),
    )
}

/// 勘定科目を1件入れる。
async fn seed_account(pool: &PgPool, code: &str, name: &str, account_type: i16) {
    sqlx::query(
        "INSERT INTO accounts (code, name, account_type, postable) \
         VALUES ($1, $2, $3, true) ON CONFLICT (code) DO NOTHING",
    )
    .bind(code)
    .bind(name)
    .bind(account_type)
    .execute(pool)
    .await
    .expect("科目を入れられること");
}

/// 仕訳を1件入れる（借方・貸方の2行）。
async fn seed_entry(pool: &PgPool, entry_no: i32, debit: &str, credit: &str, amount: i64) {
    let mut tx = pool.begin().await.expect("トランザクション");
    let id: String = sqlx::query_scalar("SELECT gen_random_uuid()::text")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO journal_entries \
         (id, fiscal_year, entry_no, entry_date, description, recorded_at) \
         VALUES ($1::uuid, 2026, $2, DATE '2026-06-15', 'テスト', now())",
    )
    .bind(&id)
    .bind(entry_no)
    .execute(&mut *tx)
    .await
    .expect("仕訳");
    sqlx::query(
        "INSERT INTO journal_lines \
         (entry_id, line_no, account_code, side, amount_minor, currency, currency_minor_unit) \
         VALUES ($1::uuid, 1, $2, 1, $4, 'JPY', 0), ($1::uuid, 2, $3, 2, $4, 'JPY', 0)",
    )
    .bind(&id)
    .bind(debit)
    .bind(credit)
    .bind(amount)
    .execute(&mut *tx)
    .await
    .expect("明細");
    tx.commit().await.expect("コミット");
}

/// **本命。** 固定資産があるのに減価償却費が0なら指摘する。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn verify_reports_missing_depreciation(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    // 1=Asset, 3=Equity。工具器具備品を元入金から取得した形にする。
    seed_account(&app, "210", "工具器具備品", 1).await;
    seed_account(&app, "400", "元入金", 3).await;
    seed_entry(&app, 1, "210", "400", 161_917).await;

    let (_stdout, stderr, ok) = run_verify(&app);

    assert!(ok, "指摘があっても検査は失敗しないこと: {stderr}");
    assert!(stderr.contains("減価償却費"), "{stderr}");
    assert!(stderr.contains("210"), "対象の科目を挙げること: {stderr}");
}

/// **本命。** 資産のマイナス残高を指摘する。
///
/// 実際に weBanana.SP で4年間気づかれなかった形である。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn verify_reports_an_asset_with_a_negative_balance(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_account(&app, "210", "工具器具備品", 1).await;
    seed_account(&app, "610", "減価償却費", 5).await;
    // 取得価額を計上しないまま償却だけ積む（借 減価償却費 / 貸 工具器具備品）。
    seed_entry(&app, 1, "610", "210", 118_800).await;

    let (_stdout, stderr, ok) = run_verify(&app);

    assert!(ok, "指摘があっても検査は失敗しないこと: {stderr}");
    assert!(stderr.contains("逆になっている"), "{stderr}");
    assert!(stderr.contains("工具器具備品"), "{stderr}");
}

/// **本命。** 正常な帳簿では指摘が出ない。
///
/// 正しい帳簿で毎回出る指摘は、当たり前になって本当の異常を覆い隠す。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn verify_is_quiet_on_a_healthy_book(pool_opts: PgPoolOptions, conn_opts: PgConnectOptions) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_account(&app, "110", "普通預金", 1).await;
    seed_account(&app, "500", "売上高", 4).await;
    seed_entry(&app, 1, "110", "500", 100_000).await;

    let (stdout, stderr, ok) = run_verify(&app);

    assert!(ok, "{stderr}");
    assert!(!stderr.contains("減価償却費"), "固定資産が無い: {stderr}");
    assert!(!stderr.contains("逆になっている"), "{stderr}");
    assert!(stdout.contains("不整合は見つかりませんでした"), "{stdout}");
}
