//! 財務諸表（貸借対照表・損益計算書）の出力。
//!
//! 入力は [`kaikei_app::policy::Statement`]（`StatementPolicy` が組み立てた
//! もの）。**様式はここで決めない**——どの科目をどの区分に入れるかは
//! `kaikei-jp` の `JpStatementPolicy` の責務で、この層は受け取った構造を
//! そのまま表に開く。区分の順も並べ替えない。
//!
//! 各行の表示名は `StatementLine::label` にある（`StatementPolicy` が科目表
//! から埋めている）ので、**勘定科目表を引き直さない**。仕訳日記帳や試算表が
//! `ChartOfAccounts` を要求するのと違うのは、入力の段階で名前が付いている
//! ためである。
//!
//! # 区分はフラットな列にする
//!
//! 区分（`StatementSection`）ごとに表を入れ子にせず、**区分名を1列目に持つ
//! 平らな表**にする。CSV に落としたときに表計算でそのまま扱え（区分でフィルタ
//! できる）、HTML でも同じ構造になるためである。区分の小計は行として本文に
//! 入れ、全体の合計は表の最後（`<tfoot>`）に置く。
//!
//! # 青色申告決算書そのものではない
//!
//! これは**帳簿としての財務諸表**であって、国税庁の青色申告決算書の様式では
//! ない。決算書の各欄への当てはめは別の設計が要る（`docs/10-report.md` §5。
//! どの科目をどの欄に入れるかは税務判断を含む）。

use crate::csv::CsvBuilder;
use crate::html::PrintableTable;
use kaikei_app::amount::money_to_plain_string;
use kaikei_app::policy::Statement;

/// 表の見出し。CSV と HTML で共有する。
const HEADERS: &[&str] = &["区分", "科目コード", "勘定科目", "金額"];

/// 右寄せにする列（金額）。`HEADERS` の添字。
const NUMERIC_COLUMNS: &[usize] = &[3];

/// 財務諸表を表に開く。
///
/// 区分ごとに明細を並べ、その区分の最後に小計行を置く。**小計を出すのは、
/// 決算書を読む人が最初に見るのが区分ごとの金額だから**である
/// （売上高がいくら、経費がいくら）。
fn to_rows(statement: &Statement) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    for section in &statement.sections {
        for line in &section.lines {
            rows.push(vec![
                section.title.clone(),
                line.account.as_str().to_string(),
                line.label.clone(),
                money_to_plain_string(&line.amount),
            ]);
        }
        // 明細が1件も無い区分でも小計行は出す。**区分ごと消えると、
        // 「その区分が無い」のか「金額が0」のかが読めない。**
        rows.push(vec![
            section.title.clone(),
            String::new(),
            "小計".to_string(),
            money_to_plain_string(&section.subtotal),
        ]);
    }
    rows
}

/// 合計行。
fn footer_rows(statement: &Statement) -> Vec<Vec<String>> {
    vec![vec![
        String::new(),
        String::new(),
        "合計".to_string(),
        money_to_plain_string(&statement.total),
    ]]
}

/// 財務諸表の CSV。
pub fn to_csv(statement: &Statement) -> String {
    let mut csv = CsvBuilder::new();
    csv.push_row(HEADERS);
    for row in to_rows(statement) {
        csv.push_row(row);
    }
    for row in footer_rows(statement) {
        csv.push_row(row);
    }
    csv.finish()
}

/// 財務諸表の印刷用 HTML。
///
/// 表題は `Statement::title`（「貸借対照表」「損益計算書」）をそのまま使う。
/// **この層で表題を決めない**——様式を決めた `StatementPolicy` が名乗った
/// ものが正である。
pub fn to_html(statement: &Statement, period: &str, notes: &[String]) -> String {
    PrintableTable {
        title: &statement.title,
        subtitle: period,
        headers: HEADERS,
        rows: &to_rows(statement),
        notes,
        numeric_columns: NUMERIC_COLUMNS,
        footer_rows: &footer_rows(statement),
        // 4列なので A4 縦に収まる。
        landscape: false,
    }
    .render()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_app::policy::{StatementLine, StatementSection};
    use kaikei_core::{AccountCode, Currency, Money};

    fn yen(amount: i128) -> Money {
        Money::from_minor(amount, Currency::JPY)
    }

    fn line(code: &str, label: &str, amount: i128) -> StatementLine {
        StatementLine {
            account: AccountCode::parse(code).unwrap(),
            label: label.to_string(),
            amount: yen(amount),
        }
    }

    fn income_statement() -> Statement {
        Statement {
            title: "損益計算書".to_string(),
            sections: vec![
                StatementSection {
                    title: "収益".to_string(),
                    lines: vec![line("500", "売上高", 1_428_000)],
                    subtotal: yen(1_428_000),
                },
                StatementSection {
                    title: "費用".to_string(),
                    lines: vec![
                        line("604", "通信費", 108_420),
                        line("609", "旅費交通費", 19_720),
                    ],
                    subtotal: yen(128_140),
                },
            ],
            total: yen(1_299_860),
        }
    }

    #[test]
    fn every_line_carries_its_section() {
        let csv = to_csv(&income_statement());

        assert!(csv.contains("収益,500,売上高,1428000"), "{csv}");
        assert!(csv.contains("費用,604,通信費,108420"), "{csv}");
        assert!(csv.contains("費用,609,旅費交通費,19720"), "{csv}");
    }

    // 区分ごとの小計が本文に、全体の合計が表の最後に出る。
    #[test]
    fn subtotals_are_in_the_body_and_the_total_is_in_the_footer() {
        let csv = to_csv(&income_statement());
        let html = to_html(&income_statement(), "2026-01-01 〜 2026-12-31", &[]);

        assert!(csv.contains("収益,,小計,1428000"), "{csv}");
        assert!(csv.contains("費用,,小計,128140"), "{csv}");
        assert!(csv.contains(",,合計,1299860"), "{csv}");

        // HTML では合計だけが tfoot に入る（小計は本文）。
        let body_end = html.find("</tbody>").unwrap();
        let subtotal_pos = html.find("小計").unwrap();
        let total_pos = html.find("合計").unwrap();
        assert!(subtotal_pos < body_end, "小計は本文に置くこと");
        assert!(total_pos > body_end, "合計は tfoot に置くこと");
    }

    // 明細の無い区分でも小計行は消さない。
    #[test]
    fn an_empty_section_still_shows_its_subtotal() {
        let statement = Statement {
            title: "貸借対照表".to_string(),
            sections: vec![StatementSection {
                title: "純資産".to_string(),
                lines: Vec::new(),
                subtotal: yen(0),
            }],
            total: yen(0),
        };

        let csv = to_csv(&statement);

        assert!(
            csv.contains("純資産,,小計,0"),
            "区分ごと消えると「区分が無い」のか「0」なのか読めない: {csv}"
        );
    }

    // 表題は StatementPolicy が名乗ったものを使う（この層で決めない）。
    #[test]
    fn the_title_comes_from_the_statement() {
        let html = to_html(&income_statement(), "", &[]);
        assert!(html.contains("<title>損益計算書</title>"));
        assert!(html.contains("<h1>損益計算書</h1>"));
    }

    // CSV と HTML が同じ値を運ぶ。
    #[test]
    fn the_csv_and_the_html_carry_the_same_values() {
        let csv = to_csv(&income_statement());
        let html = to_html(&income_statement(), "2026-01-01 〜 2026-12-31", &[]);

        for header in HEADERS {
            assert!(csv.contains(header), "CSV に見出しが無い: {header}");
            assert!(html.contains(header), "HTML に見出しが無い: {header}");
        }
        for value in ["売上高", "1428000", "旅費交通費", "1299860"] {
            assert!(csv.contains(value), "CSV に値が無い: {value}");
            assert!(html.contains(value), "HTML に値が無い: {value}");
        }
    }

    // 区分の順は入力のまま（並べ替えない）。
    #[test]
    fn sections_keep_the_order_they_came_in() {
        let csv = to_csv(&income_statement());
        let revenue = csv.find("収益").unwrap();
        let expense = csv.find("費用").unwrap();
        assert!(revenue < expense, "様式が決めた順を変えないこと");
    }
}
