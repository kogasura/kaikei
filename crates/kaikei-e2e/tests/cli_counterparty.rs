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
    // **ソースの場所はここで決める。** CARGO_MANIFEST_DIR は kaikei-e2e を
    // 指すので、CLI とその依存の位置を相対で辿る。
    let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/ ディレクトリ");
    kaikei_e2e::cli_binary_or_panic(&[
        &crates.join("kaikei-cli"),
        &crates.join("kaikei-app"),
        &crates.join("kaikei-jp"),
        &crates.join("kaikei-store"),
        &crates.join("kaikei-report"),
        &crates.join("kaikei-core"),
    ])
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
    // **親の環境変数を引き継がせない。** `Command` は既定で引き継ぐので、
    // `.env` を読んだシェルから走らせると**渡していないつもりの設定が
    // 子プロセスに届く**。それだと「設定が無くても動く」を検査したつもりで
    // 検査できていない。
    //
    // 実際に2度踏んだ（D-113 / D-132）。どちらも「設定が要る処理」を足して
    // 手元では通り、CI で落ちた。**手元と CI で条件を揃える。**
    for key in [
        "KAIKEI_TAX_MODE",
        "KAIKEI_ROUNDING",
        "KAIKEI_ROUNDING_UNIT",
        "KAIKEI_IS_TAXABLE_BUSINESS",
        "KAIKEI_SIMPLIFIED_TAXATION",
        // **手元の .env が漏れると、CIと違う結果になる。** 2026-08-18 に
        // KAIKEI_BLOB_ROOT を .env へ入れたところ、verify が証憑の中身を
        // 検証しようとして手元だけ落ちた（CIには無いので通る）。
        "KAIKEI_BLOB_ROOT",
    ] {
        command.env_remove(key);
    }
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
         bitech,株式会社ビーテック,T7123456789012,true\n",
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

/// `kaikei counterparty verify` を走らせる。
fn run_verify(pool: &PgPool, args: &[&str]) -> (String, String, bool) {
    let mut command = Command::new(cli_binary());
    for key in [
        "KAIKEI_TAX_MODE",
        "KAIKEI_ROUNDING",
        "KAIKEI_ROUNDING_UNIT",
        "KAIKEI_IS_TAXABLE_BUSINESS",
        "KAIKEI_SIMPLIFIED_TAXATION",
        // **手元の .env が漏れると、CIと違う結果になる。** 2026-08-18 に
        // KAIKEI_BLOB_ROOT を .env へ入れたところ、verify が証憑の中身を
        // 検証しようとして手元だけ落ちた（CIには無いので通る）。
        "KAIKEI_BLOB_ROOT",
    ] {
        command.env_remove(key);
    }
    command
        .args(["counterparty", "verify"])
        .args(args)
        .env("APP_DATABASE_URL", app_url(pool));
    let output = command.output().expect("kaikei を起動できること");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    )
}

/// 取引先を1件、登録番号なしで入れる。
async fn seed_counterparty(pool: &PgPool, code: &str, name: &str) {
    sqlx::query(
        "INSERT INTO counterparties (code, name) VALUES ($1, $2) \
         ON CONFLICT (code) DO NOTHING",
    )
    .bind(code)
    .bind(name)
    .execute(pool)
    .await
    .expect("取引先");
}

/// **本命。** 既存の取引先に登録番号を後から入れられる。
///
/// # この経路が無いと詰む
///
/// `counterparty import` は `ON CONFLICT DO NOTHING` なので、既存行に登録番号を
/// 入れられない。**実帳簿の取引先31件はすべて登録番号が空**で、CSV から入れ
/// 直そうとしても「既存を優先」で無視されていた（警告は出るが書き込まれない）。
///
/// 相手が適格請求書発行事業者かどうかは**後から分かる**情報なので、追加しか
/// できないと運用が成り立たない。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_registration_number_can_be_recorded_later(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_counterparty(&app, "jdf", "JDF株式会社").await;

    let (stdout, stderr, ok) = run_verify(
        &app,
        &[
            "--code",
            "jdf",
            "--registration-no",
            "T7123456789012",
            "--qualified",
            "true",
            "--on",
            "2026-08-18",
            "--commit",
        ],
    );

    assert!(ok, "{stderr}");
    assert!(stdout.contains("1 件を更新しました"), "{stdout}");

    // 日付は文字列で受ける（この crate は chrono を直接持たない）。
    let row: (String, Option<String>, Option<bool>, Option<String>) = sqlx::query_as(
        "SELECT name, invoice_reg_no, is_qualified, verified_at::text          FROM counterparties WHERE code = 'jdf'",
    )
    .fetch_one(&app)
    .await
    .expect("取引先");

    assert_eq!(row.0, "JDF株式会社", "**名前は変えない**");
    assert_eq!(row.1.as_deref(), Some("T7123456789012"));
    assert_eq!(row.2, Some(true));
    assert_eq!(row.3.as_deref(), Some("2026-08-18"));
}

/// **本命。** 下見では書き込まない。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_dry_run_does_not_write(pool_opts: PgPoolOptions, conn_opts: PgConnectOptions) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_counterparty(&app, "jdf", "JDF株式会社").await;

    let (stdout, stderr, ok) = run_verify(
        &app,
        &["--code", "jdf", "--registration-no", "T7123456789012"],
    );

    assert!(ok, "{stderr}");
    assert!(stdout.contains("下見"), "{stdout}");

    let reg: Option<String> =
        sqlx::query_scalar("SELECT invoice_reg_no FROM counterparties WHERE code = 'jdf'")
            .fetch_one(&app)
            .await
            .expect("取引先");
    assert_eq!(reg, None, "書き込んでいないこと");
}

/// **本命。** 「非適格と確認した」を記録できる。
///
/// 非適格と分かっていれば経過措置で処理できる。**「未確認」とは別物**である
/// （D-122）。登録番号が無くても記録できなければならない。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_non_qualified_issuer_can_be_recorded(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_counterparty(&app, "povo", "povo").await;

    let (_stdout, stderr, ok) = run_verify(
        &app,
        &["--code", "povo", "--qualified", "false", "--commit"],
    );

    assert!(ok, "{stderr}");
    let is_qualified: Option<bool> =
        sqlx::query_scalar("SELECT is_qualified FROM counterparties WHERE code = 'povo'")
            .fetch_one(&app)
            .await
            .expect("取引先");
    assert_eq!(is_qualified, Some(false), "**未確認(NULL)と区別すること**");
}

/// **本命。** 省略した項目は既存の値を残す。
///
/// `None` をそのまま渡すと消える。登録番号を入れた後に `--qualified` だけを
/// 直したとき、番号が消えてはいけない。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn an_omitted_field_keeps_its_existing_value(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_counterparty(&app, "jdf", "JDF株式会社").await;
    run_verify(
        &app,
        &[
            "--code",
            "jdf",
            "--registration-no",
            "T7123456789012",
            "--commit",
        ],
    );

    // 登録番号は指定せず、適格の判定だけ変える。
    let (_stdout, stderr, ok) =
        run_verify(&app, &["--code", "jdf", "--qualified", "true", "--commit"]);

    assert!(ok, "{stderr}");
    let reg: Option<String> =
        sqlx::query_scalar("SELECT invoice_reg_no FROM counterparties WHERE code = 'jdf'")
            .fetch_one(&app)
            .await
            .expect("取引先");
    assert_eq!(
        reg.as_deref(),
        Some("T7123456789012"),
        "省略した登録番号が消えないこと"
    );
}

/// 登録番号の形（チェックデジット）が違えば、書き込む前に止める。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_bad_registration_number_is_rejected_before_writing(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_counterparty(&app, "jdf", "JDF株式会社").await;

    let (_stdout, stderr, ok) = run_verify(
        &app,
        &[
            "--code",
            "jdf",
            "--registration-no",
            // **わざと検査用数字を誤らせた番号。** 基礎番号 123456789012 の
            // 検査用数字は 7 なので、先頭の 1 は誤りである。
            // 一括置換でここまで正しい番号にしないこと（実際にやって、
            // 「弾かれること」を確かめるテストが弾かれなくなった）。
            "T1123456789012",
            "--commit",
        ],
    );

    assert!(!ok, "止まること");
    assert!(stderr.contains("チェックデジット"), "{stderr}");
}

/// 居ない取引先は、追加せずに止める。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn an_unknown_counterparty_stops(pool_opts: PgPoolOptions, conn_opts: PgConnectOptions) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;

    let (_stdout, stderr, ok) = run_verify(
        &app,
        &["--code", "nosuch", "--qualified", "true", "--commit"],
    );

    assert!(!ok, "止まること");
    assert!(stderr.contains("見つかりません"), "{stderr}");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM counterparties")
        .fetch_one(&app)
        .await
        .unwrap();
    assert_eq!(count, 0, "**勝手に作らないこと**");
}
