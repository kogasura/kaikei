//! 試算表の出力。
//!
//! 入力は [`kaikei_app::view::TrialBalanceView`]（read model の SQL 集計結果）。
//! **仕訳から計算し直さない**——同じ試算表を2箇所で組み立てると、いつか
//! 食い違う（`DECISIONS.md` D-031 / D-093 の住み分け）。
//!
//! # 合計行を必ず出す
//!
//! 試算表は**借方合計と貸方合計が一致すること**を確かめるための帳簿である。
//! 明細だけ並べて合計を出さないと、その用が果たせない。合計行は `<tfoot>` に
//! 入れて明細と見た目を変える（同じ見た目だと読み飛ばす）。
//!
//! 貸借が一致しない試算表は**そもそも返ってこない**
//! （`kaikei_app::usecase::report::execute` が `AppError::Inconsistent` で
//! 弾く）。ここに届いた時点で一致しているので、出力側では検算しない——
//! **2箇所で同じ検算をすると、片方だけ直したときに食い違う。**

use crate::csv::CsvBuilder;
use crate::html::PrintableTable;
use kaikei_app::amount::money_to_plain_string;
use kaikei_app::view::TrialBalanceView;
use kaikei_core::{AccountCode, ChartOfAccounts};

/// 表の見出し。CSV と HTML で共有する。
const HEADERS: &[&str] = &["科目コード", "勘定科目", "借方合計", "貸方合計", "残高"];

/// 右寄せにする列（金額）。`HEADERS` の添字。
const NUMERIC_COLUMNS: &[usize] = &[2, 3, 4];

/// 試算表を表に開く。
fn to_rows(view: &TrialBalanceView, chart: &ChartOfAccounts) -> Vec<Vec<String>> {
    view.rows()
        .iter()
        .map(|row| {
            vec![
                row.account.as_str().to_string(),
                account_name(chart, &row.account),
                money_to_plain_string(&row.debit_total),
                money_to_plain_string(&row.credit_total),
                money_to_plain_string(&row.balance),
            ]
        })
        .collect()
}

/// 合計行。
///
/// `TrialBalanceView::totals` は行の通貨が帳簿通貨と食い違う場合などに
/// エラーを返すが、**ここに届く時点で `report::execute` が検算済み**である。
/// それでも `Result` を握り潰さず、失敗したら合計欄に理由を出す——
/// 合計が空欄の試算表が黙って刷られるより、何が起きたか分かるほうがよい。
fn footer_rows(view: &TrialBalanceView) -> Vec<Vec<String>> {
    match view.totals() {
        Ok((debit, credit)) => vec![vec![
            String::new(),
            "合計".to_string(),
            money_to_plain_string(&debit),
            money_to_plain_string(&credit),
            String::new(),
        ]],
        Err(error) => vec![vec![
            String::new(),
            "合計を計算できません".to_string(),
            error.to_string(),
            String::new(),
            String::new(),
        ]],
    }
}

/// 科目コードから科目名を引く。
///
/// 帳簿に無いコードは空にせず、引けなかったことが分かる形で返す
/// （空だと「名前の無い科目」に見える）。
fn account_name(chart: &ChartOfAccounts, code: &AccountCode) -> String {
    chart
        .get(code)
        .map(|def| def.name.clone())
        .unwrap_or_else(|| format!("（{}：勘定科目表にありません）", code.as_str()))
}

/// 試算表の CSV。
pub fn to_csv(view: &TrialBalanceView, chart: &ChartOfAccounts) -> String {
    let mut csv = CsvBuilder::new();
    csv.push_row(HEADERS);
    for row in to_rows(view, chart) {
        csv.push_row(row);
    }
    // CSV では合計行も普通の行として続ける（表計算では最終行という位置で足りる）。
    for row in footer_rows(view) {
        csv.push_row(row);
    }
    csv.finish()
}

/// 試算表の印刷用 HTML。
pub fn to_html(
    view: &TrialBalanceView,
    chart: &ChartOfAccounts,
    period: &str,
    notes: &[String],
) -> String {
    PrintableTable {
        title: "試算表",
        subtitle: period,
        headers: HEADERS,
        rows: &to_rows(view, chart),
        notes,
        numeric_columns: NUMERIC_COLUMNS,
        footer_rows: &footer_rows(view),
        // 5列なので A4 縦に収まる。
        landscape: false,
    }
    .render()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_app::view::{BalanceRowView, GroupKeyView};
    use kaikei_core::{AccountDef, AccountType, Currency, Money};

    fn def(code: &str, name: &str, account_type: AccountType) -> AccountDef {
        AccountDef {
            code: AccountCode::parse(code).unwrap(),
            name: name.to_string(),
            account_type,
            parent: None,
            postable: true,
        }
    }

    fn chart() -> ChartOfAccounts {
        ChartOfAccounts::new(vec![
            def("100", "現金", AccountType::Asset),
            def("500", "売上高", AccountType::Revenue),
        ])
        .unwrap()
    }

    fn yen(amount: i128) -> Money {
        Money::from_minor(amount, Currency::JPY)
    }

    fn row(code: &str, account_type: AccountType, debit: i128, credit: i128) -> BalanceRowView {
        let balance = if account_type.is_debit_normal() {
            debit - credit
        } else {
            credit - debit
        };
        BalanceRowView {
            account: AccountCode::parse(code).unwrap(),
            account_type,
            group: GroupKeyView::default(),
            debit_total: yen(debit),
            credit_total: yen(credit),
            balance: yen(balance),
        }
    }

    fn view() -> TrialBalanceView {
        TrialBalanceView::new(
            vec![
                row("100", AccountType::Asset, 110_000, 0),
                row("500", AccountType::Revenue, 0, 110_000),
            ],
            Currency::JPY,
        )
    }

    #[test]
    fn the_csv_carries_account_names_next_to_the_codes() {
        let csv = to_csv(&view(), &chart());

        assert!(csv.contains("100,現金,110000,0,110000"), "{csv}");
        assert!(csv.contains("500,売上高,0,110000,110000"), "{csv}");
    }

    // 試算表は「借方合計＝貸方合計」を確かめるための帳簿なので、
    // 合計が出ていなければ用が果たせない。
    #[test]
    fn both_outputs_carry_the_totals() {
        let csv = to_csv(&view(), &chart());
        let html = to_html(&view(), &chart(), "2026-01-01 〜 2026-12-31", &[]);

        assert!(csv.contains("合計,110000,110000"), "{csv}");
        assert!(html.contains("<tfoot>"), "{html}");
        assert!(html.contains("<td class=\"total\">合計</td>"), "{html}");
        assert!(
            html.contains("<td class=\"num total\">110000</td>"),
            "{html}"
        );
    }

    // 帳簿に無い科目コードでも、名前欄を空にせず引けなかったことを示す。
    #[test]
    fn an_unknown_account_code_is_labelled_as_missing() {
        let unknown = TrialBalanceView::new(
            vec![row("999", AccountType::Expense, 1_000, 0)],
            Currency::JPY,
        );

        let csv = to_csv(&unknown, &chart());

        assert!(csv.contains("勘定科目表にありません"), "{csv}");
    }

    // 0 件の試算表も成立する（その期間に取引が無かった）。
    #[test]
    fn an_empty_trial_balance_still_shows_zero_totals() {
        let empty = TrialBalanceView::new(Vec::new(), Currency::JPY);

        let csv = to_csv(&empty, &chart());
        let html = to_html(&empty, &chart(), "2026-01-01 〜 2026-01-31", &[]);

        assert!(csv.contains("合計,0,0"), "{csv}");
        assert!(
            html.contains("この期間に該当する記録はありません"),
            "{html}"
        );
        // 0 件でも合計行は出す（「合計が無い」と「合計が0」は違う）。
        assert!(html.contains("<tfoot>"), "{html}");
    }

    // CSV と HTML が同じ値を運ぶ。
    #[test]
    fn the_csv_and_the_html_carry_the_same_values() {
        let csv = to_csv(&view(), &chart());
        let html = to_html(&view(), &chart(), "2026-01-01 〜 2026-12-31", &[]);

        for header in HEADERS {
            assert!(csv.contains(header), "CSV に見出しが無い: {header}");
            assert!(html.contains(header), "HTML に見出しが無い: {header}");
        }
        for value in ["現金", "売上高", "110000"] {
            assert!(csv.contains(value), "CSV に値が無い: {value}");
            assert!(html.contains(value), "HTML に値が無い: {value}");
        }
    }

    // 試算表は列が少ないので縦向きで刷る。
    #[test]
    fn the_trial_balance_prints_portrait() {
        let html = to_html(&view(), &chart(), "", &[]);
        assert!(!html.contains("landscape"), "{html}");
    }
}
