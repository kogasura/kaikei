//! 青色申告決算書の貸借対照表のデータ出力。
//!
//! 金額を埋めるのは `kaikei-jp`（`blue_return_bs`）の仕事で、この module は
//! **受け取った行を表に開くだけ**である（`blue_return` と同じ分担）。
//!
//! # 期首欄が空の行を「0」にしない
//!
//! 事業主貸・事業主借・青色申告特別控除前の所得金額は、様式で期首欄に斜線が
//! 引かれている。**0 円と書くのと斜線は違う**ので、空欄であることが分かる
//! 表示にする（`—`）。0 を出すと、期首残高があったのに 0 だったと読まれる。
//!
//! # 貸借が合わないことを隠さない
//!
//! 資産合計と負債・資本合計が一致しないときは、差額を注記として必ず出す。
//! 様式の書き方が「一致しない場合には、記帳誤りや計算誤りがあると思われます」
//! と明記しており、**気づかずに提出されるのが最も困る**。

use crate::csv::CsvBuilder;
use crate::html::PrintableTable;
use kaikei_app::amount::money_to_plain_string;
use kaikei_core::Money;

/// 様式の行1つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BsRow {
    /// 行名。空欄の行で科目が当てはまっていなければ `None`。
    pub label: Option<String>,
    /// 期首の金額。様式で斜線の行は `None`。
    pub opening: Option<Money>,
    /// 期末の金額。
    pub closing: Money,
}

/// 様式の区分。
#[derive(Debug, Clone)]
pub struct BsSection {
    /// 区分名（資産の部 / 負債・資本の部）。
    pub title: String,
    /// 行（様式の印字順）。
    pub rows: Vec<BsRow>,
}

/// 見出し。
const HEADERS: &[&str] = &["区分", "科目", "期首", "期末"];

/// 右寄せにする列。
const NUMERIC_COLUMNS: &[usize] = &[2, 3];

/// 科目が当てはまっていない空欄の表示。
const BLANK_LABEL: &str = "（空欄）";

/// 様式で期首欄に斜線が引かれている行の表示。
///
/// **空文字にしない。** 0 円との区別がつかなくなる。
const STRUCK_THROUGH: &str = "—";

fn to_rows(sections: &[BsSection]) -> Vec<Vec<String>> {
    sections
        .iter()
        .flat_map(|section| {
            section.rows.iter().map(move |row| {
                vec![
                    section.title.clone(),
                    row.label.clone().unwrap_or_else(|| BLANK_LABEL.to_string()),
                    match &row.opening {
                        Some(amount) => money_to_plain_string(amount),
                        None => STRUCK_THROUGH.to_string(),
                    },
                    money_to_plain_string(&row.closing),
                ]
            })
        })
        .collect()
}

/// 貸借対照表の CSV。
pub fn to_csv(sections: &[BsSection]) -> String {
    let mut csv = CsvBuilder::new();
    csv.push_row(HEADERS);
    for row in to_rows(sections) {
        csv.push_row(row);
    }
    csv.finish()
}

/// 貸借対照表の印刷用 HTML。
///
/// `imbalance` は資産合計 − 負債・資本合計。**0 でなければ注記に必ず出す**。
pub fn to_html(
    title: &str,
    period: &str,
    sections: &[BsSection],
    imbalance: Option<Money>,
    notes: &[String],
) -> String {
    let mut all_notes: Vec<String> = notes.to_vec();
    match imbalance {
        Some(diff) if !diff.is_zero() => all_notes.push(format!(
            "資産合計と負債・資本合計が {} 円ずれています。\
             決算書としては貸借が一致している必要があります\
             （記帳誤り・計算誤り、または損益計算書から除いた収益の\
             相手科目が資産に残っていることが考えられます）。",
            money_to_plain_string(&diff)
        )),
        Some(_) => all_notes.push("資産合計と負債・資本合計は一致しています。".to_string()),
        // 検算できないこと自体を黙らない。
        None => all_notes.push(
            "区分が2つ揃っていないため、貸借が一致するかを確認できませんでした。".to_string(),
        ),
    }

    PrintableTable {
        title,
        subtitle: period,
        headers: HEADERS,
        rows: &to_rows(sections),
        notes: &all_notes,
        numeric_columns: NUMERIC_COLUMNS,
        footer_rows: &[],
        // 4列なので A4 縦に収まる。
        landscape: false,
    }
    .render()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_core::Currency;

    fn yen(minor: i128) -> Money {
        Money::from_minor(minor, Currency::JPY)
    }

    fn row(label: Option<&str>, opening: Option<i128>, closing: i128) -> BsRow {
        BsRow {
            label: label.map(|s| s.to_string()),
            opening: opening.map(yen),
            closing: yen(closing),
        }
    }

    fn sections() -> Vec<BsSection> {
        vec![
            BsSection {
                title: "資産の部".to_string(),
                rows: vec![
                    row(Some("現金"), Some(557_052), 552_542),
                    row(None, None.map(|x: i128| x).or(Some(0)), 0),
                    row(Some("事業主貸"), None, 10_069_266),
                    row(Some("合計"), Some(50_975), 10_037_243),
                ],
            },
            BsSection {
                title: "負債・資本の部".to_string(),
                rows: vec![
                    row(Some("未払金"), Some(1_865_236), 2_470_356),
                    row(Some("元入金"), Some(-1_814_261), -1_814_261),
                    row(Some("青色申告特別控除前の所得金額"), None, 8_368_714),
                    row(Some("合計"), Some(50_975), 10_036_809),
                ],
            },
        ]
    }

    // BS-CSV-1: 区分・科目・期首・期末がそのまま出る。
    #[test]
    fn the_csv_carries_both_columns() {
        let csv = to_csv(&sections());

        assert!(csv.contains("資産の部,現金,557052,552542"), "{csv}");
        assert!(
            csv.contains("負債・資本の部,未払金,1865236,2470356"),
            "{csv}"
        );
    }

    // BS-CSV-2: **本命。** 期首欄が斜線の行を 0 と書かない。
    //
    //           0 円と斜線は違う。0 を出すと「期首残高があって 0 だった」と
    //           読まれる。
    #[test]
    fn a_struck_through_opening_column_is_not_written_as_zero() {
        let csv = to_csv(&sections());

        assert!(
            csv.contains("資産の部,事業主貸,—,10069266"),
            "期首欄が斜線の行は 0 ではなく空欄と分かる表示にすること: {csv}"
        );
        assert!(
            !csv.contains("事業主貸,0,"),
            "0 と書くと期首残高があったと読まれる: {csv}"
        );
    }

    // BS-CSV-3: 科目が当てはまっていない空欄も行として出す。
    #[test]
    fn a_blank_row_is_still_a_row() {
        let csv = to_csv(&sections());
        assert!(csv.contains("資産の部,（空欄）,0,0"), "{csv}");
    }

    // BS-HTML-1: **本命。** 貸借が合わなければ差額を注記に出す。
    #[test]
    fn an_imbalance_is_always_reported() {
        let html = to_html("貸借対照表", "", &sections(), Some(yen(434)), &[]);

        assert!(html.contains("434"), "差額を出すこと: {html}");
        assert!(html.contains("ずれています"), "{html}");
    }

    // BS-HTML-2: 合っているときもその旨を出す（確認したことが分かる）。
    #[test]
    fn a_balanced_sheet_says_so() {
        let html = to_html("貸借対照表", "", &sections(), Some(yen(0)), &[]);
        assert!(html.contains("一致しています"), "{html}");
    }

    // BS-HTML-3: 検算できなかったこと自体も黙らない。
    #[test]
    fn being_unable_to_check_is_reported_too() {
        let html = to_html("貸借対照表", "", &sections(), None, &[]);
        assert!(html.contains("確認できませんでした"), "{html}");
    }

    // BS-HTML-4: CSV と HTML が同じ値を運ぶ。
    #[test]
    fn the_csv_and_the_html_carry_the_same_values() {
        let csv = to_csv(&sections());
        let html = to_html("貸借対照表", "", &sections(), Some(yen(0)), &[]);

        for value in ["557052", "10069266", "8368714", "-1814261"] {
            assert!(csv.contains(value), "CSV に値が無い: {value}");
            assert!(html.contains(value), "HTML に値が無い: {value}");
        }
    }
}
