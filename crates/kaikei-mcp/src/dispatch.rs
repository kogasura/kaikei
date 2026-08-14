//! ★この PR の本体★ ツール1回分の呼び出しを**監査ログで挟む**唯一の経路。
//!
//! # 「監査ログを書き忘れる」を検査ではなく構造で塞ぐ
//!
//! `DECISIONS.md` D-076 が fail-closed / fail-open の手順を
//! [`kaikei_app::audit::with_audit`] に閉じたのは、**書き忘れても正常系の
//! テストが全て緑のまま通る**種類の規律だからである。
//! **同じ理由が MCP 層にも当てはまる。** 各ツールが `with_audit` を呼ぶ形に
//! すると、11ツールのうち1つで呼び忘れても誰も気づかない。
//!
//! そこで**ツールを書く側からは呼び忘れる形が無い**ようにしてある。
//!
//! **どこまでが型で、どこからが検査かを混ぜないこと**（PR-F レビュー3巡目
//! C-1 / 4巡目 D。この表には以前「手段」の列が無く、下の §「何が型で閉じて
//! いて、何が閉じていないか」を読む前に**無条件の保証として読めて**しまった）。
//!
//! | 塞ぐもの | 実体 | 手段 |
//! |---|---|---|
//! | ツールは `CallToolResult` を組み立てられない | [`McpTool::run`] の戻り値は `Result<`[`ToolSuccess`]`, `[`ToolFailure`]`>` であり、応答（`isError` を含む）を組み立てるのは [`call`] だけ | 型 |
//! | ツールは監査ログの記録先に触れない | [`McpTool::run`] が受け取るのは [`ToolContext`] で、[`kaikei_app::ports::AuditSink`] を**露出しない**。[`crate::startup::Runtime`] 自体が渡らない | 型 |
//! | [`ToolContext`] を自分で作れない | フィールドも `new` も private。作れるのはこのモジュールだけ | 型 |
//! | [`ToolRegistry`] に [`McpTool`] 以外を載せられない | 載せる口は [`ToolRegistry::with`]`::<T: `[`McpTool`]`>` だけで、その中身は必ず [`call`] である | 型 |
//! | fail-open の警告を捨てられない | [`call`] は [`kaikei_app::audit::AuditedCall::into_result_noting_outcome`]（既定経路）しか使わず、積まれた警告を必ず応答の `warnings` に載せる | 型＋走査（`into_parts_unchecked` がこの crate に無いことは走査） |
//! | **別のルータ・別の `ServerHandler`・別のプロトコル入口を書き足す** | `rmcp` を名指しできるファイルの許可リスト（`dispatch.rs` / `error.rs`） | **検査**（型では止まらない） |
//! | **上の全部**（書き方に依らず、監査ログが2行残ること） | 実バイナリに `tools/call` を送り `audit_log` を見る（`crates/kaikei-e2e/tests/mcp_stdio_server.rs`） | **振る舞い検査** |
//!
//! 最後の2行は型ではない。**「呼び忘れる形が存在しない」は、ツールを追加する
//! 実装者の視点での話であって、この crate に何でも書ける立場の人間に対する
//! 保証ではない**（そちらを見ているのは検査である）。
//!
//! # ★このファイルは `rmcp` を名指しできる2ファイルのうちの1つである★
//!
//! **`kaikei-mcp` の `src/` で `rmcp` という識別子を書いてよいのは
//! `dispatch.rs` と `error.rs` だけ**であり、`tests/audit_is_structural.rs`
//! の `rmcp_is_named_only_in_the_files_allowed_to_name_it`（許可リスト）が
//! 機械的に検査する。だから `rmcp` に触るコード——`ServerHandler` の実装
//! （`call_tool` / `list_tools` / `get_tool` / `get_info`）も、stdio
//! トランスポートの起動（[`serve_stdio`]）も、このファイルに集めてある。
//!
//! ## なぜ禁止リストではなく許可リストなのか（PR-F レビュー3巡目 B）
//!
//! **識別子の禁止リストは2巡続けて破られた。**
//!
//! | 巡 | 破り方 | 禁止リストに無かったもの |
//! |---|---|---|
//! | 1 | `ToolRouter::with_async_tool::<T>()` / `with_sync_tool` / `(Tool, handler)` タプル | `with_async_tool` / `AsyncTool` / `ToolBase` / `IntoToolRoute` |
//! | 2 | `#[tool_handler]` の impl に `call_tool` を**手書き**する | `call_tool` / `CallToolRequestParams` / `ToolCallContext` / `into_call_tool_result` |
//!
//! 2 は特に静かだった。`rmcp-macros` 3.1.0 の `#[tool_handler]` は
//! `if !has_method("call_tool", &item_impl)` で条件付き生成するので、
//! 同じ impl ブロックに `call_tool` を手書きすると**マクロが生成する
//! dispatch 経路が黙って置き換わる**。`tools/list` は正規の2件のまま、
//! `tools/call` を1回送ると `journal_entries` に1件・`audit_log` に0行。
//! `cargo build` も `clippy -D warnings` も `cargo test` も全緑だった。
//!
//! 禁止する識別子を足し続ける限り、`rmcp` が API を1つ増やすたび・
//! レビュアーが1つ見落とすたびに同じことが起きる（**原理的に不完全**）。
//! そこで向きを反転し、**`rmcp` を名指しできるファイルの側を許可リストで
//! 限定した**。どの API を使う迂回であっても `rmcp` の名前は必要なので、
//! 迂回は必ず許可された2ファイルのどちらかに現れる。
//! `docs/07-mcp-server.md` §10 MC-30（依存の許可リスト）や
//! `tests/forbidden_tools.rs` の許可リスト側検査と同じ形である。
//!
//! ## その許可リストも3巡目に破られた（4巡目 A）
//!
//! 許可リストが見ているのは**走査が読んだファイル**だけである。3巡目の
//! 迂回は `#[path = "../probe_handler.rs"] mod probe_handler;` と
//! `include!("probe_handler.inc")` で、当時の走査（`src/**/*.rs`）は
//! **その2つのファイルを一度も読まなかった**。監査ログを通らない別の
//! `ServerHandler` を `main.rs` から実際に待ち受けさせた状態で、
//! `cargo build` / `clippy -D warnings` / `fmt --check` /
//! `cargo test -p kaikei-mcp` が全緑だった。
//!
//! **走査は「ソースがどう書かれているか」しか見られない**ので、書き方を
//! 変える迂回に対して原理的に後手に回る。そこで**網羅の担い手を走査から
//! 振る舞い検査へ移した**:
//! `crates/kaikei-e2e/tests/mcp_stdio_server.rs` が**実バイナリを stdio で
//! 起動して `tools/call` を送り**、`journal_entries` と `audit_log` の行を
//! 数える。識別子が何であれ、ファイルがどこに在ろうと、別の入口から来よう
//! と、**監査ログが2行無ければ落ちる**。
//! 走査（許可リスト・識別子の閉じ込め・`#[path]` / `include!` の禁止）は
//! 「**書いた瞬間に、DB 無しで、手元で落ちる**」二線目として残してある。
//!
//! ## 何が型で閉じていて、何が閉じていないか（PR-F レビュー3巡目 C-1）
//!
//! **`rmcp` を「型として見えなくする」ことはできない。** `rmcp` は
//! `kaikei-mcp` の直接依存であり `ToolRouter` は `pub` なので、
//! 同一 crate の他モジュールから `use rmcp::...` を妨げる仕組みは Rust に
//! 無い。以前この doc / `server.rs` / `docs/07` / `DECISIONS.md` D-084 が
//! 書いていた「`ToolRouter` は見えない」「型として存在しない」は
//! **成立していなかった**（レビュアーが実際にコンパイルを通している）。
//!
//! 実際の内訳はこうである。
//!
//! | 担保 | 手段 |
//! |---|---|
//! | [`ToolRegistry`] に `McpTool` 以外を載せられない | **型**（`with` の境界が `T: `[`McpTool`]、内側の `ToolRouter` は private フィールド） |
//! | [`ToolContext`] を自作できない／[`kaikei_app::ports::AuditSink`] に触れない | **型**（private フィールド・private な `new`・`Runtime` を渡さない） |
//! | 別のルータ・別の `ServerHandler`・別のハンドラを**書き足す** | **ファイル許可リスト**（`rmcp` を名指しできるのは2ファイル） |
//! | 走査の外にファイルを置く（`#[path]` / `include!`） | **走査**（`tests/source_scan/mod.rs` の `assert_no_out_of_tree_inclusion`。4巡目 B） |
//! | 同一 crate から `kaikei_app::audit::with_audit` を直接呼ぶ | **識別子の閉じ込め**（`tests/audit_is_structural.rs` の `CONFINED`） |
//! | **このファイルの中に別のプロトコル入口を足す**（`ServerHandler` は `call_tool` 以外にも既定実装を持つ: `read_resource` / `get_prompt` / `complete` / `get_task` …） | 許可リストの内側なので走査では見えない。**振る舞い検査**が見る（`crates/kaikei-e2e/tests/mcp_stdio_server.rs` の `no_protocol_entry_point_other_than_tools_call_touches_the_ledger`） |
//! | 上の全部を**振る舞いで**見る | **実バイナリへの `tools/call`**（`crates/kaikei-e2e/tests/mcp_stdio_server.rs`） |
//!
//! 3〜6行目は型ではなく**検査**であり、6行目に至っては検査も無い。
//! ここを型で閉じたと書かないこと。**穴が「1つだけ」だとも書かないこと**
//! （4巡目 C-1。以前は「再輸出が唯一の穴」と書いていたが、上のとおり
//! 少なくとも3つある）。提供していない保証を書かない、は本 PR が繰り返し
//! 適用してきた規律である。
//!
//! # `AuditCall::tool` にはレジストリの名前しか載らない
//!
//! `docs/07-mcp-server.md` §9（PR-C からの申し送り）:
//! `tool` / `actor` は `audit_log` の **TEXT 列**で、`input` / `output`
//! （JSONB）に掛かる無害化を通らない。`tools/call` の `name` は
//! クライアント（AI）由来なので、それをそのまま載せると
//! **`tool` に U+0000 を1文字入れられただけで開始レコードが書けず
//! fail-closed になる**（D-075 が JSONB 側で塞いだ事故が TEXT 側で再発する）。
//!
//! ここでは [`ToolName`] を通す。フィールドは private で、構築子は
//! [`ToolName::resolve`] ただ1つ、その中身は
//! [`crate::server::is_registered_tool`]（＝`tools/list` と同じレジストリ）
//! である。**登録済みの名前以外から [`ToolName`] を作る方法が存在しない**ので、
//! `tool` 列に入るのは常にサーバが知っている有限個の文字列になる。
//!
//! **担保の内訳を正確に書く**（PR-F レビュー C-6）。`AuditCall` は凍結済みの
//! `kaikei-app` にあり、そのフィールドは `pub tool: &'a str` である。
//! つまり「`AuditCall::tool` に渡せる型が [`ToolName`] に限られている」わけ
//! ではない——このモジュールの中で `tool: T::NAME` と書くことは型としては
//! 妨げられていない。実際の担保は次の2つの重ね合わせである:
//!
//! 1. `AuditCall` という識別子が `dispatch.rs` にしか現れないことを
//!    `tests/audit_is_structural.rs` が走査で見張る（＝組み立てるのは1箇所）
//! 2. その1箇所（[`call`]）が [`ToolName::resolve`] を通す。
//!    **`resolve` が登録済み以外を弾くこと自体は型で閉じている**
//!
//! なお未登録の名前で `tools/call` された場合、`rmcp` の `ToolRouter` は
//! この経路に**到達させない**（`has_route` が偽なら
//! `invalid_params: tool not found` を返す）。`docs/07-mcp-server.md` §6 が
//! 認めている唯一のプロトコルエラーであり、監査ログにも当然載らない。

use std::future::Future;
use std::sync::Arc;

use kaikei_app::audit::{actor, with_audit, AuditCall, AuditSuccess, AuditableError, RequestId};
use kaikei_app::clock::SystemClock;
use kaikei_app::context::BookSettings;
use kaikei_app::error::codes;
use kaikei_app::id::UuidV7IdGenerator;
use kaikei_app::ports::{LedgerQuery, SearchEntriesQuery};
use kaikei_app::usecase::import_chart::ChartDifference;
use kaikei_core::EntryId;
use kaikei_jp::compose::Composition;
use kaikei_store::pool::PgStore;
use kaikei_store::query::PgTrialBalanceQuery;
use rmcp::handler::server::router::tool::{ToolRoute, ToolRouter};
use rmcp::handler::server::tool::{schema_for_input, ToolCallContext};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ListToolsResult,
    PaginatedRequestParams, ResultType,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::error::ToolError;
use crate::server::KaikeiServer;
use crate::startup::Runtime;

/// `rmcp` の型のうち、**`rmcp` を名指しできない他モジュールにも要るもの**の
/// 再輸出。
///
/// `src/server.rs` は `tools/list` の定義（[`Tool`]）とサーバー情報
/// （[`ServerInfo`]）を組み立てるが、モジュール doc の許可リストにより
/// `rmcp` を名指しできない。そこでここから貸す。
///
/// **登録経路に関わる型（`ToolRouter` / `ToolRoute` / `ToolCallContext` /
/// `CallToolResult` …）は再輸出しないこと。** 再輸出すると
/// 「`rmcp` を名指しせずに登録経路へ届く」抜け道ができ、許可リストの意味が
/// 消える。`tests/audit_is_structural.rs` の `CONFINED`（識別子の閉じ込め）が
/// その漏れに対する second line として残してある。
pub use rmcp::model::{Implementation, ServerCapabilities, ServerInfo, Tool};

/// 応答に fail-open の警告を載せるキー。
///
/// ツール本体が [`ToolError::with_detail`] で同じキーを使わないこと
/// （[`call`] が最後に上書きする）。
const WARNINGS_KEY: &str = "warnings";

// ---------------------------------------------------------------------------
// ツール名（レジストリ由来であることの証明）
// ---------------------------------------------------------------------------

/// **レジストリに登録済みであることが保証されたツール名。**
///
/// `audit_log.tool`（TEXT 列）に載せてよい唯一の型。モジュール doc
/// 「`AuditCall::tool` にはレジストリの名前しか載らない」を参照。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolName(&'static str);

impl ToolName {
    /// レジストリに登録されていれば [`ToolName`] にする。**唯一の構築子。**
    ///
    /// 判定は [`crate::server::is_registered_tool`]（`tools/list` が返すのと
    /// 同じ集合）に委ねる。ここに名前の一覧を書き写さない
    /// （`PROGRESS.md` Phase 1 の教訓6「手で維持する一覧は必ず腐る」）。
    #[must_use]
    pub fn resolve(name: &'static str) -> Option<Self> {
        crate::server::is_registered_tool(name).then_some(ToolName(name))
    }

    /// 名前そのもの。
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        self.0
    }
}

// ---------------------------------------------------------------------------
// ツールが受け取るもの / 返すもの
// ---------------------------------------------------------------------------

/// ツール本体が使える実行時依存。**監査ログの記録先は含まれない。**
///
/// [`crate::startup::Runtime`] をそのまま渡さないのは、
/// `Runtime::audit_sink` に触れると各ツールが自前で `with_audit` を呼べて
/// しまい、このモジュールが型で閉じている不変条件が崩れるため
/// （モジュール doc の表）。
///
/// 依存は**所有値（または `Arc`）で取り出す**。`kaikei_app::tx::with_tx` /
/// `with_tx_err` に渡すクロージャは HRTB で全称量化されており、
/// `'static` でない借用をキャプチャできない（`crates/kaikei-app/src/tx.rs`
/// の doc「クロージャに渡せるもの」）。借用を返すアクセサを生やすと、
/// 呼び出し側が必ずそこで詰まる。
pub struct ToolContext<'a> {
    runtime: &'a Runtime,
}

impl<'a> ToolContext<'a> {
    /// **private。** [`call`] だけがこれを作れる。
    fn new(runtime: &'a Runtime) -> Self {
        ToolContext { runtime }
    }

    /// 帳簿の読み書き（`kaikei_app` ロール）。
    pub fn store(&self) -> &'a PgStore {
        self.runtime.store.as_ref()
    }

    /// 試算表の read model（`Store` / `Tx` を経由しない。`CLAUDE.md` §6）。
    ///
    /// 読み取り系ツールはここから引き、`store()` 側の
    /// [`kaikei_app::ports::JournalRepo`] で全件ロードして自分で集計する
    /// ようなことはしない（`DECISIONS.md` D-086）。
    pub fn trial_balance_query(&self) -> &'a PgTrialBalanceQuery {
        self.runtime.trial_balance.as_ref()
    }

    /// 証憑の read model（**`Store` / `Tx` を経由しない**）。
    pub fn document_query(&self) -> &'a kaikei_store::documents::PgDocumentQuery {
        self.runtime.documents.as_ref()
    }

    /// 起動時にテンプレートと定義が食い違い、**既存を残した**科目
    /// （`DECISIONS.md` D-081 / D-086）。
    ///
    /// `get_settings` がこれを応答に載せる。stderr だけを出口にしないため
    /// （`docs/07-mcp-server.md` §7 の PR-G への申し送り）。
    pub fn chart_differences(&self) -> &'a [ChartDifference] {
        &self.runtime.chart_differences
    }

    /// `kaikei-jp` の組み立て結果（税額計算 policy・タグ定義・科目テンプレート）。
    ///
    /// `Arc` を複製して返す。`with_tx_err` のクロージャに `move` で
    /// 持ち込めるようにするため（上記の HRTB 制約）。
    pub fn composition(&self) -> Arc<Composition> {
        Arc::clone(&self.runtime.composition)
    }

    /// 帳簿全体の設定（帳簿通貨・会計年度の区切り規則）。
    pub fn book_settings(&self) -> BookSettings {
        self.runtime.book_settings
    }

    /// 仕訳IDの生成（UUID v7）。
    pub fn id_gen(&self) -> UuidV7IdGenerator {
        self.runtime.id_gen
    }

    /// 記帳時刻の取得（`CLAUDE.md` §7）。
    pub fn clock(&self) -> SystemClock {
        self.runtime.clock
    }

    /// 仕訳検索の read model（`CLAUDE.md` §6。`Tx` を通さない）。
    pub fn search_entries_query(&self) -> &'a dyn SearchEntriesQuery {
        self.runtime.search_query.as_ref()
    }

    /// 総勘定元帳の read model（同上）。
    pub fn ledger_query(&self) -> &'a dyn LedgerQuery {
        self.runtime.ledger_query.as_ref()
    }
}

/// ツールが成功したときに返す値。
///
/// `body` は応答の `structuredContent` になり、**既定では
/// `audit_log.output` にも同じものが載る**（`docs/07-mcp-server.md` §9）。
/// 書き込み系はこの既定を使う——**結果そのものが変更の記録**であり、
/// 記帳した仕訳の姿を後から縮めて記録する理由が無い。
///
/// # 読み取り系だけが要約に差し替える（`DECISIONS.md` D-089 の決定6）
///
/// 読み取り系は応答本文が帳簿の抜粋そのものなので、上限まで返すと
/// 1回の呼び出しで数十〜百数十 KB が `audit_log.output` に入る。しかも
/// **読み取りは AI が最も多く呼ぶ操作**である。
///
/// 監査ログにおける読み取りの目的は「**誰がいつ何を読んだか**」であり、
/// 返した内容そのものは (問い合わせ条件 = `input` + その時点の帳簿) から
/// 再現できる（帳簿は追記のみなので過去の状態を再構成できる）。
/// そこで [`ToolSuccess::with_audit_summary`] で要約に差し替える。
///
/// **2箇所で別の JSON を「組み立てる」形にはしない。** 要約は各ツールが
/// `body` から不要な配列を落として作る（`search_entries` / `get_ledger` の
/// `audit_summary`）ので、応答と要約の値が食い違うことはない。
#[derive(Debug, Clone)]
pub struct ToolSuccess {
    body: Map<String, Value>,
    audit_output: Option<Map<String, Value>>,
    entry_id: Option<EntryId>,
}

impl ToolSuccess {
    /// 応答本体から作る。`audit_log.output` にも同じものが載る。
    #[must_use]
    pub fn new(body: Map<String, Value>) -> Self {
        ToolSuccess {
            body,
            audit_output: None,
            entry_id: None,
        }
    }

    /// `audit_log.output` に**本文の代わりに**載せる要約。
    ///
    /// 読み取り系だけが使う（理由は型の doc）。要約は `body` から
    /// 導いたものにすること（別に組み立てると値が食い違う）。
    #[must_use]
    pub fn with_audit_summary(mut self, summary: Map<String, Value>) -> Self {
        self.audit_output = Some(summary);
        self
    }

    /// 記帳した仕訳ID（`audit_log.entry_id` に入る）。書き込み系だけが付ける。
    #[must_use]
    pub fn with_entry_id(mut self, entry_id: EntryId) -> Self {
        self.entry_id = Some(entry_id);
        self
    }

    /// `audit_log.output` に載せる JSON（要約があればそちら）。
    fn audit_output_json(&self) -> String {
        let object = self.audit_output.as_ref().unwrap_or(&self.body);
        Value::Object(object.clone()).to_string()
    }
}

/// ツールが失敗したときに返す値。
///
/// [`ToolError`] を包んでいるだけだが、[`AuditableError`] を実装することで
/// **分類コードと `public_message()` が自動的に結果レコードへ載る**
/// （`Display` が `audit_log.output` に届く経路を作らない。D-076）。
#[derive(Debug, Clone)]
pub struct ToolFailure(ToolError);

impl From<ToolError> for ToolFailure {
    fn from(error: ToolError) -> Self {
        ToolFailure(error)
    }
}

impl AuditableError for ToolFailure {
    fn audit_error_code(&self) -> &'static str {
        self.0.code()
    }

    fn audit_public_message(&self) -> String {
        self.0.message().to_string()
    }

    /// **AI に返した失敗応答の本文をそのまま記録する**（PR-F レビュー C-4）。
    ///
    /// 成功時は応答 body（読み取り系は
    /// [`ToolSuccess::with_audit_summary`] の要約）が `audit_log.output` に
    /// 載るのに、失敗時が
    /// `{"message": ...}` だけだと、`hint.suggested_lines` /
    /// `candidate_accounts` / `difference` / `policy_notes` / `line` が
    /// 記録に残らない。**`hint` は AI の次の記帳内容を直接決める提案**であり、
    /// 「サーバが何を返して次の一手を誘導したか」を後から追う目的
    /// （`DECISIONS.md` D-070 / `docs/07-mcp-server.md` §9）に対して
    /// 失敗側だけ情報が薄くなる。
    ///
    /// [`ToolError::to_json`] の `message` は `public_message()` 由来であり、
    /// `Display`（下位層の生メッセージ）はここに入らない。
    fn audit_output_json(&self) -> Option<String> {
        Some(self.0.to_json().to_string())
    }
}

/// Phase 3 のツール1件。**1ツール1ファイル**（`src/tools/<ツール名>.rs`）。
///
/// この trait を実装しただけでは呼び出されない。ルータに載るのは
/// [`route`] を通ったものだけで、[`route`] は必ず [`call`]（＝監査ログで
/// 挟む経路）を経由する。
pub trait McpTool: Send + Sync + 'static {
    /// 線上の入力 DTO。`input_schema` はこの型から生成する。
    type Input: DeserializeOwned + JsonSchema + Send + 'static;

    /// MCP のツール名。`docs/07-mcp-server.md` §2 の11件のいずれか。
    const NAME: &'static str;

    /// ツールの説明文（AI が最初に読む）。
    ///
    /// `CLAUDE.md` §10（税務判断を断定しない／「法令に準拠」と書かない）と
    /// §11（次の手が分かる文言）はこの文面にも及ぶ。
    /// `docs/07-mcp-server.md` §5 の「金額は文字列」もここに書く。
    const DESCRIPTION: &'static str;

    /// ツール本体。
    ///
    /// **応答（`CallToolResult`）を組み立てないこと。** 成功時の JSON は
    /// [`ToolSuccess`]、失敗時は [`ToolFailure`]（＝[`ToolError`]）で返し、
    /// `isError` の扱いと監査ログは [`call`] に任せる。
    fn run(
        ctx: &ToolContext<'_>,
        input: Self::Input,
    ) -> impl Future<Output = Result<ToolSuccess, ToolFailure>> + Send;
}

// ---------------------------------------------------------------------------
// 呼び出し（唯一の経路）
// ---------------------------------------------------------------------------

/// ツールを**監査ログで挟んで**1回実行し、MCP の応答を組み立てる。
///
/// 手順（`docs/07-mcp-server.md` §9 / `DECISIONS.md` D-070 / D-076）:
///
/// 1. `request_id` を採番する（JSON-RPC の `id` は流用しない）
/// 2. 受け取った引数**そのもの**を `audit_log.input` 用の JSON にする
/// 3. [`with_audit`] に渡す。開始レコードが書けなければ **ツール本体は
///    一度も呼ばれない**（fail-closed）
/// 4. 入力 DTO へのデシリアライズも**操作の中**で行う。金額を JSON number で
///    渡された場合（MC-09）のような入力エラーも、開始・結果の2行として
///    監査ログに残る
/// 5. 結果レコードが書けなかった場合の警告は
///    [`kaikei_app::audit::AuditedCall::into_result`] が注記に積み、
///    ここが応答の `warnings` に必ず載せる（fail-open。捨てられる形にしない）
///
/// エラーは**すべてツール結果エラー**（`isError: true`）で返す。
/// JSON-RPC のプロトコルエラーは返さない（D-071）。
pub async fn call<T: McpTool>(
    runtime: &Runtime,
    arguments: Option<Map<String, Value>>,
) -> CallToolResult {
    // レジストリ由来でない名前は audit に載せる前にここで止まる。
    // 到達しうるのは「`route` を通さずに `call` を呼んだ」場合だけである。
    let Some(tool) = ToolName::resolve(T::NAME) else {
        return ToolError::new(
            codes::REJECTED,
            format!(
                "ツール {} は登録されていません。tools/list に出ているツール名を\
                 使ってください",
                T::NAME
            ),
        )
        .into_call_tool_result();
    };

    let request_id = RequestId::new_v7();
    let input_json = arguments
        .as_ref()
        .map(|args| Value::Object(args.clone()).to_string());
    let audit_call = AuditCall {
        request_id,
        actor: actor::MCP,
        tool: tool.as_str(),
        input_json: input_json.as_deref(),
    };

    let audited = with_audit(
        runtime.audit_sink.as_ref(),
        &runtime.clock,
        &audit_call,
        || async {
            reject_nul_in_input::<T>(arguments.as_ref())?;
            let input = deserialize_input::<T>(arguments)?;
            let ctx = ToolContext::new(runtime);
            T::run(&ctx, input).await
        },
        |success| AuditSuccess {
            entry_id: success.entry_id,
            output_json: Some(success.audit_output_json()),
        },
    )
    .await;

    let audited = match audited {
        Ok(audited) => audited,
        Err(unavailable) => {
            // ★fail-closed★ 帳簿は変更されていない。
            // 診断用の `Display`（下位層の生メッセージを含む）は stderr へ、
            // 応答には `public_message()` だけを載せる
            // （`docs/07-mcp-server.md` §9。`cause.public_message()` を
            // 出すと「訂正は逆仕訳で」という的外れな案内が復活する）。
            eprintln!("[kaikei-mcp] {unavailable}");
            return ToolError::new(unavailable.code(), unavailable.public_message())
                .into_call_tool_result();
        }
    };

    // ★fail-open★ 警告を受け取る唯一の既定経路。`into_parts_unchecked`
    // （逃げ道）はこの crate では使わない（`docs/07-mcp-server.md` §9）。
    //
    // `into_result` ではなく `into_result_noting_outcome` を使う。
    // 応答がここでは成功にも失敗にもなるので、拒否応答に
    // 「操作は完了しました……やり直さないでください」が載ると、同じ応答が
    // 示している次の手（入力を直して再送）と矛盾する（`CLAUDE.md` §11）。
    let mut warnings: Vec<String> = Vec::new();
    match audited.into_result_noting_outcome(&mut warnings) {
        Ok(success) => {
            let mut body = success.body;
            insert_warnings(&mut body, warnings);
            CallToolResult::structured(Value::Object(body))
        }
        Err(failure) => {
            let mut value = failure.0.to_json();
            if let Some(object) = value.as_object_mut() {
                insert_warnings(object, warnings);
            }
            CallToolResult::structured_error(value)
        }
    }
}

/// fail-open の警告を応答へ載せる（無ければキーごと出さない）。
///
/// # 予約キーの衝突で**何も失わせない**（PR-F レビュー D-3）
///
/// 素朴に `body.insert(WARNINGS_KEY, ..)` と書くと、ツールが `warnings` を
/// 使っていた場合にその値が黙って消える。しかも消えるのは**fail-open の
/// ときだけ**なので、正常系のテストでは永久に検出できない。
///
/// そこで2段で手当てする。
///
/// 1. [`ToolSuccess::new`] / [`ToolError::to_json`] を通った body に
///    `warnings` があれば、警告の有無に関わらず `debug_assert!` で落とす
///    （[`assert_warnings_key_is_free`]）。開発中・テスト中の**毎回**の
///    呼び出しで踏むので、fail-open を再現しなくても気づける
/// 2. それでも release で衝突した場合は**併合する**。既存の配列には
///    後ろに足し、配列でない値は先頭要素として残す。監査ログの警告も
///    ツールの値も捨てない
fn insert_warnings(body: &mut Map<String, Value>, warnings: Vec<String>) {
    assert_warnings_key_is_free(body);
    if warnings.is_empty() {
        return;
    }
    let mut merged = match body.remove(WARNINGS_KEY) {
        Some(Value::Array(existing)) => existing,
        Some(other) => vec![other],
        None => Vec::new(),
    };
    merged.extend(warnings.into_iter().map(Value::String));
    body.insert(WARNINGS_KEY.to_string(), Value::Array(merged));
}

/// ツールが予約キー `warnings` を使っていないことを**毎回**確かめる。
///
/// `debug_assert!` にしてあるのは、これが**サーバ側の実装の誤り**であって
/// 呼び出し元（AI）が直せるものではないためである。記帳が成功している
/// 応答をこの理由でエラーに変えるのは実害が大きい（既に確定した記帳の
/// 結果を AI に渡せなくなる）。開発・テストでは必ず落ちる。
fn assert_warnings_key_is_free(body: &Map<String, Value>) {
    debug_assert!(
        !body.contains_key(WARNINGS_KEY),
        "ツールの応答本文が予約キー \"{WARNINGS_KEY}\" を使っています。\
         このキーは dispatch 層が fail-open の警告を載せるために予約しています\
         （crates/kaikei-mcp/src/dispatch.rs）。別のキー名にしてください"
    );
}

// ---------------------------------------------------------------------------
// 入力に混じった U+0000 の受け皿
// ---------------------------------------------------------------------------

/// 入力に **U+0000（NUL）** が混じっていたら、**どこに入っているか**を添えて
/// 拒否する（PR-F レビュー C-3）。
///
/// # なぜここで見るのか
///
/// U+0000 は JSON としても JSON-RPC としても正当だが、PostgreSQL の
/// `text` にも `jsonb` にも格納できない。素通しすると `description` に
/// 1文字混ざっただけで帳簿側の INSERT が `RepoError::Corrupt` になり、
/// その `public_message()` は
/// 「この操作は完了していません。**入力を変えても解消しません**。
/// サーバのログを添えて管理者に連絡してください」——**断定が事実と逆**である
/// （実際には1文字取り除けば通る）。`docs/07-mcp-server.md` §9 が D-038 の
/// 誤診クラスとして名指しで避けている形（正常なサーバを「壊れている」と
/// 誤診させ、AI が原因である自分の1文字に辿り着けない）が帳簿側で再発する。
///
/// 監査ログ側は D-075 のとおり無害化して**記録を残す**のが正しい
/// （記録できない入力でも「誰が何をしようとしたか」は残す）。
/// 帳簿側は**保存できないものを受理しない**のが正しい。
/// この関数は後者だけを担い、[`with_audit`] の操作の中で走るので、
/// この呼び出しも監査ログには2行残る。
///
/// # Errors
///
/// いずれかの文字列（オブジェクトのキーを含む）に U+0000 があれば
/// [`codes::REJECTED`]（入力を直せば通る拒否）。
fn reject_nul_in_input<T: McpTool>(
    arguments: Option<&Map<String, Value>>,
) -> Result<(), ToolFailure> {
    let Some(arguments) = arguments else {
        return Ok(());
    };
    let Some(found) = find_nul_in_object(arguments, "") else {
        return Ok(());
    };

    Err(ToolError::new(
        codes::REJECTED,
        format!(
            "入力の {location} に制御文字 U+0000（NUL）が含まれています\
             （{position} 文字目）。この文字は帳簿に保存できないため、\
             {tool} は実行していません。帳簿は変更されていません。\
             該当箇所からその1文字を取り除いて送り直してください\
             （他の箇所は直す必要がありません）",
            location = found.location,
            position = found.char_position,
            tool = T::NAME,
        ),
    )
    .with_detail("field", json_string(&found.location))
    .into())
}

/// U+0000 が見つかった位置。
struct NulLocation {
    /// 入力の中の場所（`description` / `lines[1].memo` / `tags` のキーなど）。
    location: String,
    /// その文字列の何文字目か（1 始まり。`char` 単位）。
    char_position: usize,
}

fn json_string(text: &str) -> Value {
    Value::String(text.to_string())
}

/// 子要素の場所を親の場所に連ねる（`lines` + `[1]` + `.memo`）。
fn join_location(parent: &str, child: &str) -> String {
    if parent.is_empty() {
        child.trim_start_matches('.').to_string()
    } else {
        format!("{parent}{child}")
    }
}

fn find_nul(value: &Value, location: &str) -> Option<NulLocation> {
    match value {
        Value::String(text) => nul_position(text).map(|char_position| NulLocation {
            location: location.to_string(),
            char_position,
        }),
        Value::Array(items) => items.iter().enumerate().find_map(|(index, item)| {
            find_nul(item, &join_location(location, &format!("[{index}]")))
        }),
        Value::Object(map) => find_nul_in_object(map, location),
        _ => None,
    }
}

fn find_nul_in_object(map: &Map<String, Value>, location: &str) -> Option<NulLocation> {
    map.iter().find_map(|(key, value)| {
        // キー自体に混じっている場合も見る（JSONB のキーにも入らない）。
        if let Some(char_position) = nul_position(key) {
            // 場所を示す文字列に U+0000 をそのまま入れない
            // （応答も監査ログも同じ文字で詰まる）。
            let shown = key.replace('\0', "\\u0000");
            return Some(NulLocation {
                location: join_location(location, &format!(".{shown}（キー名）")),
                char_position,
            });
        }
        find_nul(value, &join_location(location, &format!(".{key}")))
    })
}

/// 文字列の何文字目に U+0000 があるか（1 始まり。無ければ `None`）。
fn nul_position(text: &str) -> Option<usize> {
    text.chars().position(|c| c == '\0').map(|index| index + 1)
}

/// 受け取った引数を入力 DTO にする。
///
/// # なぜ `Parameters<T>` を使わず自前でやるのか
///
/// `rmcp` の `Parameters<T>` はツール本体に**入る前**にデシリアライズし、
/// 失敗を `CallToolResult::error`（テキストのみ）に変換する。その経路は
/// [`call`] の外側なので、**監査ログに1行も残らない**。
/// `docs/07-mcp-server.md` §10 MC-09 の (3)「この呼び出しも audit_log に
/// 残る」を満たすには、デシリアライズを操作の中で行う必要がある。
/// 構造化コンテンツ（`error` / `message`）で返せるという副次的な利点もある。
fn deserialize_input<T: McpTool>(
    arguments: Option<Map<String, Value>>,
) -> Result<T::Input, ToolFailure> {
    let value = Value::Object(arguments.unwrap_or_default());
    serde_json::from_value::<T::Input>(value).map_err(|source| {
        ToolError::new(
            codes::REJECTED,
            format!(
                "{tool} の引数を解釈できませんでした: {source}。\
                 引数の形は tools/list の inputSchema を参照してください\
                 （金額は文字列で渡します。例: \"110000\"）",
                tool = T::NAME,
            ),
        )
        .into()
    })
}

// ---------------------------------------------------------------------------
// レジストリへの登録（唯一の経路）
// ---------------------------------------------------------------------------

/// `tools/list` と `tools/call` が引くレジストリ。`rmcp` の `ToolRouter` を
/// **private フィールドとして包む**型。
///
/// # 何が閉じているか（PR-F レビュー B-1 / 3巡目 B）
///
/// `ToolRouter` を [`crate::server`] に持たせていたときは、そこで
/// `with_async_tool::<T>()` / `with_sync_tool::<T>()`（`rmcp` が `ToolBase` +
/// `AsyncTool` の実装型に対して用意している登録口）を呼べば、
/// **[`call`] を通らないツールをルータに載せられた**。その形は `ToolRoute` も
/// `CallToolResult` も `with_audit` も書かないので、識別子の走査では捕まらない。
///
/// この型はツールを載せる口を [`ToolRegistry::with`]（境界が
/// `T: `[`McpTool`]）だけにし、内側の `ToolRouter` を外に出さない。
/// **この型を経由して `McpTool` 以外を載せる方法は型として無い。**
///
/// **ただし「別のルータを新しく作る」ことは型では止まらない。**
/// `rmcp` は直接依存であり `ToolRouter` は `pub` なので、同一 crate の
/// 他モジュールが `use rmcp::...` するのを妨げる仕組みは Rust に無い。
/// そちらを止めているのはモジュール doc の**ファイル許可リスト**
/// （`rmcp` を名指しできるのは `dispatch.rs` と `error.rs` だけ）である。
///
/// [`ToolRegistry::list_all`] / [`ToolRegistry::get`] と private な
/// [`ToolRegistry::call`] は、このファイルが手書きしている
/// [`ServerHandler`] の `list_tools` / `get_tool` / `call_tool` の実体である。
#[derive(Clone)]
pub struct ToolRegistry {
    router: ToolRouter<KaikeiServer>,
}

impl ToolRegistry {
    /// 空のレジストリ。
    #[must_use]
    pub fn new() -> Self {
        ToolRegistry {
            router: ToolRouter::new(),
        }
    }

    /// ツールを1件載せる。**唯一の登録経路。**
    ///
    /// ハンドラの中身は必ず [`call`]（＝監査ログで挟む経路）である。
    ///
    /// # Panics
    ///
    /// `T::Input` から `input_schema` を生成できない場合（`schemars` が
    /// オブジェクト以外のスキーマを返す型を入力に使った場合）。起動時に
    /// レジストリを組み立てた時点で必ず露見するので、ツール応答には現れない。
    #[must_use]
    pub fn with<T: McpTool>(mut self) -> Self {
        self.router = self.router.with_route(route::<T>());
        self
    }

    /// `tools/call`（下の [`ServerHandler`] 実装の `call_tool` の実体）。
    ///
    /// 未登録の名前には `invalid_params: tool not found`
    /// （`docs/07-mcp-server.md` §6 が認める唯一のプロトコルエラー）を返す。
    ///
    /// **private。** `ToolCallContext` を引数に持つメソッドを公開すると、
    /// 他モジュールが `rmcp` を名指しせずに `tools/call` の入口へ届いて
    /// しまう（ファイル許可リストの抜け道になる）。
    async fn call(
        &self,
        context: ToolCallContext<'_, KaikeiServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        self.router.call(context).await
    }

    /// `tools/list`（`list_tools` の実体）。
    #[must_use]
    pub fn list_all(&self) -> Vec<Tool> {
        self.router.list_all()
    }

    /// ツール定義を名前で引く（`get_tool` の実体）。
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Tool> {
        self.router.get(name)
    }

    /// そのツール名が登録されているか（[`ToolRegistry::call`] が
    /// 「tool not found」を返すかどうかを決めている述語）。
    #[must_use]
    pub fn has_route(&self, name: &str) -> bool {
        self.router.has_route(name)
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// ツールを `rmcp` のルータに載せる形にする。
///
/// **private。** 外から使う口は [`ToolRegistry::with`] だけである
/// （`ToolRoute` を作れる場所を1箇所に保つ。
/// `tests/audit_is_structural.rs` が見張る）。
fn route<T: McpTool>() -> ToolRoute<KaikeiServer> {
    ToolRoute::new_dyn(
        tool_attr::<T>(),
        |ctx: ToolCallContext<'_, KaikeiServer>| {
            Box::pin(async move {
                let runtime = ctx.service.runtime();
                let result = call::<T>(runtime, ctx.arguments).await;
                Ok(result.into())
            })
        },
    )
}

/// `tools/list` に出るツール定義（名前・説明・入力スキーマ）。
fn tool_attr<T: McpTool>() -> Tool {
    let schema = schema_for_input::<T::Input>().unwrap_or_else(|error| {
        panic!("ツール {} の入力スキーマを生成できません: {error}", T::NAME)
    });
    Tool::new(T::NAME, T::DESCRIPTION, schema)
}

// ---------------------------------------------------------------------------
// MCP のサーバーハンドラ（`tools/list` と `tools/call` の入口）
// ---------------------------------------------------------------------------

/// `rmcp` が JSON-RPC の要求を配る先。
///
/// # `#[tool_handler]` を使わず手書きする（PR-F レビュー3巡目 B）
///
/// `rmcp-macros` 3.1.0 の `#[tool_handler]` は
/// `if !has_method("call_tool", &item_impl)` で条件付き生成する。つまり
/// **同じ impl ブロックに `call_tool` を手書きすると、マクロが生成する
/// dispatch 経路が黙って置き換わる**。実測では `tools/list` は正規の2件の
/// まま、`tools/call` を1回送ると `journal_entries` に1件・`audit_log` に
/// 0行で、`cargo build` / `clippy -D warnings` / `cargo test` は全緑だった。
/// しかも当時この impl は既に `get_info` を手書きしており、
/// 「メソッドを自分で書けばマクロが引き下がる」形が見本として置かれていた。
///
/// マクロを外して4つとも手書きにすれば、生成物と手書きが入れ替わるという
/// 事象そのものが起きない。中身はマクロが生成していたものと同じで、
/// `call_tool` は [`ToolRegistry::call`]（＝[`route`] が載せたハンドラ＝
/// [`call`]）へ委譲する。
///
/// この impl がこのファイルに在るのは、モジュール doc の許可リスト
/// （`rmcp` を名指しできるのは `dispatch.rs` と `error.rs` だけ）による。
/// **「`call_tool` を手書きする」形自体が許可リストの内側に入っている**ので、
/// これを別のファイルで書き直す迂回は検査で落ちる。
impl ServerHandler for KaikeiServer {
    fn get_info(&self) -> ServerInfo {
        crate::server::server_info()
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let call_context = ToolCallContext::new(self, request, context);
        self.tools().call(call_context).await
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult {
            result_type: Some(ResultType::COMPLETE),
            tools: self.tools().list_all(),
            meta: None,
            next_cursor: None,
            ttl_ms: None,
            cache_scope: None,
        })
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tools().get(name).cloned()
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
/// この関数が `server.rs` ではなくここに在るのは、`rmcp` のトランスポートを
/// 名指しするためである（モジュール doc の許可リスト）。
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

    // レジストリに無い名前からは `ToolName` を作れない
    // （＝`audit_log.tool` に載せられない）。
    #[test]
    fn tool_name_cannot_be_built_from_a_name_outside_the_registry() {
        for name in [
            "delete_journal_entry",
            "execute_sql",
            "post_journal_entry\u{0}",
            "",
            "close_period",
        ] {
            assert!(
                ToolName::resolve(name).is_none(),
                "レジストリに無い名前から ToolName ができています: {name:?}"
            );
        }
    }

    // 登録済みの名前からは作れる（上の検査が「常に None」で緑になっていない
    // ことの対照実験）。
    #[test]
    fn tool_name_resolves_the_registered_tools() {
        use crate::tools::post_journal_entry::PostJournalEntry;
        use crate::tools::reverse_journal_entry::ReverseJournalEntry;

        assert!(
            !crate::server::registered_tool_names().is_empty(),
            "レジストリが空です。対照実験になりません"
        );
        for name in [PostJournalEntry::NAME, ReverseJournalEntry::NAME] {
            assert_eq!(
                ToolName::resolve(name).map(|resolved| resolved.as_str()),
                Some(name),
            );
        }
    }

    // U+0000 が混じった名前は audit に載る前に弾かれる
    // （TEXT 列は無害化を通らない。`docs/07-mcp-server.md` §9）。
    #[test]
    fn a_nul_character_in_the_tool_name_never_reaches_the_audit_log() {
        assert!(ToolName::resolve("post_journal_entry\u{0}").is_none());
    }

    // ---- 入力に混じった U+0000（PR-F レビュー C-3）----

    /// 検査用のツール型（`reject_nul_in_input` はツール名しか使わない）。
    struct ProbeTool;

    impl McpTool for ProbeTool {
        type Input = Value;
        const NAME: &'static str = "post_journal_entry";
        const DESCRIPTION: &'static str = "検査用";
        async fn run(_: &ToolContext<'_>, _: Self::Input) -> Result<ToolSuccess, ToolFailure> {
            unreachable!("この検査では呼ばない")
        }
    }

    fn reject_nul(arguments: Value) -> Option<ToolError> {
        let arguments = arguments.as_object().cloned().expect("オブジェクト");
        reject_nul_in_input::<ProbeTool>(Some(&arguments))
            .err()
            .map(|failure| failure.0)
    }

    // 「入力を変えても解消しません」と誤診しない。**自分の1文字**に辿り着ける。
    #[test]
    fn a_nul_in_the_input_is_rejected_with_the_position_that_carries_it() {
        let error = reject_nul(serde_json::json!({
            "entry_date": "2026-04-15",
            "description": "A\u{0}B"
        }))
        .expect("U+0000 は拒否される");

        assert_eq!(error.code(), codes::REJECTED);
        let message = error.message();
        assert!(message.contains("description"), "{message}");
        assert!(message.contains("2 文字目"), "{message}");
        assert!(message.contains("U+0000"), "{message}");
        // 次の手（`CLAUDE.md` §11）。
        assert!(message.contains("取り除いて送り直して"), "{message}");
        assert!(message.contains("帳簿は変更されていません"), "{message}");
        // ★誤診しない★ 入力を直せば通るのに「解消しません」と言わない。
        assert!(!message.contains("入力を変えても解消しません"), "{message}");
        assert!(!message.contains("管理者に連絡"), "{message}");
        assert_eq!(
            error.to_json()["field"],
            Value::String("description".into())
        );
    }

    // 入れ子（明細の memo・タグの値・タグのキー）でも場所が分かる。
    #[test]
    fn a_nul_deep_in_the_input_reports_the_path_to_it() {
        let error = reject_nul(serde_json::json!({
            "lines": [
                { "account": "100", "memo": "ok" },
                { "account": "500", "memo": "A\u{0}" }
            ]
        }))
        .expect("U+0000 は拒否される");
        assert!(
            error.message().contains("lines[1].memo"),
            "{}",
            error.message()
        );

        let error = reject_nul(serde_json::json!({
            "lines": [ { "tags": { "counterparty": "CP\u{0}1" } } ]
        }))
        .expect("U+0000 は拒否される");
        assert!(
            error.message().contains("lines[0].tags.counterparty"),
            "{}",
            error.message()
        );

        let error = reject_nul(serde_json::json!({ "ta\u{0}gs": "x" }))
            .expect("キー名の U+0000 も拒否される");
        assert!(error.message().contains("キー名"), "{}", error.message());
        // 場所の表示に U+0000 をそのまま入れない。
        assert!(!error.message().contains('\0'), "{}", error.message());
    }

    // U+0000 を含まない入力は素通りする（「常に拒否」で緑になっていない）。
    #[test]
    fn an_input_without_a_nul_passes_through() {
        assert!(reject_nul(serde_json::json!({
            "entry_date": "2026-04-15",
            "description": "A社への請求",
            "lines": [ { "account": "135", "amount": "110000" } ]
        }))
        .is_none());
        assert!(reject_nul_in_input::<ProbeTool>(None).is_ok());
    }

    // ---- fail-open の警告を載せるキー（PR-F レビュー D-3）----

    // 警告が無ければキーごと出さない。
    #[test]
    fn no_warnings_key_appears_when_there_is_nothing_to_warn_about() {
        let mut body = Map::new();
        body.insert("entry_no".to_string(), Value::from(1));
        insert_warnings(&mut body, Vec::new());
        assert!(body.get(WARNINGS_KEY).is_none());
    }

    // ツールが `warnings` を使っていても**何も失わない**（併合する）。
    //
    // 予約キーの衝突は debug ビルドでは `debug_assert!` が落とすので、
    // ここでは release での併合だけを見る（`debug_assert!` が有効な
    // テストビルドでは併合経路に入る前に落ちる）。
    #[test]
    #[cfg(not(debug_assertions))]
    fn a_tool_supplied_warnings_value_is_merged_instead_of_being_dropped() {
        let mut body = Map::new();
        body.insert(
            WARNINGS_KEY.to_string(),
            Value::Array(vec![Value::String("ツールの警告".to_string())]),
        );
        insert_warnings(&mut body, vec!["監査ログの警告".to_string()]);

        let merged = body[WARNINGS_KEY].as_array().expect("配列");
        assert_eq!(merged.len(), 2, "{merged:?}");
        assert_eq!(merged[0], Value::String("ツールの警告".to_string()));
        assert_eq!(merged[1], Value::String("監査ログの警告".to_string()));
    }

    // 予約キーの衝突は**警告の有無に関わらず**毎回落ちる
    // （fail-open を再現しないと気づけない、という形にしない）。
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "予約キー")]
    fn using_the_reserved_warnings_key_fails_loudly_even_without_a_warning() {
        let mut body = Map::new();
        body.insert(WARNINGS_KEY.to_string(), Value::from("ツールが置いた値"));
        insert_warnings(&mut body, Vec::new());
    }
}
