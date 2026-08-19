//! `kaikei journalize` の下見。
//!
//! # なぜ E2E で見るのか
//!
//! 下見は**記帳する前に確かめる**ためのものである。ルールに書いた税区分と
//! 取引先が出ていなければ、確かめようがない。**金額が正しくても税区分が
//! 違えば税務上の扱いが変わる**（課税仕入れが対象外になれば控除が消える）。
//!
//! 組み立て（`tags_for_preview`）は単体テストで見ているが、**それを表示に
//! 繋いだかは別**である。実際、繋ぎを外す変異を入れても単体テストは通った。

mod common;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::PgPool;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

fn cli_binary() -> PathBuf {
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

fn app_url(pool: &PgPool) -> String {
    let options = pool.connect_options();
    let database = options.get_database().expect("データベース名");
    let port = options.get_port();
    let password = std::env::var("KAIKEI_APP_PASSWORD").unwrap_or_else(|_| "app".to_string());
    format!("postgres://kaikei_app:{password}@localhost:{port}/{database}")
}

fn run_journalize(pool: &PgPool, rules: &Path) -> (String, String, bool) {
    let mut command = Command::new(cli_binary());
    for key in [
        "KAIKEI_TAX_MODE",
        "KAIKEI_ROUNDING",
        "KAIKEI_ROUNDING_UNIT",
        "KAIKEI_IS_TAXABLE_BUSINESS",
        "KAIKEI_SIMPLIFIED_TAXATION",
        "KAIKEI_BLOB_ROOT",
    ] {
        command.env_remove(key);
    }
    let output = command
        .args(["journalize", "--rules"])
        .arg(rules)
        .env("APP_DATABASE_URL", app_url(pool))
        .env("KAIKEI_BOOK_CURRENCY", "JPY")
        .env("KAIKEI_FISCAL_YEAR_RULE", "calendar_year")
        .output()
        .expect("kaikei journalize を起動できること");
    (
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
        output.status.success(),
    )
}

/// ルールを一時ファイルに書く。
fn write_rules(name: &str, yaml: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("kaikei_rules_{name}.yaml"));
    let mut file = std::fs::File::create(&path).expect("ルールを書けること");
    file.write_all(yaml.as_bytes()).expect("書き込めること");
    path
}

async fn seed(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO accounts (code, name, account_type, postable) \
         VALUES ('609','通信費',5,true), ('110','普通預金',1,true) \
         ON CONFLICT (code) DO NOTHING",
    )
    .execute(pool)
    .await
    .expect("科目");
    sqlx::query(
        "INSERT INTO imported_transactions \
         (id, source, external_key, occurred_on, amount_minor, currency, direction, \
          raw_description, raw_row, status, imported_at) \
         VALUES (gen_random_uuid(), 'test', 'k1', DATE '2026-05-01', 1100, 'JPY', 2, \
                 'テストショウテン', '{}'::jsonb, 'pending', now())",
    )
    .execute(pool)
    .await
    .expect("取込明細");
}

/// **本命。** ルールに書いた税区分と取引先が下見に出る。
///
/// 出ていないと、記帳する前に確かめられない。金額は1円も動かないので、
/// 後から見ても気づけない。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn the_preview_shows_the_tags_from_the_rule(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed(&app).await;
    let rules = write_rules(
        "with_tags",
        "- id: shop\n  priority: 10\n  match:\n    contains: テストショウテン\n  \
         account: \"609\"\n  counter_account: \"110\"\n  \
         tax_category: PURCHASE_10_QUALIFIED\n  counterparty: bitech\n  active: true\n",
    );

    let (stdout, stderr, ok) = run_journalize(&app, &rules);

    assert!(ok, "{stderr}");
    assert!(stdout.contains("ルール: shop"), "当たること: {stdout}");
    assert!(
        stdout.contains("tax_category=PURCHASE_10_QUALIFIED"),
        "税区分を出すこと: {stdout}"
    );
    assert!(
        stdout.contains("counterparty=bitech"),
        "取引先を出すこと: {stdout}"
    );
}

/// タグの無いルールでは、金額の後ろに余計なものを出さない。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_rule_without_tags_shows_no_brackets(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed(&app).await;
    let rules = write_rules(
        "no_tags",
        "- id: shop\n  priority: 10\n  match:\n    contains: テストショウテン\n  \
         account: \"609\"\n  counter_account: \"110\"\n  active: true\n",
    );

    let (stdout, stderr, ok) = run_journalize(&app, &rules);

    assert!(ok, "{stderr}");
    assert!(stdout.contains("ルール: shop"), "{stdout}");
    assert!(!stdout.contains('['), "括弧を出さないこと: {stdout}");
}
