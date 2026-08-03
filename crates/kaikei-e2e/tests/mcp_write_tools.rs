//! Phase 3 PR-F: MCP の書き込み系ツール（`post_journal_entry` /
//! `reverse_journal_entry`）を**実 PostgreSQL に対して**通す。
//!
//! # なぜ `kaikei-mcp` 側ではなくここに置くのか
//!
//! `docs/07-mcp-server.md` §10 が定めているとおり、`kaikei-mcp` は
//! `sqlx` に依存しない（MC-30 の許可リスト）ため `#[sqlx::test]` を使えず、
//! **使い捨てDBも `audit_log` の SELECT も持てない**。
//! 「記帳が失敗しても監査ログには2行残る」（D-070 / D-077 の核心）を
//! 確かめるには、仕訳を書ける使い捨てDBと SQL の両方が要る。
//! 両方を持ち、かつ `kaikei-mcp` に依存してよいのはこの crate だけである。
//!
//! # ここで見るもの
//!
//! | # | 見るもの |
//! |---|---|
//! | MC-01 | 貸借一致の仕訳が post でき、確定後明細が返る |
//! | MC-02 | 貸借不一致は `unbalanced` のツール結果エラー。差額と `hint` が付く |
//! | MC-03 | `auto_tax_lines: true` で税額行が自動追加される |
//! | MC-04 | 存在しない科目コードには候補（`hint`）が付く |
//! | MC-05 | 締め済み期間への post は拒否される |
//! | MC-09 | 金額を JSON number で渡すと日本語のエラー。**この呼び出しも監査ログに残る** |
//! | MC-11 | 1回の呼び出しにつき同一 `request_id` で2行。`tool` 列が一致し、成功時は `entry_id` も一致 |
//! | MC-12 | `reason` が空白のみなら `empty_reverse_reason` |
//! | MC-22 | 記帳が失敗して rollback されても**開始レコードは残る**（帳簿は0件） |
//! | MC-25 | `PolicyNote` が応答と `audit_log.output` の両方に残る |
//! | MC-26 | ドメインのエラーはプロトコルエラーではなく `isError: true` |
//! | MC-27 | 出力の金額が全て JSON 文字列 |
//! | MC-29 | 未登録のタグキーはエラーで、有効なキー一覧が本文に出る |
//! | — | 逆仕訳が通り、元仕訳と逆仕訳の両方が残る（**元仕訳は書き換わらない**） |
//!
//! MC-20 / MC-21 / MC-31 / MC-32（fail-closed / fail-open / 無害化）は
//! PR-C が `crates/kaikei-store/tests/audit_log.rs` で実証済みなので重ねない。

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
use kaikei_mcp::tools::post_journal_entry::PostJournalEntry;
use kaikei_mcp::tools::reverse_journal_entry::ReverseJournalEntry;
use kaikei_store::audit::PgAuditSink;
use kaikei_store::pool::PgStore;
use serde_json::{json, Value};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::PgPool;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// セットアップ
// ---------------------------------------------------------------------------

fn book_settings() -> BookSettings {
    BookSettings {
        fiscal_year_rule: FiscalYearRule::CalendarYear,
        book_currency: Currency::JPY,
    }
}

/// 課税事業者・税抜経理（同梱 2026 年度マスタの既定）で組み立てる。
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

/// 本番の合成ルート（`kaikei_mcp::startup::assemble`）と**同じ形**の
/// [`Runtime`] を、`#[sqlx::test]` が作った使い捨てDBに対して組み立てる。
///
/// `assemble` をそのまま呼ばないのは、あちらが `APP_DATABASE_URL` を
/// 環境変数として受け取る（＝使い捨てDBを指せない）ためである。
/// 組み立ての中身（`compose` / `PgStore` / `PgAuditSink`）は同じものを使う。
async fn runtime(app: &PgPool) -> Runtime {
    let composition = compose(compose_options()).expect("合成に失敗しました");
    seed_chart(app, &composition.chart).await;
    Runtime {
        store: Arc::new(PgStore::new(app.clone())),
        // 帳簿と**同じプール**から別の接続を acquire する（本番と同じ形。
        // 分離の実体は「別プール」ではなく「トランザクションを経由しない」
        // ことである。`docs/07-mcp-server.md` §9）。
        audit_sink: Arc::new(PgAuditSink::new(app.clone())),
        composition: Arc::new(composition),
        book_settings: book_settings(),
        id_gen: UuidV7IdGenerator,
        clock: SystemClock,
    }
}

/// 勘定科目マスタを投入する（本番の起動と同じ経路。`DECISIONS.md` D-081）。
async fn seed_chart(pool: &PgPool, chart: &ChartOfAccounts) {
    let store = PgStore::new(pool.clone());
    with_tx(&store, |tx| {
        let chart = chart.clone();
        Box::pin(async move { import_chart::execute(tx, &chart).await })
    })
    .await
    .unwrap_or_else(|e| panic!("勘定科目マスタの投入に失敗しました: {e}"));
}

// ---------------------------------------------------------------------------
// ツールの呼び出しと応答の読み取り
// ---------------------------------------------------------------------------

/// ツールを1回呼び、MCP の応答を線上の JSON として返す。
///
/// `rmcp` の `ToolRouter::call` は `RequestContext` を要求し、その構築に
/// 必要な `rmcp::service::Peer::new` は `pub(crate)` で外部 crate から
/// 組み立てられない（`docs/07-mcp-server.md` §10 MC-10 の注記）。
/// **`dispatch::call` はルータのハンドラが呼ぶのと同じ関数**なので、
/// ここを直接叩けば経路は本物と同一である。
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

fn message(response: &Value) -> &str {
    body(response)["message"]
        .as_str()
        .expect("エラー応答には message がある")
}

// ---------------------------------------------------------------------------
// audit_log の読み取り
// ---------------------------------------------------------------------------

#[derive(Debug, sqlx::FromRow)]
struct AuditRow {
    request_id: sqlx::types::Uuid,
    actor: String,
    tool: String,
    status: String,
    input: Option<Value>,
    output: Option<Value>,
    error_code: Option<String>,
    entry_id: Option<sqlx::types::Uuid>,
}

/// `audit_log` の全行（使い捨てDBなので、そのテストが書いた分しか無い）。
async fn audit_rows(pool: &PgPool) -> Vec<AuditRow> {
    sqlx::query_as::<_, AuditRow>(
        "SELECT request_id, actor, tool, status, input, output, error_code, entry_id \
         FROM audit_log ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .expect("audit_log を読めること")
}

async fn journal_entry_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM journal_entries")
        .fetch_one(pool)
        .await
        .expect("journal_entries の件数を取れること")
}

/// 「1回の呼び出し＝同一 `request_id` の2行（開始・結果）」を確かめる。
fn assert_two_rows(rows: &[AuditRow], tool: &str, expected_status: &str) {
    assert_eq!(
        rows.len(),
        2,
        "1回のツール呼び出しにつき開始・結果の2行が残るはず: {rows:?}"
    );
    assert_eq!(
        rows[0].request_id, rows[1].request_id,
        "2行は同じ request_id を持つはず"
    );
    assert_eq!(rows[0].status, "started");
    assert_eq!(rows[1].status, expected_status);
    for row in rows {
        assert_eq!(row.tool, tool, "tool 列が呼び出したツール名と一致すること");
        assert_eq!(row.actor, "mcp");
    }
    // 開始レコードには入力が、結果レコードには出力が入る。
    assert!(rows[0].input.is_some(), "開始レコードに input が無い");
    assert!(
        rows[0].output.is_none(),
        "開始レコードに output が入っている"
    );
    assert!(rows[1].output.is_some(), "結果レコードに output が無い");
}

// ---------------------------------------------------------------------------
// 記帳（成功）
// ---------------------------------------------------------------------------

/// MC-01 / MC-03 / MC-11 / MC-25 / MC-27。
///
/// 税抜経理・課税事業者の設定で `auto_tax_lines: true` を渡すと税額行が
/// 追加されて貸借が一致し、**開始・結果の2行**が監査ログに残る。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_successful_posting_returns_the_final_lines_and_leaves_two_audit_rows(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::roles(pool_opts, conn_opts).await.app;
    let runtime = runtime(&app).await;

    let response = call::<PostJournalEntry>(
        &runtime,
        json!({
            "entry_date": "2026-04-15",
            "description": "A社への請求",
            "lines": [
                { "account": "135", "side": "debit",  "amount": "110000" },
                { "account": "500", "side": "credit", "amount": "100000",
                  "memo": "4月分",
                  "tags": { "tax_category": "SALES_10" } }
            ],
            "auto_tax_lines": true
        }),
    )
    .await;

    assert!(!is_error(&response), "記帳が失敗しました: {response}");
    let body = body(&response);

    // 確定後の明細（入力2行 + 仮受消費税の1行）が返る。
    let lines = body["lines"].as_array().expect("lines が配列でない");
    assert_eq!(lines.len(), 3, "税額行が追加されるはず: {body}");
    assert!(lines.iter().any(|l| l["account"] == json!("330")));

    // MC-27: 出力の金額は全て JSON 文字列。
    assert_eq!(body["debit_total"], json!("110000"));
    assert_eq!(body["credit_total"], json!("110000"));
    for line in lines {
        assert!(line["amount"].is_string(), "金額が文字列でない: {line}");
    }
    // 件数・年度・仕訳番号は number のままでよい（§5）。
    assert!(body["entry_no"].is_number());
    assert_eq!(body["fiscal_year"], json!(2026));
    // 仕訳IDは UUID の正準表記（10進表記にしない）。
    let entry_id = body["entry_id"].as_str().expect("entry_id が無い");
    assert_eq!(entry_id.len(), 36);
    assert_eq!(entry_id.matches('-').count(), 4);
    // `policy_notes` はキーとして必ず出す（空でも「注記が無い」とは限らない）。
    assert!(body["policy_notes"].is_array());

    // MC-11: 開始・結果の2行。結果レコードの entry_id が返した仕訳IDと一致。
    let rows = audit_rows(&app).await;
    assert_two_rows(&rows, "post_journal_entry", "ok");
    assert_eq!(
        rows[1].entry_id.map(|id| id.to_string()).as_deref(),
        Some(entry_id)
    );
    assert!(rows[1].error_code.is_none());
    // MC-25: 応答に載せた本体がそのまま output に残る。
    let output = rows[1].output.as_ref().unwrap();
    assert!(output["policy_notes"].is_array(), "{output}");
    assert_eq!(output["debit_total"], json!("110000"));
    // 入力もそのまま残る（AI が何をしようとしたかが読める）。
    assert_eq!(
        rows[0].input.as_ref().unwrap()["description"],
        json!("A社への請求")
    );

    assert_eq!(journal_entry_count(&app).await, 1);
}

/// MC-25: 経過措置対象の税区分で記帳すると `PolicyNote` が応答にも
/// `audit_log.output` にも残る（D-059 / D-070）。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_transitional_measure_note_reaches_both_the_response_and_the_audit_log(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::roles(pool_opts, conn_opts).await.app;
    let runtime = runtime(&app).await;

    let response = call::<PostJournalEntry>(
        &runtime,
        json!({
            "entry_date": "2026-04-15",
            "description": "非適格事業者からの仕入",
            "lines": [
                { "account": "555", "side": "debit",  "amount": "100000",
                  "tags": { "tax_category": "PURCHASE_10_NON_QUALIFIED" } },
                { "account": "100", "side": "credit", "amount": "110000" }
            ],
            "auto_tax_lines": true
        }),
    )
    .await;

    assert!(!is_error(&response), "{response}");
    let notes = body(&response)["policy_notes"]
        .as_array()
        .expect("policy_notes が配列でない");
    assert!(
        !notes.is_empty(),
        "経過措置の注記が応答に載っていない: {response}"
    );
    assert!(notes[0]["severity"].is_string());
    assert!(!notes[0]["message"].as_str().unwrap().is_empty());

    let rows = audit_rows(&app).await;
    let output = rows[1].output.as_ref().unwrap();
    assert_eq!(
        &output["policy_notes"],
        &body(&response)["policy_notes"],
        "応答と audit_log.output で注記が食い違っている"
    );
}

// ---------------------------------------------------------------------------
// 記帳（失敗）
// ---------------------------------------------------------------------------

/// MC-02 / MC-22 / MC-26。**D-077 の核心**をツール経由で見る。
///
/// 記帳が失敗して `with_tx` が rollback しても、監査ログの2行は残る。
/// 帳簿は1件も増えていない。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn an_unbalanced_entry_is_rejected_with_a_hint_while_the_audit_rows_survive(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::roles(pool_opts, conn_opts).await.app;
    let runtime = runtime(&app).await;

    // 税額行を生成しない指定で、税抜の売上を立てる（借方 110,000 / 貸方 100,000）。
    let response = call::<PostJournalEntry>(
        &runtime,
        json!({
            "entry_date": "2026-04-15",
            "description": "A社への請求",
            "lines": [
                { "account": "135", "side": "debit",  "amount": "110000" },
                { "account": "500", "side": "credit", "amount": "100000",
                  "tags": { "tax_category": "SALES_10" } }
            ],
            "auto_tax_lines": false
        }),
    )
    .await;

    // MC-26: プロトコルエラーではなくツール結果エラー。
    assert!(is_error(&response), "{response}");
    let body = body(&response);
    assert_eq!(body["error"], json!("unbalanced"));
    // 本文には差額が入る（`CLAUDE.md` §11）。
    assert!(
        message(&response).contains("差額"),
        "{}",
        message(&response)
    );
    // 機械可読フィールドは区切り無しの文字列（§5）。
    assert_eq!(body["debit_total"], json!("110000"));
    assert_eq!(body["credit_total"], json!("100000"));
    assert_eq!(body["difference"], json!("10000"));

    // §3 の `hint`: 税額行を足せば一致することを、dry-run の結果で示す。
    let hint = &body["hint"];
    let suggested = hint["suggested_lines"]
        .as_array()
        .unwrap_or_else(|| panic!("hint.suggested_lines が無い: {body}"));
    assert_eq!(
        suggested.len(),
        3,
        "税額行を含む3行が提案されるはず: {hint}"
    );
    assert!(suggested.iter().any(|l| l["account"] == json!("330")));
    assert_eq!(hint["debit_total"], hint["credit_total"]);

    // ★D-077 の核心★ 帳簿は0件だが監査ログには2行残る。
    assert_eq!(journal_entry_count(&app).await, 0, "帳簿が変わっている");
    let rows = audit_rows(&app).await;
    assert_two_rows(&rows, "post_journal_entry", "error");
    assert_eq!(rows[1].error_code.as_deref(), Some("unbalanced"));
    assert!(rows[1].entry_id.is_none());
    // 結果レコードの output には `public_message()` が入る。
    assert!(rows[1].output.as_ref().unwrap()["message"]
        .as_str()
        .unwrap()
        .contains("貸借不一致"));
}

/// MC-04: 存在しない科目コードには候補（`hint`）を返す。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn an_unknown_account_is_rejected_with_a_short_list_of_candidates(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::roles(pool_opts, conn_opts).await.app;
    let runtime = runtime(&app).await;

    let response = call::<PostJournalEntry>(
        &runtime,
        json!({
            "entry_date": "2026-04-15",
            "description": "存在しない科目",
            "lines": [
                { "account": "136", "side": "debit",  "amount": "1000" },
                { "account": "500", "side": "credit", "amount": "1000",
                  "tags": { "tax_category": "SALES_10" } }
            ]
        }),
    )
    .await;

    assert!(is_error(&response), "{response}");
    let body = body(&response);
    assert_eq!(body["error"], json!("unknown_account"));
    let candidates = body["hint"]["candidate_accounts"]
        .as_array()
        .unwrap_or_else(|| panic!("候補が無い: {body}"));
    assert!(!candidates.is_empty() && candidates.len() <= 5, "{body}");
    // コードが近い記帳可能な科目が挙がる（全件を返さない）。
    let codes: Vec<&str> = candidates
        .iter()
        .map(|c| c["account"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"135"), "{codes:?}");
    assert!(candidates[0]["name"].is_string());

    assert_eq!(journal_entry_count(&app).await, 0);
}

/// MC-05: 締め済み期間への post は拒否される。
///
/// Phase 3 に `close_period` は無いので、`period_snapshots` に直接 INSERT して
/// 締め状態を作る（`kaikei_app` は INSERT 権限を持つ）。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn posting_into_a_closed_period_is_rejected(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::roles(pool_opts, conn_opts).await.app;
    let runtime = runtime(&app).await;

    sqlx::query(
        "INSERT INTO period_snapshots \
         (fiscal_year, period_end, closed_at, balances, currency, currency_minor_unit, \
          entry_count, last_entry_no, checksum) \
         VALUES (2026, DATE '2026-06-30', now(), '{}'::jsonb, 'JPY', 0, 0, 0, 'x')",
    )
    .execute(&app)
    .await
    .expect("締め状態を作れること");

    let response = call::<PostJournalEntry>(
        &runtime,
        json!({
            "entry_date": "2026-04-15",
            "description": "締め済み期間への記帳",
            "lines": [
                { "account": "100", "side": "debit",  "amount": "1000" },
                { "account": "500", "side": "credit", "amount": "1000",
                  "tags": { "tax_category": "SALES_10" } }
            ]
        }),
    )
    .await;

    assert!(is_error(&response), "{response}");
    assert_eq!(body(&response)["error"], json!("period_closed"));
    assert_eq!(journal_entry_count(&app).await, 0);
    // 拒否された呼び出しも監査ログに残る。
    assert_two_rows(&audit_rows(&app).await, "post_journal_entry", "error");
}

/// MC-09: 金額を JSON number で渡すと**日本語**のエラーになり、
/// **この呼び出しも監査ログに残る**（(3) は PR-F の担当）。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn an_amount_given_as_a_json_number_is_rejected_in_japanese_and_still_audited(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::roles(pool_opts, conn_opts).await.app;
    let runtime = runtime(&app).await;

    let response = call::<PostJournalEntry>(
        &runtime,
        json!({
            "entry_date": "2026-04-15",
            "description": "number で渡す",
            "lines": [
                { "account": "100", "side": "debit",  "amount": 1000 },
                { "account": "500", "side": "credit", "amount": "1000" }
            ]
        }),
    )
    .await;

    assert!(is_error(&response), "{response}");
    let text = message(&response);
    assert!(text.contains("金額は文字列で渡してください"), "{text}");
    assert!(
        !text.contains("invalid type"),
        "英語の型エラーに落ちている: {text}"
    );
    // 「入力を直せば通る」拒否である（サーバ都合の失敗と混同させない）。
    assert_eq!(body(&response)["error"], json!("rejected"));

    assert_eq!(journal_entry_count(&app).await, 0);
    assert_two_rows(&audit_rows(&app).await, "post_journal_entry", "error");
}

/// MC-29: 未登録のタグキーはエラーで、**有効なキー一覧**が本文に出る。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn an_unregistered_tag_key_is_rejected_listing_the_valid_keys(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::roles(pool_opts, conn_opts).await.app;
    let runtime = runtime(&app).await;

    let response = call::<PostJournalEntry>(
        &runtime,
        json!({
            "entry_date": "2026-04-15",
            "description": "タグキーの綴り間違い",
            "lines": [
                { "account": "100", "side": "debit",  "amount": "1000" },
                { "account": "500", "side": "credit", "amount": "1000",
                  "tags": { "tax_cat": "SALES_10" } }
            ]
        }),
    )
    .await;

    assert!(is_error(&response), "{response}");
    assert_eq!(body(&response)["error"], json!("unknown_tag_key"));
    let text = message(&response);
    assert!(
        text.contains("tax_category"),
        "有効なキー一覧が無い: {text}"
    );
    // 何行目かが分かる（`CLAUDE.md` §11）。
    assert!(text.contains("明細 2 行目"), "{text}");
    assert_eq!(body(&response)["line"], json!(2));

    assert_eq!(journal_entry_count(&app).await, 0);
}

// ---------------------------------------------------------------------------
// 逆仕訳
// ---------------------------------------------------------------------------

/// 逆仕訳が通り、**元仕訳と逆仕訳の両方が残る**（元仕訳は書き換わらない）。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn reversing_adds_a_second_entry_and_never_rewrites_the_original(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::roles(pool_opts, conn_opts).await.app;
    let runtime = runtime(&app).await;

    let posted = call::<PostJournalEntry>(
        &runtime,
        json!({
            "entry_date": "2026-04-15",
            "description": "A社への請求",
            "lines": [
                { "account": "135", "side": "debit",  "amount": "1000" },
                { "account": "500", "side": "credit", "amount": "1000",
                  "tags": { "tax_category": "SALES_10" } }
            ]
        }),
    )
    .await;
    assert!(!is_error(&posted), "{posted}");
    let original_id = body(&posted)["entry_id"].as_str().unwrap().to_string();
    let original_before = fetch_entry(&app, &original_id).await;

    let reversed = call::<ReverseJournalEntry>(
        &runtime,
        json!({
            "original_id": original_id,
            "reverse_date": "2026-05-01",
            "reason": "請求金額の誤り（税率の適用誤り）"
        }),
    )
    .await;

    assert!(!is_error(&reversed), "{reversed}");
    let body = body(&reversed);
    assert_eq!(body["reverses"], json!(original_id));
    assert_eq!(body["description"], json!("【訂正】A社への請求"));
    // 借方・貸方が入れ替わっている。
    let lines = body["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 2);
    let debit = lines.iter().find(|l| l["side"] == json!("debit")).unwrap();
    assert_eq!(debit["account"], json!("500"));
    // `policy_notes` は**キーごと出さない**（注記の発生経路が無い）。
    assert!(
        body.get("policy_notes").is_none(),
        "reverse に policy_notes を置かないこと: {body}"
    );

    // ★元仕訳は1文字も変わっていない★
    assert_eq!(fetch_entry(&app, &original_id).await, original_before);
    assert_eq!(journal_entry_count(&app).await, 2, "両方が残るはず");

    // 2回の呼び出しでそれぞれ2行、計4行。
    let rows = audit_rows(&app).await;
    assert_eq!(rows.len(), 4, "{rows:?}");
    assert_eq!(rows[2].tool, "reverse_journal_entry");
    assert_eq!(rows[3].tool, "reverse_journal_entry");
    assert_eq!(rows[3].status, "ok");
    assert_eq!(
        rows[3].entry_id.map(|id| id.to_string()).as_deref(),
        body["entry_id"].as_str()
    );
}

/// MC-12: 訂正理由が空白のみなら `empty_reverse_reason`。
///
/// 検証は `kaikei-app` のユースケース層にあり（D-074）、MCP 層は写像するだけ。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_blank_reverse_reason_is_rejected_before_any_io(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::roles(pool_opts, conn_opts).await.app;
    let runtime = runtime(&app).await;

    // 全角スペースのみも拒否される（`str::trim` は Unicode の空白を落とす）。
    for reason in ["", "   ", "\u{3000}"] {
        let response = call::<ReverseJournalEntry>(
            &runtime,
            json!({
                "original_id": "0192a7b3-1234-7abc-8def-0123456789ab",
                "reverse_date": "2026-05-01",
                "reason": reason
            }),
        )
        .await;
        assert!(is_error(&response), "reason={reason:?}: {response}");
        assert_eq!(
            body(&response)["error"],
            json!("empty_reverse_reason"),
            "reason={reason:?}"
        );
    }
    assert_eq!(journal_entry_count(&app).await, 0);
}

/// 存在しない仕訳IDは `not_found` で、**UUID の正準表記**で示される。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn reversing_a_missing_entry_reports_the_id_in_canonical_uuid_form(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::roles(pool_opts, conn_opts).await.app;
    let runtime = runtime(&app).await;

    let missing = "0192a7b3-1234-7abc-8def-0123456789ab";
    let response = call::<ReverseJournalEntry>(
        &runtime,
        json!({
            "original_id": missing,
            "reverse_date": "2026-05-01",
            "reason": "誤記帳のため"
        }),
    )
    .await;

    assert!(is_error(&response), "{response}");
    assert_eq!(body(&response)["error"], json!("not_found"));
    assert!(
        message(&response).contains(missing),
        "{}",
        message(&response)
    );

    // UUID ですらない文字列は `not_found` と区別する（次の手が違う）。
    let response = call::<ReverseJournalEntry>(
        &runtime,
        json!({
            "original_id": "42",
            "reverse_date": "2026-05-01",
            "reason": "誤記帳のため"
        }),
    )
    .await;
    assert_eq!(body(&response)["error"], json!("invalid_entry_id"));
}

/// 二重訂正は既定で拒否され、**既存の赤伝の仕訳ID**が返る。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_second_reversal_is_refused_and_points_at_the_existing_one(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::roles(pool_opts, conn_opts).await.app;
    let runtime = runtime(&app).await;

    let posted = call::<PostJournalEntry>(
        &runtime,
        json!({
            "entry_date": "2026-04-15",
            "description": "A社への請求",
            "lines": [
                { "account": "135", "side": "debit",  "amount": "1000" },
                { "account": "500", "side": "credit", "amount": "1000",
                  "tags": { "tax_category": "SALES_10" } }
            ]
        }),
    )
    .await;
    let original_id = body(&posted)["entry_id"].as_str().unwrap().to_string();

    let first = call::<ReverseJournalEntry>(
        &runtime,
        json!({ "original_id": original_id, "reverse_date": "2026-05-01", "reason": "誤り" }),
    )
    .await;
    assert!(!is_error(&first), "{first}");
    let reversal_id = body(&first)["entry_id"].as_str().unwrap().to_string();

    let second = call::<ReverseJournalEntry>(
        &runtime,
        json!({ "original_id": original_id, "reverse_date": "2026-05-02", "reason": "もう一度" }),
    )
    .await;

    assert!(is_error(&second), "{second}");
    let body = body(&second);
    assert_eq!(body["error"], json!("already_reversed"));
    assert_eq!(body["reversal_id"], json!(reversal_id));
    assert!(body["reversal_no"].is_number());
    assert!(
        message(&second).contains("allow_double_reversal"),
        "{}",
        message(&second)
    );

    // 明示的に許可すれば通る。
    let third = call::<ReverseJournalEntry>(
        &runtime,
        json!({
            "original_id": original_id,
            "reverse_date": "2026-05-02",
            "reason": "二重訂正の許可を明示",
            "allow_double_reversal": true
        }),
    )
    .await;
    assert!(!is_error(&third), "{third}");
    assert_eq!(journal_entry_count(&app).await, 3);
}

/// 仕訳1件の生の行（元仕訳が書き換わっていないことの比較用）。
#[derive(Debug, PartialEq, sqlx::FromRow)]
struct RawEntry {
    entry_no: i32,
    entry_date: sqlx::types::chrono::NaiveDate,
    description: String,
    recorded_at: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>,
    reverses: Option<sqlx::types::Uuid>,
    reverse_reason: Option<String>,
}

async fn fetch_entry(pool: &PgPool, entry_id: &str) -> RawEntry {
    let uuid: sqlx::types::Uuid = entry_id.parse().expect("UUID として読めること");
    sqlx::query_as::<_, RawEntry>(
        "SELECT entry_no, entry_date, description, recorded_at, reverses, reverse_reason \
         FROM journal_entries WHERE id = $1",
    )
    .bind(uuid)
    .fetch_one(pool)
    .await
    .expect("仕訳を読めること")
}
