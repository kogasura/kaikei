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

/// `kaikei attach` を走らせる。
fn run_attach(pool: &PgPool, blob_root: &Path, file: &Path, extra: &[&str]) -> (String, bool) {
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
         VALUES ($1::uuid, 2026, 900, DATE '2026-07-14', 'カ)サンプル', now())",
    )
    .bind(ENTRY_ID)
    .execute(pool)
    .await
    .expect("仕訳");
    sqlx::query(
        "INSERT INTO journal_lines \
         (entry_id, line_no, account_code, side, amount_minor, currency, currency_minor_unit, tags) \
         VALUES ($1::uuid, 1, '609', 1, 9145, 'JPY', 0, \
                 '{\"counterparty\":{\"t\":\"code\",\"v\":\"CP0001\"}}'), \
                ($1::uuid, 2, '110', 2, 9145, 'JPY', 0, '{}')",
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
    let file = temp_file("filled.txt", "サンプルの領収書");

    let (out, ok) = run_attach(&app, &blob, &file, &["--entry", ENTRY_ID]);

    assert!(ok, "--date なしで通ること: {out}");
    let (date, amount, counterparty) = registered(&app).await;
    assert_eq!(date, "2026-07-14", "仕訳の取引日");
    assert_eq!(amount, Some(9_145), "仕訳の借方合計");
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

/// **本命。** 金額で仕訳を引ける。
///
/// 仕訳IDを人が探すのが、証憑を登録するときのいちばんの手間である。
/// 領収書を見れば金額は分かる。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn an_entry_can_be_found_by_its_amount(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_entry(&app).await;
    let blob = std::env::temp_dir().join("kaikei-attach-blob-4");
    let file = temp_file("byamount.txt", "サンプルの領収書");

    let (out, ok) = run_attach(
        &app,
        &blob,
        &file,
        &["--match-amount", "9145", "--match-year", "2026"],
    );

    assert!(ok, "金額だけで紐付けられること: {out}");
    let (date, amount, _) = registered(&app).await;
    assert_eq!(date, "2026-07-14", "見つけた仕訳の取引日");
    assert_eq!(amount, Some(9_145));
}

/// **本命。** 仕訳番号だけで紐付けられる。
///
/// `invoices_to_collect.csv`（適格請求書を揃えるべき取引の一覧）が出すのは
/// **仕訳番号**である。UUID しか受けないと、一覧の行ごとに帳簿を引き直して
/// UUID を調べることになる。検証帳簿でも数十件がその対象だった。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn an_entry_number_is_enough_to_link(pool_opts: PgPoolOptions, conn_opts: PgConnectOptions) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_entry(&app).await;
    let blob = std::env::temp_dir().join("kaikei-attach-blob-no");
    let file = temp_file("byno.txt", "仕訳番号で引く");

    let (out, ok) = run_attach(
        &app,
        &blob,
        &file,
        &["--entry-no", "900", "--match-year", "2026"],
    );

    assert!(ok, "仕訳番号だけで紐付けられること: {out}");
    // 取引年月日と取引金額は仕訳から埋まる。
    let (date, amount, _) = registered(&app).await;
    assert_eq!(date, "2026-07-14");
    assert_eq!(amount, Some(9_145));
}

/// **本命。** 無い仕訳番号は、年と番号を言って止まる。
///
/// 黙って何にも紐付けずに登録すると、付けたつもりで付いていない証憑ができる。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn an_unknown_entry_number_stops(pool_opts: PgPoolOptions, conn_opts: PgConnectOptions) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_entry(&app).await;
    let blob = std::env::temp_dir().join("kaikei-attach-blob-no2");
    let file = temp_file("byno2.txt", "無い番号");

    let (out, ok) = run_attach(
        &app,
        &blob,
        &file,
        &["--entry-no", "99999", "--match-year", "2026"],
    );

    assert!(!ok, "止まること: {out}");
    assert!(out.contains("99999"), "番号を出すこと: {out}");
    assert!(out.contains("2026"), "年を出すこと: {out}");
}

/// 年が決まらなければ止まる（仕訳番号は年度の中でしか一意でない）。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn an_entry_number_without_a_year_stops(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_entry(&app).await;
    let blob = std::env::temp_dir().join("kaikei-attach-blob-no3");
    let file = temp_file("byno3.txt", "年が無い");

    let (out, ok) = run_attach(&app, &blob, &file, &["--entry-no", "900"]);

    assert!(!ok, "止まること: {out}");
    assert!(
        out.contains("--match-year"),
        "何を足せばよいか言うこと: {out}"
    );
}

/// **本命。** 同じ額の仕訳が複数あれば止めて候補を出す。
///
/// 勝手に1つ選ぶと意図しない仕訳に証憑が付く。**紐付けは追記のみなので
/// 消せない。**
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn several_entries_with_the_same_amount_stop_with_candidates(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_entry(&app).await;
    // 同じ額の仕訳をもう1件（毎月同額のサブスクリプションを模す）。
    sqlx::query(
        "INSERT INTO journal_entries          (id, fiscal_year, entry_no, entry_date, description, recorded_at)          VALUES ('66666666-6666-6666-6666-666666666666'::uuid, 2026, 901,                  DATE '2026-08-14', 'カ)サンプル 2回目', now())",
    )
    .execute(&app)
    .await
    .expect("仕訳");
    sqlx::query(
        "INSERT INTO journal_lines          (entry_id, line_no, account_code, side, amount_minor, currency, currency_minor_unit)          VALUES ('66666666-6666-6666-6666-666666666666'::uuid, 1, '609', 1, 9145, 'JPY', 0),                 ('66666666-6666-6666-6666-666666666666'::uuid, 2, '110', 2, 9145, 'JPY', 0)",
    )
    .execute(&app)
    .await
    .expect("明細");
    let blob = std::env::temp_dir().join("kaikei-attach-blob-5");
    let file = temp_file("ambiguous.txt", "どちらの領収書か分からない");

    let (out, ok) = run_attach(
        &app,
        &blob,
        &file,
        &["--match-amount", "9145", "--match-year", "2026"],
    );

    assert!(!ok, "止まること: {out}");
    assert!(out.contains("2 件あります"), "件数を言うこと: {out}");
    // どれを選べばよいか分からないまま止めない。
    assert!(out.contains(ENTRY_ID), "候補を並べること: {out}");
    assert!(out.contains("--entry"), "次の手を示すこと: {out}");
}

/// **本命。** 赤伝は候補にしない。
///
/// 訂正で起こした赤伝は、原仕訳と同じ額で立つ。候補に入れると**必ず2件に
/// なって止まる**——訂正した取引には証憑を付けられなくなる。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_reversal_is_not_a_candidate(pool_opts: PgPoolOptions, conn_opts: PgConnectOptions) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_entry(&app).await;
    // 原仕訳を取り消す赤伝（同じ額・借方貸方が逆）。
    sqlx::query(
        "INSERT INTO journal_entries          (id, fiscal_year, entry_no, entry_date, description, recorded_at,           reverses, reverse_reason)          VALUES ('77777777-7777-7777-7777-777777777777'::uuid, 2026, 902,                  DATE '2026-07-20', '【訂正】カ)サンプル', now(), $1::uuid, '誤記帳のため')",
    )
    .bind(ENTRY_ID)
    .execute(&app)
    .await
    .expect("赤伝");
    sqlx::query(
        "INSERT INTO journal_lines          (entry_id, line_no, account_code, side, amount_minor, currency, currency_minor_unit)          VALUES ('77777777-7777-7777-7777-777777777777'::uuid, 1, '110', 1, 9145, 'JPY', 0),                 ('77777777-7777-7777-7777-777777777777'::uuid, 2, '609', 2, 9145, 'JPY', 0)",
    )
    .execute(&app)
    .await
    .expect("明細");
    let blob = std::env::temp_dir().join("kaikei-attach-blob-7");
    let file = temp_file("withreversal.txt", "原仕訳の領収書");

    let (out, ok) = run_attach(
        &app,
        &blob,
        &file,
        &["--match-amount", "9145", "--match-year", "2026"],
    );

    assert!(ok, "赤伝を数えず1件に絞れること: {out}");
    let (date, _, _) = registered(&app).await;
    assert_eq!(date, "2026-07-14", "原仕訳の取引日であること");
}

/// 取引先タグの**無い**仕訳を仕込む。
///
/// 実帳簿はこちらが普通である（1,395明細中、取引先タグは0件）。
async fn seed_entry_without_counterparty(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO accounts (code, name, account_type, postable)          VALUES ('609','消耗品費',5,true), ('110','普通預金',1,true)          ON CONFLICT (code) DO NOTHING",
    )
    .execute(pool)
    .await
    .expect("科目");
    sqlx::query(
        "INSERT INTO journal_entries          (id, fiscal_year, entry_no, entry_date, description, recorded_at)          VALUES ($1::uuid, 2026, 900, DATE '2026-07-14', 'カ)サンプル', now())",
    )
    .bind(ENTRY_ID)
    .execute(pool)
    .await
    .expect("仕訳");
    sqlx::query(
        "INSERT INTO journal_lines          (entry_id, line_no, account_code, side, amount_minor, currency, currency_minor_unit)          VALUES ($1::uuid, 1, '609', 1, 9145, 'JPY', 0),                 ($1::uuid, 2, '110', 2, 9145, 'JPY', 0)",
    )
    .bind(ENTRY_ID)
    .execute(pool)
    .await
    .expect("明細");
}

/// **本命。** 取引先が空のまま登録するときは知らせる。
///
/// 電子取引データは取引年月日・取引金額・取引先で検索できる必要がある。
/// 実帳簿には取引先タグが1件も無い（1,395明細中0件）ので、仕訳から埋め
/// られない。**証憑は後から書き換えられない**ので、登録時に言う。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn an_empty_counterparty_is_reported(pool_opts: PgPoolOptions, conn_opts: PgConnectOptions) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    // 取引先タグの無い仕訳（実帳簿はこちらが普通）。
    seed_entry_without_counterparty(&app).await;
    let blob = std::env::temp_dir().join("kaikei-attach-blob-8");
    let file = temp_file("nocp.txt", "取引先の分からない領収書");

    let (out, ok) = run_attach(&app, &blob, &file, &["--entry", ENTRY_ID]);

    // 止めない——ファイルを保存しないより、取引先が空でも保存した方がよい。
    assert!(ok, "登録は通ること: {out}");
    assert!(out.contains("取引先が空"), "{out}");
    assert!(out.contains("--counterparty"), "次の手を示すこと: {out}");
}

/// 取引先を指定していれば知らせない。
///
/// 正しい使い方で毎回出る指摘は、当たり前になって本当の抜けを覆い隠す。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_given_counterparty_is_not_reported(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_entry(&app).await;
    let blob = std::env::temp_dir().join("kaikei-attach-blob-9");
    let file = temp_file("withcp.txt", "取引先の分かる領収書");

    let (out, ok) = run_attach(
        &app,
        &blob,
        &file,
        &["--entry", ENTRY_ID, "--counterparty", "CP0001"],
    );

    assert!(ok, "{out}");
    assert!(!out.contains("取引先が空"), "{out}");
}

/// `--entry-no` と `--entry` も同時には指定できない。
///
/// **どちらを使ったのか分からないまま登録されると、意図しない仕訳に紐付いても
/// 気づけない。** 紐付けは追記のみで消せない。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn entry_and_entry_no_cannot_be_combined(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    seed_entry(&app).await;
    let blob = std::env::temp_dir().join("kaikei-attach-blob-no4");
    let file = temp_file("byno4.txt", "両方");

    let (out, ok) = run_attach(
        &app,
        &blob,
        &file,
        &[
            "--entry",
            ENTRY_ID,
            "--entry-no",
            "900",
            "--match-year",
            "2026",
        ],
    );

    assert!(!ok, "止まること: {out}");
    assert!(out.contains("同時に指定できません"), "{out}");
}

/// `--entry` と `--match-amount` は同時に指定できない。
///
/// どちらを使ったのかが分からないまま登録されると、意図しない仕訳に
/// 紐付いても気づけない。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn entry_and_match_amount_cannot_be_combined(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    let blob = std::env::temp_dir().join("kaikei-attach-blob-6");
    let file = temp_file("both.txt", "両方指定");

    let (out, ok) = run_attach(
        &app,
        &blob,
        &file,
        &["--entry", ENTRY_ID, "--match-amount", "9145"],
    );

    assert!(!ok, "拒否されること: {out}");
    assert!(out.contains("同時に指定できません"), "{out}");
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
