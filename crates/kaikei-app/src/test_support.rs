//! ユースケースの結合テスト（`usecase::post_entry` / `usecase::reverse_entry`）と
//! [`crate::testing`] の内部テストが共有する最小限のテストヘルパ。
//!
//! `sample_chart()` / `settings()` / `fixed_clock()` / `AllOpen`
//! （常に Open を返す `PeriodGuard`）がこれまで各テストモジュールに
//! ほぼ同一の形で重複していたため、ここに集約する。`#[cfg(test)]` 専用
//! （本番コードからは参照されない）であり、[`crate::testing`] の公開 fake
//! （`InMemoryStore` 等。`DECISIONS.md` D-029 で凍結された契約の一部）の
//! 挙動には一切触れない。

use crate::context::{BookSettings, FiscalYearRule};
use kaikei_core::{
    AccountCode, AccountDef, AccountType, AccountingDate, ChartOfAccounts, Currency, FixedClock,
    PeriodGuard, PeriodStatus, Timestamp,
};

/// 現金(100・資産)と売上高(500・収益)のみを持つ最小の勘定科目表。
pub(crate) fn sample_chart() -> ChartOfAccounts {
    ChartOfAccounts::new(vec![
        AccountDef {
            code: AccountCode::parse("100").unwrap(),
            name: "現金".to_string(),
            account_type: AccountType::Asset,
            parent: None,
            postable: true,
        },
        AccountDef {
            code: AccountCode::parse("500").unwrap(),
            name: "売上高".to_string(),
            account_type: AccountType::Revenue,
            parent: None,
            postable: true,
        },
    ])
    .unwrap()
}

/// [`sample_chart`] に仮受消費税(330・負債)を加えた勘定科目表。
/// 税額行の自動生成（`FlatRateTaxPolicy`）を使うテストに使う。
pub(crate) fn sample_chart_with_tax_account() -> ChartOfAccounts {
    ChartOfAccounts::new(vec![
        AccountDef {
            code: AccountCode::parse("100").unwrap(),
            name: "現金".to_string(),
            account_type: AccountType::Asset,
            parent: None,
            postable: true,
        },
        AccountDef {
            code: AccountCode::parse("500").unwrap(),
            name: "売上高".to_string(),
            account_type: AccountType::Revenue,
            parent: None,
            postable: true,
        },
        AccountDef {
            code: AccountCode::parse("330").unwrap(),
            name: "仮受消費税".to_string(),
            account_type: AccountType::Liability,
            parent: None,
            postable: true,
        },
        // 費用を2つ。**科目の一貫性の検査（`check_inconsistent_accounts`）は
        // 費用の科目だけを見る**ので、これが無いと何も拾えない。
        AccountDef {
            code: AccountCode::parse("604").unwrap(),
            name: "通信費".to_string(),
            account_type: AccountType::Expense,
            parent: None,
            postable: true,
        },
        AccountDef {
            code: AccountCode::parse("621").unwrap(),
            name: "新聞図書費".to_string(),
            account_type: AccountType::Expense,
            parent: None,
            postable: true,
        },
    ])
    .unwrap()
}

/// 暦年ルール・帳簿通貨 JPY の `BookSettings`。
pub(crate) fn settings() -> BookSettings {
    BookSettings {
        fiscal_year_rule: FiscalYearRule::CalendarYear,
        book_currency: Currency::JPY,
    }
}

/// Unix epoch を返す固定時刻の `Clock`。
pub(crate) fn fixed_clock() -> FixedClock {
    FixedClock(Timestamp::from_unix_nanos(0))
}

/// 常に Open を返す `PeriodGuard`。締め状態を検証しないテストに使う。
pub(crate) struct AllOpen;

impl PeriodGuard for AllOpen {
    fn status(&self, _date: AccountingDate) -> PeriodStatus {
        PeriodStatus::Open
    }
}
