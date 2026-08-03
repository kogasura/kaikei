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

use kaikei_core::{AccountCode, AccountType, CoreError, Currency, Money};
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
///
/// # 通貨は行から推論せず、明示的に受け取る（PR-B 2巡目）
///
/// 1巡目は行から通貨を推論していたため、**0行の期間では通貨が決まらず**
/// `totals()` が `Ok(None)` を返していた。その結果、`get_trial_balance` の
/// 応答は空期間で通貨を名乗れず、合計欄も出せなかった
/// （「集計対象の通貨が単一であることを要求する」という `DECISIONS.md`
/// D-042 の要件も、行が無いと検査できない）。
///
/// [`TrialBalanceView::new`] は帳簿通貨
/// （[`crate::context::BookSettings::book_currency`]）を必須の引数として
/// 受け取る。これにより:
///
/// - **0行でも通貨を名乗れる**（合計は `0`）。
/// - 行の通貨が帳簿通貨と食い違えば `totals()` が
///   `CoreError::CurrencyMismatch` を返す（D-042 の実効的な検査）。
/// - `totals()` の戻り値から `Option` が消え、呼び出し側の分岐が減る。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrialBalanceView {
    rows: Vec<BalanceRowView>,
    currency: Currency,
}

impl TrialBalanceView {
    /// 行一覧と**この試算表の通貨**から試算表ビューを作る。
    ///
    /// `currency` は帳簿通貨（[`crate::context::BookSettings::book_currency`]）
    /// を渡す。行が0件でもこの値が応答の通貨になる。
    pub fn new(rows: Vec<BalanceRowView>, currency: Currency) -> Self {
        TrialBalanceView { rows, currency }
    }

    /// 全行を返す。
    pub fn rows(&self) -> &[BalanceRowView] {
        &self.rows
    }

    /// この試算表の通貨を返す。**行が0件でも決まる。**
    ///
    /// 応答の `currency` フィールドはこの値を使う
    /// （`kaikei_core::Currency` はコードと小数桁数の組なので、
    /// 金額文字列の解釈にも必要になる。`docs/07-mcp-server.md` §5）。
    pub fn currency(&self) -> Currency {
        self.currency
    }

    /// 借方合計・貸方合計を返す。**行が0件なら両方ゼロ**
    /// （[`TrialBalanceView::currency`] 建て）。
    ///
    /// # Errors
    ///
    /// 行の通貨がこの試算表の通貨と食い違う場合は
    /// `CoreError::CurrencyMismatch`（`DECISIONS.md` D-042「集計対象の通貨が
    /// 単一であること」の検査）。合算がオーバーフローする場合は
    /// `CoreError::InvalidAmount`。
    pub fn totals(&self) -> Result<(Money, Money), CoreError> {
        let mut debit = Money::zero(self.currency);
        let mut credit = Money::zero(self.currency);
        for row in &self.rows {
            // `Money::add` が通貨不一致を弾くため、ここで明示的な比較は要らない。
            debit = debit.add(&row.debit_total)?;
            credit = credit.add(&row.credit_total)?;
        }
        Ok((debit, credit))
    }

    /// 借方合計と貸方合計が一致するかどうかを検算する。行が無ければ自明に `true`。
    pub fn is_balanced(&self) -> Result<bool, CoreError> {
        let (debit, credit) = self.totals()?;
        Ok(debit == credit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    // TBV-1（PR-B 2巡目）: 0行でも通貨を名乗れ、合計はゼロになる。
    #[test]
    fn empty_trial_balance_view_still_names_its_currency() {
        let tb = TrialBalanceView::new(Vec::new(), Currency::JPY);
        assert_eq!(tb.currency(), Currency::JPY);
        let (debit, credit) = tb.totals().unwrap();
        assert_eq!(debit.minor(), 0);
        assert_eq!(credit.minor(), 0);
        assert_eq!(debit.currency(), Currency::JPY);
        assert_eq!(credit.currency(), Currency::JPY);
        assert!(tb.is_balanced().unwrap());
    }

    // TBV-2: 空でも通貨は行から推論していない（USD の空期間は USD を名乗る）。
    #[test]
    fn empty_trial_balance_view_uses_the_declared_currency_not_a_default() {
        let tb = TrialBalanceView::new(Vec::new(), Currency::USD);
        assert_eq!(tb.currency(), Currency::USD);
        assert_eq!(tb.totals().unwrap().0.currency(), Currency::USD);
    }

    #[test]
    fn balanced_rows_are_balanced() {
        let tb = TrialBalanceView::new(
            vec![
                row("100", AccountType::Asset, 1_000, 0),
                row("500", AccountType::Revenue, 0, 1_000),
            ],
            Currency::JPY,
        );
        let (debit, credit) = tb.totals().unwrap();
        assert_eq!(debit.minor(), 1_000);
        assert_eq!(credit.minor(), 1_000);
        assert!(tb.is_balanced().unwrap());
    }

    #[test]
    fn unbalanced_rows_are_not_balanced() {
        let tb = TrialBalanceView::new(
            vec![
                row("100", AccountType::Asset, 1_000, 0),
                row("500", AccountType::Revenue, 0, 900),
            ],
            Currency::JPY,
        );
        assert!(!tb.is_balanced().unwrap());
    }

    // TBV-3（PR-B 2巡目 / `DECISIONS.md` D-042）: 行の通貨が試算表の通貨と
    // 食い違えば、合計を出す時点で検出される。
    #[test]
    fn rows_in_another_currency_are_rejected_when_totalling() {
        let usd_row = BalanceRowView {
            account: AccountCode::parse("100").unwrap(),
            account_type: AccountType::Asset,
            group: GroupKeyView::new(),
            debit_total: Money::from_minor(1_000, Currency::USD),
            credit_total: Money::zero(Currency::USD),
            balance: Money::from_minor(1_000, Currency::USD),
        };
        let tb = TrialBalanceView::new(vec![usd_row], Currency::JPY);
        assert!(matches!(
            tb.totals(),
            Err(CoreError::CurrencyMismatch { .. })
        ));
    }
}
