//! MCP サーバー（stdio）。AI エージェントに会計操作を開く presentation 層。
//!
//! **この crate は薄い層である**（`docs/07-mcp-server.md` §4）。
//! ビジネスロジックも、線上表現の語彙も、ここには書かない。
//!
//! # ここに書かないもの（PR-B が公開 API として用意済み）
//!
//! | 線上に出るもの | 入口 |
//! |---|---|
//! | エラーの分類コード | [`kaikei_app::error::AppError::code`] / `RepoError::code` / `core_error_code` / `policy_error_code` |
//! | エラーの本文 | [`kaikei_app::error::AppError::public_message`] |
//! | 金額の文字列化 | [`kaikei_app::amount::money_to_plain_string`] / `strip_thousands_separators` |
//! | `side` / `account_type` / `severity` / `fiscal_year_rule` | [`kaikei_app::wire`] |
//! | 仕訳IDの表記とパース | [`kaikei_app::id`] |
//! | `tax_mode` / `rounding` / `rounding_unit` | `kaikei_jp::tax` の `as_code` / `from_code` |
//! | タグ値の相互変換 | `kaikei_jp::tags::TagCatalog` / `tag_value_to_string` |
//!
//! 同じ対応表を `kaikei-mcp` / `kaikei-api` / `audit_log` の3箇所に手書きすると
//! 必ず綴りがずれる（`DECISIONS.md` D-072）。**再実装しないこと。**
//!
//! # この crate が持ってよいもの
//!
//! - 線上の DTO（[`wire`]）。`kaikei-core` / `kaikei-app` の型は serde を
//!   実装しないため、詰め替えはこの層の責務である（`docs/07-mcp-server.md` §4）。
//! - `kaikei_jp::JpError` → 分類コードの対応表（[`error`]）。`kaikei-app` は
//!   `kaikei-jp` に依存できないため、ここが唯一の置き場になる（同 §6）。
//! - ツールレジストリ（[`server`]）と、**監査ログで挟む唯一の呼び出し経路**
//!   （[`dispatch`]）。
//!
//! # 監査ログを通らないツールを書けなくする
//!
//! `ROADMAP.md` Phase 3 の完了条件は「全操作が audit_log に記録される」で
//! ある。それを11ツールの手作業（各ツールが `with_audit` を呼ぶ）に委ねると、
//! **書き忘れても正常系のテストが全て緑のまま通る**（`DECISIONS.md` D-076 が
//! fail-closed について挙げた却下理由と同じ性質）。
//!
//! そこで [`dispatch`] が「呼び忘れる形が存在しない」ようにしてある。
//! ツールが実装する [`dispatch::McpTool`] は応答（`CallToolResult`）を
//! 組み立てられず、受け取る [`dispatch::ToolContext`] は
//! [`kaikei_app::ports::AuditSink`] を露出しない。
//!
//! **ただし「書けない」を型だけで達成してはいない。** MCP のツールを
//! 登録・実行する能力は `rmcp` の API から来るので、`rmcp` を名指しできる
//! ファイルを **`dispatch.rs` と `error.rs` の2つに限る許可リスト**を
//! `tests/audit_is_structural.rs` が機械的に検査している。
//! 詳細は [`dispatch`] のモジュール doc（`DECISIONS.md` D-084）。
//!
//! # このPR（Phase 3 PR-H）の範囲
//!
//! | PR | 内容 | 状態 |
//! |---|---|---|
//! | PR-D | `wire.rs` / `server.rs` / `error.rs`（骨組み） | 済 |
//! | PR-E | 合成ルート（[`config`] / [`startup`] / `src/main.rs`） | 済 |
//! | PR-F | dispatch 層（[`dispatch`]）と書き込み系ツール2件（[`tools`]） | 済 |
//! | PR-G | 残りの読み取り系5件（`list_accounts` / `get_entry` / `get_trial_balance` / `list_tax_categories` / `get_settings`）と提案系・検証系2件 | 未 |
//! | PR-H | `search_entries` / `get_ledger`（read model を新設した） | **このPR** |
//!
//! 状態はこの crate の `server.rs`（レジストリ）が実物である。**同じ表を
//! 2箇所で手入れすると必ず割れる**ので、片方を直したらもう片方も直すこと
//! （PR-H レビュー D-1。この表が「PR-F がこのPR」のまま `server.rs` だけ
//! PR-H を反映していた）。
//!
//! # stdout は JSON-RPC 専用チャネル
//!
//! stdio トランスポートでは、`println!` や stdout に出る `tracing` が1行でも
//! 混ざるとプロトコルが壊れて接続ごと落ちる。ログ・診断出力は必ず **stderr**
//! に出すこと（`docs/07-mcp-server.md` §4）。
//!
//! この crate は `tracing_subscriber` を持たない。購読者を登録しない限り
//! `tracing` のイベントはどこにも出ないため、下位層の `tracing::warn!` が
//! stdout に漏れることはない。**購読者を入れる場合は writer を stderr に
//! 固定すること**（既定は stdout）。

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod config;
pub mod dispatch;
pub mod error;
pub mod server;
pub mod startup;
pub mod tools;
pub mod wire;
