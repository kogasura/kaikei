//! `cargo run --example hello_kaikei` で動く動作確認用の最小デモ。
//!
//! `ROADMAP.md`「動作確認の例」に対応する。ただし科目コードは
//! `crates/kaikei-jp-data/chart/sole_proprietor.yaml` の実際の値に合わせている。
//! 同 yaml では 609 が「消耗品費」、610 は「減価償却費」であり、
//! `ROADMAP.md` の例に書かれている「610 消耗品費」は取り違いなので、
//! この example では正しい 609 を使う。
//!
//! 貸借一致は `JournalEntry::new` の時点で構造的に検証されるため、貸借不一致の
//! 仕訳はそもそもプログラム上に存在できない。それを示すため、末尾でわざと
//! 貸借を崩した仕訳を登録し、返ってくるエラーメッセージを表示する。

use kaikei_core::{
    AccountCode, AccountDef, AccountType, AccountingDate, ChartOfAccounts, Currency, EntryId,
    EntryNumber, FiscalYear, FixedClock, JournalEntry, JournalLine, Money, NewEntry, PeriodGuard,
    PeriodStatus, Side, TagSchema, TagSet, Timestamp, TrialBalance,
};

/// 常に `Open` を返す期間ガード。
///
/// 締め状態の実データは store 層が持つ（`period.rs` の `PeriodGuard` の doc コメント
/// 参照）。この example にはそもそも store 層が無いため、「常に記帳可能」という
/// 最小の実装をここで用意する。
struct AlwaysOpen;

impl PeriodGuard for AlwaysOpen {
    fn status(&self, _date: AccountingDate) -> PeriodStatus {
        PeriodStatus::Open
    }
}

/// 個人事業主標準の勘定科目表のうち、このデモで使う3科目だけを抜き出したもの。
///
/// コード・名称は `crates/kaikei-jp-data/chart/sole_proprietor.yaml` と一致させている。
fn chart() -> ChartOfAccounts {
    let account = |code: &str, name: &str, account_type: AccountType| AccountDef {
        code: AccountCode::parse(code).expect("科目コードは常に有効"),
        name: name.to_string(),
        account_type,
        parent: None,
        postable: true,
    };
    ChartOfAccounts::new(vec![
        account("100", "現金", AccountType::Asset),
        account("500", "売上高", AccountType::Revenue),
        account("609", "消耗品費", AccountType::Expense),
    ])
    .expect("勘定科目表の構築に失敗しました")
}

/// ASCII文字を幅1、それ以外（日本語等）を幅2として数える簡易的な表示幅。
///
/// 試算表の列を揃えるためだけに使う。桁数計算用の外部crateは
/// `kaikei-core` の依存に追加できない（`Cargo.toml` 参照）ため自前で計算する。
fn display_width(s: &str) -> usize {
    s.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum()
}

/// `s` の表示幅が `width` に届くまで右側に半角スペースを足す。
fn pad_right(s: &str, width: usize) -> String {
    let mut out = s.to_string();
    out.push_str(&" ".repeat(width.saturating_sub(display_width(s))));
    out
}

/// `s` の表示幅が `width` に届くまで左側に半角スペースを足す。
fn pad_left(s: &str, width: usize) -> String {
    let mut out = " ".repeat(width.saturating_sub(display_width(s)));
    out.push_str(s);
    out
}

fn main() {
    let chart = chart();
    let schema = TagSchema::empty();
    let guard = AlwaysOpen;
    let clock = FixedClock(Timestamp::from_unix_nanos(1_774_000_000_000_000_000));
    let fy = FiscalYear::calendar_year(2026);

    let account = |code: &str| AccountCode::parse(code).expect("科目コードは常に有効");
    let jpy = |minor: i128| Money::from_minor(minor, Currency::JPY);

    let mut entries: Vec<JournalEntry> = Vec::new();

    // 仕訳1: 売上の入金（現金 100,000 / 売上高 100,000）
    let sales_amount = jpy(100_000);
    let entry1 = JournalEntry::new(
        NewEntry {
            id: EntryId::new(1),
            entry_no: EntryNumber::new(1),
            entry_date: AccountingDate::new(2026, 4, 1).expect("2026-04-01は有効な日付"),
            description: "売上".to_string(),
            lines: vec![
                JournalLine::new(
                    account("100"),
                    Side::Debit,
                    sales_amount,
                    TagSet::new(),
                    None,
                )
                .expect("借方明細の構築に失敗しました"),
                JournalLine::new(
                    account("500"),
                    Side::Credit,
                    sales_amount,
                    TagSet::new(),
                    None,
                )
                .expect("貸方明細の構築に失敗しました"),
            ],
            document_refs: Vec::new(),
        },
        &fy,
        &chart,
        &schema,
        &guard,
        &clock,
    )
    .expect("仕訳1の登録に失敗しました");
    println!(
        "仕訳を登録: 現金 {} / 売上高 {}",
        sales_amount.to_display_string(),
        sales_amount.to_display_string()
    );
    entries.push(entry1);

    // 仕訳2: 消耗品の購入（消耗品費 1,980 / 現金 1,980）
    let supplies_amount = jpy(1_980);
    let entry2 = JournalEntry::new(
        NewEntry {
            id: EntryId::new(2),
            entry_no: EntryNumber::new(2),
            entry_date: AccountingDate::new(2026, 4, 2).expect("2026-04-02は有効な日付"),
            description: "消耗品購入".to_string(),
            lines: vec![
                JournalLine::new(
                    account("609"),
                    Side::Debit,
                    supplies_amount,
                    TagSet::new(),
                    None,
                )
                .expect("借方明細の構築に失敗しました"),
                JournalLine::new(
                    account("100"),
                    Side::Credit,
                    supplies_amount,
                    TagSet::new(),
                    None,
                )
                .expect("貸方明細の構築に失敗しました"),
            ],
            document_refs: Vec::new(),
        },
        &fy,
        &chart,
        &schema,
        &guard,
        &clock,
    )
    .expect("仕訳2の登録に失敗しました");
    println!(
        "仕訳を登録: 消耗品費 {} / 現金 {}",
        supplies_amount.to_display_string(),
        supplies_amount.to_display_string()
    );
    entries.push(entry2);

    println!();
    println!("試算表:");
    let trial_balance = TrialBalance::from_entries(entries.iter(), &chart, &schema, &[])
        .expect("試算表の構築に失敗しました");
    for row in trial_balance.rows() {
        let def = chart
            .get(&row.account)
            .expect("試算表に現れる科目は勘定科目表に存在する");
        println!(
            "  {} {}{}",
            row.account.as_str(),
            pad_right(&def.name, 10),
            pad_left(&row.balance.to_display_string(), 10),
        );
    }
    let (debit_total, credit_total) = trial_balance.totals();
    let mark = if trial_balance.is_balanced() {
        "✓"
    } else {
        "✗"
    };
    println!(
        "  借方合計: {} / 貸方合計: {}  {mark}",
        debit_total.to_display_string(),
        credit_total.to_display_string(),
    );

    // ---- わざと貸借不一致の仕訳を作ってみる ----
    //
    // `JournalEntry::new` は貸借一致を構造的に検証するため、不一致の仕訳は
    // そもそも生成できない。ここでは実際にエラーになる様子とエラーメッセージの
    // 品質（`CLAUDE.md` §11）を示す。
    println!();
    println!("わざと貸借不一致の仕訳を登録してみる:");
    let result = JournalEntry::new(
        NewEntry {
            id: EntryId::new(3),
            entry_no: EntryNumber::new(3),
            entry_date: AccountingDate::new(2026, 4, 3).expect("2026-04-03は有効な日付"),
            description: "貸借が合わない仕訳（デモ用）".to_string(),
            lines: vec![
                JournalLine::new(
                    account("100"),
                    Side::Debit,
                    jpy(110_000),
                    TagSet::new(),
                    None,
                )
                .expect("借方明細の構築に失敗しました"),
                JournalLine::new(
                    account("500"),
                    Side::Credit,
                    jpy(100_000),
                    TagSet::new(),
                    None,
                )
                .expect("貸方明細の構築に失敗しました"),
            ],
            document_refs: Vec::new(),
        },
        &fy,
        &chart,
        &schema,
        &guard,
        &clock,
    );
    match result {
        Ok(_) => println!("  （想定外）貸借不一致の仕訳が登録できてしまいました"),
        Err(err) => println!("  エラー: {err}"),
    }
}
