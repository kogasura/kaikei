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
//! 検査は [`KaikeiServer::tool_names`] / [`KaikeiServer::has_tool`] と、
//! `ServerHandler::get_tool` を通して行う。ツール名の一覧をテスト側に手で
//! 書き写すのではなく**レジストリから導出する**ので、ツールが増えても
//! 一覧だけが腐るということが起きない
//! （`PROGRESS.md` Phase 1 の教訓6「手で維持する一覧は必ず腐る。構造で閉じる」）。

use crate::startup::Runtime;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::{tool_handler, ServerHandler};
use std::sync::Arc;

/// このサーバーが MCP クライアントに名乗る名前。
pub const SERVER_NAME: &str = "kaikei-mcp";

/// MCP サーバー本体。
///
/// # 実行時依存は合成ルートから受け取る
///
/// PR-E で [`KaikeiServer::with_runtime`] が入った。ツール（PR-F / PR-G）は
/// [`KaikeiServer::runtime`] から `PgStore` / `PgAuditSink` /
/// `JpTaxPolicy` / `TagCatalog` を取る。**ツールの中で `compose` を呼んだり
/// プールを張り直したりしないこと**（起動時に一度だけ組み立てる、が
/// `DECISIONS.md` D-025 / D-057 の前提）。
#[derive(Clone)]
pub struct KaikeiServer {
    tool_router: ToolRouter<Self>,
    runtime: Option<Arc<Runtime>>,
}

impl KaikeiServer {
    /// **実行時依存を持たない**サーバーを組み立てる。
    ///
    /// ツールレジストリ（`tools/list` に出る集合）と `get_info` の検査だけを
    /// 行うテスト用の構成。DB も設定も要らない代わりに、依存を要するツールは
    /// 動かせない。本番の起動は [`with_runtime`] を使う。
    ///
    /// [`with_runtime`]: KaikeiServer::with_runtime
    pub fn new() -> Self {
        Self {
            tool_router: tool_router(),
            runtime: None,
        }
    }

    /// 合成ルート（[`crate::startup::assemble`]）が組み立てた依存を持つ
    /// サーバーを作る。
    pub fn with_runtime(runtime: Arc<Runtime>) -> Self {
        Self {
            tool_router: tool_router(),
            runtime: Some(runtime),
        }
    }

    /// 実行時依存。[`new`] で作った構成では `None`。
    ///
    /// [`new`]: KaikeiServer::new
    pub fn runtime(&self) -> Option<&Arc<Runtime>> {
        self.runtime.as_ref()
    }

    /// 登録済みツール名の一覧を返す（`tools/list` に出るのと同じ集合）。
    ///
    /// `#[tool_handler]` が生成する `list_tools` は
    /// `self.tool_router.list_all()` をそのまま返すので、この関数が見ている
    /// 集合と `tools/list` の応答は同一である。
    pub fn tool_names(&self) -> Vec<String> {
        self.tool_router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect()
    }

    /// そのツール名が登録されているか。
    ///
    /// 登録されていない名前で `tools/call` された場合、`rmcp` は
    /// ツール結果エラーではなく**プロトコルエラー**
    /// （`invalid_params: tool not found`）を返す。これは
    /// `docs/07-mcp-server.md` §6 が認めている唯一の例外
    /// （「ツール呼び出しに到達できない異常」）である。
    pub fn has_tool(&self, name: &str) -> bool {
        self.tool_router.has_route(name)
    }
}

impl Default for KaikeiServer {
    fn default() -> Self {
        Self::new()
    }
}

/// Phase 3 の11ツールを合成する。
///
/// 1ツール1ファイル（`src/tools/<ツール名>.rs`）とし、各ファイルの
/// `#[tool_router(router = <ツール名>_router, vis = "pub")]` が生成する
/// ルータをここで `+` で合成する（`docs/07-mcp-server.md` §4）。
///
/// **このPR（Phase 3 PR-D）では0件である。** ツールの実装は PR-F / PR-G。
/// 追加してよいのは `docs/07-mcp-server.md` §2 の表で **Phase 3** と
/// 書かれた11件だけで、「存在させないツール」の4件をここに足してはならない。
pub fn tool_router() -> ToolRouter<KaikeiServer> {
    ToolRouter::new()
    // 例（PR-F 以降）:
    // + crate::tools::post_journal_entry::post_journal_entry_router()
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for KaikeiServer {
    fn get_info(&self) -> ServerInfo {
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

    // このPRの時点ではツールを1つも登録していない（PR-E は前工事であり、
    // ツールは PR-F / PR-G）。
    #[test]
    fn the_skeleton_registers_no_tools_yet() {
        assert!(KaikeiServer::new().tool_names().is_empty());
    }

    // 依存を持たない構成では `runtime()` が `None`（本番の起動は
    // `with_runtime` を通る）。
    #[test]
    fn a_server_built_without_a_runtime_reports_it() {
        assert!(KaikeiServer::new().runtime().is_none());
    }

    // サーバーは tools capability を名乗り、名前とバージョンを持つ。
    #[test]
    fn get_info_declares_the_tools_capability() {
        let info = KaikeiServer::new().get_info();
        assert!(info.capabilities.tools.is_some());
        assert_eq!(info.server_info.name, SERVER_NAME);
        assert!(!info.server_info.version.is_empty());
    }

    // 説明文が `CLAUDE.md` §10 の禁止表現を含まない。
    #[test]
    fn instructions_avoid_forbidden_claims() {
        let info = KaikeiServer::new().get_info();
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
