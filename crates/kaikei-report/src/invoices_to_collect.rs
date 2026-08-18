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

/// 相手先の手がかりを摘要から取り出す。
///
/// # なぜ「手がかり」なのか
///
/// **帳簿に記録された相手先ではない。** 実帳簿では32件すべてに取引先タグが
/// 無く、頼れるのは摘要だけである。ここで返すのは推測なので、列の名前も
/// 「相手先の手がかり」にしてある。
///
/// # 拾い方
///
/// 1. `摘要 / 取引先名` の形なら `/` の後ろ（freee からの同期がこの形で書く）
/// 2. 摘要が科目名と同じなら**何も返さない**——「地代家賃」という摘要から
///    分かることは無い
/// 3. それ以外は摘要そのもの（`CLAUDE.AI SUBSCRIPTION` や `AMAZON.CO.JP`）
///
/// 実帳簿の32件では、1 が12件・3 が11件・2 が9件だった。**9件は明細を
/// 見に行くしかない。** それを名指しできるのがこの関数の値打ちである。
#[must_use]
pub fn counterparty_hint(description: &str, account_name: &str) -> Option<String> {
    if let Some((_, party)) = description.rsplit_once(" / ") {
        let party = party.trim();
        if !party.is_empty() {
            return Some(party.to_string());
        }
    }
    let trimmed = description.trim();
    if trimmed.is_empty() || trimmed == account_name.trim() {
        return None;
    }
    Some(trimmed.to_string())
}

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
const HEADER: [&str; 7] = [
    "取引日",
    "仕訳番号",
    "金額",
    "摘要",
    "科目",
    "科目名",
    "相手先の手がかり",
];

/// CSV にする。0 件でも見出しだけのファイルを書く
/// （`blue_return_not_on_form.csv` と同じ。ファイルが無いのと0件は違う）。
pub fn to_csv(rows: &[InvoiceToCollect]) -> String {
    // **同じ相手先を隣り合わせにする。** 請求書を集めるのは相手先ごとの
    // 作業である（GMOに1回入れば2件とも取れる）。日付順に散らばっていると
    // 同じ場所へ何度も行くことになる。
    //
    // **かたまりの並びは合計額の大きい順。** 先頭しか読まれないことがある
    // ので、並び順が「何を見せるか」になる。相手先でまとめたうえで、金額の
    // 大きいかたまりを先に出す。
    //
    // **手がかりが無いものは末尾へ。** 合計額では上位に来るが、そこだけ
    // 明細を見に行くという別の作業になる。作業リストの先頭に置くと最初で
    // つまずく。
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<Option<String>, (i128, Vec<&InvoiceToCollect>)> = BTreeMap::new();
    for row in rows {
        let slot = groups
            .entry(counterparty_hint(&row.description, &row.account_name))
            .or_insert((0, Vec::new()));
        slot.0 += row.amount_minor;
        slot.1.push(row);
    }
    let mut ordered: Vec<(Option<String>, i128, Vec<&InvoiceToCollect>)> = groups
        .into_iter()
        .map(|(hint, (total, rows))| (hint, total, rows))
        .collect();
    ordered.sort_by(|a, b| {
        a.0.is_none()
            .cmp(&b.0.is_none())
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| a.0.cmp(&b.0))
    });
    let rows: Vec<&InvoiceToCollect> = ordered
        .into_iter()
        .flat_map(|(_, _, mut rows)| {
            rows.sort_by(|a, b| {
                b.amount_minor
                    .cmp(&a.amount_minor)
                    .then_with(|| a.date.cmp(&b.date))
                    .then_with(|| a.entry_no.cmp(&b.entry_no))
            });
            rows
        })
        .collect();

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
            counterparty_hint(&row.description, &row.account_name).unwrap_or_default(),
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
    fn row_on(
        date: &str,
        no: i64,
        amount: i128,
        description: &str,
        account_name: &str,
    ) -> InvoiceToCollect {
        InvoiceToCollect {
            date: date.to_string(),
            entry_no: no,
            amount_minor: amount,
            description: description.to_string(),
            account: "604".to_string(),
            account_name: account_name.to_string(),
        }
    }

    // ─── 相手先の手がかり ───────────────────────────

    // **本命。** `摘要 / 取引先名` の形なら後ろを取る。
    //
    // freee からの同期がこの形で書く。実帳簿の32件では12件がこれ。
    #[test]
    fn the_hint_comes_from_the_part_after_the_slash() {
        assert_eq!(
            counterparty_hint(
                "ムームードメイン byGMOペパボ（ドメイン） / GMOペパボ",
                "通信費"
            ),
            Some("GMOペパボ".to_string())
        );
    }

    // **本命。** 摘要が科目名と同じなら手がかりにならない。
    //
    // 「地代家賃」という摘要から分かることは無い。**返してしまうと、
    // 手がかりがあるように見えて末尾へ寄らない。** 実帳簿では9件がこれで、
    // その9件は明細を見に行くしかない。
    #[test]
    fn a_description_equal_to_the_account_name_is_not_a_hint() {
        assert_eq!(counterparty_hint("地代家賃", "地代家賃"), None);
        assert_eq!(counterparty_hint("  通信費  ", "通信費"), None);
    }

    // **本命。** それ以外は摘要そのものが手がかりになる。
    #[test]
    fn a_plain_description_is_used_as_the_hint() {
        assert_eq!(
            counterparty_hint("CLAUDE.AI SUBSCRIPTION", "通信費"),
            Some("CLAUDE.AI SUBSCRIPTION".to_string())
        );
    }

    // 空の摘要は手がかりにならない。
    #[test]
    fn an_empty_description_is_not_a_hint() {
        assert_eq!(counterparty_hint("", "通信費"), None);
        assert_eq!(counterparty_hint("   ", "通信費"), None);
    }

    // スラッシュの後ろが空なら、そこは使わない。
    #[test]
    fn an_empty_part_after_the_slash_falls_back() {
        assert_eq!(
            counterparty_hint("Amazon / ", "消耗品費"),
            Some("Amazon /".to_string())
        );
    }

    // ─── 並び順 ─────────────────────────────────

    // **本命。** 同じ相手先を隣り合わせにする。
    //
    // 請求書を集めるのは相手先ごとの作業である（GMOに1回入れば2件とも
    // 取れる）。日付順に散らばっていると同じ場所へ何度も行くことになる。
    #[test]
    fn the_same_counterparty_is_grouped_together() {
        let csv = to_csv(&[
            row_on("2026-05-29", 1, 43_967, "ドメイン / GMOペパボ", "通信費"),
            row_on("2026-04-27", 2, 35_835, "CLAUDE.AI SUBSCRIPTION", "通信費"),
            row_on(
                "2026-06-17",
                3,
                43_967,
                "ドメイン更新 / GMOペパボ",
                "通信費",
            ),
        ]);

        // **並びの先頭がどちらかは問わない。** 手がかりの文字列順なので
        // ASCII の CLAUDE が日本語の GMO より先に来る。確かめたいのは
        // 「同じ相手先が隣り合う」ことだけである。
        let lines: Vec<&str> = csv.lines().collect();
        let gmo: Vec<usize> = (1..=3)
            .filter(|i| lines[*i].contains("GMOペパボ"))
            .collect();
        assert_eq!(gmo.len(), 2, "{csv}");
        assert_eq!(gmo[1] - gmo[0], 1, "隣り合わせにすること: {csv}");
    }

    // **本命。** 手がかりが無いものは末尾へ寄せる。
    //
    // そこだけ明細を見に行く必要がある。**先頭に来ると、作業リストの
    // 最初でつまずく。**
    #[test]
    fn rows_without_a_hint_go_last() {
        let csv = to_csv(&[
            row_on("2026-01-23", 1, 410_000, "地代家賃", "地代家賃"),
            row_on("2026-04-27", 2, 35_835, "CLAUDE.AI SUBSCRIPTION", "通信費"),
        ]);

        let lines: Vec<&str> = csv.lines().collect();
        assert!(
            lines[1].contains("CLAUDE.AI"),
            "手がかりのある方が先: {csv}"
        );
        assert!(lines[2].contains("地代家賃"), "{csv}");
    }

    // **本命。** かたまりの並びは合計額の大きい順。
    //
    // 先頭しか読まれないことがあるので、並び順が「何を見せるか」になる。
    // 相手先でまとめたうえで、金額の大きいかたまりを先に出す。
    #[test]
    fn the_groups_are_ordered_by_total_amount() {
        let csv = to_csv(&[
            row_on("2026-01-01", 1, 1_000, "小さい / A社", "通信費"),
            row_on("2026-01-02", 2, 500_000, "大きい / B社", "通信費"),
            row_on("2026-01-03", 3, 1_000, "小さい2 / A社", "通信費"),
        ]);

        let lines: Vec<&str> = csv.lines().collect();
        assert!(lines[1].contains("B社"), "合計の大きい方が先: {csv}");
        assert!(lines[2].contains("A社"), "{csv}");
        assert!(lines[3].contains("A社"), "{csv}");
    }

    // 同じ相手先のなかは金額の大きい順。
    #[test]
    fn within_one_counterparty_the_order_is_by_amount() {
        let csv = to_csv(&[
            row_on("2026-05-29", 1, 100, "a / GMO", "通信費"),
            row_on("2026-06-17", 2, 900, "b / GMO", "通信費"),
        ]);

        assert!(csv.lines().nth(1).unwrap().contains("900"), "{csv}");
    }

    // **本命。** 手がかりの列を書き出す。
    #[test]
    fn the_hint_column_is_written() {
        let csv = to_csv(&[row_on(
            "2026-05-29",
            1,
            43_967,
            "ドメイン / GMOペパボ",
            "通信費",
        )]);

        assert!(csv.contains("相手先の手がかり"), "見出し: {csv}");
        assert!(csv.lines().nth(1).unwrap().ends_with("GMOペパボ"), "{csv}");
    }
}
