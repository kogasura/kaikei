//! [`kaikei_app::ports::ChartRepo`] の PostgreSQL 実装。

use crate::error::from_sqlx_error;
use crate::store::PgTx;
use async_trait::async_trait;
use kaikei_app::error::RepoError;
use kaikei_app::ports::ChartRepo;
use kaikei_app::{Counterparty, CounterpartyIndex};
use kaikei_core::{AccountCode, AccountDef, ChartOfAccounts};

#[async_trait]
impl ChartRepo for PgTx<'_> {
    async fn load_chart(&mut self) -> Result<ChartOfAccounts, RepoError> {
        let rows: Vec<(String, String, i16, Option<String>, bool)> = sqlx::query_as(
            "SELECT code, name, account_type, parent_code, postable FROM accounts ORDER BY code",
        )
        .fetch_all(self.conn())
        .await
        .map_err(from_sqlx_error)?;

        let mut defs = Vec::with_capacity(rows.len());
        for (code, name, account_type, parent_code, postable) in rows {
            let code = AccountCode::parse(&code).map_err(|e| RepoError::Corrupt {
                reason: format!("保存されている科目コードが不正です（{e}）"),
            })?;
            let account_type = crate::convert::account_type_from_i16(account_type)?;
            let parent = parent_code
                .map(|p| AccountCode::parse(&p))
                .transpose()
                .map_err(|e| RepoError::Corrupt {
                    reason: format!("保存されている親科目コードが不正です（{e}）"),
                })?;
            defs.push(AccountDef {
                code,
                name,
                account_type,
                parent,
                postable,
            });
        }

        ChartOfAccounts::new(defs).map_err(|e| RepoError::Corrupt {
            reason: format!("保存されている勘定科目表が整合しません: {e}"),
        })
    }

    async fn load_counterparties(&mut self) -> Result<CounterpartyIndex, RepoError> {
        let rows: Vec<(String, String, Option<String>, Option<bool>)> = sqlx::query_as(
            "SELECT code, name, invoice_reg_no, is_qualified FROM counterparties ORDER BY code",
        )
        .fetch_all(self.conn())
        .await
        .map_err(from_sqlx_error)?;

        let counterparties = rows
            .into_iter()
            .map(|(code, name, invoice_reg_no, is_qualified)| Counterparty {
                code,
                name,
                invoice_registration_no: invoice_reg_no,
                is_qualified_invoice_issuer: is_qualified,
            })
            .collect();

        Ok(CounterpartyIndex::new(counterparties))
    }
}
