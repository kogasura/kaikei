//! `kaikei_app::ports::SearchEntriesQuery`（仕訳検索）の SQL による実装。
//!
//! `CLAUDE.md` §6「read model は物理的に分離する」の原則どおり、`Store`/`PgTx`
//! を一切経由せず、`sqlx::query!` で SQL から直接
//! `kaikei_app::view::EntrySummaryView` を組み立てる。
//!
//! # SQLインジェクション対策（可変長の条件）
//!
//! 条件の数は呼び出しごとに変わるが、**SQL文そのものは固定である**
//! （条件ごとに文字列連結で SQL を組み立てない）。`trial_balance` の
//! `group_by` と同じ方針で:
//!
//! - 省略可の条件は `($1::date IS NULL OR ...)` の形にし、未指定は `NULL` を渡す
//! - タグの絞り込みは `unnest($9::text[], $10::text[])` で配列パラメータを
//!   行に展開し、`NOT EXISTS (... WHERE NOT EXISTS (...))` で「すべてのタグを
//!   満たす」を表す
//! - 摘要の部分一致は `ILIKE` に**エスケープ済みのパターン**を渡す
//!   （[`like_contains_pattern`]。`%` / `_` / `\` を含む検索語が
//!   ワイルドカードとして効いてしまうのを防ぐ）
//!
//! # 総件数と1ページ（`DECISIONS.md` D-089）
//!
//! 「上限で切ったことが応答から分かる」ためには、1ページの行と**条件に
//! 一致した総件数**の両方が要る。1つの SQL 文で両方を返すために
//! `total`（1行だけの CTE）に `page` を `LEFT JOIN` している。
//! 素朴に `(SELECT count(*) FROM matched)` を各行に載せる形にすると、
//! **ページが0行のとき総件数が消える**（カーソルが末尾を越えた要求と
//! 「そもそも0件」を区別できなくなる）。
//!
//! 続きがあるかどうかは `LIMIT` に `limit + 1` を渡して判定する
//! （`limit + 1` 行目が返ってきたら続きがある）。総件数から引き算しないのは、
//! 総件数がカーソルより前の行を含むためである。
//!
//! # 取り消された仕訳（`DECISIONS.md` D-088）
//!
//! 帳簿は追記のみなので、赤伝で訂正された仕訳も検索に出続ける。
//! **それが分からないと AI は同じ仕訳をもう一度訂正しようとする。**
//! そこで `LEFT JOIN LATERAL` で「この仕訳を訂正している赤伝」を1件だけ
//! 引き、`EntrySummaryView::reversed_by` に載せる。
//! `LEFT JOIN` を素朴に書かないのは、`allow_double_reversal` により
//! **同じ仕訳に対する赤伝が2件以上ありうる**ためである（そのまま結合すると
//! 検索結果に同じ仕訳が2行現れる）。
//!
//! # 明細を一緒に返す
//!
//! 明細は2本目の SQL（`entry_id = ANY(...)`）でまとめて引く。1件ずつ
//! 引き直す形にすると、1ページ分で N+1 回の往復になる。

use async_trait::async_trait;
use kaikei_app::error::RepoError;
use kaikei_app::id::{entry_id_from_uuid, entry_id_to_uuid};
use kaikei_app::ports::{SearchEntriesParams, SearchEntriesQuery};
use kaikei_app::view::{EntryCursor, EntrySearchPageView, EntrySummaryView, ReversalRef};
use kaikei_core::{AccountCode, JournalLine};
use sqlx::PgPool;
use std::collections::BTreeMap;
use uuid::Uuid;

use crate::convert::{
    accounting_date_to_naive_date, entry_no_from_i32, money_from_columns,
    naive_date_to_accounting_date, side_from_i16,
};
use crate::error::from_sqlx_error;
use crate::tags::tag_set_from_json;

/// [`SearchEntriesQuery`] の PostgreSQL 実装。
///
/// `Store`/`PgTx`（書き込み側）とは独立に自前の `PgPool` を持つ
/// （`CLAUDE.md` §6。read model は Repository を経由しない）。
pub struct PgSearchEntriesQuery {
    pool: PgPool,
}

impl PgSearchEntriesQuery {
    /// 接続プールから read model クエリを作る。
    pub fn new(pool: PgPool) -> Self {
        PgSearchEntriesQuery { pool }
    }
}

#[async_trait]
impl SearchEntriesQuery for PgSearchEntriesQuery {
    async fn search_entries(
        &self,
        params: &SearchEntriesParams,
    ) -> Result<EntrySearchPageView, RepoError> {
        let from = params.from.map(accounting_date_to_naive_date).transpose()?;
        let to = params.to.map(accounting_date_to_naive_date).transpose()?;
        let description = params
            .description_contains
            .as_deref()
            .map(like_contains_pattern);
        let account = params
            .account
            .as_ref()
            .map(|code| code.as_str().to_string());

        // 金額の範囲は「同じ1行が両方を満たす」で判定するので、通貨も
        // その行と突き合わせる（別通貨の 1000 と一致してしまうのを防ぐ）。
        let amount_currency = params
            .min_amount
            .as_ref()
            .or(params.max_amount.as_ref())
            .map(|money| {
                (
                    money.currency().code().to_string(),
                    i16::from(money.currency().minor_unit()),
                )
            });
        let min_amount = params.min_amount.as_ref().map(minor_as_i64).transpose()?;
        let max_amount = params.max_amount.as_ref().map(minor_as_i64).transpose()?;

        let tag_keys: Vec<String> = params
            .tags
            .iter()
            .map(|(key, _)| key.as_str().to_string())
            .collect();
        let tag_values: Vec<String> = params.tags.iter().map(|(_, value)| value.clone()).collect();

        let cursor = params.cursor;
        let cursor_date = cursor
            .map(|c| accounting_date_to_naive_date(c.entry_date))
            .transpose()?;
        let cursor_no = cursor
            .map(|c| i32::try_from(c.entry_no.as_u32()))
            .transpose()
            .map_err(|_| RepoError::OutOfRange {
                reason: "cursor の仕訳番号が保存できる範囲を超えています".to_string(),
            })?;
        let cursor_id = cursor.map(|c| entry_id_to_uuid(c.entry_id));

        // 続きがあるかどうかを見るために1件多く取る。
        let fetch_limit = i64::from(params.limit) + 1;

        let records = sqlx::query!(
            r#"
            WITH matched AS (
                SELECT e.id, e.fiscal_year, e.entry_no, e.entry_date, e.description,
                       e.reverses, e.reverse_reason
                FROM journal_entries e
                WHERE ($1::date IS NULL OR e.entry_date >= $1)
                  AND ($2::date IS NULL OR e.entry_date <= $2)
                  AND ($3::text IS NULL OR e.description ILIKE $3 ESCAPE '\')
                  AND ($4::text IS NULL OR EXISTS (
                        SELECT 1 FROM journal_lines l
                         WHERE l.entry_id = e.id AND l.account_code = $4))
                  AND (($5::bigint IS NULL AND $6::bigint IS NULL) OR EXISTS (
                        SELECT 1 FROM journal_lines l
                         WHERE l.entry_id = e.id
                           AND l.currency = $7::text
                           AND l.currency_minor_unit = $8::smallint
                           AND ($5::bigint IS NULL OR l.amount_minor >= $5)
                           AND ($6::bigint IS NULL OR l.amount_minor <= $6)))
                  AND NOT EXISTS (
                        SELECT 1 FROM unnest($9::text[], $10::text[]) AS f(k, v)
                         WHERE NOT EXISTS (
                            SELECT 1 FROM journal_lines l
                             WHERE l.entry_id = e.id AND l.tags -> f.k ->> 'v' = f.v))
            ),
            total AS (SELECT count(*) AS n FROM matched),
            page AS (
                SELECT m.id, m.fiscal_year, m.entry_no, m.entry_date, m.description,
                       m.reverses, m.reverse_reason,
                       rev.id         AS reversed_by_id,
                       rev.entry_no   AS reversed_by_no,
                       rev.entry_date AS reversed_by_date
                FROM matched m
                LEFT JOIN LATERAL (
                    SELECT r.id, r.entry_no, r.entry_date
                      FROM journal_entries r
                     WHERE r.reverses = m.id
                     ORDER BY r.entry_date, r.entry_no, r.id
                     LIMIT 1
                ) rev ON TRUE
                WHERE NOT $11::boolean
                   OR (m.entry_date, m.entry_no, m.id)
                      > ($12::date, $13::integer, $14::uuid)
                ORDER BY m.entry_date, m.entry_no, m.id
                LIMIT $15
            )
            SELECT t.n                AS "total_matches!",
                   p.id               AS "id?",
                   p.fiscal_year      AS "fiscal_year?",
                   p.entry_no         AS "entry_no?",
                   p.entry_date       AS "entry_date?",
                   p.description      AS "description?",
                   p.reverses         AS "reverses?",
                   p.reverse_reason   AS "reverse_reason?",
                   p.reversed_by_id   AS "reversed_by_id?",
                   p.reversed_by_no   AS "reversed_by_no?",
                   p.reversed_by_date AS "reversed_by_date?"
            FROM total t
            LEFT JOIN page p ON TRUE
            ORDER BY p.entry_date, p.entry_no, p.id
            "#,
            from,
            to,
            description,
            account,
            min_amount,
            max_amount,
            amount_currency.as_ref().map(|(code, _)| code.as_str()),
            amount_currency.as_ref().map(|(_, unit)| *unit),
            &tag_keys[..],
            &tag_values[..],
            cursor.is_some(),
            cursor_date,
            cursor_no,
            cursor_id,
            fetch_limit,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx_error)?;

        let total_matches = records
            .first()
            .map(|record| record.total_matches)
            .unwrap_or(0);
        let total_matches = u64::try_from(total_matches).unwrap_or(0);

        // `LEFT JOIN page` なので、ページが0行でも「行のない1行」が返る。
        let mut headers: Vec<EntryHeader> = Vec::new();
        for record in records {
            let Some(id) = record.id else {
                continue;
            };
            headers.push(EntryHeader {
                id,
                fiscal_year: required(record.fiscal_year, "fiscal_year")?,
                entry_no: required(record.entry_no, "entry_no")?,
                entry_date: required(record.entry_date, "entry_date")?,
                description: required(record.description, "description")?,
                reverses: record.reverses,
                reverse_reason: record.reverse_reason,
                reversed_by: match (
                    record.reversed_by_id,
                    record.reversed_by_no,
                    record.reversed_by_date,
                ) {
                    (Some(id), Some(no), Some(date)) => Some((id, no, date)),
                    _ => None,
                },
            });
        }

        // 続きがあるか（1件多く取った分が返ってきたか）。
        let has_more = headers.len() > params.limit as usize;
        headers.truncate(params.limit as usize);

        let ids: Vec<Uuid> = headers.iter().map(|header| header.id).collect();
        let mut lines_by_entry = self.load_lines(&ids).await?;

        let mut entries = Vec::with_capacity(headers.len());
        for header in headers {
            let lines = lines_by_entry.remove(&header.id).unwrap_or_default();
            if lines.is_empty() {
                return Err(RepoError::Corrupt {
                    reason: format!("仕訳 {} に明細が1行もありません", header.id),
                });
            }
            entries.push(EntrySummaryView {
                entry_id: entry_id_from_uuid(header.id),
                entry_no: entry_no_from_i32(header.entry_no)?,
                fiscal_year: header.fiscal_year,
                entry_date: naive_date_to_accounting_date(header.entry_date)?,
                description: header.description,
                lines,
                reverses: header.reverses.map(entry_id_from_uuid),
                reverse_reason: header.reverse_reason,
                reversed_by: header
                    .reversed_by
                    .map(|(id, no, date)| {
                        Ok::<_, RepoError>(ReversalRef {
                            entry_id: entry_id_from_uuid(id),
                            entry_no: entry_no_from_i32(no)?,
                            entry_date: naive_date_to_accounting_date(date)?,
                        })
                    })
                    .transpose()?,
            });
        }

        let next_cursor = has_more
            .then(|| entries.last().map(entry_cursor_of))
            .flatten();

        Ok(EntrySearchPageView {
            entries,
            total_matches,
            next_cursor,
        })
    }
}

impl PgSearchEntriesQuery {
    /// ページに載る仕訳の明細をまとめて引く（N+1 を避ける）。
    async fn load_lines(
        &self,
        ids: &[Uuid],
    ) -> Result<BTreeMap<Uuid, Vec<JournalLine>>, RepoError> {
        let mut grouped: BTreeMap<Uuid, Vec<JournalLine>> = BTreeMap::new();
        if ids.is_empty() {
            return Ok(grouped);
        }

        let records = sqlx::query!(
            r#"
            SELECT entry_id, line_no, account_code, side, amount_minor,
                   currency, currency_minor_unit, tags, memo
            FROM journal_lines
            WHERE entry_id = ANY($1::uuid[])
            ORDER BY entry_id, line_no
            "#,
            ids,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx_error)?;

        for record in records {
            let account =
                AccountCode::parse(&record.account_code).map_err(|e| RepoError::Corrupt {
                    reason: format!("保存されている勘定科目コードを復元できません: {e}"),
                })?;
            let side = side_from_i16(record.side)?;
            let amount = money_from_columns(
                record.amount_minor,
                &record.currency,
                record.currency_minor_unit,
            )?;
            let tags = tag_set_from_json(&record.tags)?;
            let line = JournalLine::new(account, side, amount, tags, record.memo).map_err(|e| {
                RepoError::Corrupt {
                    reason: format!("仕訳 {} の明細を復元できません: {e}", record.entry_id),
                }
            })?;
            grouped.entry(record.entry_id).or_default().push(line);
        }

        Ok(grouped)
    }
}

/// SQL から読んだ仕訳ヘッダ（明細を付ける前の中間表現）。
struct EntryHeader {
    id: Uuid,
    fiscal_year: i32,
    entry_no: i32,
    entry_date: chrono::NaiveDate,
    description: String,
    reverses: Option<Uuid>,
    reverse_reason: Option<String>,
    reversed_by: Option<(Uuid, i32, chrono::NaiveDate)>,
}

/// この仕訳を指すカーソル（次のページの開始位置に使う）。
fn entry_cursor_of(entry: &EntrySummaryView) -> EntryCursor {
    EntryCursor {
        entry_date: entry.entry_date,
        entry_no: entry.entry_no,
        entry_id: entry.entry_id,
    }
}

/// `LEFT JOIN` で `Option` になった列のうち、行が存在するなら必ず値がある列。
///
/// `id` が `Some` である行では他の列も `Some` である（同じ `page` 行に
/// 由来するため）。それでも `unwrap` せず [`RepoError::Corrupt`] にするのは、
/// 永続化層からの復元で panic させないという規律による
/// （`crates/kaikei-store/src/journal/mapper.rs` のモジュール doc）。
fn required<T>(value: Option<T>, column: &str) -> Result<T, RepoError> {
    value.ok_or_else(|| RepoError::Corrupt {
        reason: format!("検索結果の {column} 列が NULL です"),
    })
}

/// 金額を最小通貨単位の `i64` にする（`journal_lines.amount_minor` と同じ型）。
fn minor_as_i64(money: &kaikei_core::Money) -> Result<i64, RepoError> {
    i64::try_from(money.minor()).map_err(|_| RepoError::OutOfRange {
        reason: format!(
            "金額 {} は保存されている金額と比較できる範囲を超えています",
            money.to_display_string()
        ),
    })
}

/// 部分一致の `ILIKE` パターンにする。
///
/// **検索語に含まれる `%` / `_` / `\` をワイルドカードとして効かせない。**
/// 効かせると「`100%` を含む摘要」の検索が「`100` で始まる摘要すべて」に
/// 化け、呼び出し元には**多すぎる結果が正しい結果として**返る。
fn like_contains_pattern(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len() + 2);
    escaped.push('%');
    for c in text.chars() {
        if matches!(c, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped.push('%');
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    // 検索語のワイルドカードはエスケープされる（部分一致のままにする）。
    #[test]
    fn like_pattern_escapes_wildcards_in_the_search_term() {
        assert_eq!(like_contains_pattern("A社"), "%A社%");
        assert_eq!(like_contains_pattern("100%"), "%100\\%%");
        assert_eq!(like_contains_pattern("a_b"), "%a\\_b%");
        assert_eq!(like_contains_pattern("c\\d"), "%c\\\\d%");
    }

    // 行が無い列は panic せず Corrupt になる。
    #[test]
    fn a_missing_column_becomes_corrupt_instead_of_panicking() {
        let result = required::<i32>(None, "entry_no");
        assert!(matches!(result, Err(RepoError::Corrupt { .. })));
        assert_eq!(required(Some(1), "entry_no").unwrap(), 1);
    }
}
