//! 消費税の集計を CSV にする。
//!
//! # 申告書ではない
//!
//! 集計値を並べるだけである。何をどの欄に書くかは申告上の判断であり、
//! この crate は決めない（`kaikei_jp::consumption_tax` の doc を参照）。
//!
//! # 反映していないものを CSV にも書く
//!
//! **画面で読む人と、ファイルを受け取る人は別である。** 税理士に渡すのは
//! ファイルの方なので、注意書きが画面にしか無いと届かない。

use crate::csv::{CsvBuilder, UTF8_BOM};
use kaikei_jp::consumption_tax::Summary;
use kaikei_jp::tax::TaxDirection;

/// 見出し。**列を増やすときは末尾に足す。**
const HEADER: [&str; 5] = ["区分", "税区分", "税区分名", "金額（税込）", "消費税相当額"];

fn direction_label(direction: TaxDirection) -> &'static str {
    match direction {
        TaxDirection::Sales => "売上",
        TaxDirection::Purchase => "仕入",
        TaxDirection::None => "対象外",
    }
}

/// CSV にする。0 件でも見出しだけのファイルを書く。
pub fn to_csv(summary: &Summary) -> String {
    let mut csv = CsvBuilder::new();
    csv.push_row(HEADER);
    for category in &summary.categories {
        csv.push_row([
            direction_label(category.direction).to_string(),
            category.code.clone(),
            category.label.clone(),
            category.amount.minor().to_string(),
            // **税率を持たない区分は空欄。** 0 と書くと「計算した結果0」に読める。
            category
                .tax
                .map(|tax| tax.minor().to_string())
                .unwrap_or_default(),
        ]);
    }
    csv.push_row([
        "合計".to_string(),
        "課税売上".to_string(),
        String::new(),
        summary.taxable_sales().minor().to_string(),
        summary.tax_on_sales().minor().to_string(),
    ]);
    csv.push_row([
        "合計".to_string(),
        "課税仕入".to_string(),
        String::new(),
        summary.taxable_purchases().minor().to_string(),
        summary.tax_on_purchases().minor().to_string(),
    ]);
    format!("{UTF8_BOM}{}", csv.finish())
}

/// CSV に添える注意書き。**別ファイルにする。**
///
/// 1つの CSV に表と注記を混ぜると表計算で読めなくなる
/// （`blue_return_not_on_form.csv` と同じ理由）。
pub fn notes_to_text(summary: &Summary) -> String {
    let mut lines = vec![
        "消費税の集計についての注意".to_string(),
        String::new(),
        "これは申告書の金額ではありません。次のものを反映していません。".to_string(),
        "  ・家事按分（どの科目が按分対象かは帳簿から決まりません）".to_string(),
        "  ・適格請求書発行事業者かどうかの確認（取引先の登録番号を確かめていません）".to_string(),
        "  ・経過措置の控除割合（80%／70%）".to_string(),
        "  ・端数処理の規定（申告書上の規定であり、帳簿の集計とは別です）".to_string(),
        "  ・課税売上割合による按分（非課税売上がある場合）".to_string(),
        String::new(),
        "原則課税・税込経理を前提にしています。".to_string(),
        "税額は税込金額から割り戻しています（税込 × 税率 ÷ (1 + 税率)、端数切捨て）。".to_string(),
    ];
    if summary.lines_without_a_category > 0 {
        lines.push(String::new());
        lines.push(format!(
            "税区分が付いていない明細が {} 件あります。",
            summary.lines_without_a_category
        ));
        lines.push("  口座や事業主貸のように税区分を持たない明細も含まれます。".to_string());
        lines.push(
            "  課税取引なのに付いていないものがあれば、その分は集計から抜けています。".to_string(),
        );
    }
    if summary.lines_with_an_unknown_category > 0 {
        lines.push(String::new());
        lines.push(format!(
            "同梱の税区分マスタに無いコードの明細が {} 件あります。この分は集計に入っていません。",
            summary.lines_with_an_unknown_category
        ));
    }
    lines.join("\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_core::{Currency, Money};
    use kaikei_jp::consumption_tax::CategoryTotal;

    fn yen(v: i128) -> Money {
        Money::from_minor(v, Currency::JPY)
    }

    fn summary() -> Summary {
        Summary {
            categories: vec![
                CategoryTotal {
                    code: "SALES_10".to_string(),
                    label: "課税売上 10%".to_string(),
                    direction: TaxDirection::Sales,
                    amount: yen(12_070_080),
                    tax: Some(yen(1_097_280)),
                },
                CategoryTotal {
                    code: "OUT_OF_SCOPE".to_string(),
                    label: "不課税".to_string(),
                    direction: TaxDirection::None,
                    amount: yen(13_000),
                    tax: None,
                },
            ],
            lines_without_a_category: 0,
            lines_with_an_unknown_category: 0,
        }
    }

    /// **本命。** 区分ごとの行と合計行が出る。
    #[test]
    fn the_csv_has_rows_and_totals() {
        let csv = to_csv(&summary());
        assert!(csv.contains("SALES_10"), "{csv}");
        assert!(csv.contains("12070080"), "{csv}");
        assert!(csv.contains("1097280"), "{csv}");
        assert!(csv.contains("課税売上"), "合計行: {csv}");
        assert!(csv.contains("課税仕入"), "合計行: {csv}");
    }

    /// **本命。** 税率を持たない区分の税額は空欄。
    ///
    /// **0 と書くと「計算した結果0」に読める。**
    #[test]
    fn a_category_without_a_rate_leaves_the_tax_cell_empty() {
        let csv = to_csv(&summary());
        let row = csv
            .lines()
            .find(|l| l.contains("OUT_OF_SCOPE"))
            .expect("不課税の行");
        assert!(row.ends_with(','), "税額の欄が空であること: {row}");
    }

    #[test]
    fn the_amount_has_no_thousands_separator() {
        assert!(to_csv(&summary()).contains("12070080"));
        assert!(!to_csv(&summary()).contains("12,070,080"));
    }

    #[test]
    fn the_csv_starts_with_a_bom() {
        assert!(to_csv(&summary()).starts_with(UTF8_BOM));
    }

    /// **本命。** 注意書きに「申告書の金額ではない」と書く。
    ///
    /// **画面で読む人と、ファイルを受け取る人は別である。** 税理士に渡すのは
    /// ファイルの方なので、注記が画面にしか無いと届かない。
    #[test]
    fn the_notes_say_it_is_not_a_tax_return() {
        let notes = notes_to_text(&summary());
        assert!(notes.contains("申告書の金額ではありません"), "{notes}");
        assert!(notes.contains("家事按分"), "{notes}");
        assert!(notes.contains("端数処理"), "{notes}");
    }

    /// 集計できなかった明細があれば、注意書きに件数が出る。
    #[test]
    fn the_notes_report_lines_that_were_skipped() {
        let mut s = summary();
        s.lines_without_a_category = 774;
        s.lines_with_an_unknown_category = 3;

        let notes = notes_to_text(&s);

        assert!(notes.contains("774 件"), "{notes}");
        assert!(notes.contains("3 件"), "{notes}");
    }

    /// 集計できなかった明細が無ければ、その段は出さない。
    #[test]
    fn the_notes_stay_short_when_nothing_was_skipped() {
        let notes = notes_to_text(&summary());
        assert!(!notes.contains("税区分が付いていない明細"), "{notes}");
    }
}
