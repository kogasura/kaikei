//! 印刷用 HTML の見た目を目で確かめるための例。
//!
//! ```sh
//! cargo run -p kaikei-report --example preview -- ./tmp
//! ```
//!
//! **見た目の設計は目で見ないと分からない。** 桁が揃っているか、罫線が細すぎ
//! ないか、印刷プレビューで見出しが各ページに繰り返されるか——テストが見られる
//! のは値の一致までである。土台は実際の帳簿に似せてある（桁数の違う金額・
//! 3行仕訳・長い摘要・赤伝・HTML の特殊文字）。

use kaikei_core::{
    AccountCode, AccountDef, AccountType, AccountingDate, ChartOfAccounts, Currency, EntryId,
    EntryNumber, FiscalYear, FixedClock, JournalEntry, JournalLine, Money, NewEntry, PeriodGuard,
    PeriodStatus, Side, TagSchema, TagSet, Timestamp,
};

struct AllOpen;
impl PeriodGuard for AllOpen {
    fn status(&self, _date: AccountingDate) -> PeriodStatus {
        PeriodStatus::Open
    }
}

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
        def("135", "売掛金", AccountType::Asset),
        def("330", "仮受消費税等", AccountType::Liability),
        def("500", "売上高", AccountType::Revenue),
        def("604", "通信費", AccountType::Expense),
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

fn entry(no: u32, date: (i32, u8, u8), description: &str, lines: Vec<JournalLine>) -> JournalEntry {
    JournalEntry::new(
        NewEntry {
            id: EntryId::new(u128::from(no)),
            entry_no: EntryNumber::new(no),
            entry_date: AccountingDate::new(date.0, date.1, date.2).unwrap(),
            description: description.to_string(),
            lines,
            document_refs: Vec::new(),
        },
        &FiscalYear::calendar_year(date.0),
        &chart(),
        &TagSchema::empty(),
        &AllOpen,
        &FixedClock(Timestamp::from_unix_nanos(1_700_000_000_000_000)),
    )
    .unwrap()
}

fn main() {
    let Some(out) = std::env::args().nth(1) else {
        eprintln!("使い方: cargo run -p kaikei-report --example preview -- <出力先ディレクトリ>");
        std::process::exit(2);
    };

    let original = entry(
        2,
        (2026, 5, 20),
        "B社 5月分 請求（金額誤り）",
        vec![
            line("135", Side::Debit, 550_000, None),
            line("500", Side::Credit, 500_000, None),
            line("330", Side::Credit, 50_000, Some("消費税10%")),
        ],
    );
    let reversal = original
        .reverse(
            EntryId::new(3),
            EntryNumber::new(3),
            AccountingDate::new(2026, 5, 21).unwrap(),
            "請求金額の桁誤り（550,000 ではなく 55,000）".to_string(),
            &FiscalYear::calendar_year(2026),
            &chart(),
            &TagSchema::empty(),
            &AllOpen,
            &FixedClock(Timestamp::from_unix_nanos(1_700_000_001_000_000)),
        )
        .unwrap();

    let entries = vec![
        entry(
            1,
            (2026, 4, 15),
            "A社 4月分 請求",
            vec![
                line("135", Side::Debit, 110_000, None),
                line("500", Side::Credit, 100_000, None),
                line("330", Side::Credit, 10_000, Some("消費税10%")),
            ],
        ),
        original,
        reversal,
        entry(
            4,
            (2026, 6, 1),
            "ドメイン更新料 <年額> & SSL",
            vec![
                line("604", Side::Debit, 4_309, Some("ムームードメイン")),
                line("100", Side::Credit, 4_309, None),
            ],
        ),
    ];

    let notes = vec![
        "集計対象で最も古い仕訳は 2026-04-15 で、集計期間の開始日（2026-01-01）から\
         離れています。期首残高の仕訳が帳簿に無い場合、貸借対照表には前期から繰り越した\
         残高が含まれません。"
            .to_string(),
    ];

    let html = kaikei_report::journal_book::to_html(
        &entries,
        &chart(),
        "2026-01-01 〜 2026-12-31",
        &notes,
    );
    let csv = kaikei_report::journal_book::to_csv(&entries, &chart());

    let html_path = format!("{out}/journal_book.html");
    let csv_path = format!("{out}/journal_book.csv");
    std::fs::write(&html_path, html).expect("HTML を書き出せること");
    std::fs::write(&csv_path, csv).expect("CSV を書き出せること");
    println!("{html_path}");
    println!("{csv_path}");
}
