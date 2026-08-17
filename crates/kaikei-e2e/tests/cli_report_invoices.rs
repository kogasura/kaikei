//! `kaikei report` が出す「適格請求書を揃えるべき取引」の一覧を、
//! **実行ファイルとして**通す。
//!
//! # なぜ実バイナリで見るのか
//!
//! 一覧の組み立ては CSV の関数（`kaikei_report::invoices_to_collect`）と、
//! 帳簿・証憑から行を選ぶ処理（`kaikei-cli` の中）に分かれている。
//! **CSV の関数だけをテストしても、選び方が正しいことは分からない。**
//! 証憑を紐付けたら一覧から消えるかどうかは、DB が無いと確かめられない。
//!
//! # ここで見るもの
//!
//! | 見るもの | なぜ |
//! |---|---|
//! | 1万円以上だけが載る | 566件を並べても作業リストにならない |
//! | 証憑を付けたら消える | **減らない一覧は作業リストとして使えない** |
//! | 契約書では消えない | 契約書があっても請求書が揃ったことにはならない |

#![cfg(feature = "pg-tests")]

mod common;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::PgPool;
use std::path::{Path, PathBuf};
use std::process::Command;

fn cli_binary() -> PathBuf {
    let test_exe = std::env::current_exe().expect("テスト実行ファイルの場所を取れること");
    let profile_dir = test_exe
        .parent()
        .and_then(Path::parent)
        .expect("<target>/<profile>/deps/ の2つ上");
    let binary = profile_dir.join(format!("kaikei{}", std::env::consts::EXE_SUFFIX));
    assert!(
        binary.is_file(),
        "kaikei の実行ファイルがありません: {}\n先に cargo build -p kaikei-cli を実行してください。",
        binary.display()
    );
    binary
}

fn app_url(pool: &PgPool) -> String {
    let options = pool.connect_options();
    format!(
        "postgres://kaikei_app:{}@{}:{}/{}",
        std::env::var("KAIKEI_APP_PASSWORD").expect("KAIKEI_APP_PASSWORD が要ります"),
        options.get_host(),
        options.get_port(),
        options.get_database().expect("DB名")
    )
}

fn run_report(pool: &PgPool, out_dir: &Path, blob_root: &Path) -> (String, bool) {
    let output = Command::new(cli_binary())
        .arg("report")
        .args(["--year", "2026"])
        .args(["--out", &out_dir.display().to_string()])
        .env("APP_DATABASE_URL", app_url(pool))
        .env("KAIKEI_BLOB_ROOT", blob_root.display().to_string())
        .env("KAIKEI_BOOK_CURRENCY", "JPY")
        .env("KAIKEI_FISCAL_YEAR_RULE", "calendar_year")
        .output()
        .expect("kaikei report を起動できること");
    (
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
        output.status.success(),
    )
}

fn run_attach(pool: &PgPool, blob_root: &Path, file: &Path, extra: &[&str]) -> (String, bool) {
    let output = Command::new(cli_binary())
        .arg("attach")
        .args(["--file", &file.display().to_string()])
        .args(["--via", "email"])
        .args(extra)
        .env("APP_DATABASE_URL", app_url(pool))
        .env("KAIKEI_BLOB_ROOT", blob_root.display().to_string())
        .env("KAIKEI_BOOK_CURRENCY", "JPY")
        .env("KAIKEI_FISCAL_YEAR_RULE", "calendar_year")
        .output()
        .expect("kaikei attach を起動できること");
    (
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
        output.status.success(),
    )
}

/// 課税仕入れの仕訳を1本入れる（取引先タグは付けない）。
async fn seed(pool: &PgPool, entry_no: i32, amount: i64) {
    sqlx::query(
        "INSERT INTO accounts (code, name, account_type, postable) \
         VALUES ('609','通信費',5,true), ('110','普通預金',1,true) \
         ON CONFLICT (code) DO NOTHING",
    )
    .execute(pool)
    .await
    .expect("科目");
    let id: String = sqlx::query_scalar("SELECT gen_random_uuid()::text")
        .fetch_one(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO journal_entries \
         (id, fiscal_year, entry_no, entry_date, description, recorded_at) \
         VALUES ($1::uuid, 2026, $2, DATE '2026-06-15', 'テスト仕入', now())",
    )
    .bind(&id)
    .bind(entry_no)
    .execute(pool)
    .await
    .expect("仕訳");
    sqlx::query(
        "INSERT INTO journal_lines \
         (entry_id, line_no, account_code, side, amount_minor, currency, currency_minor_unit, tags) \
         VALUES ($1::uuid, 1, '609', 1, $2, 'JPY', 0, \
                 '{\"tax_category\":{\"t\":\"code\",\"v\":\"PURCHASE_10_QUALIFIED\"}}'), \
                ($1::uuid, 2, '110', 2, $2, 'JPY', 0, '{}')",
    )
    .bind(&id)
    .bind(amount)
    .execute(pool)
    .await
    .expect("明細");
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kaikei-report-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("作業ディレクトリ");
    dir
}

fn read_list(out_dir: &Path) -> String {
    let text = std::fs::read_to_string(out_dir.join("invoices_to_collect.csv"))
        .expect("invoices_to_collect.csv が出ていること");
    text.trim_start_matches('\u{feff}').to_string()
}

/// **本命。** 1万円以上だけが載る。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn only_transactions_of_ten_thousand_or_more_are_listed(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed(&app, 1, 43_967).await;
    seed(&app, 2, 220).await;
    let out = temp_dir("list-out");
    let blob = temp_dir("list-blob");

    let (log, ok) = run_report(&app, &out, &blob);

    assert!(ok, "{log}");
    let csv = read_list(&out);
    assert!(csv.contains("43967"), "1万円以上は載る: {csv}");
    assert!(!csv.contains(",220,"), "1万円未満は載せない: {csv}");
    assert!(log.contains("揃えるべき取引が 1 件"), "{log}");
}

/// **本命。** 証憑を付けたら一覧から消える。
///
/// **減らない一覧は作業リストとして使えない。** 32件を順に片付けても件数が
/// 変わらなければ、どこまで進んだか分からない。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn an_entry_with_an_invoice_leaves_the_list(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed(&app, 1, 43_967).await;
    seed(&app, 2, 35_829).await;
    let out = temp_dir("done-out");
    let blob = temp_dir("done-blob");
    let file = blob.join("invoice.txt");
    std::fs::write(&file, "請求書").expect("ファイル");

    let (before, ok) = run_report(&app, &out, &blob);
    assert!(ok, "{before}");
    assert!(before.contains("揃えるべき取引が 2 件"), "{before}");

    let (attached, ok) = run_attach(
        &app,
        &blob,
        &file,
        &[
            "--type",
            "invoice",
            "--entry-no",
            "1",
            "--match-year",
            "2026",
        ],
    );
    assert!(ok, "{attached}");

    let (after, ok) = run_report(&app, &out, &blob);
    assert!(ok, "{after}");
    assert!(after.contains("揃えるべき取引が 1 件"), "減ること: {after}");
    assert!(
        after.contains("1 件は証憑が登録済みなので一覧から外しました"),
        "外したことを言うこと: {after}"
    );
    let csv = read_list(&out);
    assert!(!csv.contains("43967"), "済んだ行は消える: {csv}");
    assert!(csv.contains("35829"), "残りは載ったまま: {csv}");
}

/// **本命。** 契約書が付いていても一覧から消えない。
///
/// 契約書があっても、**その取引の請求書が揃ったことにはならない**。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_contract_does_not_take_it_off_the_list(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed(&app, 1, 43_967).await;
    let out = temp_dir("contract-out");
    let blob = temp_dir("contract-blob");
    let file = blob.join("contract.txt");
    std::fs::write(&file, "契約書").expect("ファイル");

    let (attached, ok) = run_attach(
        &app,
        &blob,
        &file,
        &[
            "--type",
            "contract",
            "--entry-no",
            "1",
            "--match-year",
            "2026",
        ],
    );
    assert!(ok, "{attached}");

    let (log, ok) = run_report(&app, &out, &blob);

    assert!(ok, "{log}");
    assert!(log.contains("揃えるべき取引が 1 件"), "残ること: {log}");
    assert!(read_list(&out).contains("43967"), "{log}");
}

/// 領収書でも消える（請求書と同じ扱い）。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_receipt_also_takes_it_off_the_list(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed(&app, 1, 43_967).await;
    let out = temp_dir("receipt-out");
    let blob = temp_dir("receipt-blob");
    let file = blob.join("receipt.txt");
    std::fs::write(&file, "領収書").expect("ファイル");

    let (attached, ok) = run_attach(
        &app,
        &blob,
        &file,
        &[
            "--type",
            "receipt",
            "--entry-no",
            "1",
            "--match-year",
            "2026",
        ],
    );
    assert!(ok, "{attached}");

    let (log, ok) = run_report(&app, &out, &blob);

    assert!(ok, "{log}");
    assert!(log.contains("揃えるべき取引が 0 件") || !log.contains("揃えるべき取引"));
    assert!(!read_list(&out).contains("43967"), "消えること");
}
