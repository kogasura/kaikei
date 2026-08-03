//! Phase 3 PR-H: MCP の読み取り系ツール `search_entries` / `get_ledger` を
//! **実 PostgreSQL に対して**通す。
//!
//! # なぜ `kaikei-mcp` 側ではなくここに置くのか
//!
//! `kaikei-mcp` は `sqlx` に依存しない（`docs/07-mcp-server.md` §10 MC-30 の
//! 許可リスト）ため `#[sqlx::test]` を使えず、**使い捨てDBも `audit_log` の
//! SELECT も持てない**。仕訳を書いてから読む検査には、仕訳を書ける使い捨てDB
//! と SQL の両方が要る（`tests/mcp_write_tools.rs` と同じ理由）。
//!
//! # ここで見るもの
//!
//! | # | 見るもの |
//! |---|---|
//! | MC-16 | `search_entries` が日付・金額・科目・摘要・タグで絞り込める。**0件でも成功として空配列**を返す |
//! | MC-17 | `get_ledger` が科目別に借方・貸方・残高を返し、期間指定が効く |
//! | MC-11 | 読み取り系も1回の呼び出しにつき同一 `request_id` で2行残る（`tool` 列が一致） |
//! | MC-27 | 出力の金額が全て JSON 文字列 |
//! | D-088 | 取り消された仕訳が「取り消された」と分かる形で返る |
//! | D-089 | 上限で切ったことが応答から分かり、`next_cursor` で続きが取れる |
//! | — | 空の結果（0件・成功）と「見つからない」（`not_found`・エラー）の区別 |
//!
//! **記帳は MCP の `post_journal_entry` / `reverse_journal_entry` で行う。**
//! 生 SQL で仕訳を作ると「書いた形」と「読める形」が独立してしまい、
//! 本番の経路で往復できることを示せない。

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
use kaikei_mcp::tools::get_ledger::GetLedger;
use kaikei_mcp::tools::post_journal_entry::PostJournalEntry;
use kaikei_mcp::tools::reverse_journal_entry::ReverseJournalEntry;
use kaikei_mcp::tools::search_entries::SearchEntries;
use kaikei_store::audit::PgAuditSink;
use kaikei_store::pool::PgStore;
use kaikei_store::query::{PgLedgerQuery, PgSearchEntriesQuery};
use serde_json::{json, Value};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::PgPool;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// セットアップ（`tests/mcp_write_tools.rs` と同じ形）
// ---------------------------------------------------------------------------

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

/// 本番の合成ルート（`kaikei_mcp::startup::assemble`）と**同じ部品**で
/// [`Runtime`] を組み立てる（`assemble` は `APP_DATABASE_URL` を読むので
/// 使い捨てDBを指せない）。
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
        // read model は書き込み側を経由しない（`CLAUDE.md` §6）。
        search_query: Arc::new(PgSearchEntriesQuery::new(app.clone())),
        ledger_query: Arc::new(PgLedgerQuery::new(app.clone())),
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

/// ツールを1回呼ぶ（ルータのハンドラが呼ぶのと同じ `dispatch::call`）。
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

#[derive(Debug, sqlx::FromRow)]
struct AuditRow {
    request_id: sqlx::types::Uuid,
    actor: String,
    tool: String,
    status: String,
    input: Option<Value>,
    output: Option<Value>,
}

async fn audit_rows(pool: &PgPool) -> Vec<AuditRow> {
    sqlx::query_as::<_, AuditRow>(
        "SELECT request_id, actor, tool, status, input, output FROM audit_log ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .expect("audit_log を読めること")
}

/// 読み取り系も「1回の呼び出し＝同一 `request_id` の2行」である（MC-11）。
fn assert_two_rows(rows: &[AuditRow], tool: &str, expected_status: &str) {
    assert_eq!(rows.len(), 2, "開始・結果の2行が残るはず: {rows:?}");
    assert_eq!(rows[0].request_id, rows[1].request_id);
    assert_eq!(rows[0].status, "started");
    assert_eq!(rows[1].status, expected_status);
    for row in rows {
        assert_eq!(row.tool, tool);
        assert_eq!(row.actor, "mcp");
    }
    assert!(rows[0].input.is_some());
    assert!(rows[1].output.is_some());
}

// ---------------------------------------------------------------------------
// 素材（本番の経路で記帳する）
// ---------------------------------------------------------------------------

/// 7件の仕訳を記帳し、うち1件を赤伝で1回、もう1件を**2回**取り消す。
///
/// 戻り値は記帳順の仕訳ID:
/// `[売上A, 消耗品, 売上B, 取消対象, 赤伝, 二重訂正の対象, 赤伝1, 赤伝2,
///   通信費4行, 郵便料金]`。
///
/// # 実帳簿の形（末尾2件）を土台に入れる理由（PR-H レビュー2巡目）
///
/// 6件目までは**すべて2行の仕訳**で、
///
/// - 同じ科目が同じ仕訳に2度現れない
/// - 1つの仕訳の明細金額が全て同額
/// - 1つの仕訳に載るタグのキーが実質1種類
///
/// だった。そのため read model の SQL を次のように壊しても
/// **両スイート緑のまま**通っていた:
/// 相手科目の `DISTINCT`/`ORDER BY` を落とす、残高の累計のウィンドウから
/// `l.line_no` を落とす、明細の並びを `line_no DESC` にする、
/// 金額範囲の `EXISTS` を min 用・max 用に割る、タグの AND を OR にする。
///
/// そこで **仮払消費税等（180）を含む4行仕訳**を足す。
/// 通信費（604）が**同じ仕訳に2行**あり、未払金（325）から見た相手科目が
/// **重複**し、明細の金額が 3,000 / 10,000 / 20,000 / 33,000 と**ばらける**。
/// タグも `tax_category` と `counterparty` の2種類が別々の行に載る。
///
/// 既存の期待値を動かさないよう、**この2件だけが使う科目（604 / 180 / 325）**
/// を選んである（609 / 500 / 620 の元帳は1行も変わらない）。
///
/// # 二重訂正を土台に入れる理由（PR-H レビュー C-2）
///
/// `search.rs` / `ledger.rs` が `LEFT JOIN LATERAL ... LIMIT 1` を採った
/// 理由（`DECISIONS.md` D-088）が、**同じ仕訳に赤伝が2件以上ありうる**こと
/// である。素朴な `LEFT JOIN` に戻すと検索は `{"error":"corrupt"}` になり、
/// 元帳は同じ明細を2行返す。土台に1件も無ければその退行が緑のまま通る。
///
/// 二重訂正の対象には**専用の科目（620 支払手数料）**を使う。
/// 609 / 500 を動かすと他の検査の期待値がまとめて変わるためである。
async fn post_sample_entries(runtime: &Runtime) -> Vec<String> {
    let mut ids = Vec::new();

    for (date, description, debit, credit, amount, tax_category) in [
        ("2026-01-10", "A社への請求", "135", "500", "10000", true),
        ("2026-02-01", "文具の購入", "609", "100", "1500", false),
        ("2026-02-01", "B社への請求", "135", "500", "3000", true),
        ("2026-03-20", "消耗品の購入", "609", "100", "800", false),
        ("2026-05-10", "振込手数料", "620", "100", "500", false),
    ] {
        let credit_tags = if tax_category {
            json!({ "tax_category": "SALES_10" })
        } else {
            json!({})
        };
        let debit_tags = if tax_category {
            json!({})
        } else {
            json!({ "tax_category": "PURCHASE_10_QUALIFIED" })
        };
        let response = call::<PostJournalEntry>(
            runtime,
            json!({
                "entry_date": date,
                "description": description,
                "lines": [
                    { "account": debit, "side": "debit", "amount": amount, "tags": debit_tags },
                    { "account": credit, "side": "credit", "amount": amount, "tags": credit_tags }
                ]
            }),
        )
        .await;
        assert!(!is_error(&response), "記帳に失敗しました: {response}");
        ids.push(body(&response)["entry_id"].as_str().unwrap().to_string());
    }

    // 4件目を赤伝で取り消す（帳簿は追記のみ。元仕訳も残る）。
    let response = call::<ReverseJournalEntry>(
        runtime,
        json!({
            "original_id": ids[3],
            "reverse_date": "2026-03-31",
            "reason": "数量の誤り"
        }),
    )
    .await;
    assert!(!is_error(&response), "逆仕訳に失敗しました: {response}");
    let single_reversal = body(&response)["entry_id"].as_str().unwrap().to_string();

    // ★二重訂正★ 5件目（620）を2回取り消す。2回目は
    // `allow_double_reversal: true` を明示しないと拒否される。
    let double_reversed = ids[4].clone();
    let mut double_reversals = Vec::new();
    for (reverse_date, reason, allow) in [
        ("2026-05-20", "金額の誤り", false),
        ("2026-05-30", "科目の誤り（二重に訂正した）", true),
    ] {
        let response = call::<ReverseJournalEntry>(
            runtime,
            json!({
                "original_id": double_reversed,
                "reverse_date": reverse_date,
                "reason": reason,
                "allow_double_reversal": allow
            }),
        )
        .await;
        assert!(!is_error(&response), "二重訂正に失敗しました: {response}");
        double_reversals.push(body(&response)["entry_id"].as_str().unwrap().to_string());
    }

    // 記帳順（4の赤伝 → 620の赤伝2件）に並べ直す。
    ids.insert(4, single_reversal);
    ids.extend(double_reversals);

    // ★実帳簿の形★ 仮払消費税等（180）を含む4行仕訳と、その対照になる
    // 2行仕訳（上の doc）。既存の添字を動かさないよう**末尾に足す**。
    for (entry_date, description, lines) in [
        (
            "2026-06-10",
            "6月の通信費（2回線分と仮払消費税）",
            json!([
                // 記帳順が科目コード順と逆になるよう仮払消費税を先頭に置く
                // （相手科目の整列が効いていることを見るため）。
                { "account": "180", "side": "debit", "amount": "3000" },
                { "account": "604", "side": "debit", "amount": "10000",
                  "tags": { "tax_category": "PURCHASE_10_QUALIFIED" } },
                { "account": "604", "side": "debit", "amount": "20000",
                  "tags": { "tax_category": "PURCHASE_10_QUALIFIED" } },
                { "account": "325", "side": "credit", "amount": "33000",
                  "tags": { "counterparty": "CP0009" } }
            ]),
        ),
        (
            "2026-06-20",
            "郵便料金",
            // `tax_category` はあるが `counterparty` は無い（タグの AND 用）。
            json!([
                { "account": "604", "side": "debit", "amount": "1000",
                  "tags": { "tax_category": "PURCHASE_10_QUALIFIED" } },
                { "account": "325", "side": "credit", "amount": "1000" }
            ]),
        ),
    ] {
        let response = call::<PostJournalEntry>(
            runtime,
            json!({
                "entry_date": entry_date,
                "description": description,
                "lines": lines
            }),
        )
        .await;
        assert!(!is_error(&response), "記帳に失敗しました: {response}");
        ids.push(body(&response)["entry_id"].as_str().unwrap().to_string());
    }

    ids
}

// ---------------------------------------------------------------------------
// search_entries（MC-16）
// ---------------------------------------------------------------------------

/// MC-16 / MC-11 / MC-27: 期間・科目・金額・摘要・タグで絞り込めて、
/// 監査ログに2行残り、金額は全て文字列である。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn search_entries_filters_by_period_account_amount_description_and_tags(
    _pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let runtime = runtime(&app).await;
    post_sample_entries(&runtime).await;

    // 期間（取引日）。1/10 の1件だけ。
    let response = call::<SearchEntries>(
        &runtime,
        json!({ "from": "2026-01-01", "to": "2026-01-31" }),
    )
    .await;
    assert!(!is_error(&response), "{response}");
    let page = body(&response);
    assert_eq!(page["total_matches"], json!(1), "{page}");
    assert_eq!(page["returned"], json!(1));
    assert_eq!(page["has_more"], json!(false));
    assert_eq!(page["entries"][0]["description"], json!("A社への請求"));
    assert_eq!(page["entries"][0]["entry_date"], json!("2026-01-10"));

    // 出力の金額は全て文字列（MC-27）。
    let amount = &page["entries"][0]["lines"][0]["amount"];
    assert!(amount.is_string(), "{amount}");

    // 科目。消耗品費（609）の明細を含む仕訳は3件（購入2件 + 赤伝1件）。
    let response = call::<SearchEntries>(&runtime, json!({ "account": "609" })).await;
    assert_eq!(body(&response)["total_matches"], json!(3));

    // 金額（明細1行の金額と比較する）。
    let response = call::<SearchEntries>(
        &runtime,
        json!({ "min_amount": "3000", "max_amount": "10000" }),
    )
    .await;
    assert!(!is_error(&response), "{response}");
    assert!(
        body(&response)["total_matches"].as_u64().unwrap() >= 2,
        "{response}"
    );

    // 摘要の部分一致。
    let response = call::<SearchEntries>(&runtime, json!({ "description": "請求" })).await;
    assert_eq!(body(&response)["total_matches"], json!(2));

    // タグ（税区分は集計軸として登録されている）。
    let response =
        call::<SearchEntries>(&runtime, json!({ "tags": { "tax_category": "SALES_10" } })).await;
    assert_eq!(body(&response)["total_matches"], json!(2));

    // MC-11: 直近の呼び出しの2行を見る（tool 列が一致する）。
    let rows = audit_rows(&app).await;
    let last_two = &rows[rows.len() - 2..];
    assert_two_rows(last_two, "search_entries", "ok");
}

/// MC-16: **0件でも成功**として空配列を返す（エラーにしない）。
/// 「見つからない」と混同させない。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_search_with_no_hits_is_a_success_with_an_empty_list(
    _pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let runtime = runtime(&app).await;
    post_sample_entries(&runtime).await;

    let response =
        call::<SearchEntries>(&runtime, json!({ "description": "存在しない摘要" })).await;

    assert!(!is_error(&response), "0件はエラーではない: {response}");
    let page = body(&response);
    assert_eq!(page["entries"], json!([]));
    assert_eq!(page["total_matches"], json!(0));
    assert_eq!(page["has_more"], json!(false));
    assert!(page.get("next_cursor").is_none());
    assert!(page.get("truncation_note").is_none());

    // 0件の呼び出しも監査ログには残る（status は ok）。
    let rows = audit_rows(&app).await;
    assert_two_rows(&rows[rows.len() - 2..], "search_entries", "ok");
}

/// ★上限で切ったことが応答から分かる★（D-089）
///
/// `limit: 2` で辿り、`total_matches` / `has_more` / `next_cursor` /
/// `truncation_note` が揃うこと、続きを最後まで取ると重複も取りこぼしも
/// 無いことを見る。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_truncated_search_says_so_and_the_cursor_walks_the_rest(
    _pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let runtime = runtime(&app).await;
    let posted = post_sample_entries(&runtime).await;
    let total = posted.len() as u64;

    let first = call::<SearchEntries>(&runtime, json!({ "limit": 2 })).await;
    let page = body(&first);
    assert_eq!(page["total_matches"], json!(total), "{page}");
    assert_eq!(page["returned"], json!(2));
    assert_eq!(page["has_more"], json!(true));
    let note = page["truncation_note"]
        .as_str()
        .expect("切ったことが応答から分かること");
    assert!(note.contains(&total.to_string()), "{note}");
    assert!(note.contains("next_cursor"), "{note}");
    // ★切れた理由が「自分の limit」だと分かる★（PR-H レビュー D-4）
    assert!(note.contains("limit=2"), "{note}");
    assert!(note.contains("100"), "上限も併記する: {note}");

    // 続きを最後まで辿る。
    let mut seen: Vec<String> = page["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["entry_id"].as_str().unwrap().to_string())
        .collect();
    let mut cursor = page["next_cursor"].as_str().unwrap().to_string();
    loop {
        let response =
            call::<SearchEntries>(&runtime, json!({ "limit": 2, "cursor": cursor })).await;
        assert!(!is_error(&response), "{response}");
        let page = body(&response);
        assert_eq!(
            page["total_matches"],
            json!(total),
            "総件数はページによらない"
        );
        seen.extend(
            page["entries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["entry_id"].as_str().unwrap().to_string()),
        );
        match page.get("next_cursor").and_then(Value::as_str) {
            Some(next) => cursor = next.to_string(),
            None => break,
        }
        assert!(seen.len() as u64 <= total, "ページングが終わらない");
    }

    assert_eq!(seen.len() as u64, total, "取りこぼしがある: {seen:?}");
    let mut sorted = seen.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted.len() as u64,
        total,
        "同じ仕訳が2回返っている: {seen:?}"
    );
    for id in &posted {
        assert!(seen.contains(id), "記帳した仕訳が返っていない: {id}");
    }
}

/// ★残りがちょうど `limit` 件のページは「切った」と言わない★
/// （D-089。PR-H レビュー3巡目 C-1）
///
/// 続きの有無は「`limit + 1` 件取って `limit` 件より多く返ってきたか」で
/// 決まる。これを `>=` にすると**残りがちょうど `limit` 件のページ**に
/// `has_more: true` / `next_cursor` / `truncation_note` が付き、
/// **上限で切ったことが応答から必ず読み取れる**という D-089 の中心的な
/// 約束が偽陽性になる。AI は続きが無いのにもう一度呼び、空のページを受け取る。
///
/// 上の [`a_truncated_search_says_so_and_the_cursor_walks_the_rest`] は
/// 10件を `limit=2`（ちょうど割り切れる）で辿っており**境界を通過している
/// のに**捕まえていない。`next_cursor` が無くなるまで辿るだけなので、
/// 余分な0件ページを1回踏んでも `seen` が変わらないためである。
/// **そこで「最後のページは切られていない」を直接主張する。**
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_page_that_exactly_fills_the_limit_is_not_reported_as_truncated(
    _pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let runtime = runtime(&app).await;
    let posted = post_sample_entries(&runtime).await;
    let total = posted.len() as u64;
    assert!(total >= 4 && total % 2 == 0, "土台が変わっている: {total}");

    // --- search_entries: 総件数と同じ `limit` ---
    let response = call::<SearchEntries>(&runtime, json!({ "limit": total })).await;
    assert!(!is_error(&response), "{response}");
    let page = body(&response);
    assert_eq!(page["total_matches"], json!(total), "{page}");
    assert_eq!(page["returned"], json!(total), "{page}");
    assert_eq!(
        page["has_more"],
        json!(false),
        "ちょうど収まったのに「続きがある」と言っています: {page}"
    );
    assert!(page.get("next_cursor").is_none(), "{page}");
    assert!(
        page.get("truncation_note").is_none(),
        "切っていないのに切ったと言っています: {page}"
    );

    // --- search_entries: 割り切れる `limit` で辿る ---
    // 最後のページもちょうど `limit` 件で、そこで終わる（空ページを踏まない）。
    let half = total / 2;
    let mut cursor: Option<String> = None;
    let mut pages = 0_u64;
    let mut seen = 0_u64;
    loop {
        let mut arguments = json!({ "limit": half });
        if let Some(cursor) = &cursor {
            arguments["cursor"] = json!(cursor);
        }
        let response = call::<SearchEntries>(&runtime, arguments).await;
        assert!(!is_error(&response), "{response}");
        let page = body(&response);
        pages += 1;
        let returned = page["returned"].as_u64().unwrap();
        assert!(
            returned > 0,
            "0件のページを返しています（{pages} ページ目）: {page}"
        );
        seen += returned;

        if page["has_more"] == json!(true) {
            assert!(page["truncation_note"].is_string(), "{page}");
            cursor = Some(page["next_cursor"].as_str().unwrap().to_string());
        } else {
            assert!(page.get("next_cursor").is_none(), "{page}");
            assert!(page.get("truncation_note").is_none(), "{page}");
            break;
        }
        assert!(pages <= total, "ページングが終わらない");
    }
    assert_eq!(seen, total);
    assert_eq!(pages, 2, "余分なページを1回踏んでいます");

    // --- get_ledger: `total_lines` と同じ `limit` ---
    // 消耗品費（609）は3行（購入2件 + 赤伝1件）。
    let all = call::<GetLedger>(
        &runtime,
        json!({ "account": "609", "from": "2026-01-01", "to": "2026-12-31" }),
    )
    .await;
    assert!(!is_error(&all), "{all}");
    let total_lines = body(&all)["total_lines"].as_u64().unwrap();
    assert!(total_lines >= 3, "土台が変わっている: {total_lines}");

    let response = call::<GetLedger>(
        &runtime,
        json!({ "account": "609", "from": "2026-01-01", "to": "2026-12-31",
                "limit": total_lines }),
    )
    .await;
    assert!(!is_error(&response), "{response}");
    let page = body(&response);
    assert_eq!(page["returned"], json!(total_lines), "{page}");
    assert_eq!(
        page["has_more"],
        json!(false),
        "ちょうど収まったのに「続きがある」と言っています: {page}"
    );
    assert!(page.get("next_cursor").is_none(), "{page}");
    assert!(
        page.get("truncation_note").is_none(),
        "切っていないのに切ったと言っています: {page}"
    );

    // --- get_ledger: 1行ずつ辿っても最後のページで止まる ---
    let mut cursor: Option<String> = None;
    let mut pages = 0_u64;
    let mut rows = 0_u64;
    loop {
        let mut arguments = json!({
            "account": "609", "from": "2026-01-01", "to": "2026-12-31", "limit": 1
        });
        if let Some(cursor) = &cursor {
            arguments["cursor"] = json!(cursor);
        }
        let response = call::<GetLedger>(&runtime, arguments).await;
        assert!(!is_error(&response), "{response}");
        let page = body(&response);
        pages += 1;
        let returned = page["returned"].as_u64().unwrap();
        assert!(
            returned > 0,
            "0行のページを返しています（{pages} ページ目）: {page}"
        );
        rows += returned;

        if page["has_more"] == json!(true) {
            cursor = Some(page["next_cursor"].as_str().unwrap().to_string());
        } else {
            assert!(page.get("next_cursor").is_none(), "{page}");
            assert!(page.get("truncation_note").is_none(), "{page}");
            break;
        }
        assert!(pages <= total_lines, "ページングが終わらない");
    }
    assert_eq!(rows, total_lines);
    assert_eq!(pages, total_lines, "余分なページを1回踏んでいます");
}

/// 壊れたカーソルは「先頭から」に落ちず、次の手を示して拒否される。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_malformed_cursor_is_rejected_instead_of_restarting_from_the_beginning(
    _pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let runtime = runtime(&app).await;
    post_sample_entries(&runtime).await;

    let response = call::<SearchEntries>(&runtime, json!({ "cursor": "1" })).await;

    assert!(is_error(&response), "{response}");
    assert_eq!(body(&response)["error"], json!("rejected"));
    let message = body(&response)["message"].as_str().unwrap();
    assert!(message.contains("next_cursor"), "{message}");

    // 失敗した呼び出しも監査ログに2行残る。
    let rows = audit_rows(&app).await;
    assert_two_rows(&rows[rows.len() - 2..], "search_entries", "error");
}

/// 上限を超える `limit` は黙って丸めず、上限を名乗って拒否する。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_limit_above_the_maximum_is_refused_instead_of_being_clamped(
    _pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let runtime = runtime(&app).await;

    let response = call::<SearchEntries>(&runtime, json!({ "limit": 1000 })).await;

    assert!(is_error(&response), "{response}");
    assert_eq!(body(&response)["error"], json!("rejected"));
    let message = body(&response)["message"].as_str().unwrap();
    assert!(message.contains("100"), "{message}");
}

/// 集計軸として登録されていないタグキーでは絞り込めない
/// （`CLAUDE.md` §4。有効なキーが分かる文言で拒否する）。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_tag_key_that_is_not_an_aggregation_axis_is_refused(
    _pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let runtime = runtime(&app).await;

    // 登録済みだが集計軸ではないキー。
    let response =
        call::<SearchEntries>(&runtime, json!({ "tags": { "business_ratio": "0.30" } })).await;
    assert!(is_error(&response), "{response}");
    assert_eq!(body(&response)["error"], json!("not_aggregatable"));

    // 未登録のキーは `kaikei-jp` の文言（有効なキー一覧を含む）で拒否される。
    let response =
        call::<SearchEntries>(&runtime, json!({ "tags": { "tax_cat": "SALES_10" } })).await;
    assert!(is_error(&response), "{response}");
    assert_eq!(body(&response)["error"], json!("unknown_tag_key"));
    assert!(
        body(&response)["message"]
            .as_str()
            .unwrap()
            .contains("tax_category"),
        "有効なキー一覧が本文に無い: {response}"
    );
}

/// ★取り消された仕訳が「取り消された」と分かる★（D-088）
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn search_marks_reversed_entries_and_their_reversals(
    _pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let runtime = runtime(&app).await;
    let posted = post_sample_entries(&runtime).await;
    let (original_id, reversal_id) = (posted[3].clone(), posted[4].clone());

    let response = call::<SearchEntries>(&runtime, json!({ "limit": 100 })).await;
    let entries = body(&response)["entries"].as_array().unwrap().clone();

    // 元仕訳は消えない（追記のみ）。
    let original = entries
        .iter()
        .find(|e| e["entry_id"] == json!(original_id))
        .expect("取り消された仕訳も検索に出る");
    assert_eq!(original["reversed_by"]["entry_id"], json!(reversal_id));
    assert!(original["reversed_by"]["entry_no"].is_number());
    assert!(original.get("reverses").is_none());

    // 赤伝の側には訂正対象と理由が付く。
    let reversal = entries
        .iter()
        .find(|e| e["entry_id"] == json!(reversal_id))
        .expect("赤伝も検索に出る");
    assert_eq!(reversal["reverses"], json!(original_id));
    assert_eq!(reversal["reverse_reason"], json!("数量の誤り"));
    assert!(reversal.get("reversed_by").is_none());

    // 取り消しに関わらない仕訳にはどちらの欄も出ない。
    let plain = entries
        .iter()
        .find(|e| e["entry_id"] == json!(posted[0]))
        .unwrap();
    assert!(plain.get("reverses").is_none());
    assert!(plain.get("reversed_by").is_none());
}

/// ★二重訂正された仕訳が1件だけ返る★（D-088。PR-H レビュー C-2）
///
/// `allow_double_reversal: true` で2回訂正された仕訳が、検索では**1件**、
/// 元帳では**明細1行につき1行**しか返らない。素朴な `LEFT JOIN` に戻すと
/// 検索は `{"error":"corrupt"}` になり、元帳は同じ明細を2行返す。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_doubly_reversed_entry_is_returned_only_once_by_search_and_by_the_ledger(
    _pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let runtime = runtime(&app).await;
    let posted = post_sample_entries(&runtime).await;
    let (original, first_reversal, second_reversal) =
        (posted[5].clone(), posted[6].clone(), posted[7].clone());

    // --- 検索 ---
    let response = call::<SearchEntries>(&runtime, json!({ "account": "620" })).await;
    assert!(
        !is_error(&response),
        "二重訂正で応答が壊れています: {response}"
    );
    let page = body(&response);
    assert_eq!(page["total_matches"], json!(3), "{page}");
    assert_eq!(page["returned"], json!(3), "{page}");

    let entries = page["entries"].as_array().unwrap();
    let occurrences = entries
        .iter()
        .filter(|e| e["entry_id"] == json!(original))
        .count();
    assert_eq!(occurrences, 1, "同じ仕訳が2件返っています: {page}");

    // `reversed_by` は最も古い赤伝1件（2件目は現れない。D-088 のトレードオフ）。
    let entry = entries
        .iter()
        .find(|e| e["entry_id"] == json!(original))
        .unwrap();
    assert_eq!(entry["reversed_by"]["entry_id"], json!(first_reversal));

    // 2件目の赤伝も検索には出る（帳簿は追記のみ）。
    let second = entries
        .iter()
        .find(|e| e["entry_id"] == json!(second_reversal))
        .expect("2件目の赤伝も返る");
    assert_eq!(second["reverses"], json!(original));
    assert_eq!(
        second["reverse_reason"],
        json!("科目の誤り（二重に訂正した）")
    );

    // --- 元帳 ---
    let response = call::<GetLedger>(
        &runtime,
        json!({ "account": "620", "from": "2026-01-01", "to": "2026-12-31" }),
    )
    .await;
    assert!(!is_error(&response), "{response}");
    let page = body(&response);

    // 借方 500（元仕訳）と貸方 500 × 2（赤伝2件）の3行。
    assert_eq!(page["total_lines"], json!(3), "{page}");
    assert_eq!(page["returned"], json!(3), "{page}");
    let rows = page["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 3, "同じ明細が2行出ています: {page}");
    let occurrences = rows
        .iter()
        .filter(|r| r["entry_id"] == json!(original))
        .count();
    assert_eq!(
        occurrences, 1,
        "二重訂正された仕訳の行が2行あります: {page}"
    );
    // 費用（借方が正）。500 − 500 − 500 = −500。
    assert_eq!(page["closing_balance"], json!("-500"));

    // ★赤伝の行だけを見て訂正理由が読める★（PR-H レビュー D-2）
    let red = rows
        .iter()
        .find(|r| r["entry_id"] == json!(second_reversal))
        .unwrap();
    assert_eq!(red["reverses"], json!(original));
    assert_eq!(red["reverse_reason"], json!("科目の誤り（二重に訂正した）"));
    // 取り消されていない行には出ない。
    let plain = rows
        .iter()
        .find(|r| r["entry_id"] == json!(original))
        .unwrap();
    assert!(plain.get("reverse_reason").is_none(), "{plain}");

    // ★元帳側でも「どちらの赤伝が付いたか」を見る★（PR-H レビュー2巡目）
    //
    // D-088 は `reversed_by` を**最も古い赤伝1件**と決めている。
    // 元帳が行数と残高しか見ていないと、`LEFT JOIN LATERAL` の `ORDER BY` を
    // DESC に（＝最新の赤伝を返すように）書き換えても緑のまま通る。
    // どちらの赤伝が付いても行数も残高も変わらないためである。
    assert_eq!(
        plain["reversed_by"]["entry_id"],
        json!(first_reversal),
        "最も古い赤伝ではなく別の赤伝が付いています: {plain}"
    );
    // 赤伝の行自体は取り消されていない。
    assert!(red.get("reversed_by").is_none(), "{red}");
}

/// ★打ち間違いを0件の成功にしない★（PR-H レビュー C-3）
///
/// `search_entries` の `account` に勘定科目マスタに無いコードを渡すと
/// `not_found` になる（`get_ledger` と同じ規律）。実在する科目に該当が
/// 無いだけなら 0 件の**成功**である。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn searching_by_an_account_code_that_is_not_in_the_chart_is_not_found(
    _pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let runtime = runtime(&app).await;
    post_sample_entries(&runtime).await;

    let response = call::<SearchEntries>(&runtime, json!({ "account": "99999" })).await;

    assert!(is_error(&response), "0件の成功にしない: {response}");
    assert_eq!(body(&response)["error"], json!("not_found"), "{response}");
    let message = body(&response)["message"].as_str().unwrap();
    assert!(message.contains("99999"), "{message}");
    // 次の手（`CLAUDE.md` §11）。`get_ledger` と同じ案内。
    assert!(message.contains("list_accounts"), "{message}");

    // 失敗した読み取りも監査ログには2行残る。
    let rows = audit_rows(&app).await;
    assert_two_rows(&rows[rows.len() - 2..], "search_entries", "error");

    // 対照実験: 実在する科目で該当が無いだけなら 0 件の成功。
    let response = call::<SearchEntries>(
        &runtime,
        json!({ "account": "620", "from": "2027-01-01", "to": "2027-12-31" }),
    )
    .await;
    assert!(!is_error(&response), "{response}");
    assert_eq!(body(&response)["entries"], json!([]));
    assert_eq!(body(&response)["total_matches"], json!(0));
}

/// ★読み取り系の `audit_log.output` は要約★（D-089 の決定6。
/// PR-H レビュー C-5）
///
/// 読み取りは AI が最も多く呼ぶ操作であり、応答本文をそのまま記録すると
/// 1回で数十 KB になる。監査ログにおける読み取りの目的は
/// 「**誰がいつ何を読んだか**」であり、返した内容そのものは
/// (`input` の条件 + その時点の帳簿) から再現できる。
///
/// **書き込み系は結果そのものが変更の記録なので全体を残す。**
/// この非対称が実際に効いていることも併せて見る。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_read_records_a_summary_in_the_audit_log_while_a_write_records_the_whole_body(
    _pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let runtime = runtime(&app).await;
    post_sample_entries(&runtime).await;

    // --- search_entries ---
    let response = call::<SearchEntries>(&runtime, json!({ "limit": 100 })).await;
    assert!(!is_error(&response), "{response}");
    let body_bytes = body(&response).to_string().len();

    let rows = audit_rows(&app).await;
    let output = rows.last().unwrap().output.clone().expect("結果レコード");
    let output_bytes = output.to_string().len();
    println!(
        "search_entries: 応答本文 {body_bytes} バイト / audit_log.output {output_bytes} バイト"
    );

    // 明細そのものは残さない。
    assert!(
        output.get("entries").is_none(),
        "明細が記録されている: {output}"
    );
    assert!(output.get("truncation_note").is_none(), "{output}");
    // 「何件のうち何件を、どこまで読んだか」は残る。
    assert_eq!(output["total_matches"], body(&response)["total_matches"]);
    assert_eq!(output["returned"], body(&response)["returned"]);
    assert!(output.get("has_more").is_some(), "{output}");
    assert!(
        output_bytes * 4 < body_bytes,
        "要約になっていない（本文 {body_bytes} / output {output_bytes}）"
    );

    // --- get_ledger ---
    let response = call::<GetLedger>(
        &runtime,
        json!({ "account": "609", "from": "2026-01-01", "to": "2026-12-31" }),
    )
    .await;
    assert!(!is_error(&response), "{response}");
    let body_bytes = body(&response).to_string().len();

    let rows = audit_rows(&app).await;
    let output = rows.last().unwrap().output.clone().expect("結果レコード");
    let output_bytes = output.to_string().len();
    println!("get_ledger: 応答本文 {body_bytes} バイト / audit_log.output {output_bytes} バイト");

    assert!(
        output.get("rows").is_none(),
        "明細が記録されている: {output}"
    );
    // 条件（何を読んだか）と期間全体の合計・件数は残る。
    for key in [
        "account",
        "from",
        "to",
        "opening_balance",
        "debit_total",
        "credit_total",
        "closing_balance",
        "total_lines",
        "returned",
        "has_more",
    ] {
        assert_eq!(output[key], body(&response)[key], "{key}: {output}");
    }
    assert!(
        output_bytes * 2 < body_bytes,
        "要約になっていない（本文 {body_bytes} / output {output_bytes}）"
    );

    // --- 対照実験: 書き込み系は本文そのものが残る ---
    let response = call::<PostJournalEntry>(
        &runtime,
        json!({
            "entry_date": "2026-07-01",
            "description": "打ち合わせの茶菓",
            "lines": [
                { "account": "623", "side": "debit", "amount": "1200",
                  "tags": { "tax_category": "PURCHASE_10_QUALIFIED" } },
                { "account": "100", "side": "credit", "amount": "1200" }
            ]
        }),
    )
    .await;
    assert!(!is_error(&response), "{response}");

    let rows = audit_rows(&app).await;
    let output = rows.last().unwrap().output.clone().expect("結果レコード");
    assert_eq!(
        &output,
        body(&response),
        "書き込み系は応答本文がそのまま残る（明細を落とさない）"
    );
    assert!(output.get("lines").is_some(), "{output}");
}

// ---------------------------------------------------------------------------
// get_ledger（MC-17）
// ---------------------------------------------------------------------------

/// MC-17 / MC-11 / MC-27: 科目別に借方・貸方・残高を返し、期間指定が効く。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn get_ledger_returns_debits_credits_and_balances_for_a_period(
    _pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let runtime = runtime(&app).await;
    post_sample_entries(&runtime).await;

    let response = call::<GetLedger>(
        &runtime,
        json!({ "account": "609", "from": "2026-01-01", "to": "2026-12-31" }),
    )
    .await;
    assert!(!is_error(&response), "{response}");
    let page = body(&response);

    assert_eq!(page["account"], json!("609"));
    assert_eq!(page["account_type"], json!("expense"));
    assert_eq!(page["currency"], json!("JPY"));
    assert!(page["account_name"].is_string());
    // 消耗品費: 1,500 + 800 −（赤伝 800）。
    assert_eq!(page["debit_total"], json!("2300"));
    assert_eq!(page["credit_total"], json!("800"));
    assert_eq!(page["opening_balance"], json!("0"));
    assert_eq!(page["closing_balance"], json!("1500"));
    assert_eq!(page["total_lines"], json!(3));
    assert_eq!(page["returned"], json!(3));
    assert_eq!(page["has_more"], json!(false));

    // MC-27: 金額は全て文字列。件数・行番号は number のままでよい。
    for key in [
        "opening_balance",
        "debit_total",
        "credit_total",
        "closing_balance",
    ] {
        assert!(page[key].is_string(), "{key} が文字列ではない: {page}");
    }
    let row = &page["rows"][0];
    assert!(row["amount"].is_string());
    assert!(row["running_balance"].is_string());
    assert!(row["line_no"].is_number());
    // 相手科目が入る（元帳としての可読性）。
    assert_eq!(row["counter_accounts"], json!(["100"]));

    // 残高の累計（借方が正の科目）。
    let balances: Vec<&str> = page["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["running_balance"].as_str().unwrap())
        .collect();
    assert_eq!(balances, vec!["1500", "2300", "1500"]);

    // 期間指定が効く（3月だけに絞ると期首残高が 1,500 になる）。
    let response = call::<GetLedger>(
        &runtime,
        json!({ "account": "609", "from": "2026-03-01", "to": "2026-03-31" }),
    )
    .await;
    let page = body(&response);
    assert_eq!(page["opening_balance"], json!("1500"));
    assert_eq!(page["total_lines"], json!(2));
    assert_eq!(page["closing_balance"], json!("1500"));

    // MC-11: 読み取り系も2行残る。
    let rows = audit_rows(&app).await;
    assert_two_rows(&rows[rows.len() - 2..], "get_ledger", "ok");
}

/// 貸方が正の科目（収益）は残高の符号が逆になる（`DOMAIN.md` §2）。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_credit_normal_account_reports_the_balance_with_the_opposite_sign(
    _pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let runtime = runtime(&app).await;
    post_sample_entries(&runtime).await;

    let response = call::<GetLedger>(
        &runtime,
        json!({ "account": "500", "from": "2026-01-01", "to": "2026-12-31" }),
    )
    .await;
    let page = body(&response);

    assert_eq!(page["account_type"], json!("revenue"));
    assert_eq!(page["credit_total"], json!("13000"));
    assert_eq!(page["debit_total"], json!("0"));
    assert_eq!(page["closing_balance"], json!("13000"));
}

/// ★空の結果と「見つからない」の区別★
///
/// - 実在する科目・取引の無い期間 → **成功**（0行）
/// - 勘定科目マスタに無い科目コード → **`not_found` のエラー**
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn an_empty_period_succeeds_but_an_unknown_account_is_not_found(
    _pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let runtime = runtime(&app).await;
    post_sample_entries(&runtime).await;

    let empty = call::<GetLedger>(
        &runtime,
        json!({ "account": "609", "from": "2027-01-01", "to": "2027-12-31" }),
    )
    .await;
    assert!(!is_error(&empty), "0行はエラーではない: {empty}");
    assert_eq!(body(&empty)["rows"], json!([]));
    assert_eq!(body(&empty)["total_lines"], json!(0));
    assert_eq!(body(&empty)["currency"], json!("JPY"));
    // 期首残高（その期間より前の累計）は残る。
    assert_eq!(body(&empty)["opening_balance"], json!("1500"));

    let missing = call::<GetLedger>(
        &runtime,
        json!({ "account": "99999", "from": "2026-01-01", "to": "2026-12-31" }),
    )
    .await;
    assert!(is_error(&missing), "{missing}");
    assert_eq!(body(&missing)["error"], json!("not_found"));
    let message = body(&missing)["message"].as_str().unwrap();
    assert!(message.contains("99999"), "{message}");
    // 次の手（`CLAUDE.md` §11）。
    assert!(message.contains("list_accounts"), "{message}");
}

/// ★元帳も上限で切ったことが応答から分かる★（D-089）
///
/// 辿っても**残高の累計が連続する**（ページ内で数え直さない）。
///
/// # `limit >= 2` で回す理由（PR-H レビュー2巡目）
///
/// 1行ずつ辿ると**ページの先頭行と末尾行が同じ**になるため、
/// `ledger.rs` の `rows.last()`（次のカーソルはページの**末尾**）を
/// `rows.first()` に書き換えても結果が変わらない。
///
/// # `returned` が `limit` を超えないことも見る理由
///
/// `ledger.rs` は続きの有無を見るために `limit + 1` 行取り、`take(limit)`
/// で切る。この `take` を落とすと、**MCP の `returned` /
/// `truncation_note` が `limit + 1` 行を「返した」と報告する**。
///
/// # 同じ仕訳に2行ある科目（604）でも回す理由
///
/// カーソルは `(entry_date, entry_no, entry_id, line_no)` の4項である。
/// 1仕訳1行の科目しか辿らないと `line_no` の役割が現れない。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_truncated_ledger_says_so_and_keeps_the_running_balance_continuous(
    _pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let runtime = runtime(&app).await;
    post_sample_entries(&runtime).await;

    for account in ["609", "604"] {
        let all = call::<GetLedger>(
            &runtime,
            json!({ "account": account, "from": "2026-01-01", "to": "2026-12-31" }),
        )
        .await;
        assert!(!is_error(&all), "{all}");
        let all = body(&all).clone();
        let expected: Vec<(String, String)> = all["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r["entry_id"].as_str().unwrap().to_string(),
                    r["running_balance"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        assert!(expected.len() >= 3, "{account}: 対照が薄すぎる");

        // ★1行ずつだけでなく2行ずつでも辿る★（上の doc）。
        for limit in [1_u64, 2] {
            let mut collected: Vec<(String, String)> = Vec::new();
            let mut cursor: Option<String> = None;
            loop {
                let mut arguments = json!({
                    "account": account, "from": "2026-01-01", "to": "2026-12-31",
                    "limit": limit
                });
                if let Some(cursor) = &cursor {
                    arguments["cursor"] = json!(cursor);
                }
                let response = call::<GetLedger>(&runtime, arguments).await;
                assert!(!is_error(&response), "{response}");
                let page = body(&response);

                // 合計と行数はページによらず期間全体の値。
                for key in [
                    "total_lines",
                    "debit_total",
                    "credit_total",
                    "closing_balance",
                ] {
                    assert_eq!(page[key], all[key], "{account}: {key}");
                }
                // ★上限を超える行を「返した」と報告しない★
                let returned = page["returned"].as_u64().unwrap();
                assert!(
                    returned <= limit,
                    "{account}: limit={limit} なのに returned={returned}: {page}"
                );
                assert_eq!(returned as usize, page["rows"].as_array().unwrap().len());

                collected.extend(page["rows"].as_array().unwrap().iter().map(|r| {
                    (
                        r["entry_id"].as_str().unwrap().to_string(),
                        r["running_balance"].as_str().unwrap().to_string(),
                    )
                }));

                if page["has_more"] == json!(true) {
                    let note = page["truncation_note"]
                        .as_str()
                        .expect("切ったことが応答から分かること");
                    assert!(note.contains("期間全体"), "{note}");
                    cursor = Some(page["next_cursor"].as_str().unwrap().to_string());
                } else {
                    assert!(page.get("next_cursor").is_none());
                    assert!(page.get("truncation_note").is_none());
                    break;
                }
                assert!(
                    collected.len() <= expected.len(),
                    "{account}: limit={limit}: ページングが終わらない"
                );
            }

            assert_eq!(
                collected, expected,
                "{account}: limit={limit}: ページをまたぐと残高が食い違う"
            );
        }
    }
}

/// ★同じ仕訳に同じ科目が2行ある元帳★（PR-H レビュー2巡目）
///
/// 仮払消費税等を含む4行仕訳を本番の経路で記帳し、
///
/// - 明細が `line_no` の昇順に並ぶ
/// - `running_balance` がその並びどおりに積み上がる
/// - `counter_accounts` が**重複を除いてコード順**で返る
///
/// ことを見る。2行の仕訳しか土台に無いと、この3つはどれも
/// 壊しても気付けない（相手科目が1件しか無く、並びが一意に決まるため）。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn the_ledger_orders_and_accumulates_two_lines_of_the_same_account_in_one_entry(
    _pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let runtime = runtime(&app).await;
    let posted = post_sample_entries(&runtime).await;
    let (four_line, two_line) = (posted[8].clone(), posted[9].clone());

    // --- 通信費（604。4行仕訳の中に2行ある） ---
    let response = call::<GetLedger>(
        &runtime,
        json!({ "account": "604", "from": "2026-01-01", "to": "2026-12-31" }),
    )
    .await;
    assert!(!is_error(&response), "{response}");
    let page = body(&response);

    assert_eq!(page["total_lines"], json!(3), "{page}");
    assert_eq!(page["debit_total"], json!("31000"));
    assert_eq!(page["closing_balance"], json!("31000"));

    let rows = page["rows"].as_array().unwrap();
    // ★並び★ 同じ仕訳の2行は line_no の昇順（2 → 3）。
    let order: Vec<(&str, u64)> = rows
        .iter()
        .map(|r| {
            (
                r["entry_id"].as_str().unwrap(),
                r["line_no"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        order,
        vec![
            (four_line.as_str(), 2),
            (four_line.as_str(), 3),
            (two_line.as_str(), 1),
        ],
        "{page}"
    );
    // ★残高の累計★ その並びどおりに 10,000 → 30,000 → 31,000。
    let balances: Vec<&str> = rows
        .iter()
        .map(|r| r["running_balance"].as_str().unwrap())
        .collect();
    assert_eq!(balances, vec!["10000", "30000", "31000"], "{page}");
    let amounts: Vec<&str> = rows.iter().map(|r| r["amount"].as_str().unwrap()).collect();
    assert_eq!(amounts, vec!["10000", "20000", "1000"], "{page}");

    // --- 未払金（325。相手科目が 180 / 604 / 604 と重複する） ---
    let response = call::<GetLedger>(
        &runtime,
        json!({ "account": "325", "from": "2026-01-01", "to": "2026-12-31" }),
    )
    .await;
    assert!(!is_error(&response), "{response}");
    let page = body(&response);
    let rows = page["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{page}");

    // ★相手科目★ 記帳順は 180 → 604 → 604 だが、重複を畳んでコード順で返る。
    assert_eq!(rows[0]["entry_id"], json!(four_line));
    assert_eq!(rows[0]["counter_accounts"], json!(["180", "604"]), "{page}");
    assert_eq!(rows[1]["counter_accounts"], json!(["604"]), "{page}");
}

/// ★金額の範囲は「明細1行」と比べる★（`docs/07-mcp-server.md` §3）
///
/// `min_amount` / `max_amount` は**同じ1行**が両方を満たすことを求める。
/// min を満たす行と max を満たす行が別々でよいことにすると、
/// 「4,000〜9,000 円の明細を含む仕訳」の検索に
/// **3,000 円と 10,000 円しか持たない仕訳**が混ざる。
///
/// # 通貨の突き合わせはここでは見られない（PR-H レビュー3巡目 C-2）
///
/// `search.rs` は範囲の `EXISTS` の中で明細の通貨も突き合わせている
/// （別通貨の 25,000 が JPY 25,000 と一致しないように）。しかし
/// `search_entries` は `min_amount` / `max_amount` を**帳簿通貨**で解釈し、
/// `post_journal_entry` も帳簿通貨でしか記帳できないので、
/// **MCP 経路からは外貨の明細を作ることも外貨で絞ることもできない**。
/// そのためこの退行は e2e では到達不能であり、read model の SQL を
/// 直接突ける差分テスト
/// （`kaikei-store/tests/search_ledger_differential.rs` の
/// `the_amount_range_compares_only_lines_in_the_same_currency`）で閉じてある。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn the_amount_range_must_be_satisfied_by_one_single_line(
    _pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let runtime = runtime(&app).await;
    post_sample_entries(&runtime).await;

    // 土台の明細金額は 10,000 / 1,500 / 3,000 / 800 / 500 / 33,000 /
    // 20,000 / 1,000 で、4,000〜9,000 に入るものは1行も無い。
    let response = call::<SearchEntries>(
        &runtime,
        json!({ "min_amount": "4000", "max_amount": "9000" }),
    )
    .await;
    assert!(!is_error(&response), "{response}");
    assert_eq!(
        body(&response)["total_matches"],
        json!(0),
        "min と max を別々の明細行が満たしています: {}",
        body(&response)
    );

    // 対照実験: 1行で両方を満たす範囲なら見つかる（空振りの検査ではない）。
    let response = call::<SearchEntries>(
        &runtime,
        json!({ "min_amount": "20000", "max_amount": "33000" }),
    )
    .await;
    assert!(!is_error(&response), "{response}");
    assert_eq!(body(&response)["total_matches"], json!(1), "{response}");
}

/// ★複数のタグは AND★（`search_entries.rs` の契約 /
/// `docs/07-mcp-server.md` §3）
///
/// 複数指定した場合は**すべてを満たす**仕訳だけが返る。
/// タグのキーが1つしか無い検査では、AND を OR に書き換えても結果が
/// 変わらないため、**2キー以上**で、かつ**片方だけを満たす仕訳**が
/// 土台にある状態で見る。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn every_tag_must_match_not_just_one_of_them(
    _pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let runtime = runtime(&app).await;
    let posted = post_sample_entries(&runtime).await;
    let four_line = posted[8].clone();

    let matches = |response: &Value| -> u64 {
        assert!(!is_error(response), "{response}");
        body(response)["total_matches"].as_u64().unwrap()
    };

    // 片方ずつなら、どちらも2件以上に当たる。
    let only_counterparty =
        call::<SearchEntries>(&runtime, json!({ "tags": { "counterparty": "CP0009" } })).await;
    assert_eq!(matches(&only_counterparty), 1, "{only_counterparty}");
    let only_tax_category = call::<SearchEntries>(
        &runtime,
        json!({ "tags": { "tax_category": "PURCHASE_10_QUALIFIED" } }),
    )
    .await;
    assert!(
        matches(&only_tax_category) >= 4,
        "{only_tax_category}: 対照が薄すぎる"
    );

    // 両方を満たすのは4行仕訳だけ（郵便料金は tax_category しか持たない）。
    let both = call::<SearchEntries>(
        &runtime,
        json!({ "tags": {
            "counterparty": "CP0009",
            "tax_category": "PURCHASE_10_QUALIFIED"
        } }),
    )
    .await;
    assert_eq!(
        matches(&both),
        1,
        "すべてのタグを満たす仕訳だけが返るはず: {both}"
    );
    assert_eq!(body(&both)["entries"][0]["entry_id"], json!(four_line));

    // どちらも単独では当たるが、両方を満たす仕訳が無い組み合わせは0件。
    let none = call::<SearchEntries>(
        &runtime,
        json!({ "tags": {
            "counterparty": "CP0009",
            "tax_category": "SALES_10"
        } }),
    )
    .await;
    assert_eq!(matches(&none), 0, "{none}");
}

/// ★元帳にも「取り消された」ことが出る★（D-088）
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn the_ledger_marks_rows_of_reversed_entries(
    _pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let runtime = runtime(&app).await;
    let posted = post_sample_entries(&runtime).await;

    let response = call::<GetLedger>(
        &runtime,
        json!({ "account": "609", "from": "2026-01-01", "to": "2026-12-31" }),
    )
    .await;
    let rows = body(&response)["rows"].as_array().unwrap().clone();

    let reversed = rows
        .iter()
        .find(|r| r["entry_id"] == json!(posted[3]))
        .expect("取り消された仕訳の行も元帳に残る");
    assert_eq!(reversed["reversed_by"]["entry_id"], json!(posted[4]));

    let red = rows
        .iter()
        .find(|r| r["entry_id"] == json!(posted[4]))
        .expect("赤伝の行も元帳に残る");
    assert_eq!(red["reverses"], json!(posted[3]));
    assert_eq!(red["side"], json!("credit"), "赤伝は貸借が入れ替わる");
}

/// `from > to` は「0行の元帳」として静かに成功させない。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_reversed_period_is_rejected_instead_of_returning_an_empty_ledger(
    _pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts).await;
    let runtime = runtime(&app).await;

    let response = call::<GetLedger>(
        &runtime,
        json!({ "account": "609", "from": "2026-12-31", "to": "2026-01-01" }),
    )
    .await;

    assert!(is_error(&response), "{response}");
    assert_eq!(body(&response)["error"], json!("rejected"));
    let message = body(&response)["message"].as_str().unwrap();
    assert!(message.contains("2026-12-31"), "{message}");
}
