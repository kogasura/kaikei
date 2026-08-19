//! 月別売上（収入）金額及び仕入金額の CSV。
//!
//! 青色申告決算書2ページ目へ**書き写すための数字**であって、様式そのもの
//! ではない（`kaikei_jp::monthly_sales` のモジュール doc を参照）。

use crate::csv::{CsvBuilder, UTF8_BOM};
use kaikei_jp::monthly_sales::{MonthlyAccount, MonthlySales};

/// CSV にする。
///
/// # 行が科目、列が月
///
/// 様式は縦に月が並ぶが、**CSV は科目を行にする。** 科目が増えても列が
/// 増えないほうが表計算で扱いやすく、様式へ書き写すときは12個の数字を
/// 横に読めばよい。
pub fn to_csv(summary: &MonthlySales) -> String {
    let mut csv = CsvBuilder::new();
    let mut header = vec!["区分".to_string(), "科目".to_string(), "科目名".to_string()];
    for month in 1..=12 {
        header.push(format!("{month}月"));
    }
    header.push("計".to_string());
    csv.push_row(header);

    let push = |csv: &mut CsvBuilder, kind: &str, row: &MonthlyAccount| {
        let mut cells = vec![
            kind.to_string(),
            row.account.as_str().to_string(),
            row.name.clone(),
        ];
        for amount in &row.by_month {
            cells.push(amount.minor().to_string());
        }
        cells.push(row.total.minor().to_string());
        csv.push_row(cells);
    };

    for row in &summary.revenue {
        push(&mut csv, "売上（収入）", row);
    }
    for row in &summary.purchases {
        push(&mut csv, "仕入", row);
    }
    format!("{UTF8_BOM}{}", csv.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_core::{AccountCode, Currency, Money};

    fn row(code: &str, name: &str, january: i128, total: i128) -> MonthlyAccount {
        let mut by_month = [Money::from_minor(0, Currency::JPY); 12];
        by_month[0] = Money::from_minor(january, Currency::JPY);
        MonthlyAccount {
            account: AccountCode::parse(code).unwrap(),
            name: name.to_string(),
            by_month,
            total: Money::from_minor(total, Currency::JPY),
        }
    }

    fn summary() -> MonthlySales {
        MonthlySales {
            revenue: vec![row("500", "売上高", 410_000, 9_757_440)],
            purchases: vec![row("555", "仕入金額", 0, 0)],
        }
    }

    // **本命。** 12か月ぶんの列と計がある。
    #[test]
    fn there_is_a_column_for_every_month_and_a_total() {
        let csv = to_csv(&summary());
        let header = csv.lines().next().unwrap();

        for month in 1..=12 {
            assert!(header.contains(&format!("{month}月")), "{header}");
        }
        assert!(header.ends_with("計"), "{header}");
    }

    // **本命。** 売上と仕入を区分で分ける。
    #[test]
    fn revenue_and_purchases_are_labelled() {
        let csv = to_csv(&summary());

        assert!(csv.contains("売上（収入）,500,売上高"), "{csv}");
        assert!(csv.contains("仕入,555,仕入金額"), "{csv}");
    }

    // **本命。** 金額に桁区切りを入れない（他の CSV と同じ）。
    #[test]
    fn the_amounts_have_no_thousands_separator() {
        let csv = to_csv(&summary());

        assert!(csv.contains("9757440"), "{csv}");
        assert!(!csv.contains("9,757,440"), "{csv}");
    }

    // 取引が無くても行は残す。**行が消えると様式の欄が埋まらない。**
    #[test]
    fn a_zero_row_is_still_written() {
        let csv = to_csv(&summary());

        assert!(csv.contains("仕入,555"), "{csv}");
    }

    // Excel が文字化けしないよう BOM を付ける。
    #[test]
    fn the_csv_starts_with_a_bom() {
        assert!(to_csv(&summary()).starts_with(UTF8_BOM));
    }
}
