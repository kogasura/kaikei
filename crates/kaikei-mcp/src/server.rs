//! ツールレジストリ（何を登録するか）とサーバー情報の文面。
//!
//! # このモジュールは `rmcp` を名指しできない
//!
//! `kaikei-mcp` の `src/` で `rmcp` という識別子を書いてよいのは
//! `dispatch.rs` と `error.rs` だけである（`crate::dispatch` のモジュール doc、
//! `tests/audit_is_structural.rs` の許可リスト）。
//! したがって MCP プロトコルの入口——`ServerHandler` の実装
//! （`call_tool` / `list_tools` / `get_tool` / `get_info`）と stdio
//! トランスポートの起動（[`crate::dispatch::serve_stdio`]）——は
//! [`crate::dispatch`] に置いてある。ここに残るのは
//! **「どのツールを登録するか」と「AI に何と名乗るか」**だけである。
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
//! # このモジュールはルータを直接持たない
//!
//! 保持するのは [`crate::dispatch::ToolRegistry`]（`rmcp` の `ToolRouter` を
//! private フィールドとして包む型）である。`ToolRouter` をここに持たせて
//! いたときは、`with_async_tool::<T>()` / `with_sync_tool::<T>()` で
//! **[`crate::dispatch::call`] を通らないツールを登録できてしまった**
//! （`DECISIONS.md` D-084 の訂正注記）。`ToolRegistry` にツールを載せる口は
//! `with::<T: McpTool>` だけなので、**その型を経由して** `McpTool` 以外を
//! 載せる方法は無い。
//!
//! **「別のルータを新しく作る」ことは型では止まらない**（`rmcp` は直接依存で
//! `ToolRouter` は `pub`。同一 crate 内の import を妨げる仕組みは Rust に
//! 無い）。そちらを止めているのは冒頭のファイル許可リストであって、型では
//! ない（3巡目 C-1。ここに「型として存在しない」と書かないこと）。
//!
//! # レジストリの検査に [`KaikeiServer`] を組み立てない
//!
//! この3つを**自由関数**にしてあるのは、[`KaikeiServer`] が実行時依存
//! （[`Runtime`]）を必須で持つためである。レジストリに何が載っているかは
//! DB にも設定にも依存しない性質なので、それを見るために DB 接続を要求する
//! のは筋が悪い。3つとも [`tool_registry`]（サーバー本体が使うのと**同じ
//! 構築関数**）から導出しており、[`crate::dispatch`] が手書きしている
//! `list_tools` / `call_tool` / `get_tool` が引くのと同じ集合を見る:
//!
//! | ハンドラのメソッド | 実体 | 対応する自由関数 |
//! |---|---|---|
//! | `list_tools` | `tools().list_all()` | [`registered_tool_names`] |
//! | `call_tool` | `tools().call(...)`（未登録名は `has_route` が偽） | [`is_registered_tool`] |
//! | `get_tool` | `tools().get(name).cloned()` | [`tool_definition`] |
//!
//! 両者が実際に一致することは、`Runtime` を組み立てられる側
//! （`tests/startup_pg.rs`）が本物の [`KaikeiServer`] に対して確かめる。

use crate::dispatch::{Implementation, ServerCapabilities, ServerInfo, Tool, ToolRegistry};
use crate::startup::Runtime;
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
    tools: ToolRegistry,
    runtime: Arc<Runtime>,
}

impl KaikeiServer {
    /// 合成ルート（[`crate::startup::assemble`]）が組み立てた依存を持つ
    /// サーバーを作る。**これが唯一の入口である。**
    pub fn with_runtime(runtime: Arc<Runtime>) -> Self {
        Self {
            tools: tool_registry(),
            runtime,
        }
    }

    /// 実行時依存。
    pub fn runtime(&self) -> &Arc<Runtime> {
        &self.runtime
    }

    /// このサーバーが引くレジストリ。
    ///
    /// **crate 内限定。** `ServerHandler` の実装は [`crate::dispatch`] に
    /// あり（`rmcp` を名指しできるファイルがそこだけであるため）、
    /// `tools/list` / `tools/call` の実体をそこから引くために貸す。
    /// [`crate::dispatch::ToolRegistry`] の公開メソッドは
    /// `with::<T: McpTool>` / `list_all` / `get` / `has_route` だけなので、
    /// これを渡してもツールを勝手に載せることはできない。
    pub(crate) fn tools(&self) -> &ToolRegistry {
        &self.tools
    }
}

/// 登録済みツール名の一覧（`tools/list` に出るのと同じ集合）。
///
/// [`crate::dispatch`] が手書きしている `list_tools` は
/// `tools().list_all()` をそのまま返すので、この関数が見ている集合と
/// `tools/list` の応答は同一である。
pub fn registered_tool_names() -> Vec<String> {
    registered_name_set().iter().cloned().collect()
}

/// 登録済みツール名の集合（[`tool_registry`] から一度だけ導出してキャッシュする）。
///
/// キャッシュするのは、[`is_registered_tool`] が**ツール呼び出しのたびに**
/// 呼ばれるためである（`crate::dispatch::ToolName::resolve`）。毎回
/// [`tool_registry`] を組み立てると、入力スキーマの生成とルート表の構築が
/// 1呼び出しあたり11回走る。
///
/// **導出元は変えていない。** ここで見るのも `tool_registry().list_all()` で
/// あり、[`crate::dispatch`] の `list_tools` と同じ集合である
/// （ツールの登録は起動前に確定し、実行中に増減しない）。
fn registered_name_set() -> &'static BTreeSet<String> {
    static NAMES: OnceLock<BTreeSet<String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        tool_registry()
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect()
    })
}

/// そのツール名が登録されているか。
///
/// 登録されていない名前で `tools/call` された場合、ルータは
/// ツール結果エラーではなく**プロトコルエラー**
/// （`invalid_params: tool not found`）を返す。これは
/// `docs/07-mcp-server.md` §6 が認めている唯一の例外
/// （「ツール呼び出しに到達できない異常」）である。`call` が
/// 「tool not found」を返すかどうかを決めているのはこの述語
/// （`ToolRegistry::has_route`）である。
pub fn is_registered_tool(name: &str) -> bool {
    registered_name_set().contains(name)
}

/// ツール定義（`tools/list` の1要素）を名前で引く。
///
/// [`crate::dispatch`] が手書きしている `get_tool` は
/// `tools().get(name).cloned()` であり、この関数と同じものを返す。
pub fn tool_definition(name: &str) -> Option<Tool> {
    tool_registry().get(name).cloned()
}

/// クライアント（＝AI）が最初に受け取るサーバー情報。
///
/// [`crate::dispatch`] が手書きしている `get_info` の実体。実行時依存に
/// 依らない値なので自由関数として切り出してある（文言の検査にサーバーを
/// 組み立てさせない）。
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
/// [`ToolRegistry::with`] に型を並べるだけにする
/// （`docs/07-mcp-server.md` §4）。
///
/// # ★ここは `McpTool` の型を並べる場所である★
///
/// [`ToolRegistry::with`]（境界は `T: McpTool`）が**このレジストリの唯一の
/// 登録経路**であり、そこを通ったハンドラは必ず [`crate::dispatch::call`]
/// （＝監査ログで挟む経路）に入る。`with_route` / `with_async_tool` /
/// `with_sync_tool` は `ToolRegistry` には無い（`DECISIONS.md` D-084 の
/// 訂正注記。`ToolRouter` を直接持たせていた間は**監査ログを通らないツールを
/// 書けた**）。
///
/// **「別のルータを作って別の `ServerHandler` を書く」ことまでは型で止まらない。**
/// それを止めているのは冒頭の**ファイル許可リスト**（`rmcp` を名指しできる
/// のは `dispatch.rs` と `error.rs` だけ。`tests/audit_is_structural.rs`）で
/// ある。
///
/// **このPR（Phase 3 PR-F）では2件**（書き込み系）。読み取り系・提案系は
/// PR-G / PR-H。追加してよいのは `docs/07-mcp-server.md` §2 の表で
/// **Phase 3** と書かれた11件だけで、「存在させないツール」の4件を
/// ここに足してはならない。
pub fn tool_registry() -> ToolRegistry {
    use crate::tools::post_journal_entry::PostJournalEntry;
    use crate::tools::reverse_journal_entry::ReverseJournalEntry;

    ToolRegistry::new()
        .with::<PostJournalEntry>()
        .with::<ReverseJournalEntry>()
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

    // キャッシュした名前の集合と `ToolRegistry::has_route`（`call` が
    // 「tool not found」を返すかどうかを決めている述語）が一致すること。
    //
    // `is_registered_tool` は呼び出しごとにレジストリを組み立てないよう
    // `OnceLock` の集合を引く。導出元が同じであることを実際に突き合わせないと、
    // 「キャッシュだけが古い」状態を検出できない。
    #[test]
    fn the_cached_name_set_agrees_with_the_registry() {
        let registry = tool_registry();
        for name in registered_tool_names() {
            assert!(registry.has_route(&name), "{name}");
            assert!(is_registered_tool(&name), "{name}");
        }
        for absent in ["delete_journal_entry", "execute_sql", ""] {
            assert_eq!(
                registry.has_route(absent),
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

    /// `inputSchema` の中に現れる全ての `description` を集める
    /// （プロパティ・配列要素・`$defs` の入れ子を含む）。
    fn schema_descriptions(value: &serde_json::Value, out: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(map) => {
                if let Some(text) = map.get("description").and_then(|d| d.as_str()) {
                    out.push(text.to_string());
                }
                for nested in map.values() {
                    schema_descriptions(nested, out);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    schema_descriptions(item, out);
                }
            }
            _ => {}
        }
    }

    // ★`inputSchema` の説明文も AI が読む面である★（PR-F レビュー D-2）
    //
    // 入力 DTO の doc コメントは `schemars` がそのまま `description` に載せる
    // ので、`tools/list` の実応答に**内部設計書への参照**（`docs/07-...` /
    // `CLAUDE.md` §7 / crate 名）や Markdown の強調記法がそのまま届く。
    // トップレベルの `description` だけを検査していたので、プロパティ側は
    // §10 / §11 の検査を一度も通っていなかった。
    #[test]
    fn every_input_schema_description_is_written_for_the_caller() {
        // AI に見せる面に出してはいけない語（内部の置き場・記法）。
        const INTERNAL_ONLY: &[&str] = &[
            "docs/",
            "CLAUDE.md",
            "DECISIONS.md",
            "PROGRESS.md",
            "kaikei-core",
            "kaikei-app",
            "kaikei-jp",
            "kaikei-mcp",
            "kaikei_core",
            "kaikei_app",
            ".rs",
            "**",
            "`",
        ];

        for name in registered_tool_names() {
            let tool = tool_definition(&name).unwrap_or_else(|| panic!("{name} の定義が引けない"));
            let schema = serde_json::to_value(&tool.input_schema).expect("スキーマは JSON");
            let mut descriptions = Vec::new();
            schema_descriptions(&schema, &mut descriptions);
            assert!(
                !descriptions.is_empty(),
                "{name} の inputSchema に説明文が1つも無い（AI が引数の意味を読めない）"
            );

            for text in descriptions {
                for forbidden in ["準拠", "法令対応", "JIIMA"] {
                    assert!(!text.contains(forbidden), "{name}: {forbidden} / {text}");
                }
                for internal in INTERNAL_ONLY {
                    assert!(
                        !text.contains(internal),
                        "{name} の inputSchema の説明文に内部向けの記述が出ています\
                         （{internal}）。ここは tools/list の応答として AI に届く面です:\n  {text}"
                    );
                }
            }
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
