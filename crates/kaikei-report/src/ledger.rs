//! 総勘定元帳の出力。
//!
//! 所得税法施行規則58条が青色申告者に備え付けを求める法定帳簿のひとつ。
//! 入力は [`kaikei_app::view::LedgerPageView`] を科目ごとに集めたもの
//! （read model の SQL 集計。仕訳から組み立て直さない）。
//!
//! # 期首残高は read model が持っている
//!
//! `LedgerPageView::opening_balance` は **`from` より前の全明細**から求めた
//! 残高である。仕訳日記帳や試算表が「指定期間の仕訳だけ」を見るのと違い、
//! 元帳は期首からの推移を映す——`running_balance` が期首残高を含むのも
//! そのためで、この層で足し直さない。
//!
//! # 科目をまたいでフラットな表にする
//!
//! 科目ごとに表を分けず、**科目コードを1列目に持つ平らな表**にする
//! （財務諸表の区分と同じ扱い）。CSV に落としたときに表計算でそのまま
//! 科目でフィルタでき、HTML でも同じ構造になる。科目ごとの期首・期末残高は
//! 行として本文に入れる。
//!
//! # 取引の無い科目は出さない
//!
//! 同梱テンプレートは56科目あり、その全部にページを作ると**使っていない
//! 科目のほうが多い帳簿**になる。出すのは「期間中に明細がある」か
//! 「期首残高がある」科目に限る——**どちらも無い科目は、その期間の帳簿に
//! 現れる理由が無い。**

use crate::csv::{CsvBuilder, UTF8_BOM};
use crate::html::PrintableTable;
use kaikei_app::amount::money_to_plain_string;
use kaikei_app::view::{LedgerPageView, LedgerRowView};
use kaikei_app::wire::side_code;
use std::collections::BTreeMap;

/// 表の見出し。CSV と HTML で共有する。
const HEADERS: &[&str] = &[
    "科目コード",
    "勘定科目",
    "取引日",
    "仕訳番号",
    "摘要",
    "相手科目",
    "貸借",
    "金額",
    "残高",
    "備考",
];

/// 右寄せにする列（金額・残高）。`HEADERS` の添字。
const NUMERIC_COLUMNS: &[usize] = &[7, 8];

/// この科目を帳簿に出すか。
///
/// 期間中に明細があるか、期首残高があるかのどちらか。**どちらも無い科目は、
/// その期間の帳簿に現れる理由が無い。**
pub fn is_worth_printing(page: &LedgerPageView) -> bool {
    page.total_lines > 0 || !page.opening_balance.is_zero()
}

/// 科目ごとのページを表に開く。
///
/// 各科目について「期首残高の行 → 明細 → 期末残高の行」を出す。
/// **期首と期末を行として出すのは、CSV に落としたときに失われないため**——
/// ヘッダやフッタに置くと、表計算で並べ替えた瞬間に意味が消える。
fn to_rows(pages: &[LedgerPageView]) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    for page in pages {
        let code = page.account.as_str().to_string();
        let name = page.account_name.clone();

        rows.push(vec![
            code.clone(),
            name.clone(),
            String::new(),
            String::new(),
            "期首残高".to_string(),
            String::new(),
            String::new(),
            String::new(),
            money_to_plain_string(&page.opening_balance),
            String::new(),
        ]);

        for row in &page.rows {
            rows.push(row_to_cells(&code, &name, row));
        }

        rows.push(vec![
            code,
            name,
            String::new(),
            String::new(),
            "期末残高".to_string(),
            String::new(),
            String::new(),
            String::new(),
            money_to_plain_string(&page.closing_balance),
            String::new(),
        ]);
    }
    rows
}

/// 明細1行。
fn row_to_cells(code: &str, name: &str, row: &LedgerRowView) -> Vec<String> {
    // 相手科目は複数ありうる（3行以上の仕訳）。読み手が「どこへ振れたか」を
    // 1行で追えるように、区切って並べる。
    let counter = row
        .counter_accounts
        .iter()
        .map(|account| account.as_str())
        .collect::<Vec<_>>()
        .join(" / ");

    vec![
        code.to_string(),
        name.to_string(),
        row.entry_date.to_iso_string(),
        row.entry_no.as_u32().to_string(),
        row.description.clone(),
        counter,
        side_code(row.side).to_string(),
        money_to_plain_string(&row.amount),
        money_to_plain_string(&row.running_balance),
        row.memo.clone().unwrap_or_default(),
    ]
}

/// 書き出した CSV を読み直して、科目ごとの最終残高を拾う。
///
/// # なぜ書いたものを読み直すのか
///
/// **`LedgerPageView` をそのまま数えると、書き出しの誤りを見逃す。**
/// 列の取り違えや行の脱落は、出力を読み直して初めて分かる（`export.json`
/// や弥生CSVで同じことをしている）。
///
/// # なぜ試算表と突き合わせるのか
///
/// 元帳と試算表は**仕訳から別々に集計される**。同じ帳簿から出ている以上
/// 一致するはずで、**しないならどちらかの集計が間違っている。**
///
/// 元帳は「科目ごとに1ページ、行ごとに残高を積む」形なので、科目の最後の
/// 行の残高がその科目の期末残高になる。
///
/// # 空欄の残高は読み飛ばす
///
/// 期首残高の行など、残高だけを持つ行もある。数として読めない行は
/// 飛ばす——**読めない行を 0 として数えると、突合が黙って通る。**
#[must_use]
pub fn closing_balances(csv: &str) -> BTreeMap<String, i128> {
    let mut last: BTreeMap<String, i128> = BTreeMap::new();
    for line in csv.lines().skip(1) {
        // **素朴に分ける。** 科目コードと残高は数字だけなので、引用符の
        // 中にカンマが来ることは無い（摘要には来るが、そこは読まない）。
        let cells: Vec<&str> = line.split(',').collect();
        // 科目コード(0) と 残高(8)。
        let (Some(code), Some(balance)) = (cells.first(), cells.get(8)) else {
            continue;
        };
        let code = code.trim_start_matches(UTF8_BOM);
        if code.is_empty() {
            continue;
        }
        let Ok(value) = balance.trim().parse::<i128>() else {
            continue;
        };
        last.insert(code.to_string(), value);
    }
    last
}

/// 総勘定元帳の CSV。
pub fn to_csv(pages: &[LedgerPageView]) -> String {
    let mut csv = CsvBuilder::new();
    csv.push_row(HEADERS);
    for row in to_rows(pages) {
        csv.push_row(row);
    }
    csv.finish()
}

/// 総勘定元帳の印刷用 HTML。
pub fn to_html(pages: &[LedgerPageView], period: &str, notes: &[String]) -> String {
    PrintableTable {
        title: "総勘定元帳",
        subtitle: period,
        headers: HEADERS,
        rows: &to_rows(pages),
        notes,
        numeric_columns: NUMERIC_COLUMNS,
        // 合計は科目ごとの期末残高が担う。帳簿全体の合計は試算表の役割で、
        // ここで足し上げても意味が無い（科目種別が混ざる）。
        footer_rows: &[],
        // 10列あるので A4 縦では収まらない。
        landscape: true,
    }
    .render()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_core::{
        AccountCode, AccountType, AccountingDate, Currency, EntryId, EntryNumber, Money, Side,
        TagSet,
    };

    fn yen(amount: i128) -> Money {
        Money::from_minor(amount, Currency::JPY)
    }

    fn row(
        entry_no: u32,
        day: u8,
        description: &str,
        side: Side,
        amount: i128,
        running: i128,
        counter: &[&str],
    ) -> LedgerRowView {
        LedgerRowView {
            entry_id: EntryId::new(u128::from(entry_no)),
            entry_no: EntryNumber::new(entry_no),
            entry_date: AccountingDate::new(2026, 6, day).unwrap(),
            line_no: 1,
            description: description.to_string(),
            side,
            amount: yen(amount),
            tags: TagSet::new(),
            memo: None,
            counter_accounts: counter
                .iter()
                .map(|c| AccountCode::parse(c).unwrap())
                .collect(),
            running_balance: yen(running),
            reverses: None,
            reverse_reason: None,
            reversed_by: None,
        }
    }

    fn page(code: &str, name: &str, opening: i128, rows: Vec<LedgerRowView>) -> LedgerPageView {
        let total_lines = rows.len() as u64;
        let closing = rows.last().map_or(opening, |r| r.running_balance.minor());
        LedgerPageView {
            account: AccountCode::parse(code).unwrap(),
            account_name: name.to_string(),
            account_type: AccountType::Asset,
            opening_balance: yen(opening),
            debit_total: yen(0),
            credit_total: yen(0),
            closing_balance: yen(closing),
            total_lines,
            rows,
            next_cursor: None,
        }
    }

    fn sample() -> Vec<LedgerPageView> {
        vec![page(
            "135",
            "売掛金",
            100_000,
            vec![
                row(1, 15, "ABC 5月分", Side::Debit, 550_000, 650_000, &["500"]),
                row(2, 30, "入金", Side::Credit, 100_000, 550_000, &["101"]),
            ],
        )]
    }

    // 期首残高と期末残高が行として出る（ヘッダやフッタに置くと、表計算で
    // 並べ替えた瞬間に意味が消える）。
    #[test]
    fn the_opening_and_closing_balances_are_rows() {
        let csv = to_csv(&sample());

        assert!(csv.contains("135,売掛金,,,期首残高,,,,100000,"), "{csv}");
        assert!(csv.contains("135,売掛金,,,期末残高,,,,550000,"), "{csv}");
    }

    // 残高の推移が行ごとに出る（元帳の要点）。
    #[test]
    fn each_row_carries_its_running_balance() {
        let csv = to_csv(&sample());

        assert!(
            csv.contains("2026-06-15,1,ABC 5月分,500,debit,550000,650000,"),
            "{csv}"
        );
        assert!(
            csv.contains("2026-06-30,2,入金,101,credit,100000,550000,"),
            "{csv}"
        );
    }

    // 相手科目が複数あるときは区切って並べる（3行以上の仕訳）。
    #[test]
    fn multiple_counter_accounts_are_joined() {
        let pages = vec![page(
            "100",
            "現金",
            0,
            vec![row(
                1,
                1,
                "複合仕訳",
                Side::Debit,
                1_000,
                1_000,
                &["500", "330"],
            )],
        )];

        let csv = to_csv(&pages);

        assert!(csv.contains("500 / 330"), "{csv}");
    }

    // 取引も期首残高も無い科目は出さない。
    #[test]
    fn an_untouched_account_is_not_worth_printing() {
        let empty = page("604", "通信費", 0, Vec::new());
        assert!(!is_worth_printing(&empty));

        let with_opening = page("100", "現金", 50_000, Vec::new());
        assert!(is_worth_printing(&with_opening));

        let with_rows = page(
            "500",
            "売上高",
            0,
            vec![row(1, 1, "売上", Side::Credit, 1_000, 1_000, &["135"])],
        );
        assert!(is_worth_printing(&with_rows));
    }

    // 科目をまたいでも1つの表になる。
    #[test]
    fn multiple_accounts_share_one_table() {
        let pages = vec![
            page(
                "135",
                "売掛金",
                0,
                vec![row(1, 1, "売上", Side::Debit, 1_000, 1_000, &["500"])],
            ),
            page(
                "500",
                "売上高",
                0,
                vec![row(1, 1, "売上", Side::Credit, 1_000, 1_000, &["135"])],
            ),
        ];

        let csv = to_csv(&pages);
        let html = to_html(&pages, "2026-01-01 〜 2026-12-31", &[]);

        assert!(csv.contains("135,売掛金"), "{csv}");
        assert!(csv.contains("500,売上高"), "{csv}");
        // 表は1つ（科目ごとに分けない）。
        assert_eq!(html.matches("<table>").count(), 1, "{html}");
    }

    // 列が多いので横向きで刷る。
    #[test]
    fn the_ledger_prints_landscape() {
        let html = to_html(&sample(), "", &[]);
        assert!(html.contains("size: A4 landscape"), "{html}");
    }

    // CSV と HTML が同じ値を運ぶ。
    #[test]
    fn the_csv_and_the_html_carry_the_same_values() {
        let csv = to_csv(&sample());
        let html = to_html(&sample(), "2026-01-01 〜 2026-12-31", &[]);

        for header in HEADERS {
            assert!(csv.contains(header), "CSV に見出しが無い: {header}");
            assert!(html.contains(header), "HTML に見出しが無い: {header}");
        }
        for value in ["売掛金", "550000", "650000", "期首残高", "期末残高"] {
            assert!(csv.contains(value), "CSV に値が無い: {value}");
            assert!(html.contains(value), "HTML に値が無い: {value}");
        }
    }
    // ─── 書き出した CSV から残高を読み直す ─────────────

    /// 見出し行。
    const HEADER: &str = "科目コード,勘定科目,取引日,仕訳番号,摘要,相手科目,貸借,金額,残高,備考";

    /// **本命。** 科目ごとの最後の行の残高を拾う。
    ///
    /// 元帳は科目ごとに行を並べて残高を積む形なので、最後の行がその科目の
    /// 期末残高になる。
    #[test]
    fn the_last_balance_of_each_account_is_picked_up() {
        let csv = [
            HEADER,
            "100,現金,2026-01-05,1,売上,500,1,1000,1000,",
            "100,現金,2026-02-05,2,仕入,600,2,300,700,",
            "110,普通預金,2026-03-05,3,振替,100,1,500,500,",
        ]
        .join(
            "
",
        );

        let balances = closing_balances(&csv);

        assert_eq!(balances.get("100"), Some(&700), "最後の行を取ること");
        assert_eq!(balances.get("110"), Some(&500));
    }

    /// **本命。** 残高が読めない行は飛ばす。
    ///
    /// 期首残高の行など、金額の欄が空の行がある。**読めない行を 0 として
    /// 数えると、突合が黙って通る。**
    #[test]
    fn a_row_without_a_readable_balance_is_skipped() {
        let csv = [
            HEADER,
            "100,現金,2026-01-05,1,売上,500,1,1000,1000,",
            "100,現金,,,合計,,,,,",
        ]
        .join(
            "
",
        );

        assert_eq!(
            closing_balances(&csv).get("100"),
            Some(&1000),
            "空の行で上書きしない"
        );
    }

    /// 負の残高も読む。
    #[test]
    fn a_negative_balance_is_read() {
        let csv = [HEADER, "110,普通預金,2026-01-05,1,出金,100,2,900,-282754,"].join(
            "
",
        );

        assert_eq!(closing_balances(&csv).get("110"), Some(&-282_754));
    }

    /// 見出しだけなら空。
    #[test]
    fn a_header_only_csv_has_no_balances() {
        assert!(closing_balances(HEADER).is_empty());
    }
}
