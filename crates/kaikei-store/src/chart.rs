//! [`kaikei_app::ports::ChartRepo`] / [`kaikei_app::ports::ChartWriteRepo`] の
//! PostgreSQL 実装。

use crate::convert::account_type_to_i16;
use crate::error::from_sqlx_error;
use crate::store::PgTx;
use async_trait::async_trait;
use kaikei_app::error::RepoError;
use kaikei_app::ports::{ChartRepo, ChartWriteRepo};
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

#[async_trait]
impl ChartWriteRepo for PgTx<'_> {
    /// # 1文でまとめて INSERT する
    ///
    /// 同梱テンプレートは約60件ある。1件ずつ INSERT すると起動のたびに60往復
    /// するので、明細の一括 INSERT（`DECISIONS.md` D-040）と同じく `UNNEST` で
    /// 1文にまとめる。
    ///
    /// # 親子関係を2パスに分けない
    ///
    /// `accounts.parent_code` は `accounts(code)` への自己参照 FK だが、
    /// **同一文の中で挿入された行同士なら順序を問わない**。PostgreSQL の
    /// 参照整合性チェックは AFTER ROW トリガとして**文の終わりに**まとめて
    /// 発火するため、子が親より先に並んでいても、親が同じ `UNNEST` に
    /// 含まれていれば通る（`crates/kaikei-store/tests/chart_import.rs` の
    /// `insert_accounts_accepts_a_child_listed_before_its_parent` が実 DB で
    /// 確認する）。
    ///
    /// 「親を NULL で入れてから `UPDATE` で親を張る」2パス方式を採らないのは、
    /// [`ChartWriteRepo`] が「既存行を `UPDATE` しない」ことを契約にしている
    /// ため（`DECISIONS.md` D-081）。
    ///
    /// # 既存行は絶対に触らない
    ///
    /// `ON CONFLICT (code) DO NOTHING`。`DO UPDATE` に書き換えないこと
    /// （既に仕訳が参照している科目の意味が後から変わる）。
    async fn insert_accounts(&mut self, defs: &[AccountDef]) -> Result<usize, RepoError> {
        if defs.is_empty() {
            return Ok(0);
        }

        let codes: Vec<String> = defs.iter().map(|d| d.code.as_str().to_string()).collect();
        let names: Vec<String> = defs.iter().map(|d| d.name.clone()).collect();
        let account_types: Vec<i16> = defs
            .iter()
            .map(|d| account_type_to_i16(d.account_type))
            .collect();
        let parents: Vec<Option<String>> = defs
            .iter()
            .map(|d| d.parent.as_ref().map(|p| p.as_str().to_string()))
            .collect();
        let postables: Vec<bool> = defs.iter().map(|d| d.postable).collect();

        let result = sqlx::query(
            "INSERT INTO accounts (code, name, account_type, parent_code, postable) \
             SELECT code, name, account_type, parent_code, postable \
             FROM UNNEST($1::text[], $2::text[], $3::smallint[], $4::text[], $5::bool[]) \
                  AS t(code, name, account_type, parent_code, postable) \
             ON CONFLICT (code) DO NOTHING",
        )
        .bind(&codes)
        .bind(&names)
        .bind(&account_types)
        .bind(&parents)
        .bind(&postables)
        .execute(self.conn())
        .await
        .map_err(from_sqlx_error)?;

        // `rows_affected` は u64。usize が 32bit の環境でも壊れないように
        // 明示的に変換する（科目数が usize を溢れることは現実には無い）。
        Ok(usize::try_from(result.rows_affected()).unwrap_or(usize::MAX))
    }
}
