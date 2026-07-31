//! read model 専用の DTO（[`BalanceRowView`] / [`TrialBalanceView`]）。
//!
//! `kaikei_core::GroupKey` には `impl` ブロックが1つも無く、公開コンストラクタも
//! アクセサも存在しない（実測確認済み）。したがって `kaikei_core::BalanceRow` /
//! `TrialBalance` は core の外から構築できない。SQL 集計（`kaikei-store::query`、
//! PR-6）から直接組み立てられる read model 専用の DTO をここに定義する
//! （`DECISIONS.md` D-031）。
//!
//! 金額は文字列ではなく [`kaikei_core::Money`] のまま保持する。`DECISIONS.md`
//! D-013「JSON では金額を文字列で扱う」は presentation 層（HTTP/MCP 応答）が
//! 外部にシリアライズする形式についての決定であり、この DTO は
//! `kaikei-app` の呼び出し元にプロセス内でそのまま渡す中間表現なので対象外。

use kaikei_core::{sum_money, AccountCode, AccountType, CoreError, Money};
use std::collections::BTreeMap;

/// `group_by` のグループキー。指定したタグキー文字列と値文字列の組。
///
/// キーの型を `kaikei_core::TagKey` ではなく `String` にしているのは、この
/// DTO が SQL の集計結果（例: `jsonb_object_agg`）から直接組み立てられることを
/// 想定しているため（`TagKey::parse` による再検証を read model の構築の
/// たびに強制しない）。
pub type GroupKeyView = BTreeMap<String, String>;

/// 試算表の1行（read model 版）。
///
/// フィールド構成は `kaikei_core::BalanceRow` に対応するが、`group` の型が
/// `kaikei_core::GroupKey`（構築不能）ではなく [`GroupKeyView`] になっている
/// 点が異なる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BalanceRowView {
    /// 勘定科目コード。
    pub account: AccountCode,
    /// 科目種別。残高の符号を決めるために保持する。
    pub account_type: AccountType,
    /// `group_by` によるグループキー。`group_by` を指定しなければ常に空。
    pub group: GroupKeyView,
    /// 借方合計。
    pub debit_total: Money,
    /// 貸方合計。
    pub credit_total: Money,
    /// `account_type.is_debit_normal()` に従った符号付き残高。
    pub balance: Money,
}

/// 試算表（read model 版）。
///
/// [`crate::ports::TrialBalanceQuery::trial_balance`] が返す行一覧をラップし、
/// 検算等の補助メソッドを提供する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrialBalanceView {
    rows: Vec<BalanceRowView>,
}

impl TrialBalanceView {
    /// 行一覧から試算表ビューを作る。
    pub fn new(rows: Vec<BalanceRowView>) -> Self {
        TrialBalanceView { rows }
    }

    /// 全行を返す。
    pub fn rows(&self) -> &[BalanceRowView] {
        &self.rows
    }

    /// 借方合計・貸方合計を返す。行が1つも無ければ通貨を決定できないため `None`。
    ///
    /// # Errors
    ///
    /// 行同士の通貨が食い違う場合（本来あってはならない）は
    /// `CoreError::CurrencyMismatch`。合算がオーバーフローする場合は
    /// `CoreError::InvalidAmount`。
    pub fn totals(&self) -> Result<Option<(Money, Money)>, CoreError> {
        let debit = sum_money(self.rows.iter().map(|row| &row.debit_total))?;
        let credit = sum_money(self.rows.iter().map(|row| &row.credit_total))?;
        Ok(match (debit, credit) {
            (Some(d), Some(c)) => Some((d, c)),
            _ => None,
        })
    }

    /// 借方合計と貸方合計が一致するかどうかを検算する。行が無ければ自明に `true`。
    pub fn is_balanced(&self) -> Result<bool, CoreError> {
        Ok(match self.totals()? {
            Some((debit, credit)) => debit == credit,
            None => true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_core::Currency;

    fn row(account: &str, account_type: AccountType, debit: i128, credit: i128) -> BalanceRowView {
        let debit_total = Money::from_minor(debit, Currency::JPY);
        let credit_total = Money::from_minor(credit, Currency::JPY);
        let balance = if account_type.is_debit_normal() {
            debit_total.sub(&credit_total).unwrap()
        } else {
            credit_total.sub(&debit_total).unwrap()
        };
        BalanceRowView {
            account: AccountCode::parse(account).unwrap(),
            account_type,
            group: GroupKeyView::new(),
            debit_total,
            credit_total,
            balance,
        }
    }

    #[test]
    fn empty_trial_balance_view_is_balanced() {
        let tb = TrialBalanceView::new(Vec::new());
        assert_eq!(tb.totals().unwrap(), None);
        assert!(tb.is_balanced().unwrap());
    }

    #[test]
    fn balanced_rows_are_balanced() {
        let tb = TrialBalanceView::new(vec![
            row("100", AccountType::Asset, 1_000, 0),
            row("500", AccountType::Revenue, 0, 1_000),
        ]);
        let (debit, credit) = tb.totals().unwrap().unwrap();
        assert_eq!(debit.minor(), 1_000);
        assert_eq!(credit.minor(), 1_000);
        assert!(tb.is_balanced().unwrap());
    }

    #[test]
    fn unbalanced_rows_are_not_balanced() {
        let tb = TrialBalanceView::new(vec![
            row("100", AccountType::Asset, 1_000, 0),
            row("500", AccountType::Revenue, 0, 900),
        ]);
        assert!(!tb.is_balanced().unwrap());
    }
}
