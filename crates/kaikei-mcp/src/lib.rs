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
//! - ツールレジストリ（[`server`]）。
//!
//! # このPR（Phase 3 PR-D）の範囲
//!
//! **ツールは1つも実装していない。** 骨組みだけである。
//!
//! | 後続 | 内容 |
//! |---|---|
//! | PR-E | 合成ルート（`config.rs` / `startup.rs` / `main.rs`） |
//! | PR-F | 書き込み系ツールと `audit.rs`（監査ログの結線） |
//! | PR-G | 読み取り系・提案系ツール |
//!
//! # stdout は JSON-RPC 専用チャネル
//!
//! stdio トランスポートでは、`println!` や stdout に出る `tracing` が1行でも
//! 混ざるとプロトコルが壊れて接続ごと落ちる。ログ・診断出力は必ず **stderr**
//! に出すこと（`docs/07-mcp-server.md` §4）。

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod error;
pub mod server;
pub mod wire;
