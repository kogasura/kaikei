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
//! そこでこのモジュールは「呼び忘れる形が存在しない」ようにしてある。
//!
//! | 塞ぎ方 | 実体 |
//! |---|---|
//! | ツールは `CallToolResult` を組み立てられない | [`McpTool::run`] の戻り値は `Result<`[`ToolSuccess`]`, `[`ToolFailure`]`>` であり、応答（`isError` を含む）を組み立てるのは [`call`] だけ |
//! | ツールは監査ログの記録先に触れない | [`McpTool::run`] が受け取るのは [`ToolContext`] で、[`kaikei_app::ports::AuditSink`] を**露出しない**。[`crate::startup::Runtime`] 自体が渡らない |
//! | [`ToolContext`] を自分で作れない | フィールドも `new` も private。作れるのはこのモジュールだけ |
//! | ルータに載せる経路が1つしかない | [`route`] だけが [`rmcp::handler::server::router::tool::ToolRoute`] を作り、その中身は必ず [`call`] である |
//! | fail-open の警告を捨てられない | [`call`] は [`kaikei_app::audit::AuditedCall::into_result`]（既定経路）しか使わず、積まれた警告を必ず応答の `warnings` に載せる。`into_parts_unchecked`（逃げ道）はこの crate に1箇所も無い |
//!
//! 型で閉じられない残り（`ToolRoute` を直に組み立てる、`with_audit` を
//! ツール側で呼ぶ）は `tests/audit_is_structural.rs` がソースを走査して
//! 見張る。**「型で閉じる → 残りをソース走査で見張る」の順番**であって、
//! ソース走査が主ではない。
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
use kaikei_core::EntryId;
use kaikei_jp::compose::Composition;
use kaikei_store::pool::PgStore;
use rmcp::handler::server::router::tool::ToolRoute;
use rmcp::handler::server::tool::{schema_for_input, ToolCallContext};
use rmcp::model::{CallToolResult, Tool};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::error::ToolError;
use crate::server::KaikeiServer;
use crate::startup::Runtime;

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
}

/// ツールが成功したときに返す値。
///
/// `body` は**応答の `structuredContent` にも `audit_log.output` にも
/// 同じものが載る**（`docs/07-mcp-server.md` §9）。2箇所で別の JSON を
/// 組み立てられる形にしない。
#[derive(Debug, Clone)]
pub struct ToolSuccess {
    body: Map<String, Value>,
    entry_id: Option<EntryId>,
}

impl ToolSuccess {
    /// 応答本体から作る。
    #[must_use]
    pub fn new(body: Map<String, Value>) -> Self {
        ToolSuccess {
            body,
            entry_id: None,
        }
    }

    /// 記帳した仕訳ID（`audit_log.entry_id` に入る）。書き込み系だけが付ける。
    #[must_use]
    pub fn with_entry_id(mut self, entry_id: EntryId) -> Self {
        self.entry_id = Some(entry_id);
        self
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
            let input = deserialize_input::<T>(arguments)?;
            let ctx = ToolContext::new(runtime);
            T::run(&ctx, input).await
        },
        |success| AuditSuccess {
            entry_id: success.entry_id,
            output_json: Some(Value::Object(success.body.clone()).to_string()),
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
    let mut warnings: Vec<String> = Vec::new();
    match audited.into_result(&mut warnings) {
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
fn insert_warnings(body: &mut Map<String, Value>, warnings: Vec<String>) {
    if warnings.is_empty() {
        return;
    }
    body.insert(
        WARNINGS_KEY.to_string(),
        Value::Array(warnings.into_iter().map(Value::String).collect()),
    );
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

/// ツールを `rmcp` のルータに載せる形にする。**唯一の登録経路。**
///
/// ハンドラの中身は必ず [`call`] であり、ツール本体はそこから呼ばれる。
/// `ToolRoute` をここ以外で組み立てないこと
/// （`tests/audit_is_structural.rs` が見張る）。
///
/// # Panics
///
/// `T::Input` から `input_schema` を生成できない場合（`schemars` が
/// オブジェクト以外のスキーマを返す型を入力に使った場合）。起動時に
/// レジストリを組み立てた時点で必ず露見するので、ツール応答には現れない。
pub fn route<T: McpTool>() -> ToolRoute<KaikeiServer> {
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
}
