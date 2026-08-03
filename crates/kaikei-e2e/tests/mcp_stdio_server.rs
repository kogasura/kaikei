//! ★実バイナリを stdio で動かし、`tools/call` を最後まで通す振る舞い検査★
//! （Phase 3 PR-F レビュー4巡目 A）。
//!
//! # なぜソース走査ではなくこれが本命なのか
//!
//! 「監査ログを通らない経路を書けない」ことを**ソースの形**で担保しようと
//! して、**3巡続けて走査の外側から破られた**（`DECISIONS.md` D-084 の
//! 訂正注記3 / 4巡目の注記）:
//!
//! | 巡 | 破り方 | 走査がそれを見なかった理由 |
//! |---|---|---|
//! | 1 | `ToolRouter::with_async_tool::<T>()` / `with_sync_tool` / タプル | 禁止識別子の一覧に無かった |
//! | 2 | `#[tool_handler]` の impl に `call_tool` を手書き | 同上（マクロが黙って生成を取り下げる） |
//! | 3 | `#[path = "../probe.rs"] mod probe;` / `include!("probe.inc")` | 走査が `src/**/*.rs` しか歩かなかった |
//!
//! 3巡目の再現では、監査ログを通らない別の `ServerHandler` を `main.rs` から
//! **実際に待ち受けさせた**状態で `cargo build` / `clippy -D warnings` /
//! `fmt --check` / `cargo test -p kaikei-mcp` が全緑だった。
//!
//! 走査は「ソースがどう書かれているか」しか見られないので、**書き方を変える
//! 迂回**に対して原理的に後手に回る（`rmcp` が API を増やすたび、レビュアーが
//! 1つ見落とすたびに穴が開く）。ここで見るのは書き方ではなく**振る舞い**で
//! ある——実際のバイナリに `tools/call` を1回送り、
//!
//! - `journal_entries` が期待どおりに増えている（あるいは増えていない）
//! - `audit_log` に `started` / 結果の**2行**が残っている
//!
//! ことを確かめる。識別子が何であれ、`#[path]` だろうと `include!` だろうと、
//! 別のルータだろうと別の `ServerHandler` だろうと、**監査ログが2行無ければ
//! 落ちる**。
//!
//! # なぜ `kaikei-e2e` に置くのか
//!
//! `kaikei-mcp` は `sqlx` に依存しない（`docs/07-mcp-server.md` §10 MC-30 の
//! 許可リスト）ため、使い捨てDBを作ることも `audit_log` を SELECT すること
//! もできない。両方を持ち、かつ `kaikei-mcp` に依存してよいのはこの crate
//! だけである（`tests/mcp_write_tools.rs` と同じ理由）。
//!
//! # `tests/mcp_write_tools.rs` との違い
//!
//! | | 通る経路 |
//! |---|---|
//! | `mcp_write_tools.rs` | `dispatch::call::<T>(runtime, args)` を**直接**呼ぶ（ルータも `call_tool` も `serve_stdio` も通らない） |
//! | **このファイル** | 実バイナリ → `main.rs` → `serve_stdio` → `ServerHandler::call_tool` → ルータ → `dispatch::call` |
//!
//! あちらはツールの応答本文（`hint` / `policy_notes` / 金額の文字列化）を
//! 細かく見る場所であり、こちらは**プロトコルの入口から監査ログまでが1本に
//! 繋がっていること**だけを見る場所である。両方が要る。
//!
//! # このテストは `kaikei-mcp` のバイナリが**ビルド済み**であることを要求する
//!
//! `CARGO_BIN_EXE_<name>` は同じ package のテストにしか渡らないので、
//! ここからは使えない。`cargo` を入れ子で起動するのは target ディレクトリの
//! ロックで詰まる恐れがあるため、**先に `cargo build -p kaikei-mcp` して
//! おく**運用にし、バイナリが無い場合・**ソースより古い場合**は
//! （黙って通さず）その旨を書いて落ちる。
//! CI では `.github/workflows/database.yml` がこのテストの直前でビルドする。

#![cfg(feature = "pg-tests")]

mod common;

// ハーネス（実バイナリの起動・stdio の読み書き・`audit_log` の読み取り）は
// `tests/common/mcp_stdio.rs` に置いてある。通し E2E（`mcp_walkthrough.rs`）
// と共有する——**必須の環境変数12個**や「実行ファイルがソースより古くない
// こと」は、複製すると片方だけが腐って検査が黙って無意味になる性質の実装で
// ある（同ファイルの doc）。
use common::mcp_stdio::{
    assert_audited_pair, audit_rows, audited_calls, body, is_error, journal_entry_count, McpServer,
};
use serde_json::json;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

// ---------------------------------------------------------------------------
// 検査
// ---------------------------------------------------------------------------

/// ★本命★ 実バイナリに `tools/call post_journal_entry` を1回送ると、
/// 帳簿に1件・`audit_log` に `started` / `ok` の2行が残る。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_tools_call_through_the_real_binary_posts_one_entry_and_leaves_two_audit_rows(
    _pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts.clone()).await;
    let mut server = McpServer::start(&conn_opts).await;

    let result = server
        .call_tool(
            "post_journal_entry",
            json!({
                "entry_date": "2026-04-15",
                "description": "A社への請求",
                "lines": [
                    { "account": "135", "side": "debit",  "amount": "110000" },
                    { "account": "500", "side": "credit", "amount": "100000",
                      "tags": { "tax_category": "SALES_10" } }
                ],
                "auto_tax_lines": true
            }),
        )
        .await;

    assert!(!is_error(&result), "記帳が失敗しました: {result}");
    let entry_id = body(&result)["entry_id"]
        .as_str()
        .unwrap_or_else(|| panic!("entry_id が無い: {result}"))
        .to_string();

    server.shutdown().await;

    assert_eq!(journal_entry_count(&app).await, 1);
    let rows = audit_rows(&app).await;
    assert_audited_pair(&rows, "post_journal_entry", "ok");
    assert_eq!(
        rows[1].entry_id.map(|id| id.to_string()).as_deref(),
        Some(entry_id.as_str()),
        "結果レコードの entry_id が応答の仕訳IDと一致しません"
    );
    assert!(rows[1].error_code.is_none(), "{rows:?}");
}

/// ★失敗系★ 貸借不一致は `isError: true` で返り、帳簿は0件のまま、
/// `audit_log` には `started` / `error` の2行が残る（D-077 の核心）。
///
/// 成功系だけを見ていると「失敗したときだけ監査ログを書かない」迂回が
/// 通ってしまう。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_failing_tools_call_through_the_real_binary_still_leaves_two_audit_rows(
    _pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts.clone()).await;
    let mut server = McpServer::start(&conn_opts).await;

    let result = server
        .call_tool(
            "post_journal_entry",
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

    // ドメインのエラーはプロトコルエラーにしない（D-071）。
    assert!(is_error(&result), "{result}");
    assert_eq!(body(&result)["error"], json!("unbalanced"), "{result}");

    server.shutdown().await;

    assert_eq!(journal_entry_count(&app).await, 0, "帳簿が変わっています");
    let rows = audit_rows(&app).await;
    assert_audited_pair(&rows, "post_journal_entry", "error");
    assert_eq!(rows[1].error_code.as_deref(), Some("unbalanced"));
    assert!(rows[1].entry_id.is_none(), "{rows:?}");
}

/// `reverse_journal_entry` も同じ経路を通る（ツールごとに迂回できない）。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn reversing_through_the_real_binary_goes_through_the_same_audited_path(
    _pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts.clone()).await;
    let mut server = McpServer::start(&conn_opts).await;

    let posted = server
        .call_tool(
            "post_journal_entry",
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
    let original_id = body(&posted)["entry_id"]
        .as_str()
        .expect("entry_id")
        .to_string();

    // ★訂正前★ この時点では誰も訂正していない（キーごと出ない）。
    let before = server
        .call_tool("get_entry", json!({ "entry_id": original_id }))
        .await;
    assert!(!is_error(&before), "{before}");
    assert!(body(&before).get("reversed_by").is_none(), "{before}");

    let reversed = server
        .call_tool(
            "reverse_journal_entry",
            json!({
                "original_id": original_id,
                "reverse_date": "2026-05-01",
                "reason": "請求金額の誤り（税率の適用誤り）"
            }),
        )
        .await;
    assert!(!is_error(&reversed), "{reversed}");
    assert_eq!(body(&reversed)["reverses"], json!(original_id));
    let reversal_id = body(&reversed)["entry_id"]
        .as_str()
        .expect("entry_id")
        .to_string();

    // 失敗する逆仕訳（空白のみの訂正理由）も2行残る。
    let refused = server
        .call_tool(
            "reverse_journal_entry",
            json!({
                "original_id": original_id,
                "reverse_date": "2026-05-02",
                "reason": "   "
            }),
        )
        .await;
    assert!(is_error(&refused), "{refused}");
    assert_eq!(body(&refused)["error"], json!("empty_reverse_reason"));

    // ★訂正後★（PR-G レビュー B）
    //
    // 帳簿は追記のみなので原仕訳そのものは1バイトも変わらない。それでも
    // **「既に訂正済みである」ことは応答から読めなければならない**
    // （`CLAUDE.md` §2 の訂正履歴。`get_trial_balance` では残高が0になって
    // いるのに `get_entry` では生きているように見える、という状態にしない）。
    let after = server
        .call_tool("get_entry", json!({ "entry_id": original_id }))
        .await;
    assert!(!is_error(&after), "{after}");
    assert_ne!(
        body(&before),
        body(&after),
        "訂正済みの仕訳が未訂正のときと同じ応答です: {after}"
    );
    assert_eq!(body(&after)["reversed_by"], json!(reversal_id), "{after}");
    assert_eq!(body(&after)["reversed_by_entry_no"], json!(2), "{after}");
    // 向きを取り違えていない（原仕訳は誰も訂正していない）。
    assert!(body(&after).get("reverses").is_none(), "{after}");

    // 逆仕訳の側は逆向きの関係（`reverses`）を持ち、誰にも訂正されていない。
    let reversal_entry = server
        .call_tool("get_entry", json!({ "entry_id": reversal_id }))
        .await;
    assert!(!is_error(&reversal_entry), "{reversal_entry}");
    assert_eq!(body(&reversal_entry)["reverses"], json!(original_id));
    assert!(
        body(&reversal_entry).get("reversed_by").is_none(),
        "{reversal_entry}"
    );

    server.shutdown().await;

    // 元仕訳・逆仕訳の2件が残る（元仕訳は書き換わらない。`CLAUDE.md` §2）。
    assert_eq!(journal_entry_count(&app).await, 2);

    // 6回の tools/call でそれぞれ2行、計12行。
    let rows = audit_rows(&app).await;
    assert_eq!(rows.len(), 12, "{rows:?}");
    assert_audited_pair(&rows[0..2], "post_journal_entry", "ok");
    assert_audited_pair(&rows[2..4], "get_entry", "ok");
    assert_audited_pair(&rows[4..6], "reverse_journal_entry", "ok");
    assert_audited_pair(&rows[6..8], "reverse_journal_entry", "error");
    assert_audited_pair(&rows[8..10], "get_entry", "ok");
    assert_audited_pair(&rows[10..12], "get_entry", "ok");
    assert_eq!(
        rows[5].entry_id.map(|id| id.to_string()).as_deref(),
        Some(reversal_id.as_str())
    );
    assert_eq!(rows[7].error_code.as_deref(), Some("empty_reverse_reason"));
}

// ---------------------------------------------------------------------------
// 読み取り系・提案系（Phase 3 PR-G）
// ---------------------------------------------------------------------------

/// ★PR-G の本命★ 読み取り系・提案系7件を実バイナリに通し、
/// **応答の中身**と**監査ログが1呼び出しにつき2行残ること**を同時に見る。
///
/// 帳簿に1件記帳してから読む（0件のときだけ通る実装になっていないこと）。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn the_read_tools_answer_through_the_real_binary_and_are_audited(
    _pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts.clone()).await;
    let mut server = McpServer::start(&conn_opts).await;

    let posted = server
        .call_tool(
            "post_journal_entry",
            json!({
                "entry_date": "2026-04-15",
                "description": "A社への請求",
                "lines": [
                    { "account": "135", "side": "debit",  "amount": "110000" },
                    { "account": "500", "side": "credit", "amount": "100000",
                      "tags": { "tax_category": "SALES_10" } }
                ],
                "auto_tax_lines": true
            }),
        )
        .await;
    assert!(!is_error(&posted), "{posted}");
    let entry_id = body(&posted)["entry_id"]
        .as_str()
        .expect("entry_id")
        .to_string();

    // ---- list_accounts ----
    let accounts = server.call_tool("list_accounts", json!({})).await;
    assert!(!is_error(&accounts), "{accounts}");
    let listed = body(&accounts)["accounts"]
        .as_array()
        .expect("配列")
        .clone();
    assert!(
        !listed.is_empty(),
        "起動時に投入した科目が1件も無い: {accounts}"
    );
    let posted_account = listed
        .iter()
        .find(|account| account["account"] == json!("135"))
        .unwrap_or_else(|| panic!("記帳に使った科目が一覧に無い: {accounts}"));
    // MC-13: 種別と記帳可否を必ず返す。
    assert_eq!(posted_account["account_type"], json!("asset"), "{accounts}");
    assert_eq!(posted_account["postable"], json!(true), "{accounts}");
    // **全件が `postable` を持つ**（記帳できない科目に当たって初めて分かる、
    // という形にしない）。
    //
    // 同梱テンプレートの科目は現時点で全て記帳可能なので、ここで
    // 「`postable: false` の科目が居ること」は確かめられない。見出し科目を
    // 含む場合の絞り込みは `kaikei-mcp` 側の単体検査
    // （`list_accounts.rs` の `postable_only_hides_the_headings_...`）が持つ。
    for account in &listed {
        assert!(account["postable"].is_boolean(), "{account}");
        assert!(account["account_type"].is_string(), "{account}");
    }

    // ---- get_entry ----
    let entry = server
        .call_tool("get_entry", json!({ "entry_id": entry_id }))
        .await;
    assert!(!is_error(&entry), "{entry}");
    assert_eq!(body(&entry)["entry_id"], json!(entry_id));
    assert_eq!(body(&entry)["description"], json!("A社への請求"));
    // 税額行が自動生成されているので明細は3行。
    assert_eq!(
        body(&entry)["lines"].as_array().unwrap().len(),
        3,
        "{entry}"
    );
    // MC-27: 金額は文字列。
    assert_eq!(body(&entry)["debit_total"], json!("110000"));
    assert_eq!(body(&entry)["credit_total"], json!("110000"));
    // 逆仕訳ではないのでキーごと出ない。
    assert!(body(&entry).get("reverses").is_none(), "{entry}");

    // ---- get_trial_balance ----
    let trial_balance = server
        .call_tool(
            "get_trial_balance",
            json!({ "from": "2026-01-01", "to": "2026-12-31" }),
        )
        .await;
    assert!(!is_error(&trial_balance), "{trial_balance}");
    let tb = body(&trial_balance);
    assert_eq!(tb["currency"], json!("JPY"));
    assert_eq!(tb["debit_total"], json!("110000"));
    assert_eq!(tb["credit_total"], json!("110000"));
    let rows = tb["rows"].as_array().expect("配列");
    assert_eq!(rows.len(), 3, "{trial_balance}");
    let sales = rows
        .iter()
        .find(|row| row["account"] == json!("500"))
        .unwrap_or_else(|| panic!("売上の行が無い: {trial_balance}"));
    assert_eq!(sales["account_type"], json!("revenue"));
    assert_eq!(sales["credit_total"], json!("100000"));
    assert_eq!(sales["balance"], json!("100000"));
    assert_eq!(sales["group"], json!({}), "group_by 未指定なら空");

    // ---- get_trial_balance（group_by が効く）----
    let grouped = server
        .call_tool(
            "get_trial_balance",
            json!({
                "from": "2026-01-01",
                "to": "2026-12-31",
                "group_by": ["tax_category"]
            }),
        )
        .await;
    assert!(!is_error(&grouped), "{grouped}");
    assert!(
        body(&grouped)["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["group"]["tax_category"] == json!("SALES_10")),
        "group_by が効いていない: {grouped}"
    );

    // ---- list_tax_categories ----
    let categories = server
        .call_tool("list_tax_categories", json!({ "date": "2026-04-15" }))
        .await;
    assert!(!is_error(&categories), "{categories}");
    let listed = body(&categories)["categories"].as_array().expect("配列");
    assert!(
        listed
            .iter()
            .any(|category| category["code"] == json!("SALES_10")),
        "記帳に使った区分が一覧に無い: {categories}"
    );
    assert!(
        body(&categories)["table"]["range"].is_string(),
        "{categories}"
    );

    // ---- get_settings ----
    let settings = server.call_tool("get_settings", json!({})).await;
    assert!(!is_error(&settings), "{settings}");
    let s = body(&settings);
    // 起動時に環境変数で渡した設定がそのまま返る（既定値に落ちていない）。
    assert_eq!(s["tax_mode"], json!("exclusive"));
    assert_eq!(s["rounding"], json!("floor"));
    assert_eq!(s["rounding_unit"], json!("line"));
    assert_eq!(s["is_taxable_business"], json!(true));
    assert_eq!(s["simplified_taxation"], json!(false));
    assert_eq!(s["fiscal_year_rule"], json!("calendar_year"));
    assert_eq!(s["book_currency"]["code"], json!("JPY"));
    // テンプレートどおりに投入した直後なので食い違いは無い（キーは必ず出る）。
    // **起動時点の観測であることが応答に残る**（PR-G レビュー C-3）。
    assert_eq!(s["chart_differences"]["items"], json!([]), "{settings}");
    assert_eq!(
        s["chart_differences"]["as_of"],
        json!("startup"),
        "{settings}"
    );

    // ---- suggest_tax_category ----
    let suggested = server
        .call_tool(
            "suggest_tax_category",
            json!({ "date": "2026-04-15", "direction": "sales" }),
        )
        .await;
    assert!(!is_error(&suggested), "{suggested}");
    // 帳簿の設定が根拠として並ぶ（PR-G レビュー C-1）。起動時に環境変数で
    // 渡した値がそのまま出る（`get_settings` と同じ値）。
    let filtered_by = &body(&suggested)["filtered_by"];
    assert_eq!(filtered_by["tax_mode"], json!("exclusive"), "{suggested}");
    assert_eq!(
        filtered_by["is_taxable_business"],
        json!(true),
        "{suggested}"
    );
    assert_eq!(
        filtered_by["simplified_taxation"],
        json!(false),
        "{suggested}"
    );
    assert_eq!(
        filtered_by["book_settings_used_for_filtering"],
        json!(false),
        "{suggested}"
    );
    let candidates = body(&suggested)["candidates"].as_array().expect("配列");
    assert!(
        candidates.len() > 1,
        "候補が絞り込まれています: {suggested}"
    );
    for candidate in candidates {
        assert_eq!(candidate["direction"], json!("sales"), "{candidate}");
        // MC-08 (1): 根拠が空でない。
        assert!(
            !candidate["reason"]
                .as_str()
                .expect("reason")
                .trim()
                .is_empty(),
            "{candidate}"
        );
    }

    // ---- validate_invoice_number ----
    let invoice = server
        .call_tool(
            "validate_invoice_number",
            json!({ "registration_number": "T7123456789012" }),
        )
        .await;
    assert!(!is_error(&invoice), "{invoice}");
    assert_eq!(body(&invoice)["format_valid"], json!(true));
    // MC-28: 実在すると断定しない。
    assert!(
        !body(&invoice)["not_checked"]
            .as_array()
            .expect("配列")
            .is_empty(),
        "{invoice}"
    );

    server.shutdown().await;

    // ★MC-08 (2)★ 提案系・読み取り系は帳簿を1行も変えない。
    assert_eq!(journal_entry_count(&app).await, 1, "帳簿が変わっています");

    // ★MC-11★ 1回の呼び出しにつき2行。読み取り系も同じ経路を通る。
    let calls = audited_calls(&audit_rows(&app).await);
    assert_eq!(
        calls,
        vec![
            ("post_journal_entry".to_string(), "ok".to_string()),
            ("list_accounts".to_string(), "ok".to_string()),
            ("get_entry".to_string(), "ok".to_string()),
            ("get_trial_balance".to_string(), "ok".to_string()),
            ("get_trial_balance".to_string(), "ok".to_string()),
            ("list_tax_categories".to_string(), "ok".to_string()),
            ("get_settings".to_string(), "ok".to_string()),
            ("suggest_tax_category".to_string(), "ok".to_string()),
            ("validate_invoice_number".to_string(), "ok".to_string()),
        ],
    );
}

/// ★空の結果と「見つからない」を区別する★（PR-G）
///
/// 読み取り系で最も危ういのは、**入力の誤りを「0件」として静かに成功させる**
/// ことである（`docs/07-mcp-server.md` §2 / §3。`from > to` を空の試算表に
/// しない、という要件がその代表）。ここでは
///
/// | 呼び出し | 期待 |
/// |---|---|
/// | 仕訳が1件も無い期間の試算表 | **成功**（`rows: []`。通貨と合計 `"0"` は返る） |
/// | 開始日が終了日より後 | **エラー**（`rejected`） |
/// | 未登録のタグキーで `group_by` | **エラー**（`unknown_tag_key`。「aggregatable = false」とは言わない） |
/// | 登録済みだが集計軸に使えないタグキーで `group_by` | **エラー**（`not_aggregatable`。上と別コード） |
/// | 存在しない仕訳ID | **エラー**（`not_found`。UUID の正準表記を含む） |
/// | 仕訳IDが UUID ですらない | **エラー**（`invalid_entry_id`。`not_found` と区別する） |
/// | 同梱していない日付の税区分 | **エラー**（有効期間を示す。空配列にしない） |
///
/// を1本で見る。失敗した呼び出しも `audit_log` に2行残る（D-070）。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn the_read_tools_tell_an_empty_result_apart_from_a_bad_request(
    _pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts.clone()).await;
    let mut server = McpServer::start(&conn_opts).await;

    // 仕訳が1件も無い期間は**成功**（空の試算表）。
    let empty = server
        .call_tool(
            "get_trial_balance",
            json!({ "from": "2026-01-01", "to": "2026-12-31" }),
        )
        .await;
    assert!(!is_error(&empty), "{empty}");
    assert_eq!(body(&empty)["rows"], json!([]));
    assert_eq!(
        body(&empty)["currency"],
        json!("JPY"),
        "0行でも通貨を名乗る"
    );
    assert_eq!(body(&empty)["debit_total"], json!("0"));

    // 開始日 > 終了日 は**エラー**（0件の空の試算表として成功させない）。
    let reversed_period = server
        .call_tool(
            "get_trial_balance",
            json!({ "from": "2026-12-31", "to": "2026-01-01" }),
        )
        .await;
    assert!(is_error(&reversed_period), "{reversed_period}");
    assert_eq!(body(&reversed_period)["error"], json!("rejected"));
    let message = body(&reversed_period)["message"].as_str().unwrap();
    assert!(message.contains("2026-12-31"), "{message}");

    // ★未登録のキーと「集計軸に使えないキー」を区別する★（PR-G レビュー C-2）
    //
    // `TagSchema::is_aggregatable` はどちらにも `false` を返すので、素直に
    // 流すと未登録のキーにも「（aggregatable = false）」という成立していない
    // 事実が返る。どちらの応答にも選べるキーの一覧が付く（§11）。
    let unregistered = server
        .call_tool(
            "get_trial_balance",
            json!({ "from": "2026-01-01", "to": "2026-12-31", "group_by": ["memo"] }),
        )
        .await;
    assert!(is_error(&unregistered), "{unregistered}");
    assert_eq!(body(&unregistered)["error"], json!("unknown_tag_key"));
    let message = body(&unregistered)["message"].as_str().unwrap();
    assert!(message.contains("登録されていません"), "{message}");
    assert!(
        !message.contains("aggregatable = false"),
        "未登録のキーに成立していない事実を述べています: {message}"
    );
    assert_eq!(
        body(&unregistered)["aggregatable_group_by_keys"],
        json!(["counterparty", "project", "tax_category"]),
        "{unregistered}"
    );

    let not_aggregatable = server
        .call_tool(
            "get_trial_balance",
            json!({ "from": "2026-01-01", "to": "2026-12-31", "group_by": ["business_ratio"] }),
        )
        .await;
    assert!(is_error(&not_aggregatable), "{not_aggregatable}");
    assert_eq!(
        body(&not_aggregatable)["error"],
        json!("not_aggregatable"),
        "登録済みのキーが未登録扱いになっています: {not_aggregatable}"
    );
    assert_eq!(
        body(&not_aggregatable)["aggregatable_group_by_keys"],
        json!(["counterparty", "project", "tax_category"]),
        "{not_aggregatable}"
    );

    // 存在しない仕訳IDは**見つからない**（空の成功にしない）。
    let missing = server
        .call_tool(
            "get_entry",
            json!({ "entry_id": "0192a7b3-1234-7abc-8def-0123456789ab" }),
        )
        .await;
    assert!(is_error(&missing), "{missing}");
    assert_eq!(body(&missing)["error"], json!("not_found"));
    assert!(
        body(&missing)["message"]
            .as_str()
            .unwrap()
            .contains("0192a7b3-1234-7abc-8def-0123456789ab"),
        "{missing}"
    );

    // 「IDが UUID ですらない」は `not_found` と混同しない（次の手が違う）。
    let malformed = server
        .call_tool("get_entry", json!({ "entry_id": "42" }))
        .await;
    assert!(is_error(&malformed), "{malformed}");
    assert_eq!(body(&malformed)["error"], json!("invalid_entry_id"));

    // 同梱していない日付の税区分は**空配列ではなくエラー**。
    let out_of_range = server
        .call_tool("list_tax_categories", json!({ "date": "2000-01-01" }))
        .await;
    assert!(is_error(&out_of_range), "{out_of_range}");
    assert_eq!(
        body(&out_of_range)["error"],
        json!("no_applicable_rule_set")
    );
    assert!(
        body(&out_of_range)["message"]
            .as_str()
            .unwrap()
            .contains("2026"),
        "有効期間が本文に無い: {out_of_range}"
    );

    // 記帳可能な科目だけに絞れる（絞ったことが応答に残る）。
    let postable = server
        .call_tool("list_accounts", json!({ "postable_only": true }))
        .await;
    assert!(!is_error(&postable), "{postable}");
    assert_eq!(body(&postable)["postable_only"], json!(true));
    assert!(body(&postable)["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .all(|account| account["postable"] == json!(true)));

    // 形式が不正な登録番号は、最初に失敗した観点だけを返す。
    let invalid_invoice = server
        .call_tool(
            "validate_invoice_number",
            json!({ "registration_number": " T7123456789012" }),
        )
        .await;
    assert!(is_error(&invalid_invoice), "{invalid_invoice}");
    assert_eq!(
        body(&invalid_invoice)["error"],
        json!("invoice_reg_no_missing_prefix"),
        "前後の空白をトリムしていないか: {invalid_invoice}"
    );

    server.shutdown().await;

    // 帳簿は1行も動いていない（読み取りと検証しか呼んでいない）。
    assert_eq!(journal_entry_count(&app).await, 0);

    // 失敗した呼び出しも2行残る（D-070。「AI が何をしようとしたか」）。
    let calls = audited_calls(&audit_rows(&app).await);
    assert_eq!(
        calls,
        vec![
            ("get_trial_balance".to_string(), "ok".to_string()),
            ("get_trial_balance".to_string(), "error".to_string()),
            ("get_trial_balance".to_string(), "error".to_string()),
            ("get_trial_balance".to_string(), "error".to_string()),
            ("get_entry".to_string(), "error".to_string()),
            ("get_entry".to_string(), "error".to_string()),
            ("list_tax_categories".to_string(), "error".to_string()),
            ("list_accounts".to_string(), "ok".to_string()),
            ("validate_invoice_number".to_string(), "error".to_string()),
        ],
    );
}

/// `tools/call` **以外**のプロトコル入口が生えていない。
///
/// 上の3本は `tools/call` を通る操作しか見ないので、`ServerHandler` の
/// 既定実装を持つ別のメソッド（`read_resource` / `get_prompt` / `complete`
/// / タスク系）を `dispatch.rs` に足すと、監査ログを通らない書き込み経路に
/// なる。許可リストの**内側**なので走査にも映らない
/// （`DECISIONS.md` D-084 の穴の列挙表）。
///
/// rmcp 3.1 の `handle_request` はこれらを capability ゲート無しで
/// ハンドラへ配るため、**宣言していない capability でも送れば届く**。
///
/// # 見るのは「返り方」ではなく「帳簿が動かないこと」
///
/// 既定実装の返し方は入口ごとに違う（`resources/read` は
/// `-32601 method not found`、`completion/complete` は**空の成功**）。
/// そこを固定すると rmcp の実装都合に縛られるだけで、守りたいものが
/// 守れない。**守りたいのは「`tools/call` 以外から帳簿が動かない」こと**
/// なので、返り方は問わず帳簿と `audit_log` を見る。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn no_protocol_entry_point_other_than_tools_call_touches_the_ledger(
    _pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    // rmcp 3.1 の `ServerHandler` で既定実装を持ち、かつ引数を受け取る入口。
    // 増えたらここに足す（`dispatch.rs` に手書きした瞬間に落ちる）。
    let entry_points = [
        ("resources/read", json!({ "uri": "kaikei://probe" })),
        ("prompts/get", json!({ "name": "probe" })),
        (
            "completion/complete",
            json!({
                "ref": { "type": "ref/prompt", "name": "probe" },
                "argument": { "name": "probe", "value": "" }
            }),
        ),
        ("resources/subscribe", json!({ "uri": "kaikei://probe" })),
    ];

    let app = common::app_pool(conn_opts.clone()).await;
    let mut server = McpServer::start(&conn_opts).await;

    for (method, params) in entry_points {
        // 応答でもエラーでもよい。落ちずに返ってくることだけ確かめる。
        let _ = server.raw_request(method, params).await;
    }

    server.shutdown().await;

    assert_eq!(
        journal_entry_count(&app).await,
        0,
        "tools/call 以外のプロトコル入口から記帳されています。\
         dispatch.rs に ServerHandler のメソッドを足したなら、\
         そこは with_audit を通っていません（DECISIONS.md D-084 の穴の列挙表）"
    );
    assert!(
        audit_rows(&app).await.is_empty(),
        "tools/call を1回も送っていないのに audit_log に行があります"
    );
}

/// ★読み取り系も同じ経路を通る★（Phase 3 PR-H。MC-11 / MC-16 / MC-17）
///
/// `search_entries` と `get_ledger` を実バイナリに送り、
///
/// - 記帳した仕訳が**プロトコルの入口から**引けること
/// - **帳簿は1件も増えないのに** `audit_log` には呼び出しごとに2行残ること
///
/// を見る。読み取り系は帳簿を変えないので、監査ログを書かない迂回を
/// 作っても正常系のテストでは気づけない（`ROADMAP.md` Phase 3 の完了条件
/// 「**全操作**が audit_log に記録される」）。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn reading_through_the_real_binary_leaves_two_audit_rows_without_touching_the_ledger(
    _pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts.clone()).await;
    let mut server = McpServer::start(&conn_opts).await;

    let posted = server
        .call_tool(
            "post_journal_entry",
            json!({
                "entry_date": "2026-04-15",
                "description": "A社への請求",
                "lines": [
                    { "account": "135", "side": "debit",  "amount": "110000" },
                    { "account": "500", "side": "credit", "amount": "100000",
                      "tags": { "tax_category": "SALES_10" } }
                ],
                "auto_tax_lines": true
            }),
        )
        .await;
    assert!(!is_error(&posted), "{posted}");
    let entry_id = body(&posted)["entry_id"]
        .as_str()
        .expect("entry_id")
        .to_string();

    // 記帳した仕訳が検索で引ける。
    let found = server
        .call_tool("search_entries", json!({ "description": "A社" }))
        .await;
    assert!(!is_error(&found), "{found}");
    let page = body(&found);
    assert_eq!(page["total_matches"], json!(1), "{page}");
    assert_eq!(page["entries"][0]["entry_id"], json!(entry_id));
    // 金額は文字列（§5）。切れていないことも応答から分かる。
    assert!(page["entries"][0]["lines"][0]["amount"].is_string());
    assert_eq!(page["has_more"], json!(false));

    // 同じ仕訳が元帳にも出る（自動生成された仮受消費税の行を含む）。
    let ledger = server
        .call_tool(
            "get_ledger",
            json!({ "account": "500", "from": "2026-01-01", "to": "2026-12-31" }),
        )
        .await;
    assert!(!is_error(&ledger), "{ledger}");
    let page = body(&ledger);
    assert_eq!(page["account_type"], json!("revenue"));
    assert_eq!(page["credit_total"], json!("100000"));
    assert_eq!(page["closing_balance"], json!("100000"));
    assert_eq!(page["total_lines"], json!(1));
    assert_eq!(page["rows"][0]["entry_id"], json!(entry_id));

    server.shutdown().await;

    // 帳簿は記帳の1件だけ（読み取りで増えも減りもしない）。
    assert_eq!(journal_entry_count(&app).await, 1);

    // 3回の tools/call でそれぞれ2行、計6行。
    let rows = audit_rows(&app).await;
    assert_eq!(rows.len(), 6, "{rows:?}");
    assert_audited_pair(&rows[0..2], "post_journal_entry", "ok");
    assert_audited_pair(&rows[2..4], "search_entries", "ok");
    assert_audited_pair(&rows[4..6], "get_ledger", "ok");
    // 読み取り系は仕訳を作らないので `entry_id` は入らない。
    assert!(rows[3].entry_id.is_none(), "{rows:?}");
    assert!(rows[5].entry_id.is_none(), "{rows:?}");
}

/// 読み取り系の**失敗**もプロトコルエラーにせず、監査ログに2行残す
/// （D-071 / MC-26）。
///
/// 勘定科目マスタに無い科目コードで元帳を引くと `not_found` になる
/// （空の元帳を返さない）。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn a_failing_read_through_the_real_binary_is_a_tool_error_with_two_audit_rows(
    _pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts.clone()).await;
    let mut server = McpServer::start(&conn_opts).await;

    let result = server
        .call_tool(
            "get_ledger",
            json!({ "account": "99999", "from": "2026-01-01", "to": "2026-12-31" }),
        )
        .await;

    // ドメインのエラーはプロトコルエラーにしない（`request_within` は
    // JSON-RPC の error が返ると panic するので、ここに来た時点で
    // ツール結果エラーである）。
    assert!(is_error(&result), "{result}");
    assert_eq!(body(&result)["error"], json!("not_found"), "{result}");

    server.shutdown().await;

    assert_eq!(journal_entry_count(&app).await, 0);
    let rows = audit_rows(&app).await;
    assert_audited_pair(&rows, "get_ledger", "error");
    assert_eq!(rows[1].error_code.as_deref(), Some("not_found"));
}
