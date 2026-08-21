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
use kaikei_core::Money;
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

/// 売上の税額だけで決まる特例（2割特例・3割特例）の試算を、行の並びとして組む。
///
/// # なぜ出すのか
///
/// 検証帳簿（2026年）では、一般課税 672,680円 に対して2割特例なら 177,408円。
/// **差は 495,272円。** しかも2026年分は2割特例が使える最後の年である。
/// 集計表に一般課税しか出さないと、この選択肢が見えない。
///
/// # 勧めない
///
/// どちらが有利かの判断も、適用できるかどうかの判定も、このソフトはしない
/// （`docs/09-tax-research.md`）。免税事業者がインボイス登録で課税事業者に
/// なった場合の特例で、基準期間の課税売上高が1,000万円を超えるなどの除外
/// 要件がある。**帳簿には2年前が入っていないことがある。**
///
/// # 表示から切り離してある
///
/// **金額だけが独り歩きすると危ない。** 「2割特例なら 177,408円」だけを
/// 読んで、使えるかどうかを確かめずに申告されると困る。金額と但し書きが
/// 必ず一緒に出ることをテストで固定したいので、組み立てを分けている。
pub fn special_rule_estimate_lines(year: i32, summary: &Summary) -> Vec<String> {
    use kaikei_jp::consumption_tax::{special_rule_for, tax_under, SpecialRule};

    let Some(rule) = special_rule_for(year) else {
        return Vec::new();
    };
    let general = summary.tax_on_sales().minor() - summary.tax_on_purchases().minor();
    let special = tax_under(rule, summary.tax_on_sales());
    let yen =
        |minor: i128| Money::from_minor(minor, kaikei_core::Currency::JPY).to_display_string();

    let mut lines = vec![
        String::new(),
        "納付税額の試算".to_string(),
        String::new(),
        format!("  一般課税  {} 円（売上の税額 − 仕入の税額）", yen(general)),
        format!(
            "  {}  {} 円（売上の税額の{}%）",
            rule.name(),
            special.to_display_string(),
            rule.percent()
        ),
        format!("  差        {} 円", yen(general - special.minor())),
        String::new(),
        "どちらが有利かも、使えるかどうかも、このソフトは判定しません。".to_string(),
        "  ・免税事業者がインボイス登録で課税事業者になった場合の特例です".to_string(),
        "  ・基準期間（2年前）の課税売上高が1,000万円を超えるなどの除外要件があり、".to_string(),
        "    2年前が帳簿に入っていないことがあるため判定していません".to_string(),
        "  ・事前の届出は要らず、申告書への付記だけで使えます（年ごとに選べます）".to_string(),
    ];
    if rule == SpecialRule::TwentyPercent && year == 2026 {
        lines.push(String::new());
        lines.push("2026年分（令和8年分）が2割特例の最後の年です。".to_string());
        lines.push("  2027・2028年分は3割特例、2029年分以降はどちらも使えません。".to_string());
    }
    lines
}

/// CSV に添える注意書き。**別ファイルにする。**
///
/// 1つの CSV に表と注記を混ぜると表計算で読めなくなる
/// （`blue_return_not_on_form.csv` と同じ理由）。
/// `year` は特例（2割・3割）が使える年かを決めるために要る。
pub fn notes_to_text(summary: &Summary, year: i32) -> String {
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

    // **特例の試算は、この注記と同じファイルに置く。** 画面には出るのに
    // 渡すファイルに無いと、税理士まで届かない。検証帳簿では差が 495,272円 で、
    // しかも2026年分が2割特例の最後の年である。
    lines.extend(special_rule_estimate_lines(year, summary));
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
                    amount: yen(9_757_440),
                    tax: Some(yen(887_040)),
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
        assert!(csv.contains("9757440"), "{csv}");
        assert!(csv.contains("887040"), "{csv}");
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
        assert!(to_csv(&summary()).contains("9757440"));
        assert!(!to_csv(&summary()).contains("9,757,440"));
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
        let notes = notes_to_text(&summary(), 2029);
        assert!(notes.contains("申告書の金額ではありません"), "{notes}");
        assert!(notes.contains("家事按分"), "{notes}");
        assert!(notes.contains("端数処理"), "{notes}");
    }

    /// 集計できなかった明細があれば、注意書きに件数が出る。
    #[test]
    fn the_notes_report_lines_that_were_skipped() {
        let mut s = summary();
        s.lines_without_a_category = 128;
        s.lines_with_an_unknown_category = 3;

        let notes = notes_to_text(&s, 2029);

        assert!(notes.contains("128 件"), "{notes}");
        assert!(notes.contains("3 件"), "{notes}");
    }

    /// 集計できなかった明細が無ければ、その段は出さない。
    #[test]
    fn the_notes_stay_short_when_nothing_was_skipped() {
        let notes = notes_to_text(&summary(), 2029);
        assert!(!notes.contains("税区分が付いていない明細"), "{notes}");
    }
    // ─── 消費税の特例の試算 ─────────────────────────

    fn summary_with(sales_tax: i128, purchase_tax: i128) -> Summary {
        use kaikei_jp::consumption_tax::CategoryTotal;
        use kaikei_jp::tax::TaxDirection;
        let yen = |v: i128| Money::from_minor(v, kaikei_core::Currency::JPY);
        Summary {
            categories: vec![
                CategoryTotal {
                    code: "SALE_10".to_string(),
                    label: "課税売上 10%".to_string(),
                    direction: TaxDirection::Sales,
                    amount: yen(sales_tax * 11),
                    tax: Some(yen(sales_tax)),
                },
                CategoryTotal {
                    code: "PURCHASE_10_QUALIFIED".to_string(),
                    label: "課税仕入 10%（適格）".to_string(),
                    direction: TaxDirection::Purchase,
                    amount: yen(purchase_tax * 11),
                    tax: Some(yen(purchase_tax)),
                },
            ],
            lines_without_a_category: 0,
            lines_with_an_unknown_category: 0,
        }
    }

    // **本命。** 金額を出すなら、判定していないことも必ず出す。
    //
    // 「2割特例なら 177,408円」だけが独り歩きして、使えるかどうかを
    // 確かめずに申告されると困る。**数字と但し書きは切り離せない。**
    #[test]
    fn the_estimate_never_appears_without_the_caveat() {
        let lines = special_rule_estimate_lines(2026, &summary_with(887_040, 214_360));

        let text = lines.join(
            "
",
        );
        assert!(text.contains("177,408"), "試算を出すこと: {text}");
        assert!(
            text.contains("このソフトは判定しません"),
            "判定していないことを言うこと: {text}"
        );
        assert!(text.contains("1,000万円"), "除外要件に触れること: {text}");
    }

    // **本命。** 一般課税との差を出す。検証帳簿の値で確かめる。
    #[test]
    fn the_estimate_shows_the_difference() {
        let text = special_rule_estimate_lines(2026, &summary_with(887_040, 214_360)).join(
            "
",
        );

        assert!(text.contains("672,680"), "一般課税: {text}");
        assert!(text.contains("495,272"), "差: {text}");
    }

    // **本命。** 2026年分が最後であることを言う。
    //
    // 見逃すと 495,272円 の選択肢を1年分まるごと失う。
    #[test]
    fn the_last_year_of_the_twenty_percent_rule_is_called_out() {
        let text = special_rule_estimate_lines(2026, &summary_with(1_000, 0)).join(
            "
",
        );

        assert!(text.contains("最後の年"), "{text}");
    }

    // 2027年分は3割特例なので、「最後の年」は言わない。
    #[test]
    fn the_last_year_notice_is_only_for_2026() {
        let text = special_rule_estimate_lines(2027, &summary_with(1_000, 0)).join(
            "
",
        );

        assert!(text.contains("3割特例"), "{text}");
        assert!(
            !text.contains("最後の年"),
            "2027年に出してはいけない: {text}"
        );
    }

    // **本命。** 特例が無い年は何も出さない。
    #[test]
    fn no_special_rule_means_no_output() {
        assert!(special_rule_estimate_lines(2029, &summary_with(1_000, 0)).is_empty());
    }
    // **本命。** 試算は注記ファイルにも載る。
    //
    // 画面には出るのに渡すファイルに無いと、税理士まで届かない。検証帳簿では
    // 差が 495,272円 で、しかも2026年分が2割特例の最後の年である。
    // **届かなければ選択肢が無かったのと同じ。**
    #[test]
    fn the_notes_carry_the_special_rule_estimate() {
        let notes = notes_to_text(&summary_with(887_040, 214_360), 2026);

        assert!(notes.contains("177,408"), "試算を載せること: {notes}");
        assert!(notes.contains("495,272"), "差を載せること: {notes}");
        assert!(
            notes.contains("このソフトは判定しません"),
            "但し書きも一緒に載せること: {notes}"
        );
        assert!(notes.contains("最後の年"), "{notes}");
    }

    // 特例の無い年には載せない。
    #[test]
    fn the_notes_say_nothing_when_there_is_no_special_rule() {
        let notes = notes_to_text(&summary_with(1_000, 0), 2029);

        assert!(!notes.contains("納付税額の試算"), "{notes}");
    }
}
