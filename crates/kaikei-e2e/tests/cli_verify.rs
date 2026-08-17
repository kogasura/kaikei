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

/// 税区分・取引先のタグを付けた仕訳を1本入れる。
async fn seed_entry_with_tags(
    pool: &PgPool,
    entry_no: i32,
    debit: &str,
    credit: &str,
    amount: i64,
    debit_tags: &str,
) {
    let mut tx = pool.begin().await.expect("トランザクション");
    let id: String = sqlx::query_scalar("SELECT gen_random_uuid()::text")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO journal_entries          (id, fiscal_year, entry_no, entry_date, description, recorded_at)          VALUES ($1::uuid, 2026, $2, DATE '2026-06-15', 'テスト', now())",
    )
    .bind(&id)
    .bind(entry_no)
    .execute(&mut *tx)
    .await
    .expect("仕訳");
    sqlx::query(
        "INSERT INTO journal_lines          (entry_id, line_no, account_code, side, amount_minor, currency, currency_minor_unit, tags)          VALUES ($1::uuid, 1, $2, 1, $4, 'JPY', 0, $5::jsonb),                 ($1::uuid, 2, $3, 2, $4, 'JPY', 0, '{}'::jsonb)",
    )
    .bind(&id)
    .bind(debit)
    .bind(credit)
    .bind(amount)
    .bind(debit_tags)
    .execute(&mut *tx)
    .await
    .expect("明細");
    tx.commit().await.expect("コミット");
}

/// 税区分のタグを**貸方**に付けた仕訳を1本入れる（返金・値引きの形）。
async fn seed_entry_with_tags_on_credit(
    pool: &PgPool,
    entry_no: i32,
    debit: &str,
    credit: &str,
    amount: i64,
    credit_tags: &str,
) {
    let mut tx = pool.begin().await.expect("トランザクション");
    let id: String = sqlx::query_scalar("SELECT gen_random_uuid()::text")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO journal_entries (id, fiscal_year, entry_no, entry_date, description, recorded_at) VALUES ($1::uuid, 2026, $2, DATE '2026-06-15', '返金', now())",
    )
    .bind(&id)
    .bind(entry_no)
    .execute(&mut *tx)
    .await
    .expect("仕訳");
    sqlx::query(
        "INSERT INTO journal_lines (entry_id, line_no, account_code, side, amount_minor, currency, currency_minor_unit, tags) VALUES ($1::uuid, 1, $2, 1, $4, 'JPY', 0, '{}'::jsonb), ($1::uuid, 2, $3, 2, $4, 'JPY', 0, $5::jsonb)",
    )
    .bind(&id)
    .bind(debit)
    .bind(credit)
    .bind(amount)
    .bind(credit_tags)
    .execute(&mut *tx)
    .await
    .expect("明細");
    tx.commit().await.expect("コミット");
}

/// **本命。** 適格請求書が要る税区分に取引先が無ければ指摘する。
///
/// 実際に weBanana.SP の帳簿が 603 件この状態だった。`JpTaxPolicy` は
/// 取引先タグが**有る**ときにしか適格性を見ないので、無いまま記帳されると
/// 検証がすり抜ける。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn verify_reports_a_qualified_purchase_without_a_counterparty(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    // 5=Expense, 1=Asset。
    seed_account(&app, "609", "通信費", 5).await;
    seed_account(&app, "110", "普通預金", 1).await;
    seed_entry_with_tags(
        &app,
        1,
        "609",
        "110",
        35_829,
        r#"{"tax_category": {"t": "code", "v": "PURCHASE_10_QUALIFIED"}}"#,
    )
    .await;

    let (_stdout, stderr, ok) = run_verify(&app);

    assert!(ok, "指摘があっても検査は失敗しないこと: {stderr}");
    assert!(stderr.contains("適格請求書"), "{stderr}");
    assert!(
        stderr.contains("1 件"),
        "件数を出すこと（明細1行だけが対象）: {stderr}"
    );
}

/// **本命。** 少額特例の境目で分けて数える。
///
/// 件数だけでは動けない。実帳簿は 603件 だが、取引単位で1万円未満と1万円以上に
/// 分けると **570件 / 33件** になる。請求書を実際に揃える必要があるのは後者だけ
/// かもしれず、**手を付けられる大きさかどうかが変わる**。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn verify_splits_the_count_at_the_small_amount_threshold(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_account(&app, "609", "通信費", 5).await;
    seed_account(&app, "110", "普通預金", 1).await;
    let qualified = r#"{"tax_category": {"t": "code", "v": "PURCHASE_10_QUALIFIED"}}"#;
    // 1万円未満が2件、1万円以上が1件。
    seed_entry_with_tags(&app, 1, "609", "110", 9_999, qualified).await;
    seed_entry_with_tags(&app, 2, "609", "110", 220, qualified).await;
    seed_entry_with_tags(&app, 3, "609", "110", 35_829, qualified).await;

    let (_stdout, stderr, ok) = run_verify(&app);

    assert!(ok, "{stderr}");
    assert!(
        stderr.contains("1万円未満 2 件"),
        "1万円未満の件数: {stderr}"
    );
    assert!(
        stderr.contains("1万円以上 1 件・35,829 円"),
        "1万円以上は件数と金額の両方: {stderr}"
    );
    // **免除されるのは請求書の保存だけ**であることを必ず添える。
    assert!(
        stderr.contains("帳簿の記載事項は免除されません"),
        "{stderr}"
    );
    assert!(
        stderr.contains("令和11年9月30日"),
        "期限を出すこと: {stderr}"
    );
}

/// **本命。** 貸方に立つ課税仕入れ（返金・値引き）は数えない。
///
/// 返還に要るのは適格請求書ではなく**適格返還請求書**である。同じ数に混ぜると
/// 「請求書を探しても見つからない」ことになる。実帳簿では 603件 のうち5件が
/// これ（ドメイン代の返金 60,831円）だった。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_refund_on_the_credit_side_is_not_counted(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_account(&app, "609", "通信費", 5).await;
    seed_account(&app, "110", "普通預金", 1).await;
    let qualified = r#"{"tax_category": {"t": "code", "v": "PURCHASE_10_QUALIFIED"}}"#;
    // 仕入れ（借方 通信費）。
    seed_entry_with_tags(&app, 1, "609", "110", 43_967, qualified).await;
    // 返金（借方 普通預金 / 貸方 通信費）。税区分は貸方に付く。
    seed_entry_with_tags_on_credit(&app, 2, "110", "609", 43_967, qualified).await;

    let (_stdout, stderr, ok) = run_verify(&app);

    assert!(ok, "{stderr}");
    assert!(
        stderr.contains("1万円以上 1 件・43,967 円"),
        "返金を数えないこと: {stderr}"
    );
}

/// **本命。** 1万円ちょうどは1万円以上として数える。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn exactly_ten_thousand_counts_as_large(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_account(&app, "609", "通信費", 5).await;
    seed_account(&app, "110", "普通預金", 1).await;
    let qualified = r#"{"tax_category": {"t": "code", "v": "PURCHASE_10_QUALIFIED"}}"#;
    seed_entry_with_tags(&app, 1, "609", "110", 9_999, qualified).await;
    seed_entry_with_tags(&app, 2, "609", "110", 10_000, qualified).await;

    let (_stdout, stderr, ok) = run_verify(&app);

    assert!(ok, "{stderr}");
    assert!(stderr.contains("1万円未満 1 件"), "{stderr}");
    assert!(
        stderr.contains("1万円以上 1 件・10,000 円"),
        "1万円ちょうどは対象外: {stderr}"
    );
}

/// 取引先が付いていれば指摘しない。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_qualified_purchase_with_a_counterparty_is_not_reported(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_account(&app, "609", "通信費", 5).await;
    seed_account(&app, "110", "普通預金", 1).await;
    seed_entry_with_tags(
        &app,
        1,
        "609",
        "110",
        35_829,
        r#"{"tax_category": {"t": "code", "v": "PURCHASE_10_QUALIFIED"}, "counterparty": {"t": "code", "v": "ANTHROPIC"}}"#,
    )
    .await;

    let (_stdout, stderr, ok) = run_verify(&app);

    assert!(ok, "{stderr}");
    assert!(
        !stderr.contains("適格請求書"),
        "取引先が記録されていれば指摘しないこと: {stderr}"
    );
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

/// **本命。** 証憑が1件も付いていないことを数字で出す。
///
/// 1件も登録されていないことは帳簿を見ても分からない。**数字が見えないと、
/// 登録が進んでいるかどうかも分からない。**
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn verify_shows_how_many_entries_have_documents(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_account(&app, "110", "普通預金", 1).await;
    seed_account(&app, "500", "売上高", 4).await;
    seed_entry(&app, 1, "110", "500", 100_000).await;

    let (stdout, stderr, ok) = run_verify(&app);

    assert!(ok, "{stderr}");
    assert!(
        stdout.contains("証憑が付いている仕訳: 0 / 1 件"),
        "件数を数字で出すこと: {stdout}"
    );
    // 数字だけでは、登録の仕方が分からないまま放置される。
    assert!(
        stdout.contains("kaikei attach"),
        "次の手を示すこと: {stdout}"
    );
    // **断定しない。** 保存義務を満たしているかは事業者の状況で変わる。
    assert!(
        stdout.contains("事業者の状況によります"),
        "断定しないこと: {stdout}"
    );
}

/// 証憑が付いていれば、その数が出る。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn verify_counts_the_entries_that_have_documents(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_account(&app, "110", "普通預金", 1).await;
    seed_account(&app, "500", "売上高", 4).await;
    seed_entry(&app, 1, "110", "500", 100_000).await;
    seed_entry(&app, 2, "110", "500", 200_000).await;

    // 1件目の仕訳にだけ証憑を紐付ける。
    let entry_id: String =
        sqlx::query_scalar("SELECT id::text FROM journal_entries ORDER BY entry_no LIMIT 1")
            .fetch_one(&app)
            .await
            .unwrap();
    sqlx::query(
        "INSERT INTO documents          (id, blob_hash, original_name, mime_type, byte_size, doc_date, doc_type,           received_via, received_at, created_at)          VALUES ('33333333-3333-3333-3333-333333333333', repeat('a', 64), 'x.pdf',                  'application/pdf', 10, DATE '2026-06-15', 'receipt', 'download', now(), now())",
    )
    .execute(&app)
    .await
    .expect("証憑");
    sqlx::query(
        "INSERT INTO entry_documents (entry_id, document_id)          VALUES ($1::uuid, '33333333-3333-3333-3333-333333333333')",
    )
    .bind(&entry_id)
    .execute(&app)
    .await
    .expect("紐付け");

    let (stdout, stderr, ok) = run_verify(&app);

    assert!(ok, "{stderr}");
    assert!(
        stdout.contains("証憑が付いている仕訳: 1 / 2 件"),
        "紐付いた仕訳だけを数えること: {stdout}"
    );
    // 全部に付いていないので、案内は出さない（0件のときだけ出す）。
    assert!(
        !stdout.contains("1件も紐付いていません"),
        "0件でないのに0件の案内を出さないこと: {stdout}"
    );
}

/// **本命。** 1つの仕訳に証憑が複数付いていても、仕訳の数として数える。
///
/// 証憑の数を数えると「何件の仕訳が裏付けられているか」が分からない。
/// 請求書と領収書の両方を付けるのは普通にあるので、放っておくと件数が
/// 仕訳の総数を超える。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn two_documents_on_one_entry_still_count_as_one_entry(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_account(&app, "110", "普通預金", 1).await;
    seed_account(&app, "500", "売上高", 4).await;
    seed_entry(&app, 1, "110", "500", 100_000).await;

    let entry_id: String = sqlx::query_scalar("SELECT id::text FROM journal_entries LIMIT 1")
        .fetch_one(&app)
        .await
        .unwrap();
    // 同じ仕訳に請求書と領収書を付ける。
    for (id, hash, doc_type) in [
        ("44444444-4444-4444-4444-444444444444", "a", "invoice"),
        ("55555555-5555-5555-5555-555555555555", "b", "receipt"),
    ] {
        sqlx::query(
            "INSERT INTO documents              (id, blob_hash, original_name, mime_type, byte_size, doc_date, doc_type,               received_via, received_at, created_at)              VALUES ($1::uuid, repeat($2, 64), 'x.pdf', 'application/pdf', 10,                      DATE '2026-06-15', $3, 'download', now(), now())",
        )
        .bind(id)
        .bind(hash)
        .bind(doc_type)
        .execute(&app)
        .await
        .expect("証憑");
        sqlx::query(
            "INSERT INTO entry_documents (entry_id, document_id) VALUES ($1::uuid, $2::uuid)",
        )
        .bind(&entry_id)
        .bind(id)
        .execute(&app)
        .await
        .expect("紐付け");
    }

    let (stdout, stderr, ok) = run_verify(&app);

    assert!(ok, "{stderr}");
    assert!(
        stdout.contains("証憑が付いている仕訳: 1 / 1 件"),
        "証憑の数ではなく仕訳の数を数えること: {stdout}"
    );
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
