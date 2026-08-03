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

    /// 応答でもエラーでも、**返ってきたメッセージをそのまま**返す。
    ///
    /// [`request_within`] は `error` が返ると panic するので、
    /// 「この入口が何を返すか分からない」検査には使えない。
    ///
    /// [`request_within`]: McpServer::request_within
    async fn raw_request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))
        .await;

        loop {
            let message = self.read_message(RESPONSE_TIMEOUT, method).await;
            if message.get("id").and_then(Value::as_i64) == Some(id) {
                return message;
            }
        }
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

// ---------------------------------------------------------------------------
// 読み取り系・提案系（Phase 3 PR-G）
// ---------------------------------------------------------------------------

/// `audit_log` を「1回の呼び出し＝2行」の組に切って、ツール名の並びを返す。
///
/// 読み取り系も**同じ経路（`dispatch::call`）を通る**ので、呼び出した順に
/// `started` / `ok`（または `error`）の対が並ぶ（`docs/07-mcp-server.md` §9。
/// MC-11 の「全11ツールに対して総当たり」の PR-G ぶん）。
fn audited_calls(rows: &[AuditRow]) -> Vec<(String, String)> {
    assert_eq!(
        rows.len() % 2,
        0,
        "監査ログの行数が奇数です（開始と結果の対になっていない）: {rows:?}"
    );
    rows.chunks(2)
        .map(|pair| {
            assert_eq!(pair[0].request_id, pair[1].request_id, "{pair:?}");
            assert_eq!(pair[0].status, "started", "{pair:?}");
            assert_eq!(pair[0].tool, pair[1].tool, "{pair:?}");
            assert_eq!(pair[0].actor, "mcp", "{pair:?}");
            (pair[0].tool.clone(), pair[1].status.clone())
        })
        .collect()
}

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
    assert_eq!(s["chart_differences"], json!([]), "{settings}");

    // ---- suggest_tax_category ----
    let suggested = server
        .call_tool(
            "suggest_tax_category",
            json!({ "date": "2026-04-15", "direction": "sales" }),
        )
        .await;
    assert!(!is_error(&suggested), "{suggested}");
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
