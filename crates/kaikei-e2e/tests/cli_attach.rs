//! `kaikei attach` を**実行ファイルとして**通す。
//!
//! # なぜ実バイナリで見るのか
//!
//! 引数の解析だけをテストしても、**仕訳から検索要件が本当に埋まることは
//! 分からない**。埋める処理は `run_attach` の中にあり、DB が要る。
//! 引数の形だけ合っていて中身が埋まらない状態を、解析のテストは通してしまう。
//!
//! # ここで見るもの
//!
//! | 見るもの | なぜ |
//! |---|---|
//! | `--entry` から取引年月日・取引金額・取引先が埋まる | 1件ごとに5つ打たせると証憑の登録が現実的でなくなる |
//! | 明示した値は仕訳より優先される | 証憑の日付が仕訳と違うことはある |
//! | 見つからない仕訳を黙って通さない | 帳簿から辿れない証憑は、登録しないのとほとんど変わらない |

#![cfg(feature = "pg-tests")]

mod common;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::PgPool;
use std::path::{Path, PathBuf};
use std::process::Command;

const ENTRY_ID: &str = "11111111-1111-1111-1111-111111111111";

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

/// `kaikei attach` を走らせる。
fn run_attach(pool: &PgPool, blob_root: &Path, file: &Path, extra: &[&str]) -> (String, bool) {
    let mut command = Command::new(cli_binary());
    command
        .arg("attach")
        .args(["--file", &file.display().to_string()])
        .args(["--type", "receipt"])
        .args(["--via", "download"])
        .args(extra)
        .env("APP_DATABASE_URL", app_url(pool))
        .env("KAIKEI_BLOB_ROOT", blob_root.display().to_string())
        .env("KAIKEI_BOOK_CURRENCY", "JPY")
        .env("KAIKEI_FISCAL_YEAR_RULE", "calendar_year");
    let output = command.output().expect("kaikei attach を起動できること");
    (
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
        output.status.success(),
    )
}

/// 取引先タグ付きの仕訳を1件仕込む。
async fn seed_entry(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO accounts (code, name, account_type, postable) \
         VALUES ('609','消耗品費',5,true), ('110','普通預金',1,true) \
         ON CONFLICT (code) DO NOTHING",
    )
    .execute(pool)
    .await
    .expect("科目");
    sqlx::query(
        "INSERT INTO journal_entries \
         (id, fiscal_year, entry_no, entry_date, description, recorded_at) \
         VALUES ($1::uuid, 2026, 900, DATE '2026-07-14', 'カ)アマゾン', now())",
    )
    .bind(ENTRY_ID)
    .execute(pool)
    .await
    .expect("仕訳");
    sqlx::query(
        "INSERT INTO journal_lines \
         (entry_id, line_no, account_code, side, amount_minor, currency, currency_minor_unit, tags) \
         VALUES ($1::uuid, 1, '609', 1, 11332, 'JPY', 0, \
                 '{\"counterparty\":{\"t\":\"code\",\"v\":\"CP0001\"}}'), \
                ($1::uuid, 2, '110', 2, 11332, 'JPY', 0, '{}')",
    )
    .bind(ENTRY_ID)
    .execute(pool)
    .await
    .expect("明細");
}

/// 登録された証憑の検索要件を読む。
async fn registered(pool: &PgPool) -> (String, Option<i64>, Option<String>) {
    // 日付は文字列で受ける（chrono への依存をテストのために増やさない）。
    let row: (String, Option<i64>, Option<String>) = sqlx::query_as(
        "SELECT doc_date::text, amount_minor, counterparty FROM documents          ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_one(pool)
    .await
    .expect("証憑があること");
    (row.0, row.1, row.2)
}

/// 一時ファイルを作る。
fn temp_file(name: &str, body: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("kaikei-attach-{name}"));
    std::fs::write(&path, body).expect("一時ファイルを書けること");
    path
}

/// **本命。** `--entry` から検索要件が埋まる。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn the_search_fields_are_filled_from_the_entry(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_entry(&app).await;
    let blob = std::env::temp_dir().join("kaikei-attach-blob-1");
    let file = temp_file("filled.txt", "アマゾンの領収書");

    let (out, ok) = run_attach(&app, &blob, &file, &["--entry", ENTRY_ID]);

    assert!(ok, "--date なしで通ること: {out}");
    let (date, amount, counterparty) = registered(&app).await;
    assert_eq!(date, "2026-07-14", "仕訳の取引日");
    assert_eq!(amount, Some(11_332), "仕訳の借方合計");
    assert_eq!(counterparty.as_deref(), Some("CP0001"), "仕訳の取引先タグ");
}

/// **本命。** 明示した値は仕訳より優先される。
///
/// 証憑の日付が仕訳と違うことはある（請求書の日付と計上日など）。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn an_explicit_value_wins_over_the_entry(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_entry(&app).await;
    let blob = std::env::temp_dir().join("kaikei-attach-blob-2");
    let file = temp_file("explicit.txt", "別の日付の請求書");

    let (out, ok) = run_attach(
        &app,
        &blob,
        &file,
        &[
            "--entry",
            ENTRY_ID,
            "--date",
            "2026-06-01",
            "--amount",
            "9999",
            "--counterparty",
            "CP9999",
        ],
    );

    assert!(ok, "{out}");
    let (date, amount, counterparty) = registered(&app).await;
    assert_eq!(date, "2026-06-01");
    assert_eq!(amount, Some(9_999));
    assert_eq!(counterparty.as_deref(), Some("CP9999"));
}

/// **本命。** 見つからない仕訳を黙って通さない。
///
/// 紐付けに失敗した証憑は帳簿から辿れないまま保存される——それは登録しないのと
/// ほとんど変わらない。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn an_unknown_entry_is_rejected(pool_opts: PgPoolOptions, conn_opts: PgConnectOptions) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    let blob = std::env::temp_dir().join("kaikei-attach-blob-3");
    let file = temp_file("missing.txt", "紐付け先の無い証憑");

    let (out, ok) = run_attach(
        &app,
        &blob,
        &file,
        &["--entry", "22222222-2222-2222-2222-222222222222"],
    );

    assert!(!ok, "拒否されること: {out}");
    assert!(out.contains("見つかりません"), "{out}");
}
