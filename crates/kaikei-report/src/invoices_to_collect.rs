//! 適格請求書を揃えるべき取引の一覧。
//!
//! # なぜ件数だけでは足りないのか
//!
//! `verify` は「取引先が記録されていない課税仕入れが 603 件」と数え、
//! 少額特例（税込1万円未満）で分けて「1万円以上は 33 件」まで出す（D-114）。
//! **そこから先が進まない。** どの取引なのかが分からないと、請求書を探しに
//! 行けないからである。
//!
//! この一覧は、**そのまま作業リストとして使える形**で書き出す。日付・金額・
//! 摘要・科目があれば、通帳やメールから元の取引を辿れる。
//!
//! # 1万円未満は載せない
//!
//! 570 件を並べても作業リストにならない。少額特例が使えるなら適格請求書の
//! 保存は要らず、使えないとしても**まず1万円以上から**である。
//! 件数は末尾の注記で伝える（黙って落とさない）。
//!
//! # 「揃えるべき」と断定はしない
//!
//! 少額特例が使えるかは事業者の規模で決まり、このソフトは判定しない
//! （D-114）。**この一覧は「確かめる価値がある取引」である。**

use crate::csv::{CsvBuilder, UTF8_BOM};

/// 一覧の1行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvoiceToCollect {
    /// 取引日（ISO 8601）。
    pub date: String,
    /// 仕訳番号。帳簿を引き直すときに使う。
    pub entry_no: i64,
    /// 取引額（税込・円）。
    pub amount_minor: i128,
    /// 摘要。**これが手がかりになる**——通帳やメールを探す起点である。
    pub description: String,
    /// 借方の科目コード。
    pub account: String,
    /// 科目名。
    pub account_name: String,
}

/// 見出し。**列を増やすときは末尾に足す**（表計算で開いたまま差し替える人が
/// いるので、既存の列位置を動かさない）。
const HEADER: [&str; 6] = ["取引日", "仕訳番号", "金額", "摘要", "科目", "科目名"];

/// CSV にする。0 件でも見出しだけのファイルを書く
/// （`blue_return_not_on_form.csv` と同じ。ファイルが無いのと0件は違う）。
pub fn to_csv(rows: &[InvoiceToCollect]) -> String {
    let mut csv = CsvBuilder::new();
    csv.push_row(HEADER);
    for row in rows {
        csv.push_row([
            row.date.clone(),
            row.entry_no.to_string(),
            row.amount_minor.to_string(),
            row.description.clone(),
            row.account.clone(),
            row.account_name.clone(),
        ]);
    }
    format!("{UTF8_BOM}{}", csv.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(date: &str, no: i64, amount: i128, description: &str) -> InvoiceToCollect {
        InvoiceToCollect {
            date: date.to_string(),
            entry_no: no,
            amount_minor: amount,
            description: description.to_string(),
            account: "615".to_string(),
            account_name: "地代家賃".to_string(),
        }
    }

    /// **本命。** 探すための手がかりが全部入っている。
    ///
    /// 日付と金額だけでは通帳から引けない（同じ日に同額の取引がある）。
    /// 摘要と仕訳番号が要る。
    #[test]
    fn a_row_carries_everything_needed_to_find_the_invoice() {
        let csv = to_csv(&[row("2026-01-23", 42, 410_000, "地代家賃")]);

        assert!(csv.contains("2026-01-23"), "{csv}");
        assert!(csv.contains("42"), "仕訳番号: {csv}");
        assert!(csv.contains("410000"), "{csv}");
        assert!(csv.contains("地代家賃"), "{csv}");
        assert!(csv.contains("615"), "{csv}");
    }

    /// 金額に桁区切りを入れない（他の CSV と同じ）。
    #[test]
    fn the_amount_has_no_thousands_separator() {
        let csv = to_csv(&[row("2026-01-23", 1, 1_640_720, "x")]);
        assert!(csv.contains("1640720"), "{csv}");
        assert!(!csv.contains("1,640,720"), "{csv}");
    }

    /// **本命。** 0 件でも見出しだけのファイルを書く。
    ///
    /// ファイルが無いのと「揃えるべき取引が無い」のは違う。前者は
    /// 「出し忘れたのでは」と疑う余地が残る。
    #[test]
    fn an_empty_list_still_has_a_header() {
        let csv = to_csv(&[]);
        assert!(csv.contains("取引日"), "{csv}");
        assert!(csv.contains("摘要"), "{csv}");
        assert_eq!(csv.lines().count(), 1, "見出しだけ: {csv}");
    }

    /// Excel が文字化けしないよう BOM を付ける。
    #[test]
    fn the_csv_starts_with_a_bom() {
        assert!(to_csv(&[]).starts_with(UTF8_BOM));
    }

    /// 摘要にカンマや引用符が入っても壊れない（RFC 4180）。
    #[test]
    fn a_description_with_a_comma_is_quoted() {
        let csv = to_csv(&[row("2026-01-23", 1, 1000, "A / B, C")]);
        assert!(csv.contains("\"A / B, C\""), "{csv}");
    }

    /// 渡した順で出す。**並べ替えは呼び出し側の責任**——何順が役に立つかは
    /// 使う場面で変わる（金額順で優先度をつけたい／日付順で通帳と突き合わせたい）。
    #[test]
    fn the_order_is_preserved() {
        let csv = to_csv(&[
            row("2026-03-24", 1, 515_720, "初期費用"),
            row("2026-01-23", 2, 410_000, "家賃"),
        ]);
        let first = csv.find("515720").unwrap();
        let second = csv.find("410000").unwrap();
        assert!(first < second, "渡した順を変えないこと: {csv}");
    }
}
