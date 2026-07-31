//! [`kaikei_app::ports::NumberingRepo`] の PostgreSQL 実装。
//!
//! # 採番は1文の upsert（phase1計画 G7）
//!
//! `entry_counters.next_no` は「次に払い出す仕訳番号」を表す。行ロックを
//! 握ったまま外部 I/O を待たないよう（`CLAUDE.md` §14 提案 / phase1計画 G8）、
//! 採番は往復を1回に閉じた単一の SQL 文で行う:
//!
//! ```sql
//! INSERT INTO entry_counters (fiscal_year, next_no) VALUES ($1, 2)
//! ON CONFLICT (fiscal_year) DO UPDATE SET next_no = entry_counters.next_no + 1
//! RETURNING next_no - 1
//! ```
//!
//! 初回（行が無い）は `next_no = 2` で INSERT し、`RETURNING next_no - 1` で
//! `1`（払い出す番号）を返す。2回目以降は既存行を `+1` した上で、更新後の
//! `next_no - 1`（＝更新前の `next_no`。今回払い出す番号）を返す。採番と
//! 仕訳の INSERT は同一トランザクションで行われるため、検証失敗時は
//! カウンタの増分も一緒に巻き戻り、欠番は原理的に発生しない
//! （`migrations/0006_entry_counters.sql`、`DECISIONS.md` の該当決定）。

use crate::convert::entry_no_from_i32;
use crate::error::from_sqlx_error;
use crate::store::PgTx;
use async_trait::async_trait;
use kaikei_app::error::RepoError;
use kaikei_app::ports::NumberingRepo;
use kaikei_core::EntryNumber;

#[async_trait]
impl NumberingRepo for PgTx<'_> {
    async fn next_entry_no(&mut self, fiscal_year: i32) -> Result<EntryNumber, RepoError> {
        let issued: i32 = sqlx::query_scalar(
            "INSERT INTO entry_counters (fiscal_year, next_no) VALUES ($1, 2) \
             ON CONFLICT (fiscal_year) DO UPDATE SET next_no = entry_counters.next_no + 1 \
             RETURNING next_no - 1",
        )
        .bind(fiscal_year)
        .fetch_one(self.conn())
        .await
        .map_err(from_sqlx_error)?;

        entry_no_from_i32(issued)
    }
}
