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

use serde_json::{json, Value};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::PgPool;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// 起動（設定の検証 → 合成 → 接続 → 勘定科目マスタの投入）を待つ上限。
const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);

/// 1つの JSON-RPC 応答を待つ上限。
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// 実バイナリの在り処
// ---------------------------------------------------------------------------

/// `kaikei-mcp` の実行ファイル。
///
/// テスト実行ファイルは `<target>/<profile>/deps/` に置かれるので、その2つ上
/// が `cargo build` の成果物ディレクトリである（`CARGO_TARGET_DIR` を変えても
/// 自分の位置から辿るので追随する）。
fn server_binary() -> PathBuf {
    let test_exe = std::env::current_exe().expect("テスト実行ファイルの場所を取れること");
    let profile_dir = test_exe
        .parent()
        .and_then(Path::parent)
        .expect("<target>/<profile>/deps/ の2つ上を取れること");
    let binary = profile_dir.join(format!("kaikei-mcp{}", std::env::consts::EXE_SUFFIX));

    assert!(
        binary.is_file(),
        "kaikei-mcp の実行ファイルがありません: {}\n\
         この検査は**実バイナリ**を stdio で起動します。先に\n\
         \x20 cargo build -p kaikei-mcp\n\
         を実行してください（CI は .github/workflows/database.yml の\
         「kaikei-mcp のバイナリをビルド」ステップで行っています）。",
        binary.display()
    );
    assert_binary_is_not_stale(&binary);
    binary
}

/// 実行ファイルが `crates/kaikei-mcp/` の**どのファイルよりも新しい**こと。
///
/// これが無いと、`dispatch.rs` を書き換えた直後に
/// `cargo test -p kaikei-e2e --features pg-tests` だけを回した場合、
/// **古いバイナリに対して緑になる**。迂回を書いて落ちることを確かめる手順
/// （D-084 の実測欄）が、まさにこの形で嘘をつく。
///
/// 走査は `src/` に限定しない。3巡目の迂回（`#[path = "../probe.rs"]` /
/// `include!("probe.inc")`）は `src/` の外に置かれていた。
fn assert_binary_is_not_stale(binary: &Path) {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/ を取れること")
        .join("kaikei-mcp");
    let built_at = modified_at(binary);
    let (newest_source, newest_at) = newest_file(&crate_dir);

    assert!(
        built_at >= newest_at,
        "kaikei-mcp の実行ファイルがソースより古いままです。\n\
         \x20 実行ファイル: {binary}\n\
         \x20 より新しいソース: {source}\n\
         古いバイナリに対して緑になると、この検査は「監査ログが2行残る」ことを\
         何も保証しません。先に `cargo build -p kaikei-mcp` を実行してください。",
        binary = binary.display(),
        source = newest_source.display(),
    );
}

fn modified_at(path: &Path) -> SystemTime {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .unwrap_or_else(|e| panic!("{} の更新時刻を取れません: {e}", path.display()))
}

/// ディレクトリ配下で最も新しいファイルとその更新時刻。
fn newest_file(dir: &Path) -> (PathBuf, SystemTime) {
    let mut newest = (dir.to_path_buf(), modified_at(dir));
    for entry in
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{} を走査できません: {e}", dir.display()))
    {
        let path = entry.expect("ディレクトリ項目を読めること").path();
        let found = if path.is_dir() {
            // 生成物は見ない（自分自身より新しいのは当たり前）。
            if path.file_name().is_some_and(|name| name == "target") {
                continue;
            }
            newest_file(&path)
        } else {
            (path.clone(), modified_at(&path))
        };
        if found.1 > newest.1 {
            newest = found;
        }
    }
    newest
}

// ---------------------------------------------------------------------------
// 使い捨てDBを指す接続文字列
// ---------------------------------------------------------------------------

/// `#[sqlx::test]` が作った使い捨てDBを **`kaikei_app` ロール**で指す URL。
///
/// 本番と同じ形（`APP_DATABASE_URL` を環境変数で渡す）で子プロセスに渡す。
fn app_database_url(conn_opts: &PgConnectOptions) -> String {
    let password = std::env::var("KAIKEI_APP_PASSWORD").unwrap_or_else(|_| {
        panic!(
            "環境変数 KAIKEI_APP_PASSWORD が未設定です。\
             この検査は kaikei_app ロールでサーバーを起動します。\
             .env.example を参照して設定してください。"
        )
    });
    // 接続文字列に素で埋め込むので、URL の区切り文字を含む値は受け付けない
    // （黙って壊れた URL を渡すと「DB に接続できません」という無関係な
    // 起動失敗として現れ、原因に辿り着けない）。
    assert!(
        !password.contains([':', '@', '/', '?', '#', '%', '[', ']']),
        "KAIKEI_APP_PASSWORD に URL の区切り文字が含まれています。\
         このテストは接続文字列を組み立てて子プロセスに渡すため、\
         パーセントエンコードが要らない値にしてください"
    );
    format!(
        "postgres://kaikei_app:{password}@{host}:{port}/{database}",
        host = conn_opts.get_host(),
        port = conn_opts.get_port(),
        database = conn_opts.get_database().expect("テストDB名が取れること"),
    )
}

// ---------------------------------------------------------------------------
// stdio で喋る MCP クライアント（最小限）
// ---------------------------------------------------------------------------

/// 起動した `kaikei-mcp` プロセスと、その stdin/stdout。
struct McpServer {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    stdout: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    /// stderr は読み続ける（パイプが詰まると子プロセスが止まる）。
    /// 失敗時の診断に使う。
    stderr: Arc<Mutex<Vec<String>>>,
    next_id: i64,
}

impl McpServer {
    /// バイナリを起動し、`initialize` の折衝まで済ませる。
    async fn start(conn_opts: &PgConnectOptions) -> Self {
        let mut child = tokio::process::Command::new(server_binary())
            .env_clear()
            .env("APP_DATABASE_URL", app_database_url(conn_opts))
            .env("KAIKEI_BOOK_CURRENCY", "JPY")
            .env("KAIKEI_FISCAL_YEAR_RULE", "calendar_year")
            .env("KAIKEI_TAX_MODE", "exclusive")
            .env("KAIKEI_ROUNDING", "floor")
            .env("KAIKEI_ROUNDING_UNIT", "line")
            .env("KAIKEI_IS_TAXABLE_BUSINESS", "true")
            .env("KAIKEI_SIMPLIFIED_TAXATION", "false")
            .env("KAIKEI_CLOSING_ACCOUNT_CAPITAL", "400")
            .env("KAIKEI_CLOSING_ACCOUNT_OWNER_DRAWINGS", "410")
            .env("KAIKEI_CLOSING_ACCOUNT_OWNER_CONTRIBUTIONS", "420")
            .env("KAIKEI_CLOSING_TAX_CATEGORY", "NOT_APPLICABLE")
            // Windows ではシステム DLL の解決に SystemRoot が要る。
            .env(
                "SystemRoot",
                std::env::var("SystemRoot").unwrap_or_default(),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("kaikei-mcp を起動できること");

        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let child_stderr = child.stderr.take().expect("stderr");

        let stderr = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&stderr);
        tokio::spawn(async move {
            let mut lines = BufReader::new(child_stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                sink.lock().expect("stderr の記録").push(line);
            }
        });

        let mut server = McpServer {
            child,
            stdin,
            stdout: BufReader::new(stdout).lines(),
            stderr,
            next_id: 0,
        };
        server.initialize().await;
        server
    }

    /// `initialize` → `notifications/initialized`。
    ///
    /// 起動（合成ルート・勘定科目マスタの投入）を待つのはここなので、
    /// 応答待ちの上限を長めに取る。
    async fn initialize(&mut self) {
        let result = self
            .request_within(
                STARTUP_TIMEOUT,
                "initialize",
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "kaikei-e2e", "version": "0" }
                }),
            )
            .await;
        assert_eq!(
            result["serverInfo"]["name"],
            json!("kaikei-mcp"),
            "initialize の応答: {result}"
        );

        self.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .await;
    }

    /// `tools/call` を1回送り、**ツール結果**（`isError` を含む）を返す。
    async fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        self.request_within(
            RESPONSE_TIMEOUT,
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
        .await
    }

    async fn request_within(&mut self, timeout: Duration, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))
        .await;

        // 通知が混ざっても id で拾えるようにしておく。
        loop {
            let message = self.read_message(timeout, method).await;
            if message.get("id").and_then(Value::as_i64) != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                panic!(
                    "{method} が JSON-RPC のプロトコルエラーを返しました: {error}\n{}",
                    self.stderr_dump()
                );
            }
            return message
                .get("result")
                .cloned()
                .unwrap_or_else(|| panic!("{method} の応答に result がありません: {message}"));
        }
    }

    async fn send(&mut self, message: Value) {
        let line = format!("{message}\n");
        self.stdin
            .write_all(line.as_bytes())
            .await
            .unwrap_or_else(|e| panic!("要求を書き込めません: {e}\n{}", self.stderr_dump()));
        self.stdin
            .flush()
            .await
            .unwrap_or_else(|e| panic!("flush できません: {e}\n{}", self.stderr_dump()));
    }

    /// stdout の1行を読んで JSON にする。
    ///
    /// **stdout は JSON-RPC 専用チャネル**（`docs/07-mcp-server.md` §4）なので、
    /// 診断が1行でも混ざればここで JSON として解釈できずに落ちる。
    async fn read_message(&mut self, timeout: Duration, method: &str) -> Value {
        let line = tokio::time::timeout(timeout, self.stdout.next_line())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "{method} の応答が {timeout:?} 以内に返りませんでした。\n{}",
                    self.stderr_dump()
                )
            })
            .unwrap_or_else(|e| panic!("stdout を読めません: {e}\n{}", self.stderr_dump()))
            .unwrap_or_else(|| {
                panic!(
                    "stdout が閉じました（サーバーが落ちています）。\n{}",
                    self.stderr_dump()
                )
            });
        serde_json::from_str(&line).unwrap_or_else(|e| {
            panic!(
                "stdout の行が JSON-RPC ではありません（{e}）: {line}\n{}",
                self.stderr_dump()
            )
        })
    }

    fn stderr_dump(&self) -> String {
        let lines = self.stderr.lock().expect("stderr の記録");
        format!("---- サーバーの stderr ----\n{}", lines.join("\n"))
    }

    async fn shutdown(mut self) {
        // stdin を閉じると待受が終わる。落ちない場合に備えて kill も打つ。
        drop(self.stdin);
        let _ = tokio::time::timeout(Duration::from_secs(10), self.child.wait()).await;
        let _ = self.child.kill().await;
    }
}

fn is_error(result: &Value) -> bool {
    result["isError"] == json!(true)
}

fn body(result: &Value) -> &Value {
    result
        .get("structuredContent")
        .unwrap_or_else(|| panic!("structuredContent が無い: {result}"))
}

// ---------------------------------------------------------------------------
// 帳簿と監査ログの読み取り
// ---------------------------------------------------------------------------

#[derive(Debug, sqlx::FromRow)]
struct AuditRow {
    request_id: sqlx::types::Uuid,
    actor: String,
    tool: String,
    status: String,
    error_code: Option<String>,
    entry_id: Option<sqlx::types::Uuid>,
}

async fn audit_rows(pool: &PgPool) -> Vec<AuditRow> {
    sqlx::query_as::<_, AuditRow>(
        "SELECT request_id, actor, tool, status, error_code, entry_id FROM audit_log ORDER BY id",
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

/// 1回の `tools/call` にあたる2行（開始・結果）を検査する。
///
/// **ここが落ちるということは、`tools/call` が `dispatch::call` を通って
/// いないということである。** ツール名・識別子・ファイルの置き場所が何で
/// あろうと、監査ログに2行無ければ落ちる。
fn assert_audited_pair(rows: &[AuditRow], tool: &str, expected_status: &str) {
    assert_eq!(
        rows.len(),
        2,
        "1回の tools/call につき開始・結果の2行が残るはずです。\
         0行なら監査ログを通らない経路（別のルータ・別の ServerHandler・\
         走査の外に置いたファイル）から実行されています: {rows:?}"
    );
    assert_eq!(rows[0].request_id, rows[1].request_id);
    assert_eq!(rows[0].status, "started", "{rows:?}");
    assert_eq!(rows[1].status, expected_status, "{rows:?}");
    for row in rows {
        assert_eq!(row.tool, tool, "{rows:?}");
        assert_eq!(row.actor, "mcp", "{rows:?}");
    }
}

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

    server.shutdown().await;

    // 元仕訳・逆仕訳の2件が残る（元仕訳は書き換わらない。`CLAUDE.md` §2）。
    assert_eq!(journal_entry_count(&app).await, 2);

    // 3回の tools/call でそれぞれ2行、計6行。
    let rows = audit_rows(&app).await;
    assert_eq!(rows.len(), 6, "{rows:?}");
    assert_audited_pair(&rows[0..2], "post_journal_entry", "ok");
    assert_audited_pair(&rows[2..4], "reverse_journal_entry", "ok");
    assert_audited_pair(&rows[4..6], "reverse_journal_entry", "error");
    assert_eq!(
        rows[3].entry_id.map(|id| id.to_string()).as_deref(),
        Some(reversal_id.as_str())
    );
    assert_eq!(rows[5].error_code.as_deref(), Some("empty_reverse_reason"));
}
