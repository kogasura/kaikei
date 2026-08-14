//! 取り込んだ明細の登録と状態遷移（[`kaikei_app::ports::ImportedTxRepo`]）の
//! PostgreSQL 実装。
//!
//! `docs/05-csv-import.md` §3・§6。マイグレーションは
//! `migrations/0011_imported_transactions.sql`。
//!
//! # 状態遷移は SQL の `WHERE` が守る
//!
//! 「未処理のものだけ仕訳済みにできる」等の条件は、読んでから書くのではなく
//! **`UPDATE ... WHERE status = '...'` の一文で表す**。読んでから書くと、
//! その間に別の取込が同じ明細を進めたときに後勝ちで上書きしてしまう。
//! 更新できた行数が 0 なら、対象が無いか状態が違ったということである。
//!
//! # 値の妥当性は DB が持つ
//!
//! 金額が正であること、状態と付随する値が食い違わないことなどは 0011 の
//! CHECK 制約が見る。**ここで二重に検査しない**——検査が2箇所にあると、
//! 片方だけ直したときに食い違う。

use async_trait::async_trait;
use kaikei_app::error::RepoError;
use kaikei_app::ports::{
    ImportDirection, ImportOutcome, ImportedTxQuery, ImportedTxRepo, NewImportedTransaction,
};
use kaikei_app::view::{ImportStatusCounts, ImportedTxQuerySpec, ImportedTxView};
use kaikei_core::EntryId;
use sqlx::PgPool;

use crate::convert::{
    accounting_date_to_naive_date, naive_date_to_accounting_date, timestamp_to_datetime,
};
use crate::store::PgTx;

/// 一覧で一度に返す上限。
///
/// **呼び出し側が上限を渡すが、それでも上限の上限を持つ。** 条件を付け忘れた
/// 一覧が取込全体を返すと、応答が明細の量に比例して膨らむ。
pub const MAX_LIST_LIMIT: u32 = 200;

/// 0011 の `direction` 列の値。
///
/// **借方/貸方ではない**（`docs/05-csv-import.md` §2）。
fn direction_to_i16(direction: ImportDirection) -> i16 {
    match direction {
        ImportDirection::In => 1,
        ImportDirection::Out => 2,
    }
}

#[async_trait]
impl ImportedTxRepo for PgTx<'_> {
    async fn insert_imported(
        &mut self,
        imported: &NewImportedTransaction,
    ) -> Result<ImportOutcome, RepoError> {
        let id = parse_uuid(&imported.id)?;

        // **既にある行には触れない。** `DO UPDATE` にすると、再取込が
        // 仕訳済みの明細を書き換えてしまう。何もしないのが正しい
        // （`docs/05-csv-import.md` §4）。
        //
        // `raw_row` は JSON 文字列として受け取り、ここで jsonb へ写す。
        // ポート側（kaikei-app）に JSON の型を持ち込まないためである。
        let result = sqlx::query(
            "INSERT INTO imported_transactions \
             (id, source, external_key, occurred_on, amount_minor, direction, \
              raw_description, balance_after, raw_row, status, imported_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::jsonb, 'pending', $10) \
             ON CONFLICT (source, external_key) DO NOTHING",
        )
        .bind(id)
        .bind(&imported.source)
        .bind(&imported.external_key)
        .bind(accounting_date_to_naive_date(imported.occurred_on)?)
        .bind(imported.amount_minor)
        .bind(direction_to_i16(imported.direction))
        .bind(&imported.raw_description)
        .bind(imported.balance_after)
        .bind(&imported.raw_row)
        .bind(timestamp_to_datetime(imported.imported_at)?)
        .execute(self.conn())
        .await
        .map_err(crate::error::from_sqlx_error)?;

        if result.rows_affected() == 0 {
            Ok(ImportOutcome::SkippedDuplicate)
        } else {
            Ok(ImportOutcome::Inserted)
        }
    }

    async fn mark_journalized(
        &mut self,
        imported_id: &str,
        entry_id: EntryId,
    ) -> Result<(), RepoError> {
        let id = parse_uuid(imported_id)?;
        let result = sqlx::query(
            "UPDATE imported_transactions \
             SET status = 'journalized', entry_id = $2, ignore_reason = NULL \
             WHERE id = $1 AND status = 'pending'",
        )
        .bind(id)
        .bind(uuid::Uuid::from_u128(entry_id.as_u128()))
        .execute(self.conn())
        .await
        .map_err(crate::error::from_sqlx_error)?;

        require_one_row(result.rows_affected(), imported_id, "未処理")
    }

    async fn mark_ignored(&mut self, imported_id: &str, reason: &str) -> Result<(), RepoError> {
        let id = parse_uuid(imported_id)?;
        let result = sqlx::query(
            "UPDATE imported_transactions \
             SET status = 'ignored', ignore_reason = $2, entry_id = NULL \
             WHERE id = $1 AND status = 'pending'",
        )
        .bind(id)
        .bind(reason)
        .execute(self.conn())
        .await
        .map_err(crate::error::from_sqlx_error)?;

        require_one_row(result.rows_affected(), imported_id, "未処理")
    }

    async fn revert_to_pending(&mut self, imported_id: &str) -> Result<(), RepoError> {
        let id = parse_uuid(imported_id)?;
        // 仕訳IDも消す。残すと「未処理なのに仕訳を指している」行になり、
        // 0011 の `imported_pending_is_clean` に弾かれる。
        let result = sqlx::query(
            "UPDATE imported_transactions \
             SET status = 'pending', entry_id = NULL, ignore_reason = NULL \
             WHERE id = $1 AND status = 'journalized'",
        )
        .bind(id)
        .execute(self.conn())
        .await
        .map_err(crate::error::from_sqlx_error)?;

        require_one_row(result.rows_affected(), imported_id, "仕訳済み")
    }
}

/// 1行だけ更新されたことを確かめる。
///
/// 0 行は「明細が無い」か「状態が違った」のどちらか。**区別しない**——
/// どちらにせよ呼び出し側は状態を確かめ直すしかなく、区別するには読み直しが
/// 要る（そのために往復を増やす価値が無い）。ただし**どの状態を期待したかは
/// 伝える**。「見つかりません」だけでは、IDの打ち間違いなのか二重に仕訳化
/// しようとしたのか分からない。
fn require_one_row(rows: u64, imported_id: &str, expected: &str) -> Result<(), RepoError> {
    if rows == 0 {
        return Err(RepoError::NotFound {
            reason: format!("{expected}の取込明細が見つかりません（id={imported_id}）"),
        });
    }
    Ok(())
}

/// 文字列のIDを UUID に直す。
fn parse_uuid(value: &str) -> Result<uuid::Uuid, RepoError> {
    value.parse::<uuid::Uuid>().map_err(|_| RepoError::Corrupt {
        reason: format!("取込明細のIDが UUID ではありません: {value}"),
    })
}

/// 取り込んだ明細の一覧（read model）。
#[derive(Debug, Clone)]
pub struct PgImportedTxQuery {
    pool: PgPool,
}

impl PgImportedTxQuery {
    /// プールから作る。
    pub fn new(pool: PgPool) -> Self {
        PgImportedTxQuery { pool }
    }
}

#[async_trait]
impl ImportedTxQuery for PgImportedTxQuery {
    async fn list_imported(
        &self,
        query: &ImportedTxQuerySpec,
        limit: u32,
    ) -> Result<Vec<ImportedTxView>, RepoError> {
        let limit = limit.min(MAX_LIST_LIMIT) as i64;

        // **SQL は固定にする。** 条件の有無で文字列を組み立てず、NULL のときは
        // その条件を素通りさせる（`$n IS NULL OR ...`）。組み立てをやめれば
        // 注入の余地が構造的に無くなる（`documents.rs` と同じ方針）。
        //
        // 並びは取引年月日の昇順——未処理は古いものから片付ける。
        let rows = sqlx::query_as::<_, ImportedRow>(
            "SELECT id::text, source, occurred_on, amount_minor, direction,                     raw_description, balance_after, status, entry_id::text, ignore_reason              FROM imported_transactions              WHERE ($1::text IS NULL OR source = $1)                AND ($2::text IS NULL OR status = $2)                AND ($3::date IS NULL OR occurred_on >= $3)                AND ($4::date IS NULL OR occurred_on <= $4)              ORDER BY occurred_on ASC, id ASC              LIMIT $5",
        )
        .bind(query.source.as_deref())
        .bind(query.status.as_deref())
        .bind(
            query
                .date_from
                .map(accounting_date_to_naive_date)
                .transpose()?,
        )
        .bind(query.date_to.map(accounting_date_to_naive_date).transpose()?)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(crate::error::from_sqlx_error)?;

        rows.into_iter().map(ImportedRow::into_view).collect()
    }

    async fn find_imported(&self, imported_id: &str) -> Result<Option<ImportedTxView>, RepoError> {
        // **UUID でない文字列は「見つからない」。** ここで Corrupt にすると、
        // 打ち間違いが「保存データが壊れている」という誤診になる
        // （壊れているのは入力であって帳簿ではない）。
        let Ok(id) = imported_id.parse::<uuid::Uuid>() else {
            return Ok(None);
        };
        let row = sqlx::query_as::<_, ImportedRow>(
            "SELECT id::text, source, occurred_on, amount_minor, direction,                     raw_description, balance_after, status, entry_id::text, ignore_reason              FROM imported_transactions              WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(crate::error::from_sqlx_error)?;

        row.map(ImportedRow::into_view).transpose()
    }

    async fn import_status_counts(
        &self,
        source: Option<&str>,
    ) -> Result<ImportStatusCounts, RepoError> {
        // 3状態を1往復で数える。状態ごとに問い合わせると、その間に取込が
        // 進んで合計が合わなくなる。
        let row: (i64, i64, i64) = sqlx::query_as(
            "SELECT                count(*) FILTER (WHERE status = 'pending'),                count(*) FILTER (WHERE status = 'journalized'),                count(*) FILTER (WHERE status = 'ignored')              FROM imported_transactions              WHERE ($1::text IS NULL OR source = $1)",
        )
        .bind(source)
        .fetch_one(&self.pool)
        .await
        .map_err(crate::error::from_sqlx_error)?;

        Ok(ImportStatusCounts {
            pending: row.0,
            journalized: row.1,
            ignored: row.2,
        })
    }
}

#[derive(sqlx::FromRow)]
struct ImportedRow {
    id: String,
    source: String,
    occurred_on: chrono::NaiveDate,
    amount_minor: i64,
    direction: i16,
    raw_description: String,
    balance_after: Option<i64>,
    status: String,
    entry_id: Option<String>,
    ignore_reason: Option<String>,
}

impl ImportedRow {
    fn into_view(self) -> Result<ImportedTxView, RepoError> {
        Ok(ImportedTxView {
            id: self.id,
            source: self.source,
            occurred_on: naive_date_to_accounting_date(self.occurred_on)?,
            amount_minor: self.amount_minor,
            is_money_in: direction_from_i16(self.direction)?,
            raw_description: self.raw_description,
            balance_after: self.balance_after,
            status: self.status,
            entry_id: self.entry_id,
            ignore_reason: self.ignore_reason,
        })
    }
}

/// `direction` 列を「入金かどうか」に直す。
///
/// **知らない値は panic させず [`RepoError::Corrupt`] にする。** 0011 の
/// CHECK 制約が 1/2 以外を弾いているので通常は起きないが、制約を落とした
/// 別経路の書き込みがあったときに落ちるより、読めなかったと言う方が良い。
fn direction_from_i16(value: i16) -> Result<bool, RepoError> {
    match value {
        1 => Ok(true),
        2 => Ok(false),
        other => Err(RepoError::Corrupt {
            reason: format!("取込明細の direction が 1/2 ではありません: {other}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_direction_column_uses_one_for_money_in() {
        // 0011 の CHECK 制約（direction IN (1, 2)）と対応する。
        assert_eq!(direction_to_i16(ImportDirection::In), 1);
        assert_eq!(direction_to_i16(ImportDirection::Out), 2);
    }

    /// 見つからなかった理由に、期待した状態が入る。
    ///
    /// 「見つかりません」だけだと、IDの打ち間違いと二重仕訳化の区別が付かない。
    #[test]
    fn the_not_found_message_says_which_status_was_expected() {
        let err = require_one_row(0, "abc", "未処理").expect_err("0 行は失敗");
        let message = err.to_string();
        assert!(message.contains("未処理"), "{message}");
        assert!(message.contains("abc"), "{message}");
    }

    #[test]
    fn updating_one_row_is_success() {
        assert!(require_one_row(1, "abc", "未処理").is_ok());
    }

    #[test]
    fn the_direction_column_reads_back_as_money_in_or_out() {
        assert!(direction_from_i16(1).unwrap(), "1 は入金");
        assert!(!direction_from_i16(2).unwrap(), "2 は出金");
    }

    /// 知らない向きは panic ではなく `Corrupt`。
    ///
    /// 0011 の CHECK 制約が弾いているので通常は起きないが、落ちるより
    /// 「読めなかった」と言う方が良い。
    #[test]
    fn an_unknown_direction_is_corrupt_not_a_panic() {
        assert!(matches!(
            direction_from_i16(3),
            Err(RepoError::Corrupt { .. })
        ));
        assert!(matches!(
            direction_from_i16(0),
            Err(RepoError::Corrupt { .. })
        ));
    }

    #[test]
    fn a_non_uuid_id_is_corrupt_not_a_panic() {
        assert!(matches!(
            parse_uuid("これはUUIDではない"),
            Err(RepoError::Corrupt { .. })
        ));
    }
}
