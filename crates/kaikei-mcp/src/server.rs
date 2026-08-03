//! ツールレジストリと `rmcp` の [`ServerHandler`] 実装。
//!
//! # 「存在させないツール」はここに現れない
//!
//! `docs/07-mcp-server.md` §2 の4件（`delete_journal_entry` /
//! `update_journal_entry` / `execute_sql` / `reopen_period`）は登録しない。
//! MCP に登録しないことは4層の防御のうち最も外側の1層にすぎないが
//! （残り3層は DB ロール権限・トリガ・`JournalRepo` のポート定義）、
//! **その1層を機械的に閉じる**のがこのモジュールと
//! `tests/forbidden_tools.rs`（MC-10）である。
//!
//! 検査は [`registered_tool_names`] / [`is_registered_tool`] /
//! [`tool_definition`] を通して行う。ツール名の一覧をテスト側に手で
//! 書き写すのではなく**レジストリから導出する**ので、ツールが増えても
//! 一覧だけが腐るということが起きない
//! （`PROGRESS.md` Phase 1 の教訓6「手で維持する一覧は必ず腐る。構造で閉じる」）。
//!
//! # レジストリの検査に [`KaikeiServer`] を組み立てない
//!
//! この3つを**自由関数**にしてあるのは、[`KaikeiServer`] が実行時依存
//! （[`Runtime`]）を必須で持つためである。レジストリに何が載っているかは
//! DB にも設定にも依存しない性質なので、それを見るために DB 接続を要求する
//! のは筋が悪い。3つとも [`tool_router`]（サーバー本体が使うのと**同じ
//! 構築関数**）から導出しており、`#[tool_handler]` が生成する
//! `list_tools` / `call_tool` / `get_tool` が引くのと同じ集合を見る:
//!
//! | 生成されるメソッド | 実体 | 対応する自由関数 |
//! |---|---|---|
//! | `list_tools` | `tool_router.list_all()` | [`registered_tool_names`] |
//! | `call_tool` | `tool_router.call(...)`（未登録名は `has_route` が偽） | [`is_registered_tool`] |
//! | `get_tool` | `tool_router.get(name).cloned()` | [`tool_definition`] |
//!
//! 両者が実際に一致することは、`Runtime` を組み立てられる側
//! （`tests/startup_pg.rs`）が本物の [`KaikeiServer`] に対して確かめる。

use crate::startup::Runtime;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo, Tool};
use rmcp::{tool_handler, ServerHandler};
use std::collections::BTreeSet;
use std::sync::{Arc, OnceLock};

/// このサーバーが MCP クライアントに名乗る名前。
pub const SERVER_NAME: &str = "kaikei-mcp";

/// MCP サーバー本体。
///
/// # 実行時依存は必須である
///
/// [`Runtime`] は `Option` ではない。**依存を持たないサーバーという状態を
/// 型の上でも作れなくする**ためで、そうしないと PR-F / PR-G の11ツールが
/// 「起こりえない `None`」を `expect` で潰す（＝パニック経路を11箇所に作る）
/// か、応答に出しようのない internal エラー分岐を11箇所に書くかの
/// どちらかになる。`DECISIONS.md` D-057 が「欠けたまま起動できる形」を
/// 設定の層で塞いだのと同じ規律を、型の層でも守る。
///
/// ツール（PR-F / PR-G）は [`KaikeiServer::runtime`] から `PgStore` /
/// `PgAuditSink` / `JpTaxPolicy` / `TagCatalog` を取る。**ツールの中で
/// `compose` を呼んだりプールを張り直したりしないこと**（起動時に一度だけ
/// 組み立てる、が `DECISIONS.md` D-025 / D-057 の前提）。
///
/// レジストリだけを見たい場合は [`registered_tool_names`] /
/// [`is_registered_tool`] / [`tool_definition`] を使う（サーバーを
/// 組み立てる必要はない）。
#[derive(Clone)]
pub struct KaikeiServer {
    tool_router: ToolRouter<Self>,
    runtime: Arc<Runtime>,
}

impl KaikeiServer {
    /// 合成ルート（[`crate::startup::assemble`]）が組み立てた依存を持つ
    /// サーバーを作る。**これが唯一の入口である。**
    pub fn with_runtime(runtime: Arc<Runtime>) -> Self {
        Self {
            tool_router: tool_router(),
            runtime,
        }
    }

    /// 実行時依存。
    pub fn runtime(&self) -> &Arc<Runtime> {
        &self.runtime
    }
}

/// 登録済みツール名の一覧（`tools/list` に出るのと同じ集合）。
///
/// `#[tool_handler]` が生成する `list_tools` は `tool_router.list_all()` を
/// そのまま返すので、この関数が見ている集合と `tools/list` の応答は同一で
/// ある。
pub fn registered_tool_names() -> Vec<String> {
    registered_name_set().iter().cloned().collect()
}

/// 登録済みツール名の集合（[`tool_router`] から一度だけ導出してキャッシュする）。
///
/// キャッシュするのは、[`is_registered_tool`] が**ツール呼び出しのたびに**
/// 呼ばれるためである（`crate::dispatch::ToolName::resolve`）。毎回
/// [`tool_router`] を組み立てると、入力スキーマの生成とルート表の構築が
/// 1呼び出しあたり11回走る。
///
/// **導出元は変えていない。** ここで見るのも `tool_router().list_all()` で
/// あり、`#[tool_handler]` が生成する `list_tools` と同じ集合である
/// （ツールの登録は起動前に確定し、実行中に増減しない）。
fn registered_name_set() -> &'static BTreeSet<String> {
    static NAMES: OnceLock<BTreeSet<String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        tool_router()
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect()
    })
}

/// そのツール名が登録されているか。
///
/// 登録されていない名前で `tools/call` された場合、`rmcp` は
/// ツール結果エラーではなく**プロトコルエラー**
/// （`invalid_params: tool not found`）を返す。これは
/// `docs/07-mcp-server.md` §6 が認めている唯一の例外
/// （「ツール呼び出しに到達できない異常」）である。`call` が
/// 「tool not found」を返すかどうかを決めているのはこの述語
/// （`ToolRouter::has_route`）である。
pub fn is_registered_tool(name: &str) -> bool {
    registered_name_set().contains(name)
}

/// ツール定義（`tools/list` の1要素）を名前で引く。
///
/// `#[tool_handler]` が生成する `get_tool` は `tool_router.get(name).cloned()`
/// であり、この関数と同じものを返す。
pub fn tool_definition(name: &str) -> Option<Tool> {
    tool_router().get(name).cloned()
}

/// クライアント（＝AI）が最初に受け取るサーバー情報。
///
/// [`ServerHandler::get_info`] の実体。実行時依存に依らない値なので
/// 自由関数として切り出してある（文言の検査にサーバーを組み立てさせない）。
pub fn server_info() -> ServerInfo {
    ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
        .with_server_info(Implementation::new(SERVER_NAME, env!("CARGO_PKG_VERSION")))
        // クライアント（＝AI）が最初に読む説明文。`CLAUDE.md` §10 の
        // 表現規律（税務判断を断定しない／「法令に準拠」と書かない）と
        // §11（次の手が分かる文言）はこの文面にも及ぶ。
        .with_instructions(
            "複式簿記の帳簿を扱うサーバーです。\
             帳簿は追記のみで、記帳した仕訳の更新・削除はできません。\
             訂正は逆仕訳（reverse_journal_entry）で行ってください。\
             金額は文字列で受け渡します（例: \"110000\"）。",
        )
}

/// Phase 3 の11ツールを合成する。
///
/// 1ツール1ファイル（`src/tools/<ツール名>.rs`）とし、ここでは
/// [`crate::dispatch::route`] に型を渡して並べるだけにする
/// （`docs/07-mcp-server.md` §4）。
///
/// # ★ここに `ToolRoute` を直接書かないこと★
///
/// [`crate::dispatch::route`] が**唯一の登録経路**であり、そこを通った
/// ハンドラは必ず [`crate::dispatch::call`]（＝監査ログで挟む経路）に入る。
/// `ToolRoute::new_dyn` や `#[tool]` マクロでここに別のルートを足すと、
/// **監査ログを通らないツールが作れてしまう**（`DECISIONS.md` D-084）。
/// `tests/audit_is_structural.rs` がソースを走査して見張っている。
///
/// **このPR（Phase 3 PR-F）では2件**（書き込み系）。読み取り系・提案系は
/// PR-G / PR-H。追加してよいのは `docs/07-mcp-server.md` §2 の表で
/// **Phase 3** と書かれた11件だけで、「存在させないツール」の4件を
/// ここに足してはならない。
pub fn tool_router() -> ToolRouter<KaikeiServer> {
    use crate::dispatch::route;
    use crate::tools::post_journal_entry::PostJournalEntry;
    use crate::tools::reverse_journal_entry::ReverseJournalEntry;

    ToolRouter::new()
        .with_route(route::<PostJournalEntry>())
        .with_route(route::<ReverseJournalEntry>())
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for KaikeiServer {
    fn get_info(&self) -> ServerInfo {
        server_info()
    }
}

/// stdio トランスポートでサーバーを起動し、切断されるまで待つ。
///
/// # stdout は JSON-RPC 専用チャネル
///
/// `println!` や stdout に出る `tracing` が1行でも混ざるとプロトコルが壊れ、
/// 接続ごと落ちる。ログ・診断出力は必ず **stderr** に出すこと
/// （`docs/07-mcp-server.md` §4）。
///
/// 設定の読み込みと合成は [`crate::config`] / [`crate::startup`] /
/// `src/main.rs`（PR-E）。
///
/// # Errors
///
/// 初期化（`initialize` の折衝）に失敗した場合、または待機中に
/// トランスポートが異常終了した場合。
pub async fn serve_stdio(server: KaikeiServer) -> Result<(), Box<dyn std::error::Error>> {
    use rmcp::transport::stdio;
    use rmcp::ServiceExt;

    let running = server.serve(stdio()).await?;
    running.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // PR-F の時点で登録されているのは書き込み系の2件（読み取り系・提案系は
    // PR-G / PR-H）。件数そのものではなく「その2件が居ること」を見る
    // （件数のリテラルは PR-G で必ず古くなる）。
    #[test]
    fn the_write_tools_are_registered() {
        let names = registered_tool_names();
        for expected in ["post_journal_entry", "reverse_journal_entry"] {
            assert!(names.iter().any(|name| name == expected), "{expected}");
        }
    }

    // キャッシュした名前の集合と `ToolRouter::has_route`（`call` が
    // 「tool not found」を返すかどうかを決めている述語）が一致すること。
    //
    // `is_registered_tool` は呼び出しごとにルータを組み立てないよう
    // `OnceLock` の集合を引く。導出元が同じであることを実際に突き合わせないと、
    // 「キャッシュだけが古い」状態を検出できない。
    #[test]
    fn the_cached_name_set_agrees_with_the_router() {
        let router = tool_router();
        for name in registered_tool_names() {
            assert!(router.has_route(&name), "{name}");
            assert!(is_registered_tool(&name), "{name}");
        }
        for absent in ["delete_journal_entry", "execute_sql", ""] {
            assert_eq!(
                router.has_route(absent),
                is_registered_tool(absent),
                "{absent}"
            );
            assert!(!is_registered_tool(absent), "{absent}");
        }
    }

    // 登録済みのツールは全て `tools/list` の定義（説明文と入力スキーマ）を持つ。
    #[test]
    fn every_registered_tool_has_a_description_and_an_object_input_schema() {
        for name in registered_tool_names() {
            let tool = tool_definition(&name).unwrap_or_else(|| panic!("{name} の定義が引けない"));
            let description = tool.description.unwrap_or_default();
            assert!(!description.is_empty(), "{name} に説明文が無い");
            // `CLAUDE.md` §10 の禁止表現はツールの説明文にも及ぶ。
            for forbidden in ["準拠", "法令対応", "JIIMA"] {
                assert!(!description.contains(forbidden), "{name}: {forbidden}");
            }
            assert_eq!(
                tool.input_schema.get("type").and_then(|t| t.as_str()),
                Some("object"),
                "{name} の inputSchema がオブジェクトではない"
            );
        }
    }

    // サーバーは tools capability を名乗り、名前とバージョンを持つ。
    #[test]
    fn get_info_declares_the_tools_capability() {
        let info = server_info();
        assert!(info.capabilities.tools.is_some());
        assert_eq!(info.server_info.name, SERVER_NAME);
        assert!(!info.server_info.version.is_empty());
    }

    // 説明文が `CLAUDE.md` §10 の禁止表現を含まない。
    #[test]
    fn instructions_avoid_forbidden_claims() {
        let info = server_info();
        let instructions = info.instructions.unwrap_or_default();
        for forbidden in ["準拠", "法令対応", "JIIMA"] {
            assert!(
                !instructions.contains(forbidden),
                "禁止表現が含まれています（CLAUDE.md §10）: {forbidden}"
            );
        }
        // 訂正の手段（次の手）は書いてある（`CLAUDE.md` §11）。
        assert!(instructions.contains("逆仕訳"));
    }
}
