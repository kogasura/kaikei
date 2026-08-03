//! **実バイナリを stdio で動かす**ための最小限の MCP クライアント。
//!
//! Phase 3 PR-F レビュー4巡目が `tests/mcp_stdio_server.rs` に書いたものを、
//! PR-I で通し E2E（`tests/mcp_walkthrough.rs`）と共有するためにここへ移した。
//!
//! # なぜ複製せずに共有するのか
//!
//! `tests/*.rs` は1本ずつ別のコンパイル単位になるので、複製すれば済む。
//! しかしこのハーネスが持っているのは**単なる補助関数ではない**——
//!
//! - 起動時に渡す12個の環境変数（1つでも欠ければサーバは起動しない。§7）
//! - stdout の行を JSON として読む（**stdout は JSON-RPC 専用チャネル**。
//!   診断が1行でも混ざればここで落ちる。§4）
//! - 実行ファイルが**ソースより古くないこと**の確認
//!
//! という、**壊れると検査が黙って無意味になる**性質の実装である。
//! 複製すると、片方だけを直したときに「古いバイナリに対して緑」や
//! 「必須設定を1つ落としたまま起動している」が片側で復活する。
//!
//! `crates/kaikei-e2e/tests/common/mod.rs`（2ロールハーネス）が
//! `kaikei-store` 側との複製を許容しているのは crate をまたぐためであり、
//! **同じ crate の中で複製する理由は無い。**

#![allow(dead_code)]

use serde_json::{json, Value};
use sqlx::postgres::PgConnectOptions;
use sqlx::PgPool;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// 起動（設定の検証 → 合成 → 接続 → 勘定科目マスタの投入）を待つ上限。
pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);

/// 1つの JSON-RPC 応答を待つ上限。
pub const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// 実バイナリの在り処
// ---------------------------------------------------------------------------

/// `kaikei-mcp` の実行ファイル。
///
/// テスト実行ファイルは `<target>/<profile>/deps/` に置かれるので、その2つ上
/// が `cargo build` の成果物ディレクトリである（`CARGO_TARGET_DIR` を変えても
/// 自分の位置から辿るので追随する）。
pub fn server_binary() -> PathBuf {
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
pub fn app_database_url(conn_opts: &PgConnectOptions) -> String {
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
pub struct McpServer {
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
    pub async fn start(conn_opts: &PgConnectOptions) -> Self {
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
    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        self.request_within(
            RESPONSE_TIMEOUT,
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
        .await
    }

    /// `tools/list` を1回送り、`tools` の配列を返す。
    pub async fn list_tools(&mut self) -> Vec<Value> {
        let result = self
            .request_within(RESPONSE_TIMEOUT, "tools/list", json!({}))
            .await;
        result["tools"]
            .as_array()
            .unwrap_or_else(|| panic!("tools/list の応答に tools が無い: {result}"))
            .clone()
    }

    /// 応答でもエラーでも、**返ってきたメッセージをそのまま**返す。
    ///
    /// [`request_within`] は `error` が返ると panic するので、
    /// 「この入口が何を返すか分からない」検査には使えない。
    ///
    /// [`request_within`]: McpServer::request_within
    pub async fn raw_request(&mut self, method: &str, params: Value) -> Value {
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

    pub fn stderr_dump(&self) -> String {
        let lines = self.stderr.lock().expect("stderr の記録");
        format!("---- サーバーの stderr ----\n{}", lines.join("\n"))
    }

    pub async fn shutdown(mut self) {
        // stdin を閉じると待受が終わる。落ちない場合に備えて kill も打つ。
        drop(self.stdin);
        let _ = tokio::time::timeout(Duration::from_secs(10), self.child.wait()).await;
        let _ = self.child.kill().await;
    }
}

pub fn is_error(result: &Value) -> bool {
    result["isError"] == json!(true)
}

pub fn body(result: &Value) -> &Value {
    result
        .get("structuredContent")
        .unwrap_or_else(|| panic!("structuredContent が無い: {result}"))
}

// ---------------------------------------------------------------------------
// 帳簿と監査ログの読み取り
// ---------------------------------------------------------------------------

#[derive(Debug, sqlx::FromRow)]
pub struct AuditRow {
    pub request_id: sqlx::types::Uuid,
    pub actor: String,
    pub tool: String,
    pub status: String,
    pub error_code: Option<String>,
    pub entry_id: Option<sqlx::types::Uuid>,
}

pub async fn audit_rows(pool: &PgPool) -> Vec<AuditRow> {
    sqlx::query_as::<_, AuditRow>(
        "SELECT request_id, actor, tool, status, error_code, entry_id FROM audit_log ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .expect("audit_log を読めること")
}

pub async fn journal_entry_count(pool: &PgPool) -> i64 {
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
pub fn assert_audited_pair(rows: &[AuditRow], tool: &str, expected_status: &str) {
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

/// `audit_log` を「1回の呼び出し＝2行」の組に切って、ツール名の並びを返す。
///
/// 読み取り系も**同じ経路（`dispatch::call`）を通る**ので、呼び出した順に
/// `started` / `ok`（または `error`）の対が並ぶ（`docs/07-mcp-server.md` §9。
/// MC-11 の「全11ツールに対して総当たり」）。
pub fn audited_calls(rows: &[AuditRow]) -> Vec<(String, String)> {
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
