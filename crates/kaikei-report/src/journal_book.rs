//! 仕訳日記帳（journal book）の出力。
//!
//! 入力は [`kaikei_core::JournalEntry`] の並び（`JournalRepo::list_entries_in_period`
//! が `(entry_date, entry_no)` 昇順で返したもの）。**並べ替えはここで行わない**——
//! 順序は帳簿の性質であってフォーマットの都合ではなく、2箇所で並べると
//! いつか食い違う。
//!
//! # 1行1明細にする
//!
//! 仕訳1件を1行に畳むと、明細が3本以上ある仕訳（消費税額の行を含む仕訳、
//! 家事按分の3行仕訳）を列に収められない。**1行1明細**にして、同じ仕訳の
//! 行には同じ仕訳番号・取引日・摘要を繰り返す。表計算で開いたときに
//! 仕訳番号でグループ化でき、取り込み先でも扱いやすい。
//!
//! # 取り消された仕訳も出す
//!
//! 赤伝で訂正された仕訳も、赤伝そのものも隠さない（`DECISIONS.md` D-088）。
//! **隠すと帳簿の合計が合わなくなる**（両者は相殺されて初めて正しい）。
//! どちらであるかは `reverses` / `reverse_reason` の列で読める。

use crate::csv::CsvBuilder;
use crate::html::PrintableTable;
use kaikei_app::amount::money_to_plain_string;
use kaikei_app::wire::side_code;
use kaikei_core::{ChartOfAccounts, JournalEntry};

/// 表の見出し。CSV と HTML で同じものを使う。
///
/// **2箇所に書かない。** 列を片方にだけ足すと、同じ帳簿を CSV で見た人と
/// 印刷して見た人が違うものを見ることになる。
const HEADERS: &[&str] = &[
    "取引日",
    "仕訳番号",
    "行番号",
    "摘要",
    "科目コード",
    "勘定科目",
    "貸借",
    "金額",
    "通貨",
    "備考",
    "訂正元",
    "訂正理由",
];

/// 右寄せにする列（金額）。`HEADERS` の添字。
const NUMERIC_COLUMNS: &[usize] = &[7];

/// 仕訳を1行1明細の表に開く。
///
/// CSV と HTML はこの結果を共有する。**フォーマットごとに組み立て直さない**
/// ——同じ帳簿が形式によって違う中身になるのを、構造で防ぐ。
fn to_rows(entries: &[JournalEntry], chart: &ChartOfAccounts) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    for entry in entries {
        // 訂正元は「この仕訳が訂正している相手」。仕訳IDではなく**仕訳番号**を
        // 出したいが、集約が持っているのは相手の EntryId だけである
        // （番号を引くには相手を読み直す必要がある）。ここは I/O を持たない
        // 層なので、ID をそのまま出す。
        let reverses = entry
            .reverses()
            .map(|id| id.as_u128().to_string())
            .unwrap_or_default();
        let reverse_reason = entry.reverse_reason().unwrap_or_default().to_string();

        for (index, line) in entry.lines().iter().enumerate() {
            rows.push(vec![
                entry.entry_date().to_iso_string(),
                entry.entry_no().as_u32().to_string(),
                (index + 1).to_string(),
                entry.description().to_string(),
                line.account().as_str().to_string(),
                account_name(chart, line.account()),
                side_code(line.side()).to_string(),
                money_to_plain_string(line.amount()),
                line.amount().currency().code().to_string(),
                line.memo().unwrap_or_default().to_string(),
                reverses.clone(),
                reverse_reason.clone(),
            ]);
        }
    }
    rows
}

/// 印刷用 HTML。
///
/// `period` は表題の下に出す期間（例: 「2026-01-01 〜 2026-12-31」）。
/// `notes` は表の下に出す注記。
pub fn to_html(
    entries: &[JournalEntry],
    chart: &ChartOfAccounts,
    period: &str,
    notes: &[String],
) -> String {
    PrintableTable {
        title: "仕訳日記帳",
        subtitle: period,
        headers: HEADERS,
        rows: &to_rows(entries, chart),
        notes,
        numeric_columns: NUMERIC_COLUMNS,
        // 仕訳日記帳に合計行は置かない。**貸借の合計は試算表が示すもの**で、
        // 期間内の明細を並べるこの帳簿で足し上げても意味が無い。
        footer_rows: &[],
        // 列が11個あるので A4 縦では右端が切れる。
        landscape: true,
    }
    .render()
}

/// 科目コードから科目名を引く。
///
/// **帳簿に無いコードでも空文字にせず、コードをそのまま返す。** 仕訳は
/// 記帳時に勘定科目表で検証されているので通常は引けるが、科目を削除した
/// 帳簿を後から出力する場合などに引けないことがありうる。そのとき名前欄が
/// 空だと「名前の無い科目」に見えてしまう——**引けなかったことが分かる形**
/// にしておく。
fn account_name(chart: &ChartOfAccounts, code: &kaikei_core::AccountCode) -> String {
    chart
        .get(code)
        .map(|def| def.name.clone())
        .unwrap_or_else(|| format!("（{}：勘定科目表にありません）", code.as_str()))
}

/// 仕訳日記帳の CSV。
///
/// 列: 取引日 / 仕訳番号 / 明細行番号 / 摘要 / 勘定科目コード / 貸借 /
/// 金額 / 通貨 / 明細メモ / 訂正元 / 訂正理由
///
/// 見出しは日本語にする。この CSV を最初に開くのは表計算ソフトの人間であり、
/// 機械可読な語彙（`side` の `debit`/`credit` 等）は値の側で保つ。
///
/// # 記帳日時（`recorded_at`）は**まだ出していない**
///
/// 電子帳簿保存法の「追加入力の履歴の確保」（施行規則第5条第5項第1号イ(2)）は
/// 入力日が確認できることを求めており、帳簿の出力に記帳日時があるのが本来である。
/// 出していないのは、**どう文字列化するかが決まっていない**ためである:
///
/// - `kaikei_core::Timestamp` は `chrono` を持たない（`CLAUDE.md` §1）ので、
///   この層に変換の手段が無い
/// - `recorded_at` は UTC で保存されている（`CLAUDE.md` §7）。**帳簿を読む人は
///   日本時間で見たい**が、どちらで出すかは表示の設計判断であり、
///   MCP の応答にも前例が無い（`get_entry` も返していない）
///
/// `docs/10-report.md` §7 の判断待ちに含めてある。
pub fn to_csv(entries: &[JournalEntry], chart: &ChartOfAccounts) -> String {
    let mut csv = CsvBuilder::new();
    csv.push_row(HEADERS);
    for row in to_rows(entries, chart) {
        csv.push_row(row);
    }
    csv.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_core::{
        AccountCode, AccountDef, AccountType, AccountingDate, ChartOfAccounts, Currency, EntryId,
        EntryNumber, FiscalYear, FixedClock, JournalLine, Money, NewEntry, PeriodGuard,
        PeriodStatus, Side, TagSchema, TagSet, Timestamp,
    };

    struct AllOpen;
    impl PeriodGuard for AllOpen {
        fn status(&self, _date: AccountingDate) -> PeriodStatus {
            PeriodStatus::Open
        }
    }

    fn chart() -> ChartOfAccounts {
        ChartOfAccounts::new(vec![
            AccountDef {
                code: AccountCode::parse("100").unwrap(),
                name: "現金".to_string(),
                account_type: AccountType::Asset,
                parent: None,
                postable: true,
            },
            AccountDef {
                code: AccountCode::parse("500").unwrap(),
                name: "売上高".to_string(),
                account_type: AccountType::Revenue,
                parent: None,
                postable: true,
            },
        ])
        .unwrap()
    }

    fn line(account: &str, side: Side, amount: i128, memo: Option<&str>) -> JournalLine {
        JournalLine::new(
            AccountCode::parse(account).unwrap(),
            side,
            Money::from_minor(amount, Currency::JPY),
            TagSet::new(),
            memo.map(str::to_string),
        )
        .unwrap()
    }

    fn entry(no: u32, description: &str, lines: Vec<JournalLine>) -> JournalEntry {
        JournalEntry::new(
            NewEntry {
                id: EntryId::new(u128::from(no)),
                entry_no: EntryNumber::new(no),
                entry_date: AccountingDate::new(2026, 4, 15).unwrap(),
                description: description.to_string(),
                lines,
                document_refs: Vec::new(),
            },
            &FiscalYear::calendar_year(2026),
            &chart(),
            &TagSchema::empty(),
            &AllOpen,
            &FixedClock(Timestamp::from_unix_nanos(1_700_000_000_000_000)),
        )
        .unwrap()
    }

    fn data_rows(csv: &str) -> Vec<String> {
        csv.trim_start_matches(crate::csv::UTF8_BOM)
            .split("\r\n")
            .filter(|row| !row.is_empty())
            .skip(1) // ヘッダ
            .map(str::to_string)
            .collect()
    }

    // 1行1明細。明細が3本なら3行出る。
    #[test]
    fn each_line_becomes_its_own_row() {
        let entries = vec![entry(
            1,
            "売上",
            vec![
                line("100", Side::Debit, 110_000, None),
                line("500", Side::Credit, 100_000, None),
                line("500", Side::Credit, 10_000, Some("消費税")),
            ],
        )];

        let rows = data_rows(&to_csv(&entries, &chart()));

        assert_eq!(rows.len(), 3);
        // 同じ仕訳の行には同じ取引日・仕訳番号・摘要が繰り返される。
        for row in &rows {
            assert!(row.starts_with("2026-04-15,1,"), "{row}");
            assert!(row.contains(",売上,"), "{row}");
        }
        // 行番号は 1 から振り直される。
        assert!(rows[0].starts_with("2026-04-15,1,1,"));
        assert!(rows[2].starts_with("2026-04-15,1,3,"));
    }

    // 金額は桁区切り無し、貸借は機械可読な語彙。
    #[test]
    fn amounts_have_no_separators_and_sides_use_the_wire_vocabulary() {
        let entries = vec![entry(
            1,
            "売上",
            vec![
                line("100", Side::Debit, 1_234_567, None),
                line("500", Side::Credit, 1_234_567, None),
            ],
        )];

        let csv = to_csv(&entries, &chart());

        assert!(csv.contains(",1234567,"), "桁区切りが入っている: {csv}");
        assert!(!csv.contains("1,234,567"));
        assert!(csv.contains(",debit,"));
        assert!(csv.contains(",credit,"));
    }

    // 摘要のカンマで列がずれない（RFC 4180 のエスケープが効く）。
    #[test]
    fn a_comma_in_the_description_does_not_shift_the_columns() {
        let entries = vec![entry(
            1,
            "A社, B社 合算",
            vec![
                line("100", Side::Debit, 1_000, None),
                line("500", Side::Credit, 1_000, None),
            ],
        )];

        let csv = to_csv(&entries, &chart());

        assert!(csv.contains("\"A社, B社 合算\""), "{csv}");
    }

    // ヘッダだけの CSV も成立する（仕訳0件は正常）。
    #[test]
    fn an_empty_book_still_has_a_header() {
        let csv = to_csv(&[], &chart());

        assert!(csv.starts_with(crate::csv::UTF8_BOM));
        assert!(csv.contains("取引日,仕訳番号,行番号"));
        assert!(data_rows(&csv).is_empty());
    }

    // ★CSV と HTML が同じ中身を出す★
    //
    // 両者は `to_rows` を共有しているが、**共有していることをテストで縛る**。
    // 片方だけ列を足す・値を変える変更が入ったら、ここが落ちる。
    // 同じ帳簿が形式によって違う中身になるのが、この検査が防ぎたい事故である。
    #[test]
    fn the_csv_and_the_html_carry_the_same_values() {
        let entries = vec![entry(
            1,
            "A社 <重要> 請求",
            vec![
                line("100", Side::Debit, 110_000, Some("備考あり")),
                line("500", Side::Credit, 110_000, None),
            ],
        )];

        let csv = to_csv(&entries, &chart());
        let html = to_html(&entries, &chart(), "2026-01-01 〜 2026-12-31", &[]);

        // 見出しは同じ集合。
        for header in HEADERS {
            assert!(csv.contains(header), "CSV に見出しが無い: {header}");
            assert!(html.contains(header), "HTML に見出しが無い: {header}");
        }

        // 値も同じ。HTML 側はエスケープされるので、素の値で比べられるものを見る。
        for value in ["2026-04-15", "110000", "debit", "credit", "JPY", "備考あり"] {
            assert!(csv.contains(value), "CSV に値が無い: {value}");
            assert!(html.contains(value), "HTML に値が無い: {value}");
        }

        // 行数が一致する（明細2本なので2行）。
        let csv_rows = data_rows(&csv).len();
        let html_rows = html.matches("<tr>").count() - 1; // ヘッダ行を除く
        assert_eq!(csv_rows, html_rows, "CSV と HTML で行数が違う");
    }

    // HTML では摘要がエスケープされる（CSV は生のまま）。
    #[test]
    fn the_html_escapes_what_the_csv_leaves_raw() {
        let entries = vec![entry(
            1,
            "A社 <重要>",
            vec![
                line("100", Side::Debit, 1_000, None),
                line("500", Side::Credit, 1_000, None),
            ],
        )];

        assert!(to_csv(&entries, &chart()).contains("A社 <重要>"));
        assert!(to_html(&entries, &chart(), "", &[]).contains("A社 &lt;重要&gt;"));
    }

    // 印刷用 HTML には期間と注記が載る。
    #[test]
    fn the_html_carries_the_period_and_notes() {
        let notes = vec!["期首残高の仕訳が帳簿にありません".to_string()];
        let html = to_html(&[], &chart(), "2026-01-01 〜 2026-12-31", &notes);

        assert!(html.contains("<h1>仕訳日記帳</h1>"));
        assert!(html.contains("2026-01-01 〜 2026-12-31"));
        assert!(html.contains("期首残高の仕訳が帳簿にありません"));
        assert!(html.contains("この期間に該当する記録はありません"));
    }

    // 赤伝は訂正元と理由が読める形で出る（隠さない。D-088）。
    #[test]
    fn a_reversal_carries_its_origin_and_reason() {
        let original = entry(
            1,
            "誤記帳",
            vec![
                line("100", Side::Debit, 5_000, None),
                line("500", Side::Credit, 5_000, None),
            ],
        );
        let reversal = original
            .reverse(
                EntryId::new(99),
                EntryNumber::new(2),
                AccountingDate::new(2026, 4, 20).unwrap(),
                "金額の誤り".to_string(),
                &FiscalYear::calendar_year(2026),
                &chart(),
                &TagSchema::empty(),
                &AllOpen,
                &FixedClock(Timestamp::from_unix_nanos(1_700_000_001_000_000)),
            )
            .unwrap();

        let rows = data_rows(&to_csv(&[original, reversal], &chart()));

        // 原仕訳も残る（隠さない）。
        assert_eq!(rows.len(), 4);
        // 赤伝の行に訂正元と理由が入る。
        let red = &rows[2];
        assert!(red.contains("金額の誤り"), "{red}");
        assert!(red.contains(",1,"), "訂正元の識別子が入る: {red}");
    }
}
