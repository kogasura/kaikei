//! 青色申告決算書の「減価償却費の計算」欄のデータ出力。
//!
//! 償却額を計算するのは `kaikei-jp`（`depreciation`）の仕事で、この module は
//! **受け取った行を表に開くだけ**である（`blue_return` / `blue_return_bs` と
//! 同じ分担）。
//!
//! # 様式の欄をそのまま列にする
//!
//! 転記する人が上から順に写せるように、様式の欄の並びに合わせる。
//! 名前を変えたり順番を入れ替えたりしない。
//!
//! # 一括償却・少額特例は償却率の欄が空く
//!
//! どちらも耐用年数と償却率を使わない（`DECISIONS.md` D-103）。
//! **0 と書かない。** 0 と斜線は違う（`blue_return_bs` と同じ扱い）。

use crate::csv::CsvBuilder;
use crate::html::PrintableTable;
use kaikei_app::amount::money_to_plain_string;
use kaikei_core::Money;

/// 「減価償却費の計算」欄の1行（＝資産1件）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepreciationRow {
    /// 減価償却資産の名称等。
    pub name: String,
    /// 取得年月（`2025-07` の形）。
    pub acquired: String,
    /// 取得価額。
    pub acquisition_cost: Money,
    /// 償却の基礎になる金額。定額法では取得価額と同じ。
    pub base_amount: Money,
    /// 償却方法（`定額法` / `一括償却` / `少額特例`）。
    pub method: String,
    /// 耐用年数。使わない方法では `None`。
    pub useful_life_years: Option<i16>,
    /// 償却率（`0.334` の形）。使わない方法では `None`。
    pub rate: Option<String>,
    /// 本年中の償却期間（`6/12` の形）。
    pub period: String,
    /// 本年分の償却費（事業専用割合を掛ける**前**）。
    pub before_ratio: Money,
    /// 事業専用割合（`100%` / `80%` の形）。
    pub business_ratio: String,
    /// 本年分の必要経費算入額（事業専用割合を掛けた**後**）。
    pub amount: Money,
    /// 未償却残高（期末）。
    pub book_value: Money,
    /// 摘要。
    pub note: String,
}

/// 見出し。**様式の欄の並びに合わせる。**
const HEADERS: &[&str] = &[
    "減価償却資産の名称等",
    "取得年月",
    "取得価額",
    "償却の基礎になる金額",
    "償却方法",
    "耐用年数",
    "償却率",
    "本年中の償却期間",
    "本年分の普通償却費",
    "事業専用割合",
    "本年分の必要経費算入額",
    "未償却残高",
    "摘要",
];

/// 右寄せにする列。
const NUMERIC_COLUMNS: &[usize] = &[2, 3, 5, 6, 8, 10, 11];

/// 使わない欄の表示。**空文字や 0 にしない。**
const NOT_APPLICABLE: &str = "—";

fn to_cells(rows: &[DepreciationRow]) -> Vec<Vec<String>> {
    rows.iter()
        .map(|row| {
            vec![
                row.name.clone(),
                row.acquired.clone(),
                money_to_plain_string(&row.acquisition_cost),
                money_to_plain_string(&row.base_amount),
                row.method.clone(),
                row.useful_life_years
                    .map(|y| y.to_string())
                    .unwrap_or_else(|| NOT_APPLICABLE.to_string()),
                row.rate
                    .clone()
                    .unwrap_or_else(|| NOT_APPLICABLE.to_string()),
                row.period.clone(),
                money_to_plain_string(&row.before_ratio),
                row.business_ratio.clone(),
                money_to_plain_string(&row.amount),
                money_to_plain_string(&row.book_value),
                row.note.clone(),
            ]
        })
        .collect()
}

/// 「減価償却費の計算」欄の CSV。
pub fn to_csv(rows: &[DepreciationRow]) -> String {
    let mut csv = CsvBuilder::new();
    csv.push_row(HEADERS);
    for cells in to_cells(rows) {
        csv.push_row(cells);
    }
    csv.finish()
}

/// 「減価償却費の計算」欄の印刷用 HTML。
///
/// `total` は本年分の必要経費算入額の合計。**損益計算書の減価償却費と
/// 一致するべき額**なので、合計を必ず出す。
pub fn to_html(period_label: &str, rows: &[DepreciationRow], total: &Money) -> String {
    let notes = vec![format!(
        "本年分の必要経費算入額の合計: {} 円。損益計算書の減価償却費と一致するか確かめてください",
        money_to_plain_string(total)
    )];
    let cells = to_cells(rows);
    PrintableTable {
        title: "減価償却費の計算",
        subtitle: period_label,
        headers: HEADERS,
        rows: &cells,
        notes: &notes,
        numeric_columns: NUMERIC_COLUMNS,
        footer_rows: &[],
        // 13列あるので A4 縦には収まらない。**刷ってから右端が切れたと
        // 気づくのが最も困る。**
        landscape: true,
    }
    .render()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_core::Currency;

    fn yen(v: i128) -> Money {
        Money::from_minor(v, Currency::JPY)
    }

    fn straight_line() -> DepreciationRow {
        DepreciationRow {
            name: "パソコン・周辺機器".to_string(),
            acquired: "2025-07".to_string(),
            acquisition_cost: yen(280_717),
            base_amount: yen(280_717),
            method: "定額法".to_string(),
            useful_life_years: Some(4),
            rate: Some("0.250".to_string()),
            period: "6/12".to_string(),
            before_ratio: yen(35_089),
            business_ratio: "100%".to_string(),
            amount: yen(35_089),
            book_value: yen(245_628),
            note: String::new(),
        }
    }

    fn lump_sum() -> DepreciationRow {
        DepreciationRow {
            name: "pc".to_string(),
            acquired: "2022-08".to_string(),
            acquisition_cost: yen(118_800),
            base_amount: yen(118_800),
            method: "一括償却".to_string(),
            useful_life_years: None,
            rate: None,
            period: "12/12".to_string(),
            before_ratio: yen(39_600),
            business_ratio: "100%".to_string(),
            amount: yen(39_600),
            book_value: yen(79_200),
            note: "3年均等".to_string(),
        }
    }

    #[test]
    fn the_csv_follows_the_form_column_order() {
        let csv = to_csv(&[straight_line()]);
        // 先頭に BOM が付く（Excel 向け。`CsvBuilder` の仕様）。
        let header = csv.lines().next().unwrap().trim_start_matches('\u{feff}');
        assert!(
            header.starts_with("減価償却資産の名称等,取得年月,取得価額"),
            "{header}"
        );
        assert!(
            header.ends_with("本年分の必要経費算入額,未償却残高,摘要"),
            "{header}"
        );
    }

    #[test]
    fn a_straight_line_row_carries_the_life_and_rate() {
        let csv = to_csv(&[straight_line()]);
        let row = csv.lines().nth(1).unwrap();
        assert!(row.contains(",4,"), "耐用年数: {row}");
        assert!(row.contains(",0.250,"), "償却率: {row}");
        assert!(row.contains(",6/12,"), "償却期間: {row}");
    }

    // **本命。** 使わない欄を 0 にしない。
    //
    // 一括償却は耐用年数も償却率も使わない。0 と書くと「耐用年数0年」と
    // 読めてしまう。
    #[test]
    fn unused_columns_are_struck_through_not_zero() {
        let csv = to_csv(&[lump_sum()]);
        let row = csv.lines().nth(1).unwrap();
        assert!(
            row.contains(",—,—,"),
            "耐用年数と償却率が斜線であること: {row}"
        );
        assert!(!row.contains(",0,0,"), "0 と書かないこと: {row}");
    }

    // 合計を必ず出す。損益計算書の減価償却費と突き合わせるための数字である。
    #[test]
    fn the_html_states_the_total() {
        let html = to_html("2025-01-01 〜 2025-12-31", &[straight_line()], &yen(35_089));
        assert!(html.contains("35089"), "{html}");
        assert!(
            html.contains("損益計算書の減価償却費と一致するか"),
            "合計の使い道を言うこと"
        );
    }

    #[test]
    fn an_empty_ledger_still_writes_the_header() {
        let csv = to_csv(&[]);
        assert_eq!(csv.lines().count(), 1, "見出しだけ出る");
    }
}
