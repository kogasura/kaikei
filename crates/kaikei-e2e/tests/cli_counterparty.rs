//! `kaikei counterparty import` を**実バイナリ**で検査する。
//!
//! 単体テストは CSV の読み取りとユースケースを別々に固定しているが、
//! 「CLI から流したときに実際に `counterparties` に行が入るか」は誰も見て
//! いなかった。この経路が繋がっていないと、取引先タグは1件も付けられない
//! （`PolicyError::UnknownCounterparty` で記帳が弾かれる）。

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
    let database = options.get_database().expect("データベース名");
    let port = options.get_port();
    let password = std::env::var("KAIKEI_APP_PASSWORD").unwrap_or_else(|_| "app".to_string());
    format!("postgres://kaikei_app:{password}@localhost:{port}/{database}")
}

/// `kaikei counterparty import` を走らせ、`(stdout, stderr, 成功したか)` を返す。
fn run_import(pool: &PgPool, csv: &Path, commit: bool) -> (String, String, bool) {
    let mut command = Command::new(cli_binary());
    command
        .arg("counterparty")
        .arg("import")
        .arg("--file")
        .arg(csv)
        .env("APP_DATABASE_URL", app_url(pool));
    if commit {
        command.arg("--commit");
    }
    let output = command.output().expect("kaikei を起動できること");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    )
}

/// CSV を一時ファイルに書く。
fn write_csv(name: &str, contents: &str) -> PathBuf {
    let path = std::env::temp_dir().join(name);
    std::fs::write(&path, contents).expect("CSV を書けること");
    path
}

async fn codes_in_db(pool: &PgPool) -> Vec<(String, String, Option<bool>)> {
    sqlx::query_as("SELECT code, name, is_qualified FROM counterparties ORDER BY code")
        .fetch_all(pool)
        .await
        .expect("取引先を読めること")
}

/// **本命。** `--commit` を付けないと1行も入らない。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_dry_run_does_not_write_anything(pool_opts: PgPoolOptions, conn_opts: PgConnectOptions) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    let csv = write_csv(
        "kaikei_cp_dry_run.csv",
        "code,name,invoice_registration_no,is_qualified\nanthropic,Anthropic,,\n",
    );

    let (stdout, stderr, ok) = run_import(&app, &csv, false);

    assert!(ok, "{stderr}");
    assert!(stdout.contains("下見"), "下見だと言うこと: {stdout}");
    assert!(
        codes_in_db(&app).await.is_empty(),
        "--commit が無ければ1行も入らないこと"
    );
}

/// **本命。** `--commit` を付けると実際に入る。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn commit_actually_inserts_rows(pool_opts: PgPoolOptions, conn_opts: PgConnectOptions) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    let csv = write_csv(
        "kaikei_cp_commit.csv",
        "code,name,invoice_registration_no,is_qualified\n\
         anthropic,Anthropic,,\n\
         bitech,株式会社ビーテック,T1234567890123,true\n",
    );

    let (stdout, stderr, ok) = run_import(&app, &csv, true);

    assert!(ok, "{stderr}");
    assert!(stdout.contains("追加 2 件"), "{stdout}");

    let rows = codes_in_db(&app).await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0].0, "anthropic");
    assert_eq!(
        rows[0].2, None,
        "空欄は未確認（NULL）で入ること。false にしてはいけない"
    );
    assert_eq!(rows[1].0, "bitech");
    assert_eq!(rows[1].2, Some(true));
}

/// **本命。** 2回流しても増えず、既存の確認結果を上書きしない。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_second_import_does_not_overwrite_the_qualified_flag(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;

    // 1回目: 適格だと確認済みで入れる。
    let first = write_csv(
        "kaikei_cp_first.csv",
        "code,name,is_qualified\nanthropic,Anthropic,true\n",
    );
    let (_stdout, stderr, ok) = run_import(&app, &first, true);
    assert!(ok, "{stderr}");

    // 2回目: 外部システムから「誰も入力していないので false」が流れてくる。
    let second = write_csv(
        "kaikei_cp_second.csv",
        "code,name,is_qualified\nanthropic,Anthropic,false\n",
    );
    let (stdout, stderr, ok) = run_import(&app, &second, true);

    assert!(ok, "{stderr}");
    assert!(stdout.contains("追加 0 件"), "{stdout}");
    assert!(
        stderr.contains("is_qualified_invoice_issuer"),
        "食い違いを知らせること: {stderr}"
    );

    let rows = codes_in_db(&app).await;
    assert_eq!(
        rows[0].2,
        Some(true),
        "確認結果が外部の値で消されないこと: {rows:?}"
    );
}

/// 入力の中に同じコードが2回あっても、1件しか入らない。
///
/// 外部システムには表記ゆれの重複が普通にある（freee には
/// 「株式会社 ビーテック」と「株式会社ビーテック」が別々に登録されていた）。
/// 挿入自体は `ON CONFLICT DO NOTHING` が飛ばすので落ちないが、
/// **入る件数と報告する件数が食い違わないこと**をここで固定する。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_duplicate_code_within_one_file_does_not_fail(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    let csv = write_csv(
        "kaikei_cp_dup.csv",
        "code,name\nbitech,株式会社ビーテック\nbitech,株式会社 ビーテック\n",
    );

    let (stdout, stderr, ok) = run_import(&app, &csv, true);

    assert!(ok, "重複があっても失敗しないこと: {stderr}");
    assert!(stdout.contains("追加 1 件"), "{stdout}");
    assert!(
        !stdout.contains("別のプロセス"),
        "同時投入のせいにしないこと（重複は入力の中にある）: {stdout}"
    );
    assert!(
        stderr.contains("株式会社ビーテック"),
        "どちらを残したかを見せること: {stderr}"
    );
    assert_eq!(codes_in_db(&app).await.len(), 1);
}
