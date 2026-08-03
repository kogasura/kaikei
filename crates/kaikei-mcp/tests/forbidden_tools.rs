//! MC-10: **「存在させないツール」がレジストリに現れないことの機械的検査。**
//!
//! `docs/07-mcp-server.md` §2 / §10 より:
//!
//! > **禁止リストをテスト側の定数にして4件すべてをループで検査する**
//! > （1件だけの検査では他が復活しても緑のまま通る）。
//!
//! 4件は**理由が同じではない**（同 §2。混ぜないこと）:
//!
//! | ツール | 理由 |
//! |---|---|
//! | `delete_journal_entry` | D-014。訂正は `reverse_journal_entry` のみ。**将来も作らない** |
//! | `update_journal_entry` | D-014。同上 |
//! | `execute_sql` | D-014。同上 |
//! | `reopen_period` | D-014 の対象ではない。締め（`close_period`）自体が Phase 4 以降なので**今は作りようがない**。取り消し手段を設けるかは `close_period` を実装する Phase で決める |
//!
//! MCP に登録しないことは4層の防御のうち最も外側の1層にすぎない
//! （残り3層は DB ロール権限・トリガ・`JournalRepo` のポート定義。同 §1 ①）。
//! ここで閉じるのはその1層だけである。

//! # なぜサーバーを組み立てないのか
//!
//! `KaikeiServer` は実行時依存（`Runtime`）を必須で持つ（`src/server.rs`）。
//! レジストリに何が載っているかは DB にも設定にも依存しない性質なので、
//! それを見るために DB 接続を要求しない。検査は
//! `kaikei_mcp::server` の自由関数（`registered_tool_names` /
//! `is_registered_tool` / `tool_definition`）を通す。3つとも
//! サーバー本体と**同じ `tool_router()`** から導出しており、
//! `#[tool_handler]` が生成する `list_tools` / `call_tool` / `get_tool` が
//! 引くのと同じ集合を見る（対応表は `src/server.rs` のモジュール doc）。
//!
//! 本物の `KaikeiServer`（`ServerHandler::get_tool` 経由）でも同じ結果に
//! なることは、`Runtime` を組み立てられる `tests/startup_pg.rs`
//! （`pg-tests`）が確かめる。

use kaikei_mcp::server::{is_registered_tool, registered_tool_names, tool_definition};

/// **存在させないツール**（`docs/07-mcp-server.md` §2）。
const FORBIDDEN_TOOLS: [&str; 4] = [
    "delete_journal_entry",
    "update_journal_entry",
    "execute_sql",
    "reopen_period",
];

/// Phase 3 で MCP に登録してよいツール（`docs/07-mcp-server.md` §2 の11件）。
///
/// 「Phase 4 以降」と書かれたツールは名前を予約しているだけで、Phase 3 では
/// 登録しない（登録しないツールは AI からは存在しないのと同じ）。
const PHASE_3_TOOLS: [&str; 11] = [
    "list_accounts",
    "get_entry",
    "get_trial_balance",
    "search_entries",
    "get_ledger",
    "list_tax_categories",
    "get_settings",
    "post_journal_entry",
    "reverse_journal_entry",
    "suggest_tax_category",
    "validate_invoice_number",
];

// MC-10 (1): `tools/list` の応答に4件のいずれも現れない。
//
// `registered_tool_names` はレジストリ（`ToolRouter::list_all`）から
// 導出しており、`#[tool_handler]` が生成する `list_tools` が返す集合と同一。
#[test]
fn forbidden_tools_are_absent_from_the_tool_list() {
    let registered = registered_tool_names();

    // 空ループで緑になっていないことを先に確かめる（`PROGRESS.md` Phase 1 の
    // 教訓: 検査が実際には1度も走っていなかった、という事故を防ぐ）。
    assert_eq!(
        FORBIDDEN_TOOLS.len(),
        4,
        "禁止リストが4件でなくなっています"
    );

    for forbidden in FORBIDDEN_TOOLS {
        assert!(
            !registered.iter().any(|name| name == forbidden),
            "存在させないツールが登録されています: {forbidden}（docs/07-mcp-server.md §2）"
        );
    }
}

// MC-10 (2): 4件の名前で `tools/call` すると**未知のツール**として拒否される。
//
// `ToolRouter::call` は未登録の名前に対して
// `ErrorData::invalid_params("tool not found")` を返す（＝プロトコルエラー）。
// これは `docs/07-mcp-server.md` §6 が認めている唯一の例外
// （「ツール呼び出しに到達できない異常」）である。
//
// `call` の実行には `RequestContext`（構築に `Peer` が要る）が必要で、
// `rmcp::service::Peer::new` は `pub(crate)` のため外部 crate からは組み立て
// られない。そこで、同じレジストリを引く2つの入口を検査する:
//
// - `tool_definition`（`tool_router.get(name)`。`#[tool_handler]` が生成する
//   `get_tool` の実体と同じ）
// - `is_registered_tool`（`tool_router.has_route(name)`。`call` が
//   「tool not found」を返すかどうかを決めているのはこの述語）
#[test]
fn calling_a_forbidden_tool_is_rejected_as_an_unknown_tool() {
    assert_eq!(
        FORBIDDEN_TOOLS.len(),
        4,
        "禁止リストが4件でなくなっています"
    );

    for forbidden in FORBIDDEN_TOOLS {
        assert!(
            !is_registered_tool(forbidden),
            "存在させないツールがレジストリに登録されています: {forbidden}"
        );
        assert!(
            tool_definition(forbidden).is_none(),
            "存在させないツールの定義が引けてしまいます: {forbidden}"
        );
    }
}

// この検査自体が働いていることの確認（対照実験）。
//
// 禁止リストの検査は「登録されていない」ことを主張するため、レジストリの
// 引き方を間違えていても緑になりうる。実在しうる名前を1つ登録した状態で
// 同じ述語が `true` を返すことを見て、述語がレジストリを実際に見ていることを
// 確かめる。
#[test]
fn the_registry_predicates_actually_observe_registered_tools() {
    use kaikei_mcp::server::KaikeiServer;
    use rmcp::handler::server::router::tool::{ToolRoute, ToolRouter};
    use rmcp::model::{CallToolResult, Tool};
    use std::sync::Arc;

    let router: ToolRouter<KaikeiServer> = ToolRouter::new().with_route(ToolRoute::new_dyn(
        Tool::new("probe_tool", "検査用", Arc::new(Default::default())),
        |_ctx| Box::pin(async { Ok(CallToolResult::success(vec![]).into()) }),
    ));

    assert!(router.has_route("probe_tool"));
    assert!(router.get("probe_tool").is_some());
    assert!(router
        .list_all()
        .iter()
        .any(|tool| tool.name == "probe_tool"));
    // 登録していない名前は引けない。
    assert!(!router.has_route("delete_journal_entry"));
}

// Phase 3 で登録してよいのは §2 の11件だけ。
//
// 禁止リスト（4件）だけを見張ると、`drop_journal_entries` のような**新しい名前**の
// 破壊的ツールが増えても緑のまま通る。許可リスト側からも閉じる。
#[test]
fn every_registered_tool_is_one_of_the_eleven_phase_3_tools() {
    for name in registered_tool_names() {
        assert!(
            PHASE_3_TOOLS.contains(&name.as_str()),
            "Phase 3 の11ツールに無いツールが登録されています: {name}\
             （docs/07-mcp-server.md §2。増やす場合は設計書と DECISIONS.md を先に更新すること）"
        );
    }
}

// 許可リストと禁止リストが交わらない（設計書の写し間違いの検出）。
#[test]
fn the_allow_list_and_the_deny_list_do_not_overlap() {
    for forbidden in FORBIDDEN_TOOLS {
        assert!(
            !PHASE_3_TOOLS.contains(&forbidden),
            "許可リストに存在させないツールが混ざっています: {forbidden}"
        );
    }
}
