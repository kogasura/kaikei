//! `list_pending_transactions` を**実 PostgreSQL に対して**通す
//! （`docs/05-csv-import.md` §3・§6）。
//!
//! # なぜ `kaikei-mcp` 側ではなくここに置くのか
//!
//! `kaikei-mcp` は `sqlx` に依存しない（`docs/07-mcp-server.md` §10 MC-30 の
//! 許可リスト）ため `#[sqlx::test]` を使えず、使い捨てDBを持てない
//! （`tests/mcp_search_ledger.rs` と同じ理由）。
//!
//! # ここで見るもの
//!
//! **「未処理が0件」の2つの意味を、AI が区別できること。** 全部片付いたのか、
//! そもそも1件も取り込んでいないのか——前者は喜ぶところだが、後者は CSV を
//! 流し忘れているということで、確定申告の直前にこれを取り違えると帳簿に
//! 丸ごと抜けができる。
//!
//! 明細の投入だけは生 SQL で行う。取込の経路（`kaikei import`）は CLI に
//! あり、ここから呼べないためである。**読む側は本番と同じ経路**（MCP の
//! ツール）を通す。

#![cfg(feature = "pg-tests")]

mod common;

use kaikei_app::clock::SystemClock;
use kaikei_app::context::{BookSettings, FiscalYearRule};
use kaikei_app::id::UuidV7IdGenerator;
use kaikei_app::tx::with_tx;
use kaikei_app::usecase::import_chart;
use kaikei_core::{AccountCode, AccountingDate, ChartOfAccounts, Currency};
use kaikei_e2e::{compose, ComposeOptions};
use kaikei_jp::closing::ClosingAccounts;
use kaikei_jp::tax::{JpSettingsOverrides, TaxRuleSets};
use kaikei_mcp::dispatch::{self, McpTool};
use kaikei_mcp::startup::Runtime;
use kaikei_mcp::tools::list_pending_transactions::ListPendingTransactions;
use kaikei_store::audit::PgAuditSink;
use kaikei_store::pool::PgStore;
use kaikei_store::query::{PgLedgerQuery, PgSearchEntriesQuery, PgTrialBalanceQuery};
use serde_json::{json, Value};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::PgPool;
use std::sync::Arc;

fn book_settings() -> BookSettings {
    BookSettings {
        fiscal_year_rule: FiscalYearRule::CalendarYear,
        book_currency: Currency::JPY,
    }
}

fn compose_options() -> ComposeOptions {
    ComposeOptions {
        rule_sets: TaxRuleSets::from_embedded().unwrap(),
        settings_overrides: JpSettingsOverrides {
            tax_mode: None,
            rounding: None,
            rounding_unit: None,
            is_taxable_business: true,
            simplified_taxation: false,
        },
        defaults_as_of: AccountingDate::new(2026, 4, 1).unwrap(),
        closing_accounts: ClosingAccounts {
            capital: AccountCode::parse("400").unwrap(),
            owner_drawings: AccountCode::parse("410").unwrap(),
            owner_contributions: AccountCode::parse("420").unwrap(),
        },
        closing_tax_category: Some("NOT_APPLICABLE".to_string()),
    }
}

async fn runtime(app: &PgPool) -> Runtime {
    let composition = compose(compose_options()).expect("合成に失敗しました");
    seed_chart(app, &composition.chart).await;
    Runtime {
        store: Arc::new(PgStore::new(app.clone())),
        audit_sink: Arc::new(PgAuditSink::new(app.clone())),
        composition: Arc::new(composition),
        book_settings: book_settings(),
        id_gen: UuidV7IdGenerator,
        clock: SystemClock,
        trial_balance: Arc::new(PgTrialBalanceQuery::new(app.clone())),
        documents: Arc::new(kaikei_store::documents::PgDocumentQuery::new(app.clone())),
        imported_tx: Arc::new(kaikei_store::imported::PgImportedTxQuery::new(app.clone())),
        search_query: Arc::new(PgSearchEntriesQuery::new(app.clone())),
        ledger_query: Arc::new(PgLedgerQuery::new(app.clone())),
        chart_differences: Vec::new(),
    }
}

async fn seed_chart(pool: &PgPool, chart: &ChartOfAccounts) {
    let store = PgStore::new(pool.clone());
    with_tx(&store, |tx| {
        let chart = chart.clone();
        Box::pin(async move { import_chart::execute(tx, &chart).await })
    })
    .await
    .unwrap_or_else(|e| panic!("勘定科目マスタの投入に失敗しました: {e}"));
}

async fn call<T: McpTool>(runtime: &Runtime, arguments: Value) -> Value {
    let arguments = arguments
        .as_object()
        .cloned()
        .expect("ツールの引数はオブジェクト");
    let result = dispatch::call::<T>(runtime, Some(arguments)).await;
    serde_json::to_value(result).expect("CallToolResult は JSON にできる")
}

fn is_error(response: &Value) -> bool {
    response["isError"] == json!(true)
}

fn body(response: &Value) -> &Value {
    response
        .get("structuredContent")
        .expect("structuredContent が無い（構造化コンテンツで返すこと）")
}

/// 仕訳を1件作り、そのIDを返す。
///
/// 「仕訳済み」の明細には仕訳IDが要る（0011 の `imported_journalized_has_entry`）。
/// 帳簿へ辿れないまま「処理済み」として一覧から消える行を作らせないための
/// 制約であり、テストもそれに従う。
async fn an_entry(pool: &PgPool, entry_no: i32) -> String {
    // ID の採番は DB に任せる（テストのためだけに uuid への依存を増やさない）。
    let id: String = sqlx::query_scalar("SELECT gen_random_uuid()::text")
        .fetch_one(pool)
        .await
        .expect("IDを採番できること");
    let mut tx = pool
        .begin()
        .await
        .expect("トランザクションを開始できること");
    sqlx::query(
        "INSERT INTO journal_entries \
         (id, fiscal_year, entry_no, entry_date, description, recorded_at) \
         VALUES ($1::uuid, 2026, $2, DATE '2026-06-15', 'テスト仕訳', now())",
    )
    .bind(&id)
    .bind(entry_no)
    .execute(&mut *tx)
    .await
    .expect("仕訳を入れられること");
    sqlx::query(
        "INSERT INTO journal_lines \
         (entry_id, line_no, account_code, side, amount_minor, currency, currency_minor_unit) \
         VALUES ($1::uuid, 1, '110', 1, 1000, 'JPY', 0), ($1::uuid, 2, '500', 2, 1000, 'JPY', 0)",
    )
    .bind(&id)
    .execute(&mut *tx)
    .await
    .expect("仕訳明細を入れられること");
    tx.commit().await.expect("コミットできること");
    id
}

/// 取込明細を1件入れる。取込の経路は CLI にあるので、ここは生 SQL。
async fn insert_imported(
    pool: &PgPool,
    key: &str,
    day: u32,
    amount: i64,
    is_money_in: bool,
    status: &str,
) {
    // 仕訳済みには仕訳IDが要り、未処理には仕訳IDも理由も無い（0011 の制約）。
    let entry_id = match status {
        "journalized" => Some(an_entry(pool, amount as i32).await),
        _ => None,
    };
    let reason = if status == "ignored" {
        Some("個人の買い物")
    } else {
        None
    };
    sqlx::query(
        "INSERT INTO imported_transactions \
         (id, source, external_key, occurred_on, amount_minor, direction, \
          raw_description, balance_after, raw_row, status, entry_id, ignore_reason, imported_at) \
         VALUES (gen_random_uuid(), 'mizuho', $1, make_date(2026, 6, $2), $3, $4, \
                 'ｶ)ｱﾏｿﾞﾝ', 500000, '[]', $5, $6::uuid, $7, now())",
    )
    .bind(key)
    .bind(day as i32)
    .bind(amount)
    .bind(if is_money_in { 1i16 } else { 2i16 })
    .bind(status)
    .bind(entry_id)
    .bind(reason)
    .execute(pool)
    .await
    .expect("取込明細を入れられること");
}

/// **本命。** 一覧が空でも、取り込み済みかどうかが分かる。
///
/// 「未処理が0件」には2つの意味がある。counts の合計だけがそれを分ける。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn an_empty_list_tells_all_done_from_never_imported(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    let runtime = runtime(&app).await;

    // まだ1件も取り込んでいない。
    let before = call::<ListPendingTransactions>(&runtime, json!({})).await;
    let before = body(&before);
    assert_eq!(before["count"], json!(0));
    assert_eq!(
        before["counts"]["total"],
        json!(0),
        "取り込んでいないことが読み取れること: {before}"
    );

    // 取り込んで、全部片付ける。
    insert_imported(&app, "k1", 15, 1_980, false, "journalized").await;
    insert_imported(&app, "k2", 16, 500, false, "ignored").await;

    let after = call::<ListPendingTransactions>(&runtime, json!({})).await;
    let after = body(&after);

    // 一覧はどちらの場合も空。合計だけが両者を分ける。
    assert_eq!(after["count"], json!(0), "未処理は無い: {after}");
    assert_eq!(after["counts"]["total"], json!(2), "{after}");
    assert_eq!(after["counts"]["pending"], json!(0), "{after}");
    assert_eq!(after["counts"]["journalized"], json!(1), "{after}");
    assert_eq!(after["counts"]["ignored"], json!(1), "{after}");
}

/// 未処理の明細が、仕訳ではない形で返る。
///
/// 借方・貸方・勘定科目は無い。あるのは入金か出金かだけである。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_pending_line_comes_back_as_a_statement_not_an_entry(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    let runtime = runtime(&app).await;
    insert_imported(&app, "k1", 15, 1_980, false, "pending").await;

    let response = call::<ListPendingTransactions>(&runtime, json!({})).await;
    let found = body(&response);
    let tx = &found["transactions"][0];

    assert_eq!(found["count"], json!(1), "{found}");
    assert_eq!(tx["occurred_on"], json!("2026-06-15"), "{tx}");
    // **金額は文字列で、常に正。** 向きは is_money_in が表す。
    assert_eq!(tx["amount_minor"], json!("1980"), "{tx}");
    assert_eq!(tx["is_money_in"], json!(false), "出金: {tx}");
    assert_eq!(tx["status"], json!("pending"), "{tx}");
    assert_eq!(tx["entry_id"], json!(null), "未処理は仕訳を指さない: {tx}");
    // 仕訳の語彙は現れない。
    assert!(tx.get("side").is_none(), "{tx}");
    assert!(tx.get("account").is_none(), "{tx}");
}

/// 状態と期間で絞れる。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn the_list_can_be_narrowed(pool_opts: PgPoolOptions, conn_opts: PgConnectOptions) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    let runtime = runtime(&app).await;
    insert_imported(&app, "k1", 10, 100, false, "pending").await;
    insert_imported(&app, "k2", 20, 200, true, "pending").await;
    insert_imported(&app, "k3", 25, 300, false, "ignored").await;

    let ignored = call::<ListPendingTransactions>(&runtime, json!({ "status": "ignored" })).await;
    assert_eq!(body(&ignored)["count"], json!(1));
    assert_eq!(
        body(&ignored)["transactions"][0]["ignore_reason"],
        json!("個人の買い物"),
        "無視の理由が残ること"
    );

    let first_half = call::<ListPendingTransactions>(
        &runtime,
        json!({ "date_from": "2026-06-01", "date_to": "2026-06-15" }),
    )
    .await;
    assert_eq!(body(&first_half)["count"], json!(1), "期間の端を含むこと");

    let other_bank =
        call::<ListPendingTransactions>(&runtime, json!({ "source": "rakuten" })).await;
    assert_eq!(body(&other_bank)["count"], json!(0));
    assert_eq!(
        body(&other_bank)["counts"]["total"],
        json!(0),
        "件数も取り込み元で絞る"
    );
}

/// **本命。** 惜しい状態指定を黙って0件にしない。
///
/// `Pending` が通ると、条件に合わないだけなのに「未処理は無い」と読み違える。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_near_miss_status_is_an_error_not_an_empty_list(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    let runtime = runtime(&app).await;
    insert_imported(&app, "k1", 15, 100, false, "pending").await;

    let response = call::<ListPendingTransactions>(&runtime, json!({ "status": "Pending" })).await;

    assert!(is_error(&response), "拒否されること: {response}");
}

/// 期間の逆指定を黙って0件にしない。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_reversed_date_range_is_an_error(pool_opts: PgPoolOptions, conn_opts: PgConnectOptions) {
    let app = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    let runtime = runtime(&app).await;

    let response = call::<ListPendingTransactions>(
        &runtime,
        json!({ "date_from": "2026-12-31", "date_to": "2026-01-01" }),
    )
    .await;

    assert!(is_error(&response), "拒否されること: {response}");
}
