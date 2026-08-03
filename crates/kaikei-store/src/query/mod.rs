//! read model（SQL集計）。`CLAUDE.md` §6「read model は物理的に分離する」の
//! 原則により、書き込み側（`Store`/`PgTx`。PR-5 本体）を一切経由せず、SQL から
//! `kaikei_app::view` の DTO へ直行する。書き込みはドメインモデル経由
//! （`journal` 等）、読み取りは SQL 集計。この2つを混ぜない。
//!
//! Phase 1（`ROADMAP.md` の完了条件「試算表が SQL 集計で出る」）で実装したのは
//! [`trial_balance`] のみ。総勘定元帳（ledger）・仕訳明細の個別取得
//! （entry_detail）・検索（search）は **Phase 3 で実装する**
//! （`ROADMAP.md` Phase 3 の成果物「読み取り系ツール」＝ `kaikei-mcp` の
//! `get_ledger` / `get_entry` / `search_entries` に対応する。
//! `DECISIONS.md` D-070、`docs/07-mcp-server.md` §2・§4）。
//!
//! 新しい read model はここ（`query/`）に追加すること。MCP 層に SQL を書いたり、
//! `JournalRepo` で全件ロードして絞り込んだりしない。

pub mod trial_balance;

pub use trial_balance::PgTrialBalanceQuery;
