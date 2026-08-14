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
use kaikei_app::ports::{ImportDirection, ImportOutcome, ImportedTxRepo, NewImportedTransaction};
use kaikei_core::EntryId;

use crate::convert::{accounting_date_to_naive_date, timestamp_to_datetime};
use crate::store::PgTx;

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
    fn a_non_uuid_id_is_corrupt_not_a_panic() {
        assert!(matches!(
            parse_uuid("これはUUIDではない"),
            Err(RepoError::Corrupt { .. })
        ));
    }
}
