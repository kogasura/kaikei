//! 会計期間と取引日。
//!
//! 取引日はタイムゾーンを持たない純粋な日付（`CLAUDE.md` §7）。`chrono` は
//! `kaikei-core` の依存に無いため、閏年判定・月ごとの日数計算は自前で行う。

use crate::error::CoreError;

/// 取引日。タイムゾーンを持たない純粋な日付。
///
/// フィールドの宣言順（`year`, `month`, `day`）が derive された `Ord` の
/// 比較順序と一致するため、日付としての自然な大小比較がそのまま成り立つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct AccountingDate {
    year: i32,
    month: u8,
    day: u8,
}

impl AccountingDate {
    /// 日付を作る。実在しない日付（2月30日、閏年でない年の2月29日等）は
    /// `CoreError::InvalidValue`。
    pub fn new(year: i32, month: u8, day: u8) -> Result<Self, CoreError> {
        if !(1..=12).contains(&month) {
            return Err(CoreError::InvalidValue {
                reason: format!("月は1〜12である必要があります: {month}"),
            });
        }
        let days = days_in_month(year, month);
        if day < 1 || day > days {
            return Err(CoreError::InvalidValue {
                reason: format!(
                    "{year}年{month}月に{day}日は存在しません（{year}年{month}月は{days}日までです）"
                ),
            });
        }
        Ok(AccountingDate { year, month, day })
    }

    /// `"2026-04-15"` のような ISO 形式（`YYYY-MM-DD`）から構築する。
    /// スラッシュ区切り等、ISO 形式以外の文字列は拒否する。
    pub fn parse(s: &str) -> Result<Self, CoreError> {
        let invalid = || CoreError::InvalidValue {
            reason: format!("日付は YYYY-MM-DD 形式である必要があります: \"{s}\""),
        };

        let parts: Vec<&str> = s.split('-').collect();
        let [year_part, month_part, day_part] = parts.as_slice() else {
            return Err(invalid());
        };

        let is_digits = |part: &str, width: usize| {
            part.len() == width && part.bytes().all(|b| b.is_ascii_digit())
        };
        if !is_digits(year_part, 4) || !is_digits(month_part, 2) || !is_digits(day_part, 2) {
            return Err(invalid());
        }

        let year: i32 = year_part.parse().map_err(|_| invalid())?;
        let month: u8 = month_part.parse().map_err(|_| invalid())?;
        let day: u8 = day_part.parse().map_err(|_| invalid())?;

        AccountingDate::new(year, month, day)
    }

    /// 年を返す。
    pub fn year(&self) -> i32 {
        self.year
    }

    /// 月（1〜12）を返す。
    pub fn month(&self) -> u8 {
        self.month
    }

    /// 日を返す。
    pub fn day(&self) -> u8 {
        self.day
    }

    /// `"2026-04-15"` 形式の文字列に変換する。
    pub fn to_iso_string(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

/// グレゴリオ暦の閏年判定。
///
/// 「4で割り切れる かつ（100で割り切れない または 400で割り切れる）」。
fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// 指定した年月の日数。`month` は呼び出し側で 1〜12 に検証済みであること。
fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// 会計年度。個人事業主は暦年（1/1〜12/31）だが、汎用的に開始日・終了日を持てる形にしておく。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FiscalYear {
    label: i32,
    start: AccountingDate,
    end: AccountingDate,
}

impl FiscalYear {
    /// 会計年度を作る。`start` が `end` より後ならエラー。
    pub fn new(label: i32, start: AccountingDate, end: AccountingDate) -> Result<Self, CoreError> {
        if start > end {
            return Err(CoreError::InvalidValue {
                reason: format!(
                    "会計年度の開始日が終了日より後です: 開始={} 終了={}",
                    start.to_iso_string(),
                    end.to_iso_string()
                ),
            });
        }
        Ok(FiscalYear { label, start, end })
    }

    /// 暦年（1/1〜12/31）の会計年度を作る。個人事業主用のショートカット。
    pub fn calendar_year(year: i32) -> Self {
        let start = AccountingDate::new(year, 1, 1).expect("暦年の1/1は常に有効な日付である");
        let end = AccountingDate::new(year, 12, 31).expect("暦年の12/31は常に有効な日付である");
        FiscalYear {
            label: year,
            start,
            end,
        }
    }

    /// 指定した日付がこの会計年度の範囲内か。開始日・終了日を含む閉区間で判定する。
    pub fn contains(&self, date: AccountingDate) -> bool {
        self.start <= date && date <= self.end
    }

    /// 会計年度ラベル（例: `2026`）を返す。
    pub fn label(&self) -> i32 {
        self.label
    }

    /// 開始日を返す。
    pub fn start(&self) -> AccountingDate {
        self.start
    }

    /// 終了日を返す。
    pub fn end(&self) -> AccountingDate {
        self.end
    }
}

/// 会計期間の締め状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeriodStatus {
    /// 記帳・訂正が可能。
    Open,
    /// 締められており、記帳・訂正ができない。
    Closed,
}

/// 締め状態の判定に使う。実データ（どの期間が締められているか）は store 層が持つ。
pub trait PeriodGuard {
    /// 指定した取引日の締め状態を返す。
    fn status(&self, date: AccountingDate) -> PeriodStatus;
}

#[cfg(test)]
mod tests {
    use super::*;

    // P-01
    #[test]
    fn accounting_date_new_rejects_feb29_in_non_leap_year() {
        assert!(AccountingDate::new(2026, 2, 29).is_err());
    }

    // 閏年判定の回帰防止: 1900年は100で割り切れるが400で割り切れないため非閏年
    #[test]
    fn accounting_date_new_rejects_feb29_in_1900() {
        assert!(AccountingDate::new(1900, 2, 29).is_err());
    }

    // P-02
    #[test]
    fn accounting_date_new_accepts_feb29_in_leap_year() {
        assert!(AccountingDate::new(2024, 2, 29).is_ok());
    }

    // 閏年判定の回帰防止: 2000年は400で割り切れるため閏年
    #[test]
    fn accounting_date_new_accepts_feb29_in_2000() {
        assert!(AccountingDate::new(2000, 2, 29).is_ok());
    }

    // P-03
    #[test]
    fn accounting_date_new_rejects_month_out_of_range() {
        assert!(AccountingDate::new(2026, 13, 1).is_err());
    }

    // P-04
    #[test]
    fn accounting_date_new_rejects_day_out_of_range() {
        assert!(AccountingDate::new(2026, 4, 31).is_err());
    }

    // P-05
    #[test]
    fn accounting_date_parse_iso_format_succeeds() {
        let d = AccountingDate::parse("2026-04-15").unwrap();
        assert_eq!(d.year(), 2026);
        assert_eq!(d.month(), 4);
        assert_eq!(d.day(), 15);
    }

    // P-06
    #[test]
    fn accounting_date_parse_slash_format_is_error() {
        assert!(AccountingDate::parse("2026/04/15").is_err());
    }

    // P-07
    #[test]
    fn accounting_date_ordering_compares_correctly() {
        let a = AccountingDate::new(2026, 1, 31).unwrap();
        let b = AccountingDate::new(2026, 2, 1).unwrap();
        let c = AccountingDate::new(2027, 1, 1).unwrap();
        assert!(a < b);
        assert!(b < c);
        assert!(a < c);
    }

    // P-08
    #[test]
    fn fiscal_year_calendar_year_spans_full_year() {
        let fy = FiscalYear::calendar_year(2026);
        assert_eq!(fy.start(), AccountingDate::new(2026, 1, 1).unwrap());
        assert_eq!(fy.end(), AccountingDate::new(2026, 12, 31).unwrap());
    }

    // P-09
    #[test]
    fn fiscal_year_contains_includes_boundary_dates() {
        let fy = FiscalYear::calendar_year(2026);
        assert!(fy.contains(AccountingDate::new(2026, 1, 1).unwrap()));
        assert!(fy.contains(AccountingDate::new(2026, 12, 31).unwrap()));
    }

    // P-10
    #[test]
    fn fiscal_year_contains_returns_false_outside_range() {
        let fy = FiscalYear::calendar_year(2026);
        assert!(!fy.contains(AccountingDate::new(2025, 12, 31).unwrap()));
        assert!(!fy.contains(AccountingDate::new(2027, 1, 1).unwrap()));
    }

    // P-11
    #[test]
    fn fiscal_year_new_rejects_start_after_end() {
        let start = AccountingDate::new(2026, 12, 31).unwrap();
        let end = AccountingDate::new(2026, 1, 1).unwrap();
        assert!(FiscalYear::new(2026, start, end).is_err());
    }
}
