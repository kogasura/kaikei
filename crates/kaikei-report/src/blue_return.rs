//! 青色申告決算書のデータ出力。
//!
//! 金額を埋めるのは `kaikei-jp`（`blue_return_fill`）の仕事で、この module は
//! **受け取った欄を表に開くだけ**である。この層が税制を知らないようにして
//! おくと、`kaikei-report` が特定の国の様式に縛られない。呼び出し側
//! （CLI・MCP）が `kaikei-jp` の結果をここの [`FormRow`] に写す。
//!
//! # 様式そのものは出さない
//!
//! 出すのは欄番号・欄名・金額の**データ**であって、国税庁の様式を模した帳票
//! ではない（`docs/10-report.md` §5、`CLAUDE.md` §10）。様式を模した帳票を
//! 出すのは、様式の正確さに責任を持つことになる。
//!
//! # 決算書に載らない科目は別の表にする
//!
//! 決算書に載らなかった科目（[`NotOnFormRow`]）を本表に混ぜない。
//! **CSV に2つの表を入れると表計算で読めなくなる**ためである。CSV は
//! [`not_on_form_to_csv`] で別に出し、HTML は同じページの下に続けて出す
//! （印刷したときに1枚で揃っている方が読み落としが少ない）。
//!
//! 載らない科目が1件も無いときも、その旨を出す。**表ごと消すと「載らない
//! 科目が無かった」のか「確認していない」のかが読めない。**

use crate::csv::CsvBuilder;
use crate::html::PrintableTable;
use kaikei_app::amount::money_to_plain_string;
use kaikei_core::Money;

/// 決算書の欄1つ（`kaikei-jp` の `FilledField` を写したもの）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormRow {
    /// 様式の丸番号。
    pub no: u32,
    /// 欄名。空欄の行で当てはめる科目が無ければ `None`。
    pub label: Option<String>,
    /// 金額。
    pub amount: Money,
}

/// 決算書に載らなかった科目1件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotOnFormRow {
    /// 科目コード。
    pub account: String,
    /// 科目名。
    pub label: String,
    /// 金額。
    pub amount: Money,
    /// 載らなかった理由（利用者に見せる文章）。
    pub reason: String,
}

/// 本表の見出し。
const HEADERS: &[&str] = &["欄", "科目", "金額"];

/// 右寄せにする列（金額）。
const NUMERIC_COLUMNS: &[usize] = &[2];

/// 載らなかった科目の表の見出し。
const NOT_ON_FORM_HEADERS: &[&str] = &["科目コード", "勘定科目", "金額", "決算書に載せない理由"];

/// 空欄の行で科目が当てはまっていないときの表示。
///
/// 空文字にすると、CSV で「欄が無い」のか「欄名が空」のかが読めない。
const BLANK_LABEL: &str = "（空欄）";

fn to_rows(fields: &[FormRow]) -> Vec<Vec<String>> {
    fields
        .iter()
        .map(|field| {
            vec![
                field.no.to_string(),
                field
                    .label
                    .clone()
                    .unwrap_or_else(|| BLANK_LABEL.to_string()),
                money_to_plain_string(&field.amount),
            ]
        })
        .collect()
}

/// 科目コード・科目名・金額の列を折り返さないための指定。
///
/// 理由の文章が長いので、指定しないと**幅を理由に取られて「金額」の見出しが
/// 2行に折れる**（実際の出力で確認した）。理由の列だけは折り返してよい。
fn nowrap_style(column: usize) -> &'static str {
    if column < 3 {
        " style=\"white-space:nowrap\""
    } else {
        ""
    }
}

fn not_on_form_rows(entries: &[NotOnFormRow]) -> Vec<Vec<String>> {
    entries
        .iter()
        .map(|entry| {
            vec![
                entry.account.clone(),
                entry.label.clone(),
                money_to_plain_string(&entry.amount),
                entry.reason.clone(),
            ]
        })
        .collect()
}

/// 決算書の本表を CSV にする。
pub fn to_csv(fields: &[FormRow]) -> String {
    let mut csv = CsvBuilder::new();
    csv.push_row(HEADERS);
    for row in to_rows(fields) {
        csv.push_row(row);
    }
    csv.finish()
}

/// 決算書に載らなかった科目を CSV にする。
///
/// **本表とは別のファイルにする。** 1つの CSV に2つの表を入れると表計算で
/// 読めなくなる。0 件でも見出しだけの CSV を返す（ファイルごと消すと
/// 「載らない科目が無かった」のか「出し忘れた」のかが読めない）。
pub fn not_on_form_to_csv(entries: &[NotOnFormRow]) -> String {
    let mut csv = CsvBuilder::new();
    csv.push_row(NOT_ON_FORM_HEADERS);
    for row in not_on_form_rows(entries) {
        csv.push_row(row);
    }
    csv.finish()
}

/// 決算書の印刷用 HTML。
///
/// 載らなかった科目は同じページの下に続けて出す。印刷したときに1枚で
/// 揃っている方が読み落としが少ない。
pub fn to_html(
    title: &str,
    period: &str,
    fields: &[FormRow],
    entries: &[NotOnFormRow],
    notes: &[String],
) -> String {
    let mut html = PrintableTable {
        title,
        subtitle: period,
        headers: HEADERS,
        rows: &to_rows(fields),
        notes,
        numeric_columns: NUMERIC_COLUMNS,
        footer_rows: &[],
        // 3列なので A4 縦に収まる。
        landscape: false,
    }
    .render();

    // 載らなかった科目の表を同じページに続ける。**0 件でも見出しを出す**
    // ——表ごと消すと、確認したのかどうかが読めない。
    let body = if entries.is_empty() {
        "<p>決算書のどの欄にも載らなかった科目はありません。</p>".to_string()
    } else {
        let rows = not_on_form_rows(entries)
            .iter()
            .map(|row| {
                let cells = row
                    .iter()
                    .enumerate()
                    .map(|(index, cell)| {
                        let class = if index == 2 { " class=\"num\"" } else { "" };
                        format!(
                            "<td{class}{}>{}</td>",
                            nowrap_style(index),
                            crate::html::escape(cell)
                        )
                    })
                    .collect::<String>();
                format!("<tr>{cells}</tr>")
            })
            .collect::<String>();
        let head = NOT_ON_FORM_HEADERS
            .iter()
            .enumerate()
            .map(|(index, header)| {
                format!(
                    "<th{}>{}</th>",
                    nowrap_style(index),
                    crate::html::escape(header)
                )
            })
            .collect::<String>();
        format!("<table><thead><tr>{head}</tr></thead><tbody>{rows}</tbody></table>")
    };

    let section =
        format!("<section class=\"not-on-form\"><h2>決算書に載らなかった科目</h2>{body}</section>");

    // `PrintableTable` が閉じた body の直前に差し込む。
    match html.rfind("</body>") {
        Some(index) => {
            html.insert_str(index, &section);
            html
        }
        // `render()` の出力に `</body>` が無いことは無いが、あっても
        // **黙って落とさない**（載らなかった科目こそ消してはいけない）。
        None => {
            html.push_str(&section);
            html
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_core::Currency;

    fn yen(minor: i128) -> Money {
        Money::from_minor(minor, Currency::JPY)
    }

    fn field(no: u32, label: Option<&str>, minor: i128) -> FormRow {
        FormRow {
            no,
            label: label.map(|s| s.to_string()),
            amount: yen(minor),
        }
    }

    fn sample_fields() -> Vec<FormRow> {
        vec![
            field(1, Some("売上（収入）金額（雑収入を含む）"), 11_435_380),
            field(25, Some("支払手数料"), 7_693),
            field(29, None, 0),
            field(45, Some("所得金額"), 7_718_714),
        ]
    }

    fn sample_not_on_form() -> Vec<NotOnFormRow> {
        vec![NotOnFormRow {
            account: "530".to_string(),
            label: "受取利息".to_string(),
            amount: yen(434),
            reason: "預貯金の利子は利子所得であり事業所得ではない".to_string(),
        }]
    }

    // BR-CSV-1: 欄番号・欄名・金額がそのまま出る。
    #[test]
    fn the_csv_carries_the_field_number_label_and_amount() {
        let csv = to_csv(&sample_fields());

        assert!(
            csv.contains("1,売上（収入）金額（雑収入を含む）,11435380"),
            "{csv}"
        );
        assert!(csv.contains("25,支払手数料,7693"), "{csv}");
        assert!(csv.contains("45,所得金額,7718714"), "{csv}");
    }

    // BR-CSV-2: 科目が当てはまっていない空欄も行として出す。
    //
    //           行ごと消すと、様式の欄と出力の対応が読めなくなる。
    #[test]
    fn a_blank_row_without_an_account_is_still_a_row() {
        let csv = to_csv(&sample_fields());

        assert!(
            csv.contains("29,（空欄）,0"),
            "空欄も行として出し、欄名が空でないと分かる形にすること: {csv}"
        );
    }

    // BR-CSV-3: **本命。** 載らなかった科目は本表に混ざらず、別の CSV に出る。
    #[test]
    fn accounts_not_on_the_form_go_to_a_separate_csv() {
        let main = to_csv(&sample_fields());
        let aside = not_on_form_to_csv(&sample_not_on_form());

        assert!(
            !main.contains("受取利息"),
            "本表に混ぜると表計算で読めなくなる: {main}"
        );
        assert!(aside.contains("530,受取利息,434,"), "{aside}");
        assert!(aside.contains("利子所得"), "理由も運ぶこと: {aside}");
    }

    // BR-CSV-4: 載らなかった科目が0件でも、見出しだけの CSV を返す。
    //
    //           ファイルごと消すと「無かった」のか「出し忘れた」のかが
    //           読めない。
    #[test]
    fn the_not_on_form_csv_exists_even_when_there_is_nothing_to_report() {
        let csv = not_on_form_to_csv(&[]);

        for header in NOT_ON_FORM_HEADERS {
            assert!(csv.contains(header), "見出しは出すこと: {csv}");
        }
    }

    // BR-HTML-1: HTML は本表と「載らなかった科目」を1ページに載せる。
    #[test]
    fn the_html_puts_both_tables_on_one_page() {
        let html = to_html(
            "青色申告決算書（損益計算書）",
            "2026-01-01 〜 2026-12-31",
            &sample_fields(),
            &sample_not_on_form(),
            &[],
        );

        assert!(
            html.contains("<h1>青色申告決算書（損益計算書）</h1>"),
            "{html}"
        );
        assert!(html.contains("所得金額"), "{html}");
        assert!(html.contains("決算書に載らなかった科目"), "{html}");
        assert!(html.contains("受取利息"), "{html}");
        assert!(html.contains("利子所得"), "理由も出すこと");

        // 差し込みで body を壊していないこと。
        assert!(html.contains("</body>"), "{html}");
        let section = html.find("not-on-form").unwrap();
        let body_end = html.rfind("</body>").unwrap();
        assert!(section < body_end, "セクションは body の中に置くこと");
    }

    // BR-HTML-2: 載らなかった科目が0件でも、その旨を出す。
    #[test]
    fn the_html_says_so_when_nothing_is_left_off_the_form() {
        let html = to_html("決算書", "", &sample_fields(), &[], &[]);

        assert!(
            html.contains("載らなかった科目はありません"),
            "表ごと消すと、確認したのかどうかが読めない: {html}"
        );
    }

    // BR-HTML-3: 理由に HTML の特殊文字があっても壊れない。
    #[test]
    fn special_characters_in_a_reason_are_escaped() {
        let entries = vec![NotOnFormRow {
            account: "900".to_string(),
            label: "テスト<script>".to_string(),
            amount: yen(1),
            reason: "a & b <c>".to_string(),
        }];

        let html = to_html("決算書", "", &[], &entries, &[]);

        assert!(!html.contains("<script>"), "エスケープすること: {html}");
        assert!(html.contains("&amp;"), "{html}");
    }

    // BR-4: CSV と HTML が同じ金額を運ぶ。
    #[test]
    fn the_csv_and_the_html_carry_the_same_amounts() {
        let csv = to_csv(&sample_fields());
        let html = to_html("決算書", "", &sample_fields(), &[], &[]);

        for value in ["11435380", "7693", "7718714"] {
            assert!(csv.contains(value), "CSV に値が無い: {value}");
            assert!(html.contains(value), "HTML に値が無い: {value}");
        }
    }
}
