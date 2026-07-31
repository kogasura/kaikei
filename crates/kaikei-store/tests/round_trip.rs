//! save→find の往復（round trip）で全フィールドが一致することを検証する。
//!
//! `kaikei_store::pool::PgStore` と `kaikei_app::tx::with_tx` という
//! 公開APIだけを経由し、`kaikei_app` ロール（帳簿への SELECT/INSERT のみ許可
//! される、実運用と同じロール）で検証する。境界値（phase1計画 R12）を
//! 明示的にケースとして含める:
//!
//! - `amount_minor` は `i64::MAX`（`BIGINT` の上限）
//! - `entry_no` は `i32::MAX` に近い値（`INTEGER` の上限）
//! - `entry_date` は `AccountingDate`/`chrono::NaiveDate` 双方が表現できる
//!   範囲内の極端な年（西暦1年・9999年）と閏日（2024-02-29）
//! - `recorded_at` はマイクロ秒境界に揃えた `Timestamp`（`D-036`。DB は
//!   `TIMESTAMPTZ` でマイクロ秒精度のため、揃っていない値は往復で一致しない）
//! - 逆仕訳（`reverses`/`reverse_reason`）・タグ・明細メモ・複数行明細

#![cfg(feature = "pg-tests")]

mod common;

use kaikei_app::ports::JournalRepo;
use kaikei_app::tx::with_tx;
use kaikei_core::{
    AccountCode, AccountDef, AccountType, AccountingDate, ChartOfAccounts, Currency, EntryId,
    EntryNumber, FiscalYear, FixedClock, JournalEntry, JournalLine, Money, NewEntry, PeriodGuard,
    PeriodStatus, Side, TagKey, TagSchema, TagSet, TagValue, Timestamp,
};
use kaikei_store::pool::PgStore;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

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

/// `business_ratio`（decimal）タグを集計軸として登録した最小限のスキーマ。
fn schema() -> TagSchema {
    TagSchema::new(vec![(
        TagKey::parse("business_ratio").unwrap(),
        kaikei_core::TagDef {
            value_type: kaikei_core::TagValueType::Decimal,
            aggregatable: true,
            required_for: Vec::new(),
        },
    )])
}

fn build_entry(
    id: u128,
    entry_no: u32,
    entry_date: AccountingDate,
    fy_label: i32,
    description: &str,
    lines: Vec<JournalLine>,
    clock_nanos: i128,
) -> JournalEntry {
    let fy = FiscalYear::calendar_year(fy_label);
    let clock = FixedClock(Timestamp::from_unix_nanos(clock_nanos));
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

fn line(account: &str, side: Side, amount_minor: i128, currency: Currency) -> JournalLine {
    JournalLine::new(
        AccountCode::parse(account).unwrap(),
        side,
        Money::from_minor(amount_minor, currency),
        TagSet::new(),
        None,
    )
    .unwrap()
}

async fn store_for(pool_opts: PgPoolOptions, conn_opts: PgConnectOptions) -> PgStore {
    let roles = common::roles(pool_opts, conn_opts).await;
    PgStore::new(roles.app)
}

fn assert_entries_equal(original: &JournalEntry, found: &JournalEntry) {
    assert_eq!(found.id(), original.id());
    assert_eq!(found.fiscal_year(), original.fiscal_year());
    assert_eq!(found.entry_no(), original.entry_no());
    assert_eq!(found.entry_date(), original.entry_date());
    assert_eq!(found.description(), original.description());
    assert_eq!(found.reverses(), original.reverses());
    assert_eq!(found.reverse_reason(), original.reverse_reason());
    assert_eq!(found.recorded_at(), original.recorded_at());
    assert_eq!(found.lines().len(), original.lines().len());
    for (found_line, original_line) in found.lines().iter().zip(original.lines()) {
        assert_eq!(found_line.account(), original_line.account());
        assert_eq!(found_line.side(), original_line.side());
        assert_eq!(found_line.amount(), original_line.amount());
        assert_eq!(
            found_line.tags().iter().collect::<Vec<_>>(),
            original_line.tags().iter().collect::<Vec<_>>()
        );
        assert_eq!(found_line.memo(), original_line.memo());
    }
}

#[sqlx::test]
async fn save_then_find_round_trips_all_fields(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let store = store_for(pool_opts, conn_opts).await;

    let mut tags = TagSet::new();
    tags.insert(
        TagKey::parse("business_ratio").unwrap(),
        TagValue::Decimal(rust_decimal::Decimal::new(8, 1)),
    );
    let debit_line = JournalLine::new(
        AccountCode::parse("100").unwrap(),
        Side::Debit,
        Money::from_minor(1_234, Currency::JPY),
        tags,
        Some("備考メモ".to_string()),
    )
    .unwrap();
    let credit_line = line("500", Side::Credit, 1_234, Currency::JPY);

    let entry = build_entry(
        1,
        1,
        AccountingDate::new(2026, 4, 15).unwrap(),
        2026,
        "テスト仕訳",
        vec![debit_line, credit_line],
        1_700_000_000_123_000, // マイクロ秒境界
    );

    let found: Option<JournalEntry> = with_tx(&store, |tx| {
        let entry = entry.clone();
        Box::pin(async move {
            tx.insert_entry(&entry).await?;
            Ok(tx.find_entry(entry.id()).await?)
        })
    })
    .await
    .unwrap();

    let found = found.expect("保存した仕訳が見つかること");
    assert_entries_equal(&entry, &found);
}

#[sqlx::test]
async fn round_trip_preserves_amount_at_bigint_upper_boundary(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let store = store_for(pool_opts, conn_opts).await;

    let entry = build_entry(
        2,
        1,
        AccountingDate::new(2026, 1, 1).unwrap(),
        2026,
        "上限金額のテスト",
        vec![
            line("100", Side::Debit, i128::from(i64::MAX), Currency::JPY),
            line("500", Side::Credit, i128::from(i64::MAX), Currency::JPY),
        ],
        1_700_000_000_000_000,
    );

    let found: Option<JournalEntry> = with_tx(&store, |tx| {
        let entry = entry.clone();
        Box::pin(async move {
            tx.insert_entry(&entry).await?;
            Ok(tx.find_entry(entry.id()).await?)
        })
    })
    .await
    .unwrap();

    let found = found.unwrap();
    assert_entries_equal(&entry, &found);
    assert_eq!(found.lines()[0].amount().minor(), i128::from(i64::MAX));
}

#[sqlx::test]
async fn round_trip_preserves_entry_no_near_i32_max(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let store = store_for(pool_opts, conn_opts).await;

    let entry_no = u32::try_from(i32::MAX).unwrap();
    let entry = build_entry(
        3,
        entry_no,
        AccountingDate::new(2026, 1, 1).unwrap(),
        2026,
        "上限に近い仕訳番号のテスト",
        vec![
            line("100", Side::Debit, 1_000, Currency::JPY),
            line("500", Side::Credit, 1_000, Currency::JPY),
        ],
        1_700_000_000_000_000,
    );

    let found: Option<JournalEntry> = with_tx(&store, |tx| {
        let entry = entry.clone();
        Box::pin(async move {
            tx.insert_entry(&entry).await?;
            Ok(tx.find_entry(entry.id()).await?)
        })
    })
    .await
    .unwrap();

    let found = found.unwrap();
    assert_eq!(found.entry_no().as_u32(), entry_no);
    assert_entries_equal(&entry, &found);
}

#[sqlx::test]
async fn round_trip_preserves_dates_at_extremes_within_representable_range(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let store = store_for(pool_opts, conn_opts).await;

    // 西暦1年・9999年・閏日（2024-02-29）。いずれも AccountingDate と
    // chrono::NaiveDate の双方が表現できる範囲内（chrono は西暦±26万年程度、
    // convert.rs のテストで別途境界を超える場合の OutOfRange を検証済み）。
    let cases = [(1, 1, 1), (9999, 12, 31), (2024, 2, 29)];
    for (index, (year, month, day)) in cases.into_iter().enumerate() {
        let date = AccountingDate::new(year, month, day).unwrap();
        let id = 100 + u128::try_from(index).unwrap();
        let entry = build_entry(
            id,
            1,
            date,
            year,
            "日付境界のテスト",
            vec![
                line("100", Side::Debit, 500, Currency::JPY),
                line("500", Side::Credit, 500, Currency::JPY),
            ],
            1_700_000_000_000_000,
        );

        let found: Option<JournalEntry> = with_tx(&store, |tx| {
            let entry = entry.clone();
            Box::pin(async move {
                tx.insert_entry(&entry).await?;
                Ok(tx.find_entry(entry.id()).await?)
            })
        })
        .await
        .unwrap();

        let found = found.unwrap();
        assert_eq!(
            found.entry_date(),
            date,
            "year={year} month={month} day={day}"
        );
        assert_entries_equal(&entry, &found);
    }
}

#[sqlx::test]
async fn round_trip_preserves_reversal_fields(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let store = store_for(pool_opts, conn_opts).await;

    let original = build_entry(
        4,
        1,
        AccountingDate::new(2026, 4, 1).unwrap(),
        2026,
        "元の仕訳",
        vec![
            line("100", Side::Debit, 2_000, Currency::JPY),
            line("500", Side::Credit, 2_000, Currency::JPY),
        ],
        1_700_000_000_000_000,
    );

    let fy = FiscalYear::calendar_year(2026);
    let clock = FixedClock(Timestamp::from_unix_nanos(1_700_000_100_000_000));
    let reversal = original
        .reverse(
            EntryId::new(5),
            EntryNumber::new(2),
            AccountingDate::new(2026, 4, 2).unwrap(),
            "入力誤りのため".to_string(),
            &fy,
            &chart(),
            &schema(),
            &AllOpen,
            &clock,
        )
        .unwrap();

    let (found_original, found_reversal): (Option<JournalEntry>, Option<JournalEntry>) =
        with_tx(&store, |tx| {
            let original = original.clone();
            let reversal = reversal.clone();
            Box::pin(async move {
                tx.insert_entry(&original).await?;
                tx.insert_entry(&reversal).await?;
                let found_original = tx.find_entry(original.id()).await?;
                let found_reversal = tx.find_entry(reversal.id()).await?;
                Ok((found_original, found_reversal))
            })
        })
        .await
        .unwrap();

    assert_entries_equal(&original, &found_original.unwrap());
    let found_reversal = found_reversal.unwrap();
    assert_entries_equal(&reversal, &found_reversal);
    assert_eq!(found_reversal.reverses(), Some(original.id()));
    assert_eq!(found_reversal.reverse_reason(), Some("入力誤りのため"));
}

#[sqlx::test]
async fn find_entry_returns_none_for_unknown_id(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let store = store_for(pool_opts, conn_opts).await;

    let found: Option<JournalEntry> = with_tx(&store, |tx| {
        Box::pin(async move { Ok(tx.find_entry(EntryId::new(999)).await?) })
    })
    .await
    .unwrap();

    assert!(found.is_none());
}

#[sqlx::test]
async fn find_reversal_of_reports_existing_and_absent_reversals(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let store = store_for(pool_opts, conn_opts).await;

    let original = build_entry(
        6,
        1,
        AccountingDate::new(2026, 5, 1).unwrap(),
        2026,
        "元の仕訳2",
        vec![
            line("100", Side::Debit, 3_000, Currency::JPY),
            line("500", Side::Credit, 3_000, Currency::JPY),
        ],
        1_700_000_000_000_000,
    );

    let fy = FiscalYear::calendar_year(2026);
    let clock = FixedClock(Timestamp::from_unix_nanos(1_700_000_100_000_000));
    let reversal = original
        .reverse(
            EntryId::new(7),
            EntryNumber::new(2),
            AccountingDate::new(2026, 5, 2).unwrap(),
            "訂正".to_string(),
            &fy,
            &chart(),
            &schema(),
            &AllOpen,
            &clock,
        )
        .unwrap();

    type Reversal = Option<(EntryId, EntryNumber)>;
    let (before, after): (Reversal, Reversal) = with_tx(&store, |tx| {
        let original = original.clone();
        let reversal = reversal.clone();
        Box::pin(async move {
            tx.insert_entry(&original).await?;
            let before = tx.find_reversal_of(original.id()).await?;
            tx.insert_entry(&reversal).await?;
            let after = tx.find_reversal_of(original.id()).await?;
            Ok((before, after))
        })
    })
    .await
    .unwrap();

    assert_eq!(before, None);
    assert_eq!(after, Some((reversal.id(), reversal.entry_no())));
}

#[sqlx::test]
async fn round_trip_preserves_multiple_lines_and_memo(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let store = store_for(pool_opts, conn_opts).await;

    let entry = build_entry(
        8,
        1,
        AccountingDate::new(2026, 6, 1).unwrap(),
        2026,
        "複数行明細のテスト",
        vec![
            line("100", Side::Debit, 600, Currency::JPY),
            line("600", Side::Debit, 400, Currency::JPY),
            line("500", Side::Credit, 1_000, Currency::JPY),
        ],
        1_700_000_000_000_000,
    );

    let found: Option<JournalEntry> = with_tx(&store, |tx| {
        let entry = entry.clone();
        Box::pin(async move {
            tx.insert_entry(&entry).await?;
            Ok(tx.find_entry(entry.id()).await?)
        })
    })
    .await
    .unwrap();

    let found = found.unwrap();
    assert_eq!(found.lines().len(), 3);
    assert_entries_equal(&entry, &found);
}
