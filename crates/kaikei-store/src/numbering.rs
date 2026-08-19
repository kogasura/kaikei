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

/// 採番カウンタと帳簿の食い違いを調べる。
///
/// # なぜ要るのか
///
/// 仕訳番号は `entry_counters` が払い出す。`journal_entries` を直に触って
/// 仕訳を入れると、この表が付いてこない。**次に払い出す番号が既にある
/// 仕訳と衝突し、以降の記帳がすべて失敗する。**
///
/// そのときのエラーは「既に存在するデータと重複するため保存できません」で、
/// **理由が分からない。** 同じ仕訳を二重に入れたのだと読める。
///
/// 現実に起きるのは**復元したとき**である。`export.json` は仕訳を仕訳番号
/// つきで持っているが、カウンタは持っていない。あれは「このソフトが
/// 無くなっても帳簿が残る」ための出口なので、そこから戻す道は必ず要る。
///
/// # Errors
///
/// 読み取りに失敗した場合は理由を返す。
pub async fn counter_drift(pool: &sqlx::PgPool) -> Result<Vec<CounterDrift>, sqlx::Error> {
    // 年度ごとに「次に払い出す番号」と「帳簿にある最大の番号」を並べる。
    // カウンタの行が無い年度も拾う（それが最も危ない形である）。
    let rows: Vec<(i32, Option<i32>, i32)> = sqlx::query_as(
        "SELECT e.fiscal_year, c.next_no, MAX(e.entry_no)::int \
         FROM journal_entries e \
         LEFT JOIN entry_counters c ON c.fiscal_year = e.fiscal_year \
         GROUP BY e.fiscal_year, c.next_no \
         ORDER BY e.fiscal_year",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(fiscal_year, next_no, max_entry_no)| CounterDrift {
            fiscal_year,
            next_no,
            max_entry_no,
        })
        .collect())
}

/// 年度ごとの、採番カウンタと帳簿の突き合わせ結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CounterDrift {
    /// 会計年度。
    pub fiscal_year: i32,
    /// 次に払い出す番号。**カウンタの行が無ければ `None`。**
    pub next_no: Option<i32>,
    /// その年度の帳簿にある最大の仕訳番号。
    pub max_entry_no: i32,
}
