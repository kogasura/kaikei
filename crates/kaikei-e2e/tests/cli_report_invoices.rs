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
    run_report_with(pool, out_dir, blob_root, &[])
}

/// `report` を、環境変数を足して走らせる。
///
/// **既定では税制の設定を渡さない。** `report` は帳簿を書き出すコマンドで
/// あって、税制の設定が無くても動かなければならない。渡してしまうと、
/// 「設定が要る処理」を足したときに気づけない（実際、消費税の集計を足した
/// ときに `report` 全体が落ちるようになり、CI で10件落ちて気づいた）。
fn run_report_with(
    pool: &PgPool,
    out_dir: &Path,
    blob_root: &Path,
    extra_env: &[(&str, &str)],
) -> (String, bool) {
    let mut command = Command::new(cli_binary());
    // **親の環境変数を引き継がせない。** 手元では `.env` を読んだシェルから
    // 走らせることがあり、渡していないつもりの設定が子プロセスに届く。
    // それだと「設定が無くても動く」を検査したつもりで検査できていない。
    command.env_remove("KAIKEI_TAX_MODE");
    command.env_remove("KAIKEI_ROUNDING");
    command.env_remove("KAIKEI_ROUNDING_UNIT");
    command.env_remove("KAIKEI_IS_TAXABLE_BUSINESS");
    command
        .arg("report")
        .args(["--year", "2026"])
        .args(["--out", &out_dir.display().to_string()])
        .env("APP_DATABASE_URL", app_url(pool))
        .env("KAIKEI_BLOB_ROOT", blob_root.display().to_string())
        .env("KAIKEI_BOOK_CURRENCY", "JPY")
        .env("KAIKEI_FISCAL_YEAR_RULE", "calendar_year");
    for (key, value) in extra_env {
        command.env(key, value);
    }
    let output = command.output().expect("kaikei report を起動できること");
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

/// 決算振替の仕訳を1本入れる（`entry_kind: closing` タグ付き）。
///
/// **タグが目印である。** 日付だけでは、12月31日の普通の取引と区別できない。
async fn seed_closing_entry(pool: &PgPool, entry_no: i32, amount: i64) {
    let id: String = sqlx::query_scalar("SELECT gen_random_uuid()::text")
        .fetch_one(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO journal_entries \
         (id, fiscal_year, entry_no, entry_date, description, recorded_at) \
         VALUES ($1::uuid, 2026, $2, DATE '2026-12-31', '決算振替', now())",
    )
    .bind(&id)
    .bind(entry_no)
    .execute(pool)
    .await
    .expect("決算振替の仕訳");
    sqlx::query(
        "INSERT INTO journal_lines \
         (entry_id, line_no, account_code, side, amount_minor, currency, currency_minor_unit, tags) \
         VALUES ($1::uuid, 1, '609', 2, $2, 'JPY', 0, \
                 '{\"entry_kind\":{\"t\":\"code\",\"v\":\"closing\"}}'), \
                ($1::uuid, 2, '400', 1, $2, 'JPY', 0, \
                 '{\"entry_kind\":{\"t\":\"code\",\"v\":\"closing\"}}')",
    )
    .bind(&id)
    .bind(amount)
    .execute(pool)
    .await
    .expect("決算振替の明細");
}

/// **本命。** 決算振替を記帳しても、青色申告決算書は1バイトも変わらない。
///
/// # なぜ大事か
///
/// 決算振替は収益・費用を元入金へ振り替える。**そのまま集計すると売上0・
/// 所得0の決算書ができる**（D-101）。集計から外しているのはそのためで、
/// 外し方を壊すと**決算書が静かに空になる**——貸借は一致したままなので、
/// 見ても気づけない。
///
/// 手順書（`/kaikei-year-end` の 4）は「前後で diff を取れ」と書いているが、
/// **人がやるので忘れる。** ここで固定する。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn the_blue_return_does_not_change_when_the_closing_entry_is_posted(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    sqlx::query(
        "INSERT INTO accounts (code, name, account_type, postable) \
         VALUES ('400','元入金',3,true) ON CONFLICT (code) DO NOTHING",
    )
    .execute(&app)
    .await
    .expect("元入金");
    seed(&app, 1, 43_967).await;
    let blob = temp_dir("closing-blob");

    let before_dir = temp_dir("closing-before");
    let (log, ok) = run_report(&app, &before_dir, &blob);
    assert!(ok, "{log}");
    let before = std::fs::read_to_string(before_dir.join("blue_return.csv")).unwrap();

    seed_closing_entry(&app, 900, 43_967).await;

    let after_dir = temp_dir("closing-after");
    let (log, ok) = run_report(&app, &after_dir, &blob);
    assert!(ok, "{log}");
    let after = std::fs::read_to_string(after_dir.join("blue_return.csv")).unwrap();

    assert_eq!(
        before, after,
        "決算振替を記帳しても決算書は変わらないこと（D-101）"
    );
    // **空になっていないことも見る。** 両方とも空なら「同じ」は成り立つ。
    assert!(
        before.contains("43967"),
        "そもそも決算書に金額が載っていること: {before}"
    );
}

/// **本命。** 仕訳日記帳のほうは決算振替のぶんだけ増える。
///
/// 決算振替は帳簿に実在する仕訳なので、**帳簿そのものを見る出力には載る**。
/// 上のテストだけだと「どの出力も変わらない」実装でも通ってしまう。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn the_journal_book_does_grow_when_the_closing_entry_is_posted(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    sqlx::query(
        "INSERT INTO accounts (code, name, account_type, postable) \
         VALUES ('400','元入金',3,true) ON CONFLICT (code) DO NOTHING",
    )
    .execute(&app)
    .await
    .expect("元入金");
    seed(&app, 1, 43_967).await;
    let blob = temp_dir("journal-blob");

    let before_dir = temp_dir("journal-before");
    run_report(&app, &before_dir, &blob);
    let before = std::fs::read_to_string(before_dir.join("journal_book.csv")).unwrap();

    seed_closing_entry(&app, 900, 43_967).await;

    let after_dir = temp_dir("journal-after");
    run_report(&app, &after_dir, &blob);
    let after = std::fs::read_to_string(after_dir.join("journal_book.csv")).unwrap();

    assert!(
        after.lines().count() > before.lines().count(),
        "仕訳日記帳は決算振替のぶん増えること（{} → {}）",
        before.lines().count(),
        after.lines().count()
    );
}

/// 期首振替の仕訳を、指定した日付で入れる（`entry_kind: opening` タグ付き）。
async fn seed_opening_transfer(pool: &PgPool, entry_no: i32, amount: i64, date: &str) {
    let id: String = sqlx::query_scalar("SELECT gen_random_uuid()::text")
        .fetch_one(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO journal_entries          (id, fiscal_year, entry_no, entry_date, description, recorded_at)          VALUES ($1::uuid, 2026, $2, $3::date, '期首振替', now())",
    )
    .bind(&id)
    .bind(entry_no)
    .bind(date)
    .execute(pool)
    .await
    .expect("期首振替の仕訳");
    sqlx::query(
        "INSERT INTO journal_lines          (entry_id, line_no, account_code, side, amount_minor, currency, currency_minor_unit, tags)          VALUES ($1::uuid, 1, '410', 2, $2, 'JPY', 0,                  '{\"entry_kind\":{\"t\":\"code\",\"v\":\"opening\"}}'),                 ($1::uuid, 2, '400', 1, $2, 'JPY', 0,                  '{\"entry_kind\":{\"t\":\"code\",\"v\":\"opening\"}}')",
    )
    .bind(&id)
    .bind(amount)
    .execute(pool)
    .await
    .expect("期首振替の明細");
}

/// 事業主貸を動かす仕訳を、年度を指定して1本入れる。
async fn seed_drawing_in(pool: &PgPool, fiscal_year: i32, entry_no: i32, amount: i64, date: &str) {
    let id: String = sqlx::query_scalar("SELECT gen_random_uuid()::text")
        .fetch_one(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO journal_entries          (id, fiscal_year, entry_no, entry_date, description, recorded_at)          VALUES ($1::uuid, $4, $2, $3::date, '事業主貸', now())",
    )
    .bind(&id)
    .bind(entry_no)
    .bind(date)
    .bind(fiscal_year)
    .execute(pool)
    .await
    .expect("事業主貸の仕訳");
    sqlx::query(
        "INSERT INTO journal_lines          (entry_id, line_no, account_code, side, amount_minor, currency, currency_minor_unit)          VALUES ($1::uuid, 1, '410', 1, $2, 'JPY', 0), ($1::uuid, 2, '110', 2, $2, 'JPY', 0)",
    )
    .bind(&id)
    .bind(amount)
    .execute(pool)
    .await
    .expect("事業主貸の明細");
}

/// 事業主貸を動かす仕訳を1本入れる。
async fn seed_drawing(pool: &PgPool, entry_no: i32, amount: i64, date: &str) {
    let id: String = sqlx::query_scalar("SELECT gen_random_uuid()::text")
        .fetch_one(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO journal_entries          (id, fiscal_year, entry_no, entry_date, description, recorded_at)          VALUES ($1::uuid, 2026, $2, $3::date, '事業主貸', now())",
    )
    .bind(&id)
    .bind(entry_no)
    .bind(date)
    .execute(pool)
    .await
    .expect("事業主貸の仕訳");
    sqlx::query(
        "INSERT INTO journal_lines          (entry_id, line_no, account_code, side, amount_minor, currency, currency_minor_unit)          VALUES ($1::uuid, 1, '410', 1, $2, 'JPY', 0), ($1::uuid, 2, '110', 2, $2, 'JPY', 0)",
    )
    .bind(&id)
    .bind(amount)
    .execute(pool)
    .await
    .expect("事業主貸の明細");
}

async fn seed_owner_accounts(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO accounts (code, name, account_type, postable)          VALUES ('400','元入金',3,true), ('410','事業主貸',3,true),                 ('420','事業主借',3,true), ('110','普通預金',1,true)          ON CONFLICT (code) DO NOTHING",
    )
    .execute(pool)
    .await
    .expect("科目");
}

/// **本命。** 期首振替を年内に記帳したら知らせる。
///
/// **決算書の貸借対照表から事業主貸が消える。** 青色申告決算書の様式には
/// この欄があり、期末残高をそのまま書く。0 で提出することになる。
///
/// 実帳簿の複製で試したら再現した——事業主貸 9,923,381円 と
/// 事業主借 1,012,434円 が消え、`verify` は終了コード0のままだった。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn an_opening_transfer_posted_within_the_year_is_reported(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_owner_accounts(&app).await;
    seed_drawing(&app, 1, 500_000, "2026-03-10").await;
    // **年内**（12月31日）に期首振替を入れる。翌年1月1日が正しい。
    seed_opening_transfer(&app, 900, 500_000, "2026-12-31").await;
    let out = temp_dir("early-out");
    let blob = temp_dir("early-blob");

    let (log, ok) = run_report(&app, &out, &blob);

    assert!(ok, "指摘があっても書き出しは成功すること: {log}");
    assert!(
        log.contains("事業主貸 は期中に動いているのに、期末残高が 0 です"),
        "{log}"
    );
    assert!(log.contains("翌年1月1日"), "何が正しいかを言うこと: {log}");
}

/// **本命。** 翌年1月1日に記帳していれば言わない。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn an_opening_transfer_on_the_first_of_january_is_quiet(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_owner_accounts(&app).await;
    seed_drawing(&app, 1, 500_000, "2026-03-10").await;
    // 2026年度の決算書には入らない日付（翌年）。
    let out = temp_dir("ontime-out");
    let blob = temp_dir("ontime-blob");

    let (log, ok) = run_report(&app, &out, &blob);

    assert!(ok, "{log}");
    assert!(!log.contains("期中に動いているのに"), "{log}");
}

/// 期中に動いていなければ言わない（0のままが正しい年はある）。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_year_without_any_drawing_is_quiet(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_owner_accounts(&app).await;
    seed(&app, 1, 43_967).await;
    let out = temp_dir("nodraw-out");
    let blob = temp_dir("nodraw-blob");

    let (log, ok) = run_report(&app, &out, &blob);

    assert!(ok, "{log}");
    assert!(!log.contains("期中に動いているのに"), "{log}");
}

/// **本命。** その年に事業主貸を使っていなくても、年内の期首振替は拾う。
///
/// # なぜ要るか
///
/// 期首振替は**前年からの繰越**を元入金へ振り替える。当年に事業主貸を1度も
/// 使っていなくても、繰越があれば振替は起きる。
///
/// 「期中に動いたか」を見るとき**期首振替そのものを除いてしまうと、この場合に
/// 何も動いていないことになり、見逃す。** 実際、除く分岐を入れたときに
/// 変異テストで気づいた（除いても他のテストが落ちなかった）。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn an_early_opening_transfer_is_reported_even_without_other_drawings(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_owner_accounts(&app).await;
    seed(&app, 1, 43_967).await;
    // **前年からの繰越**を作る。貸借対照表は帳簿の最初からの累計なので、
    // 前年の事業主貸が残高として乗る。
    seed_drawing_in(&app, 2025, 1, 500_000, "2025-06-10").await;
    // 当年は事業主貸を1度も使わず、年内の期首振替だけを入れる。
    seed_opening_transfer(&app, 900, 500_000, "2026-12-31").await;
    let out = temp_dir("earlyonly-out");
    let blob = temp_dir("earlyonly-blob");

    let (log, ok) = run_report(&app, &out, &blob);

    assert!(ok, "{log}");
    assert!(
        log.contains("事業主貸 は期中に動いているのに、期末残高が 0 です"),
        "他に動きが無くても拾うこと: {log}"
    );
}

/// **本命。** 消費税の集計が決算書と一緒に出る。
///
/// 確定申告には消費税の申告も含まれる。**画面で読む人と、ファイルを受け取る
/// 人は別である**——税理士に渡すのはファイルの方なので、`report` の一式に
/// 入っていなければ届かない。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn the_report_includes_the_consumption_tax_summary(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed(&app, 1, 43_967).await;
    let out = temp_dir("ctax-out");
    let blob = temp_dir("ctax-blob");

    let (log, ok) = run_report_with(&app, &out, &blob, &[("KAIKEI_TAX_MODE", "inclusive")]);

    assert!(ok, "{log}");
    let csv = std::fs::read_to_string(out.join("consumption_tax.csv"))
        .expect("consumption_tax.csv が出ていること");
    assert!(csv.contains("PURCHASE_10_QUALIFIED"), "{csv}");
    assert!(csv.contains("43967"), "{csv}");
    // 43,967 × 10/110 = 3,997
    assert!(csv.contains("3997"), "税額を出すこと: {csv}");

    // **注意書きも一緒に出す。** 数字だけ渡すと申告書の金額と読まれる。
    let notes = std::fs::read_to_string(out.join("consumption_tax_notes.txt"))
        .expect("consumption_tax_notes.txt が出ていること");
    assert!(notes.contains("申告書の金額ではありません"), "{notes}");
}

/// **本命。** 税制の設定が無くても `report` は成功する。
///
/// # この穴で実際に壊れた
///
/// 消費税の集計を足したとき、`jp_settings()?` を呼んだために
/// `KAIKEI_ROUNDING` / `KAIKEI_TAX_MODE` が無い環境で **`report` 全体が
/// 落ちるようになった**。手元では `.env` を読んでいたので気づかず、
/// CI の E2E が10件落ちて分かった。
///
/// **設定が無いことを理由に、本体の出力まで止めない**（D-113 と同じ）。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn the_report_works_without_any_tax_settings(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed(&app, 1, 43_967).await;
    let out = temp_dir("noenv-out");
    let blob = temp_dir("noenv-blob");

    // 税制の設定を1つも渡さない。
    let (log, ok) = run_report(&app, &out, &blob);

    assert!(ok, "設定が無くても書き出せること: {log}");
    assert!(out.join("blue_return.csv").is_file(), "決算書は出ること");
    // **月別も出る。** 決算書2ページ目の月別欄は、税制の設定が無くても
    // 帳簿だけで埋まる（消費税と違い経理方式に依らない）。
    assert!(
        out.join("monthly_sales.csv").is_file(),
        "月別売上は出ること"
    );
    // 消費税の集計は出ない（経理方式が分からないので）。**黙って空の集計を
    // 出すよりよい。**
    assert!(
        !out.join("consumption_tax.csv").is_file(),
        "経理方式が分からなければ消費税の集計は出さない"
    );
    // **出さなかったことを言う。** 書き出したファイルの一覧しか出さないと、
    // 受け取った側は「これで全部」と読む。確定申告には消費税の申告も
    // 含まれるので、黙って落とすと痛い。
    assert!(
        log.contains("消費税の集計は出していません"),
        "出さなかったことを言うこと: {log}"
    );
    assert!(
        log.contains("KAIKEI_TAX_MODE"),
        "理由（設定が無い）を言うこと: {log}"
    );
}
