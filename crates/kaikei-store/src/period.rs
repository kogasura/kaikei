//! [`kaikei_app::ports::PeriodRepo`] の PostgreSQL 実装。

use crate::convert::naive_date_to_accounting_date;
use crate::error::from_sqlx_error;
use crate::store::PgTx;
use async_trait::async_trait;
use kaikei_app::error::RepoError;
use kaikei_app::ports::PeriodRepo;
use kaikei_core::AccountingDate;

#[async_trait]
impl PeriodRepo for PgTx<'_> {
    async fn closed_through(
        &mut self,
        fiscal_year: i32,
    ) -> Result<Option<AccountingDate>, RepoError> {
        // `period_snapshots` は締めのたびに1行追加される（append-only）。
        // 「どこまで締まっているか」は、その会計年度で最も新しい
        // `period_end`（締めた期間の終端日）で表される。該当行が無ければ
        // 集約関数 MAX は SQL NULL を返す（行自体は必ず1行返る）ため
        // `fetch_optional` ではなく `fetch_one` でよい。
        let period_end: Option<chrono::NaiveDate> = sqlx::query_scalar(
            "SELECT MAX(period_end) FROM period_snapshots WHERE fiscal_year = $1",
        )
        .bind(fiscal_year)
        .fetch_one(self.conn())
        .await
        .map_err(from_sqlx_error)?;

        period_end.map(naive_date_to_accounting_date).transpose()
    }
}
