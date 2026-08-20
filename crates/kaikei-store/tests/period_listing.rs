//! [`kaikei_app::ports::JournalRepo::list_entries_in_period`] の検証。
//!
//! このポートは Phase 5（決算書・帳簿出力）のクリティカルパスにある。
//! [`kaikei_core::TrialBalance::from_entries`] が `&JournalEntry` のイテレータを
//! 要求するため、決算振替（`ClosingPolicy`）も財務諸表（`StatementPolicy`）も
//! この経路を通ってしか作れない。**ここが静かに壊れると決算書の金額が狂う。**
//!
//! # 土台を実際の帳簿に似せる
//!
//! `PROGRESS.md` Phase 3 の教訓2（read model の SQL を壊しても緑のまま通った
//! 11件）の原因は、一貫して「土台が実際の帳簿に似ていなかった」ことだった。
//! （11件はテストの件数であって帳簿の規模ではない。ci-allow: real-ledger-mention）
//! ここでは実装が壊れたときに落ちるよう、土台に次を必ず含める:
//!
//! - **同じ取引日に複数の仕訳**（並びが `entry_no` で決まることを見るため。
//!   1日1件の土台では `ORDER BY` から `entry_no` を落としても気づけない）
//! - **仕訳ごとに異なる明細本数**（2本・3本・4本。明細の振り分けが1件でも
//!   ずれれば本数が合わなくなる。全部2本の土台では取り違えても気づけない）
//! - **明細ごとに異なる金額**（同額だと入れ替わっても一致してしまう）
//! - **期間の両端ちょうどの仕訳**（閉区間であることを見るため）
//! - **期間外の仕訳**（前後1日ずつ。範囲の絞り込みが効いていることを見るため）
//! - **赤伝**（取り消された仕訳を隠さないこと。`DECISIONS.md` D-088）
//!
//! 明細をまとめて1回のクエリで取り、`entry_id` で振り分ける実装なので、
//! **振り分けの誤りがこのテストの主対象**である。

#![cfg(feature = "pg-tests")]

mod common;

use common::AllOpen;
use kaikei_app::ports::JournalRepo;
use kaikei_app::tx::with_tx;
use kaikei_core::{
    AccountCode, AccountDef, AccountType, AccountingDate, ChartOfAccounts, Currency, EntryId,
    EntryNumber, FiscalYear, FixedClock, JournalEntry, JournalLine, Money, NewEntry, Side,
    TagSchema, TagSet, Timestamp,
};
use kaikei_store::pool::PgStore;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

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
        AccountDef {
            code: AccountCode::parse("600").unwrap(),
            name: "仕入高".to_string(),
            account_type: AccountType::Expense,
            parent: None,
            postable: true,
        },
    ])
    .unwrap()
}

fn schema() -> TagSchema {
    TagSchema::new(Vec::new())
}

fn line(account: &str, side: Side, amount_minor: i128) -> JournalLine {
    JournalLine::new(
        AccountCode::parse(account).unwrap(),
        side,
        Money::from_minor(amount_minor, Currency::JPY),
        TagSet::new(),
        None,
    )
    .unwrap()
}

fn date(year: i32, month: u8, day: u8) -> AccountingDate {
    AccountingDate::new(year, month, day).unwrap()
}

/// 貸借が一致する `line_count` 本の明細を作る。金額はすべて異なる値にする
/// （同額だと明細が入れ替わっても一致してしまい、振り分けの誤りを検出できない）。
fn lines_with_distinct_amounts(line_count: usize, base: i128) -> Vec<JournalLine> {
    assert!(line_count >= 2, "貸借を作るには最低2本要る");
    // 借方を line_count-1 本に分け、貸方1本でまとめて相殺する。
    let debits: Vec<i128> = (0..line_count - 1)
        .map(|i| base + (i as i128 + 1) * 7) // 7 の倍数でずらして重複を避ける
        .collect();
    let total: i128 = debits.iter().sum();
    let mut lines: Vec<JournalLine> = debits
        .into_iter()
        .map(|amount| line("600", Side::Debit, amount))
        .collect();
    lines.push(line("100", Side::Credit, total));
    lines
}

#[allow(clippy::too_many_arguments)]
fn build_entry(
    id: u128,
    entry_no: u32,
    entry_date: AccountingDate,
    description: &str,
    lines: Vec<JournalLine>,
) -> JournalEntry {
    let fy = FiscalYear::calendar_year(entry_date.year());
    let clock = FixedClock(Timestamp::from_unix_nanos(1_700_000_000_000_000));
    JournalEntry::new(
        NewEntry {
            id: EntryId::new(id),
            entry_no: EntryNumber::new(entry_no),
            entry_date,
            description: description.to_string(),
            lines,
            document_refs: Vec::new(),
        },
        &fy,
        &chart(),
        &schema(),
        &AllOpen,
        &clock,
    )
    .unwrap()
}

/// 土台。実際の帳簿に似せた8件を返す（期間外2件を含む）。
///
/// | id | 日付 | 明細本数 | 位置づけ |
/// |---|---|---|---|
/// | 1 | 2026-05-31 | 2 | 期間の**前日**（返ってはいけない） |
/// | 2 | 2026-06-01 | 3 | 期間の**開始日ちょうど** |
/// | 3 | 2026-06-15 | 2 | 同じ日の1件目（`entry_no` 小） |
/// | 4 | 2026-06-15 | 4 | 同じ日の2件目（`entry_no` 大） |
/// | 5 | 2026-06-20 | 2 | 赤伝の対象 |
/// | 6 | 2026-06-21 | 2 | 5 の赤伝 |
/// | 7 | 2026-06-30 | 3 | 期間の**終了日ちょうど** |
/// | 8 | 2026-07-01 | 2 | 期間の**翌日**（返ってはいけない） |
fn seed() -> Vec<JournalEntry> {
    let original = build_entry(
        5,
        5,
        date(2026, 6, 20),
        "赤伝の対象",
        lines_with_distinct_amounts(2, 5_000),
    );
    let clock = FixedClock(Timestamp::from_unix_nanos(1_700_000_001_000_000));
    let reversal = original
        .reverse(
            EntryId::new(6),
            EntryNumber::new(6),
            date(2026, 6, 21),
            "取消のテスト".to_string(),
            &FiscalYear::calendar_year(2026),
            &chart(),
            &schema(),
            &AllOpen,
            &clock,
        )
        .unwrap();

    vec![
        build_entry(
            1,
            1,
            date(2026, 5, 31),
            "期間の前日",
            lines_with_distinct_amounts(2, 1_000),
        ),
        build_entry(
            2,
            2,
            date(2026, 6, 1),
            "開始日ちょうど",
            lines_with_distinct_amounts(3, 2_000),
        ),
        build_entry(
            3,
            3,
            date(2026, 6, 15),
            "同日の1件目",
            lines_with_distinct_amounts(2, 3_000),
        ),
        build_entry(
            4,
            4,
            date(2026, 6, 15),
            "同日の2件目",
            lines_with_distinct_amounts(4, 4_000),
        ),
        original,
        reversal,
        build_entry(
            7,
            7,
            date(2026, 6, 30),
            "終了日ちょうど",
            lines_with_distinct_amounts(3, 7_000),
        ),
        build_entry(
            8,
            8,
            date(2026, 7, 1),
            "期間の翌日",
            lines_with_distinct_amounts(2, 8_000),
        ),
    ]
}

/// 土台を DB に入れる順序。**同じ取引日のグループ内を逆順にする。**
///
/// 挿入順と `ORDER BY` の順が一致していると、SQL の `ORDER BY` から
/// `entry_no` を落としても PostgreSQL が物理順（＝挿入順）でそのまま返し、
/// **検査が緑のまま通ってしまう**（実際にこの変異を入れて確認した）。
/// `PROGRESS.md` Phase 3 の教訓2 で PR-H が踏んだのと同じ形で、そちらは
/// 「`line_no` は正しいまま行を逆順に INSERT する」ことで閉じている。
///
/// 全体を逆順にはできない。`reverses` に外部キー制約があり
/// （`0003_journal.sql`）、赤伝を原仕訳より先に INSERT できないためである。
/// 日付をまたぐ並び（`entry_date` が先に効くこと）は
/// [`ordering_across_fiscal_years_is_by_date_first`] が別途、
/// 日付の逆順で INSERT して確かめる。
fn insert_order(entries: &[JournalEntry]) -> Vec<JournalEntry> {
    let mut result: Vec<JournalEntry> = Vec::with_capacity(entries.len());
    let mut group: Vec<JournalEntry> = Vec::new();
    for entry in entries {
        if group
            .last()
            .is_some_and(|last| last.entry_date() != entry.entry_date())
        {
            group.reverse();
            result.append(&mut group);
        }
        group.push(entry.clone());
    }
    group.reverse();
    result.append(&mut group);
    result
}

async fn store_with_seed(pool_opts: PgPoolOptions, conn_opts: PgConnectOptions) -> PgStore {
    let roles = common::roles(pool_opts, conn_opts).await;
    let store = PgStore::new(roles.app);
    let entries = insert_order(&seed());
    with_tx(&store, |tx| {
        let entries = entries.clone();
        Box::pin(async move {
            for entry in &entries {
                tx.insert_entry(entry).await?;
            }
            Ok::<(), kaikei_app::error::AppError>(())
        })
    })
    .await
    .unwrap();
    store
}

async fn list(store: &PgStore, from: AccountingDate, to: AccountingDate) -> Vec<JournalEntry> {
    with_tx(store, |tx| {
        Box::pin(async move { Ok(tx.list_entries_in_period(from, to).await?) })
    })
    .await
    .unwrap()
}

/// 期間の両端を含み、期間外は含まない。並びは `(entry_date, entry_no)`。
#[sqlx::test]
async fn returns_entries_within_the_closed_period_in_date_then_number_order(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let store = store_with_seed(pool_opts, conn_opts).await;

    let found = list(&store, date(2026, 6, 1), date(2026, 6, 30)).await;

    let ids: Vec<u128> = found.iter().map(|e| e.id().as_u128()).collect();
    assert_eq!(
        ids,
        vec![2, 3, 4, 5, 6, 7],
        "開始日・終了日ちょうどを含み、前日(1)と翌日(8)を含まず、\
         同じ日(6/15)は entry_no 順に並ぶこと"
    );
}

/// 明細が仕訳ごとに正しく振り分けられる。
///
/// 明細は1回のクエリでまとめて取り、`entry_id` で振り分ける実装なので、
/// **ここが本命**。本数と金額の両方を見る（本数だけでは同数の仕訳同士の
/// 取り違えを検出できない）。
#[sqlx::test]
async fn each_entry_gets_exactly_its_own_lines(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let store = store_with_seed(pool_opts, conn_opts).await;

    let found = list(&store, date(2026, 6, 1), date(2026, 6, 30)).await;
    let expected: Vec<JournalEntry> = seed()
        .into_iter()
        .filter(|e| e.entry_date() >= date(2026, 6, 1) && e.entry_date() <= date(2026, 6, 30))
        .collect();

    assert_eq!(found.len(), expected.len());
    for (found_entry, expected_entry) in found.iter().zip(expected.iter()) {
        assert_eq!(
            found_entry.id(),
            expected_entry.id(),
            "並びが土台と一致していること"
        );
        assert_eq!(
            found_entry.lines().len(),
            expected_entry.lines().len(),
            "仕訳 {} の明細本数",
            found_entry.id().as_u128()
        );
        for (found_line, expected_line) in found_entry.lines().iter().zip(expected_entry.lines()) {
            assert_eq!(
                found_line.amount(),
                expected_line.amount(),
                "仕訳 {} の明細金額（振り分けか並びが誤っている）",
                found_entry.id().as_u128()
            );
            assert_eq!(found_line.account(), expected_line.account());
            assert_eq!(found_line.side(), expected_line.side());
        }
    }
}

/// 取り消された仕訳も、赤伝そのものも隠さない（`DECISIONS.md` D-088）。
///
/// 試算表は両者を含めて集計することで相殺される。隠すと決算書が狂う。
#[sqlx::test]
async fn reversed_entries_and_their_reversals_are_both_returned(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let store = store_with_seed(pool_opts, conn_opts).await;

    let found = list(&store, date(2026, 6, 20), date(2026, 6, 21)).await;

    assert_eq!(found.len(), 2, "原仕訳と赤伝の両方が返ること");
    assert_eq!(found[0].id().as_u128(), 5);
    assert_eq!(found[1].id().as_u128(), 6);
    assert_eq!(
        found[1].reverses(),
        Some(EntryId::new(5)),
        "赤伝が原仕訳を指していること"
    );
    assert_eq!(found[1].reverse_reason(), Some("取消のテスト"));
}

/// 1日だけを指定しても、その日の仕訳が返る（閉区間なので from == to が有効）。
#[sqlx::test]
async fn a_single_day_period_is_valid(pool_opts: PgPoolOptions, conn_opts: PgConnectOptions) {
    let store = store_with_seed(pool_opts, conn_opts).await;

    let found = list(&store, date(2026, 6, 15), date(2026, 6, 15)).await;

    let ids: Vec<u128> = found.iter().map(|e| e.id().as_u128()).collect();
    assert_eq!(ids, vec![3, 4]);
}

/// 仕訳が1件も無い期間は空の成功（エラーではない）。
#[sqlx::test]
async fn an_empty_period_returns_an_empty_vec(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let store = store_with_seed(pool_opts, conn_opts).await;

    let found = list(&store, date(2026, 8, 1), date(2026, 8, 31)).await;

    assert!(found.is_empty(), "該当なしは空の成功: {} 件", found.len());
}

/// 年度をまたぐ期間では `entry_date` が先に効く。
///
/// `entry_no` は会計年度ごとの連番なので、`ORDER BY entry_no, entry_date` の
/// ように順序を入れ替えると年度の切り替わりで並びが崩れる。
#[sqlx::test]
async fn ordering_across_fiscal_years_is_by_date_first(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    let store = PgStore::new(roles.app);

    // 2026年の末尾に大きい番号、2027年の先頭に小さい番号を置く。
    // 日付順なら 2026-12-31(no=99) → 2027-01-01(no=1)。
    // 番号順に並べると逆転する。
    let entries = [
        build_entry(
            101,
            99,
            date(2026, 12, 31),
            "2026年の最後",
            lines_with_distinct_amounts(2, 9_000),
        ),
        build_entry(
            102,
            1,
            date(2027, 1, 1),
            "2027年の最初",
            lines_with_distinct_amounts(3, 10_000),
        ),
    ];
    // **日付の逆順で INSERT する。** 挿入順が日付順だと、`ORDER BY` から
    // `entry_date` が消えても物理順で正しく見えてしまう（上の `insert_order`
    // の doc を参照）。ここは赤伝が無いので全体を逆順にできる。
    with_tx(&store, |tx| {
        let entries: Vec<JournalEntry> = entries.iter().rev().cloned().collect();
        Box::pin(async move {
            for entry in &entries {
                tx.insert_entry(entry).await?;
            }
            Ok::<(), kaikei_app::error::AppError>(())
        })
    })
    .await
    .unwrap();

    let found = list(&store, date(2026, 12, 1), date(2027, 1, 31)).await;

    let ids: Vec<u128> = found.iter().map(|e| e.id().as_u128()).collect();
    assert_eq!(
        ids,
        vec![101, 102],
        "日付が先に効くこと（番号順なら 102 が先に来てしまう）"
    );
    // 明細本数も年度をまたいで正しく振り分けられていること。
    assert_eq!(found[0].lines().len(), 2);
    assert_eq!(found[1].lines().len(), 3);
}
