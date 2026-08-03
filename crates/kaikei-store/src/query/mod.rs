//! read model（SQL集計）。`CLAUDE.md` §6「read model は物理的に分離する」の
//! 原則により、書き込み側（`Store`/`PgTx`。PR-5 本体）を一切経由せず、SQL から
//! `kaikei_app::view` の DTO へ直行する。書き込みはドメインモデル経由
//! （`journal` 等）、読み取りは SQL 集計。この2つを混ぜない。
//!
//! | モジュール | 対応するポート | 実装 Phase |
//! |---|---|---|
//! | [`trial_balance`] | `kaikei_app::ports::TrialBalanceQuery` | Phase 1 |
//! | [`search`] | `kaikei_app::ports::SearchEntriesQuery` | **Phase 3（PR-H）** |
//! | [`ledger`] | `kaikei_app::ports::LedgerQuery` | **Phase 3（PR-H）** |
//!
//! Phase 3 で `search` / `ledger` を新設したのは、`ROADMAP.md` Phase 3 の
//! 成果物「読み取り系ツール」＝ `kaikei-mcp` の `search_entries` /
//! `get_ledger` に対応するためである（`DECISIONS.md` D-070 の決定1、
//! `docs/07-mcp-server.md` §2・§4）。Phase 3 は「この時点で自分の帳簿を
//! 付け始める」＝ドッグフーディングの起点であり、記帳したものを検索も
//! 元帳確認もできない状態では、記帳の誤りに気づく手段が無い。
//!
//! 仕訳明細の個別取得（entry_detail）は read model を新設していない。
//! `get_entry` が扱うのは仕訳1件で、集約をそのまま返す
//! `JournalRepo::find_entry` で足りるためである（集計も結合も無い経路に
//! read model を増やすと、同じ復元処理が2箇所に育つ）。
//!
//! 新しい read model はここ（`query/`）に追加すること。MCP 層に SQL を書いたり、
//! `JournalRepo` で全件ロードして絞り込んだりしない。

pub mod ledger;
pub mod search;
pub mod trial_balance;

pub use ledger::PgLedgerQuery;
pub use search::PgSearchEntriesQuery;
pub use trial_balance::PgTrialBalanceQuery;
