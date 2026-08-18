//! 青色申告決算書2ページ目の「月別売上（収入）金額及び仕入金額」に書く数字。
//!
//! # 様式は模さない
//!
//! この帳簿は**国税庁の様式を模した帳票を作らない**（`docs/10-report.md` §5)。
//! 様式の正確さに責任を持つことになるためである。ここで出すのは
//! **様式へ書き写すための数字**であって、様式そのものではない。
//!
//! そのため科目ごとに分けて出す。売上高・家事消費等・雑収入・仕入金額を
//! 混ぜずに並べれば、様式の行がどう分かれていても書き写せる。**まとめて
//! しまうと、様式の行と合わないときに分解できない。**
//!
//! # 決算書1ページ目と突き合わせられる
//!
//! 月ごとの合計は、損益計算書の同じ科目の年計と一致するはずである。
//! [`MonthlySales::total_of`] で引けるようにしてあるのはそのため。
//! **一致しないなら、どちらかの集計が間違っている。**

use kaikei_core::{AccountCode, JournalEntry, Money, Side};
use std::collections::BTreeMap;

/// 1つの科目の、月ごとの金額。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonthlyAccount {
    /// 科目コード。
    pub account: AccountCode,
    /// 科目名。
    pub name: String,
    /// 1月から12月までの金額。添字0が1月。
    pub by_month: [Money; 12],
    /// 年計。
    pub total: Money,
}

/// 月別の集計。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonthlySales {
    /// 収益の科目（売上高・家事消費等・雑収入など）。科目コード順。
    pub revenue: Vec<MonthlyAccount>,
    /// 仕入の科目。科目コード順。
    pub purchases: Vec<MonthlyAccount>,
}

impl MonthlySales {
    /// その科目の年計。**決算書と突き合わせるために使う。**
    #[must_use]
    pub fn total_of(&self, account: &AccountCode) -> Option<Money> {
        self.revenue
            .iter()
            .chain(self.purchases.iter())
            .find(|row| &row.account == account)
            .map(|row| row.total)
    }

    /// 収益の月ごとの合計（全科目を足したもの）。
    #[must_use]
    pub fn revenue_by_month(&self, currency: kaikei_core::Currency) -> [Money; 12] {
        sum_by_month(&self.revenue, currency)
    }

    /// 仕入の月ごとの合計。
    #[must_use]
    pub fn purchases_by_month(&self, currency: kaikei_core::Currency) -> [Money; 12] {
        sum_by_month(&self.purchases, currency)
    }
}

fn sum_by_month(rows: &[MonthlyAccount], currency: kaikei_core::Currency) -> [Money; 12] {
    let mut totals = [Money::from_minor(0, currency); 12];
    for row in rows {
        for (slot, amount) in totals.iter_mut().zip(row.by_month.iter()) {
            *slot = Money::from_minor(slot.minor() + amount.minor(), currency);
        }
    }
    totals
}

/// 月ごとに集計する。
///
/// # 貸方から借方を引く
///
/// 収益は貸方に立つが、返品・値引きは借方に立つ。**引かないと、返品した
/// 月の売上が実際より多く出る。** 仕入はその逆（借方が正）。
///
/// # 逆仕訳も数える
///
/// 訂正の逆仕訳は元の仕訳を打ち消すためのものなので、除くと訂正が効かない。
/// 重複の検査（`check_suspected_duplicates`）が逆仕訳を除くのとは逆である
/// ——あちらは「同じ形が2つある」ことを見るので、鏡写しを数えると誤る。
///
/// # 決算振替は入らない
///
/// 呼び出し側が渡す仕訳の範囲で決まる。決算振替を含めて渡すと収益がゼロ化
/// された分まで数えるので、**決算書と同じく決算振替を外して渡すこと**
/// （`DECISIONS.md` D-101）。
#[must_use]
pub fn summarize(
    entries: &[JournalEntry],
    chart: &kaikei_core::ChartOfAccounts,
    revenue_accounts: &[AccountCode],
    purchase_accounts: &[AccountCode],
    currency: kaikei_core::Currency,
) -> MonthlySales {
    let collect = |wanted: &[AccountCode], credit_is_positive: bool| {
        let mut by_account: BTreeMap<AccountCode, [i128; 12]> = BTreeMap::new();
        for code in wanted {
            by_account.insert(code.clone(), [0; 12]);
        }
        for entry in entries {
            let month = entry.entry_date().month() as usize;
            // 年度をまたぐ仕訳は呼び出し側が除いている前提だが、
            // 万一入っていても添字で落ちないようにする。
            let Some(index) = month.checked_sub(1).filter(|index| *index < 12) else {
                continue;
            };
            for line in entry.lines() {
                let Some(slot) = by_account.get_mut(line.account()) else {
                    continue;
                };
                let signed = if (line.side() == Side::Credit) == credit_is_positive {
                    line.amount().minor()
                } else {
                    -line.amount().minor()
                };
                slot[index] += signed;
            }
        }
        by_account
            .into_iter()
            .map(|(account, months)| {
                let by_month = months.map(|minor| Money::from_minor(minor, currency));
                MonthlyAccount {
                    name: chart
                        .get(&account)
                        .map_or_else(|| account.as_str().to_string(), |def| def.name.clone()),
                    total: Money::from_minor(months.iter().sum::<i128>(), currency),
                    account,
                    by_month,
                }
            })
            .collect::<Vec<_>>()
    };

    MonthlySales {
        revenue: collect(revenue_accounts, true),
        purchases: collect(purchase_accounts, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::new_entry;
    use kaikei_core::{
        AccountDef, AccountType, AccountingDate, ChartOfAccounts, Currency, FiscalYear,
        JournalLine, TagSchema, TagSet,
    };

    fn chart() -> ChartOfAccounts {
        let def = |code: &str, name: &str, kind: AccountType| AccountDef {
            code: AccountCode::parse(code).unwrap(),
            name: name.to_string(),
            account_type: kind,
            parent: None,
            postable: true,
        };
        ChartOfAccounts::new(vec![
            def("110", "普通預金", AccountType::Asset),
            def("500", "売上高", AccountType::Revenue),
            def("520", "雑収入", AccountType::Revenue),
            def("555", "仕入金額", AccountType::Expense),
        ])
        .unwrap()
    }

    fn code(s: &str) -> AccountCode {
        AccountCode::parse(s).unwrap()
    }

    fn yen(v: i128) -> Money {
        Money::from_minor(v, Currency::JPY)
    }

    fn line(account: &str, side: Side, amount: i128) -> JournalLine {
        JournalLine::new(code(account), side, yen(amount), TagSet::new(), None).unwrap()
    }

    fn sale(
        id: u128,
        month: u8,
        day: u8,
        account: &str,
        amount: i128,
    ) -> kaikei_core::JournalEntry {
        new_entry(
            id,
            id as u32,
            &FiscalYear::calendar_year(2026),
            &chart(),
            &TagSchema::empty(),
            AccountingDate::new(2026, month, day).unwrap(),
            "売上",
            vec![
                line("110", Side::Debit, amount),
                line(account, Side::Credit, amount),
            ],
        )
    }

    fn run(entries: &[kaikei_core::JournalEntry]) -> MonthlySales {
        summarize(
            entries,
            &chart(),
            &[code("500"), code("520")],
            &[code("555")],
            Currency::JPY,
        )
    }

    // **本命。** 取引日の月に振り分ける。
    #[test]
    fn each_sale_lands_in_the_month_it_was_dated() {
        let summary = run(&[sale(1, 1, 15, "500", 100), sale(2, 12, 31, "500", 200)]);

        let sales = &summary.revenue[0];
        assert_eq!(sales.by_month[0].minor(), 100, "1月");
        assert_eq!(sales.by_month[11].minor(), 200, "12月");
        assert_eq!(sales.by_month[1].minor(), 0, "2月は空");
        assert_eq!(sales.total.minor(), 300);
    }

    // **本命。** 同じ月の売上は足し合わせる。
    #[test]
    fn sales_in_the_same_month_are_added() {
        let summary = run(&[sale(1, 3, 1, "500", 100), sale(2, 3, 31, "500", 50)]);

        assert_eq!(summary.revenue[0].by_month[2].minor(), 150);
    }

    // **本命。** 返品・値引きは引く。
    //
    // 収益は貸方に立つが、返品は借方に立つ。**引かないと、返品した月の
    // 売上が実際より多く出る。** 月別の数字はそのまま様式に書き写すので、
    // 多く出れば所得を過大に申告することになる。
    #[test]
    fn a_return_is_subtracted_from_the_month() {
        let refund = new_entry(
            9,
            9,
            &FiscalYear::calendar_year(2026),
            &chart(),
            &TagSchema::empty(),
            AccountingDate::new(2026, 3, 20).unwrap(),
            "返品",
            vec![line("500", Side::Debit, 30), line("110", Side::Credit, 30)],
        );
        let summary = run(&[sale(1, 3, 1, "500", 100), refund]);

        assert_eq!(summary.revenue[0].by_month[2].minor(), 70, "100 − 30");
        assert_eq!(summary.revenue[0].total.minor(), 70);
    }

    // **本命。** 仕入は借方が正。
    //
    // 収益とは逆。取り違えると仕入が負になり、原価がマイナスになる。
    #[test]
    fn purchases_count_the_debit_side_as_positive() {
        let purchase = new_entry(
            1,
            1,
            &FiscalYear::calendar_year(2026),
            &chart(),
            &TagSchema::empty(),
            AccountingDate::new(2026, 5, 10).unwrap(),
            "仕入",
            vec![
                line("555", Side::Debit, 800),
                line("110", Side::Credit, 800),
            ],
        );
        let summary = run(&[purchase]);

        assert_eq!(summary.purchases[0].by_month[4].minor(), 800);
        assert_eq!(summary.purchases[0].total.minor(), 800);
    }

    // **本命。** 科目を混ぜない。
    //
    // 様式の行がどう分かれていても書き写せるようにするため、売上高と
    // 雑収入は別々に出す。**まとめると分解できない。**
    #[test]
    fn revenue_accounts_are_kept_apart() {
        let summary = run(&[sale(1, 6, 1, "500", 100), sale(2, 6, 2, "520", 7)]);

        assert_eq!(summary.revenue.len(), 2);
        assert_eq!(summary.revenue[0].account.as_str(), "500");
        assert_eq!(summary.revenue[0].name, "売上高");
        assert_eq!(summary.revenue[0].by_month[5].minor(), 100);
        assert_eq!(summary.revenue[1].account.as_str(), "520");
        assert_eq!(summary.revenue[1].by_month[5].minor(), 7);
    }

    // 取引が無い科目も行として残す。**行が消えると様式の欄が埋まらない。**
    #[test]
    fn an_account_with_no_activity_still_gets_a_row() {
        let summary = run(&[sale(1, 6, 1, "500", 100)]);

        assert_eq!(summary.revenue.len(), 2, "雑収入の行も残ること");
        assert_eq!(summary.revenue[1].total.minor(), 0);
        assert_eq!(summary.purchases.len(), 1, "仕入の行も残ること");
    }

    // **本命。** 月ごとの合計を出す。
    #[test]
    fn the_monthly_totals_add_up_across_accounts() {
        let summary = run(&[sale(1, 6, 1, "500", 100), sale(2, 6, 2, "520", 7)]);

        assert_eq!(summary.revenue_by_month(Currency::JPY)[5].minor(), 107);
    }

    // 決算書と突き合わせるために、科目コードで年計を引ける。
    #[test]
    fn a_total_can_be_looked_up_by_account() {
        let summary = run(&[sale(1, 6, 1, "500", 100)]);

        assert_eq!(summary.total_of(&code("500")), Some(yen(100)));
        assert_eq!(summary.total_of(&code("999")), None);
    }
}
