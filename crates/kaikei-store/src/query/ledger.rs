//! `kaikei_app::ports::LedgerQuery`（総勘定元帳）の SQL による実装。
//!
//! `CLAUDE.md` §6 のとおり `Store`/`PgTx` を経由せず、SQL から直接
//! `kaikei_app::view::LedgerPageView` を組み立てる。
//!
//! # 3本の SQL とその役割
//!
//! | # | 見るもの | なぜ分けるか |
//! |---|---|---|
//! | 1 | 対象科目が `accounts` に在るか（名称・種別） | **無い科目に空の元帳を返さない。** 科目コードの打ち間違いと「その期間に取引が無い」は次の手が違う（前者はコードを調べ直す、後者は期間を広げる） |
//! | 2 | 期首残高・期間合計・期間の明細行数 | **ページングと無関係に期間全体**で求める。ページ内の行を合計しても期間合計にはならない |
//! | 3 | 1ページ分の明細（残高の累計付き） | 上限で切る対象はここだけ |
//!
//! # 期首残高は「`from` より前の全明細」
//!
//! 会計年度の開始日ではなく、指定期間の開始日より前の**すべて**を積む。
//! 帳簿の開設以来の累計であり、資産・負債の残高としてはこれが正しい。
//! 収益・費用は決算振替でゼロに戻るため、期首日を年度途中に取ると
//! 「その年度の期首からの累計」にはならない——この定義は
//! `docs/07-mcp-server.md` §3 と応答の説明文に明記してある。
//!
//! # 残高の累計はウィンドウ関数で求める
//!
//! ページの途中から読み始めても残高が合うように、`SUM(...) OVER (ORDER BY ...)`
//! を**カーソルで絞る前**に計算する。行数の上限（`LIMIT`）は最後に効くので、
//! 「2ページ目の1行目の残高」も期間先頭からの累計になる。
//! ウィンドウには `PARTITION BY` が無いため、PostgreSQL がカーソル条件を
//! CTE の内側へ押し込むことはない（押し込まれると累計が狂う）。
//!
//! # 通貨が混在した場合の扱い（`DECISIONS.md` D-042）
//!
//! 集計を `(currency, currency_minor_unit)` でグループ化し、2種類以上
//! 現れたら [`RepoError::Unsupported`] を返す（`trial_balance` と同じ粒度）。

use async_trait::async_trait;
use kaikei_app::error::RepoError;
use kaikei_app::id::{entry_id_from_uuid, entry_id_to_uuid};
use kaikei_app::ports::{LedgerParams, LedgerQuery};
use kaikei_app::view::{EntryCursor, LedgerCursor, LedgerPageView, LedgerRowView, ReversalRef};
use kaikei_core::{AccountCode, AccountType, Money};
use sqlx::PgPool;

use crate::convert::{
    account_type_from_i16, accounting_date_to_naive_date, entry_no_from_i32, money_from_columns,
    naive_date_to_accounting_date, side_from_i16,
};
use crate::error::from_sqlx_error;
use crate::tags::tag_set_from_json;

/// [`LedgerQuery`] の PostgreSQL 実装。
pub struct PgLedgerQuery {
    pool: PgPool,
}

impl PgLedgerQuery {
    /// 接続プールから read model クエリを作る。
    pub fn new(pool: PgPool) -> Self {
        PgLedgerQuery { pool }
    }
}

#[async_trait]
impl LedgerQuery for PgLedgerQuery {
    async fn ledger(&self, params: &LedgerParams) -> Result<LedgerPageView, RepoError> {
        let account = params.account.as_str().to_string();
        let from = accounting_date_to_naive_date(params.from)?;
        let to = accounting_date_to_naive_date(params.to)?;

        // 1. 科目の実在確認。**空の元帳に化けさせない。**
        let definition = sqlx::query!(
            r#"SELECT name, account_type FROM accounts WHERE code = $1"#,
            account,
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx_error)?
        .ok_or_else(|| RepoError::NotFound {
            reason: format!(
                "勘定科目 {account} は勘定科目マスタにありません。\
                 list_accounts で登録済みの科目コードを確認してください"
            ),
        })?;
        let account_type = account_type_from_i16(definition.account_type)?;

        // 2. 期首残高・期間合計・期間の行数（ページングと無関係）。
        let totals = sqlx::query!(
            r#"
            SELECT l.currency                                                    AS "currency!",
                   l.currency_minor_unit                                         AS "minor_unit!",
                   COALESCE(SUM(CASE WHEN e.entry_date < $2 AND l.side = 1
                                     THEN l.amount_minor ELSE 0 END), 0)::BIGINT AS "opening_debit!",
                   COALESCE(SUM(CASE WHEN e.entry_date < $2 AND l.side = 2
                                     THEN l.amount_minor ELSE 0 END), 0)::BIGINT AS "opening_credit!",
                   COALESCE(SUM(CASE WHEN e.entry_date >= $2 AND l.side = 1
                                     THEN l.amount_minor ELSE 0 END), 0)::BIGINT AS "period_debit!",
                   COALESCE(SUM(CASE WHEN e.entry_date >= $2 AND l.side = 2
                                     THEN l.amount_minor ELSE 0 END), 0)::BIGINT AS "period_credit!",
                   COUNT(*) FILTER (WHERE e.entry_date >= $2)                    AS "line_count!"
            FROM journal_lines l
            JOIN journal_entries e ON e.id = l.entry_id
            WHERE l.account_code = $1 AND e.entry_date <= $3
            GROUP BY 1, 2
            ORDER BY 1, 2
            "#,
            account,
            from,
            to,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx_error)?;

        if totals.len() > 1 {
            return Err(RepoError::Unsupported {
                reason: format!(
                    "勘定科目 {account} の元帳に複数の通貨が混在しています（{}）。\
                     この read model は単一通貨のみをサポートします",
                    totals
                        .iter()
                        .map(|row| row.currency.clone())
                        .collect::<Vec<_>>()
                        .join(" と ")
                ),
            });
        }

        // 明細が1行も無ければ帳簿通貨建てのゼロで組み立てる
        // （`Money` は通貨なしでは作れない。`LedgerParams::book_currency` の doc）。
        let zero = Money::zero(params.book_currency);
        let (opening_balance, debit_total, credit_total, closing_balance, total_lines) =
            match totals.first() {
                None => (zero, zero, zero, zero, 0_u64),
                Some(row) => {
                    let money = |minor: i64| -> Result<Money, RepoError> {
                        money_from_columns(minor, &row.currency, row.minor_unit)
                    };
                    let opening_debit = money(row.opening_debit)?;
                    let opening_credit = money(row.opening_credit)?;
                    let debit_total = money(row.period_debit)?;
                    let credit_total = money(row.period_credit)?;
                    let opening = signed_balance(&opening_debit, &opening_credit, account_type)?;
                    let closing_debit = opening_debit.add(&debit_total).map_err(out_of_range)?;
                    let closing_credit = opening_credit.add(&credit_total).map_err(out_of_range)?;
                    let closing = signed_balance(&closing_debit, &closing_credit, account_type)?;
                    (
                        opening,
                        debit_total,
                        credit_total,
                        closing,
                        u64::try_from(row.line_count).unwrap_or(0),
                    )
                }
            };

        // 3. 1ページ分の明細。
        let cursor = params.cursor;
        let cursor_date = cursor
            .map(|c| accounting_date_to_naive_date(c.entry.entry_date))
            .transpose()?;
        let cursor_no = cursor
            .map(|c| i32::try_from(c.entry.entry_no.as_u32()))
            .transpose()
            .map_err(|_| RepoError::OutOfRange {
                reason: "cursor の仕訳番号が保存できる範囲を超えています".to_string(),
            })?;
        let cursor_id = cursor.map(|c| entry_id_to_uuid(c.entry.entry_id));
        let cursor_line_no = cursor
            .map(|c| i16::try_from(c.line_no))
            .transpose()
            .map_err(|_| RepoError::OutOfRange {
                reason: "cursor の明細行番号が保存できる範囲を超えています".to_string(),
            })?;
        // 続きがあるかどうかを見るために1行多く取る。
        let fetch_limit = i64::from(params.limit) + 1;

        let records = sqlx::query!(
            r#"
            WITH opening AS (
                SELECT COALESCE(SUM(CASE WHEN l.side = 1
                                         THEN l.amount_minor
                                         ELSE -l.amount_minor END), 0)::BIGINT AS signed
                FROM journal_lines l
                JOIN journal_entries e ON e.id = l.entry_id
                WHERE l.account_code = $1 AND e.entry_date < $2
            ),
            ordered AS (
                SELECT e.id AS entry_id, e.entry_no, e.entry_date, e.description, e.reverses,
                       l.line_no, l.side, l.amount_minor, l.currency, l.currency_minor_unit,
                       l.tags, l.memo,
                       ((SELECT o.signed FROM opening o)
                        + SUM(CASE WHEN l.side = 1 THEN l.amount_minor ELSE -l.amount_minor END)
                            OVER (ORDER BY e.entry_date, e.entry_no, e.id, l.line_no
                                  ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW)
                       )::BIGINT AS running_signed,
                       (SELECT array_agg(DISTINCT c.account_code ORDER BY c.account_code)
                          FROM journal_lines c
                         WHERE c.entry_id = l.entry_id AND c.side <> l.side) AS counter_accounts,
                       rev.id         AS reversed_by_id,
                       rev.entry_no   AS reversed_by_no,
                       rev.entry_date AS reversed_by_date
                FROM journal_lines l
                JOIN journal_entries e ON e.id = l.entry_id
                LEFT JOIN LATERAL (
                    SELECT r.id, r.entry_no, r.entry_date
                      FROM journal_entries r
                     WHERE r.reverses = e.id
                     ORDER BY r.entry_date, r.entry_no, r.id
                     LIMIT 1
                ) rev ON TRUE
                WHERE l.account_code = $1 AND e.entry_date BETWEEN $2 AND $3
            )
            SELECT entry_id                    AS "entry_id!",
                   entry_no                    AS "entry_no!",
                   entry_date                  AS "entry_date!",
                   description                 AS "description!",
                   reverses                    AS "reverses?",
                   line_no                     AS "line_no!",
                   side                        AS "side!",
                   amount_minor                AS "amount_minor!",
                   currency                    AS "currency!",
                   currency_minor_unit         AS "minor_unit!",
                   tags                        AS "tags!",
                   memo                        AS "memo?",
                   running_signed              AS "running_signed!",
                   counter_accounts            AS "counter_accounts?: Vec<String>",
                   reversed_by_id              AS "reversed_by_id?",
                   reversed_by_no              AS "reversed_by_no?",
                   reversed_by_date            AS "reversed_by_date?"
            FROM ordered
            WHERE NOT $4::boolean
               OR (entry_date, entry_no, entry_id, line_no)
                  > ($5::date, $6::integer, $7::uuid, $8::smallint)
            ORDER BY entry_date, entry_no, entry_id, line_no
            LIMIT $9
            "#,
            account,
            from,
            to,
            cursor.is_some(),
            cursor_date,
            cursor_no,
            cursor_id,
            cursor_line_no,
            fetch_limit,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx_error)?;

        let has_more = records.len() > params.limit as usize;
        let mut rows = Vec::with_capacity(records.len().min(params.limit as usize));
        for record in records.into_iter().take(params.limit as usize) {
            let amount =
                money_from_columns(record.amount_minor, &record.currency, record.minor_unit)?;
            let running_debit_minus_credit =
                money_from_columns(record.running_signed, &record.currency, record.minor_unit)?;
            let running_balance = if account_type.is_debit_normal() {
                running_debit_minus_credit
            } else {
                running_debit_minus_credit.neg()
            };
            let counter_accounts = record
                .counter_accounts
                .unwrap_or_default()
                .iter()
                .map(|code| {
                    AccountCode::parse(code).map_err(|e| RepoError::Corrupt {
                        reason: format!("保存されている勘定科目コードを復元できません: {e}"),
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;

            rows.push(LedgerRowView {
                entry_id: entry_id_from_uuid(record.entry_id),
                entry_no: entry_no_from_i32(record.entry_no)?,
                entry_date: naive_date_to_accounting_date(record.entry_date)?,
                line_no: line_no_from_i16(record.line_no)?,
                description: record.description,
                side: side_from_i16(record.side)?,
                amount,
                tags: tag_set_from_json(&record.tags)?,
                memo: record.memo,
                counter_accounts,
                running_balance,
                reverses: record.reverses.map(entry_id_from_uuid),
                reversed_by: match (
                    record.reversed_by_id,
                    record.reversed_by_no,
                    record.reversed_by_date,
                ) {
                    (Some(id), Some(no), Some(date)) => Some(ReversalRef {
                        entry_id: entry_id_from_uuid(id),
                        entry_no: entry_no_from_i32(no)?,
                        entry_date: naive_date_to_accounting_date(date)?,
                    }),
                    _ => None,
                },
            });
        }

        let next_cursor = has_more
            .then(|| {
                rows.last().map(|row| LedgerCursor {
                    entry: EntryCursor {
                        entry_date: row.entry_date,
                        entry_no: row.entry_no,
                        entry_id: row.entry_id,
                    },
                    line_no: row.line_no,
                })
            })
            .flatten();

        Ok(LedgerPageView {
            account: params.account.clone(),
            account_name: definition.name,
            account_type,
            opening_balance,
            debit_total,
            credit_total,
            closing_balance,
            total_lines,
            rows,
            next_cursor,
        })
    }
}

/// 科目種別に従った符号付き残高（`DOMAIN.md` §2）。
fn signed_balance(
    debit: &Money,
    credit: &Money,
    account_type: AccountType,
) -> Result<Money, RepoError> {
    if account_type.is_debit_normal() {
        debit.sub(credit)
    } else {
        credit.sub(debit)
    }
    .map_err(out_of_range)
}

fn out_of_range(error: kaikei_core::CoreError) -> RepoError {
    RepoError::OutOfRange {
        reason: format!("残高の計算が範囲を超えました: {error}"),
    }
}

/// `journal_lines.line_no`（SMALLINT）を明細の行番号にする。
fn line_no_from_i16(value: i16) -> Result<u16, RepoError> {
    u16::try_from(value).map_err(|_| RepoError::Corrupt {
        reason: format!("保存されている明細の行番号が不正です: {value}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_core::Currency;

    // 借方が正の科目と貸方が正の科目で残高の向きが逆になる（`DOMAIN.md` §2）。
    #[test]
    fn signed_balance_follows_the_account_type() {
        let debit = Money::from_minor(1_000, Currency::JPY);
        let credit = Money::from_minor(300, Currency::JPY);

        assert_eq!(
            signed_balance(&debit, &credit, AccountType::Asset)
                .unwrap()
                .minor(),
            700
        );
        assert_eq!(
            signed_balance(&debit, &credit, AccountType::Revenue)
                .unwrap()
                .minor(),
            -700
        );
    }

    // 明細の行番号は負にならない（保存データが壊れていれば Corrupt）。
    #[test]
    fn a_negative_line_no_is_reported_as_corrupt() {
        assert!(matches!(
            line_no_from_i16(-1),
            Err(RepoError::Corrupt { .. })
        ));
        assert_eq!(line_no_from_i16(3).unwrap(), 3);
    }
}
