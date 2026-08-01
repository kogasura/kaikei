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

use common::AllOpen;
use kaikei_app::error::{AppError, RepoError};
use kaikei_app::ports::JournalRepo;
use kaikei_app::tx::with_tx;
use kaikei_core::{
    AccountCode, AccountDef, AccountType, AccountingDate, ChartOfAccounts, Currency, DocumentRef,
    EntryId, EntryNumber, FiscalYear, FixedClock, JournalEntry, JournalLine, Money, NewEntry, Side,
    TagDef, TagKey, TagSchema, TagSet, TagValue, TagValueType, Timestamp,
};
use kaikei_store::pool::PgStore;
use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::TestRunner;
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

/// `TagValue` の4バリアントそれぞれに対応するタグキーを登録したスキーマ
/// （`business_ratio`=Decimal, `memo_text`=Text, `tax_code`=Code,
/// `memo_date`=Date）。いずれも集計要件の都合ではなく、NULバイト拒否
/// （`journal::reject_nul`）や proptest がタグの4バリアントを組み合わせて
/// 生成できるようにするためのテスト専用スキーマ。
fn schema() -> TagSchema {
    TagSchema::new(vec![
        (
            TagKey::parse("business_ratio").unwrap(),
            TagDef {
                value_type: TagValueType::Decimal,
                aggregatable: true,
                required_for: Vec::new(),
            },
        ),
        (
            TagKey::parse("memo_text").unwrap(),
            TagDef {
                value_type: TagValueType::Text,
                aggregatable: false,
                required_for: Vec::new(),
            },
        ),
        (
            TagKey::parse("tax_code").unwrap(),
            TagDef {
                value_type: TagValueType::Code,
                aggregatable: true,
                required_for: Vec::new(),
            },
        ),
        (
            TagKey::parse("memo_date").unwrap(),
            TagDef {
                value_type: TagValueType::Date,
                aggregatable: false,
                required_for: Vec::new(),
            },
        ),
    ])
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

// ---- insert_entry の書き込み時防御（document_refs非対応・NULバイト拒否）----
//
// `journal::PgTx::insert_entry` は F-1（document_refs非対応）と R12（NULバイト
// 摘要の拒否）の2つを検証しているが、修正前はどちらもテストから一度も実行
// されていなかった。ここで両方の防御を、拡張された適用範囲（摘要・明細メモ・
// 逆仕訳理由・タグのText/Code値）を含めて検証する。

#[sqlx::test]
async fn insert_entry_rejects_non_empty_document_refs(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let store = store_for(pool_opts, conn_opts).await;

    let fy = FiscalYear::calendar_year(2026);
    let clock = FixedClock(Timestamp::from_unix_nanos(1_700_000_000_000_000));
    let entry = JournalEntry::new(
        NewEntry {
            id: EntryId::new(900),
            entry_no: EntryNumber::new(1),
            entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
            description: "証憑付き仕訳".to_string(),
            lines: vec![
                line("100", Side::Debit, 1_000, Currency::JPY),
                line("500", Side::Credit, 1_000, Currency::JPY),
            ],
            document_refs: vec![DocumentRef {
                document_id: 1,
                label: "領収書".to_string(),
            }],
        },
        &fy,
        &chart(),
        &schema(),
        &AllOpen,
        &clock,
    )
    .unwrap();

    let result: Result<(), AppError> = with_tx(&store, |tx| {
        let entry = entry.clone();
        Box::pin(async move {
            tx.insert_entry(&entry).await?;
            Ok(())
        })
    })
    .await;

    match result {
        Err(AppError::Repo(RepoError::Unsupported { .. })) => {}
        other => panic!("RepoError::Unsupported を期待しましたが {other:?} でした"),
    }
}

#[sqlx::test]
async fn insert_entry_rejects_description_with_nul_byte(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let store = store_for(pool_opts, conn_opts).await;

    // JournalEntry::new の摘要検証は trim().is_empty() のみで NUL を拒否しない
    // ため、構築自体は成功する。insert_entry 側の防御を検証する。
    let entry = build_entry(
        901,
        1,
        AccountingDate::new(2026, 4, 1).unwrap(),
        2026,
        "テスト\0仕訳",
        vec![
            line("100", Side::Debit, 1_000, Currency::JPY),
            line("500", Side::Credit, 1_000, Currency::JPY),
        ],
        1_700_000_000_000_000,
    );

    let result: Result<(), AppError> = with_tx(&store, |tx| {
        let entry = entry.clone();
        Box::pin(async move {
            tx.insert_entry(&entry).await?;
            Ok(())
        })
    })
    .await;

    match result {
        Err(AppError::Repo(RepoError::Corrupt { .. })) => {}
        other => panic!("RepoError::Corrupt を期待しましたが {other:?} でした"),
    }
}

#[sqlx::test]
async fn insert_entry_rejects_memo_with_nul_byte(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let store = store_for(pool_opts, conn_opts).await;

    let debit_line = JournalLine::new(
        AccountCode::parse("100").unwrap(),
        Side::Debit,
        Money::from_minor(1_000, Currency::JPY),
        TagSet::new(),
        Some("メモ\0".to_string()),
    )
    .unwrap();
    let credit_line = line("500", Side::Credit, 1_000, Currency::JPY);

    let entry = build_entry(
        902,
        1,
        AccountingDate::new(2026, 4, 1).unwrap(),
        2026,
        "テスト仕訳",
        vec![debit_line, credit_line],
        1_700_000_000_000_000,
    );

    let result: Result<(), AppError> = with_tx(&store, |tx| {
        let entry = entry.clone();
        Box::pin(async move {
            tx.insert_entry(&entry).await?;
            Ok(())
        })
    })
    .await;

    match result {
        Err(AppError::Repo(RepoError::Corrupt { .. })) => {}
        other => panic!("RepoError::Corrupt を期待しましたが {other:?} でした"),
    }
}

#[sqlx::test]
async fn insert_entry_rejects_reverse_reason_with_nul_byte(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let store = store_for(pool_opts, conn_opts).await;

    let original = build_entry(
        903,
        1,
        AccountingDate::new(2026, 4, 1).unwrap(),
        2026,
        "元の仕訳",
        vec![
            line("100", Side::Debit, 1_000, Currency::JPY),
            line("500", Side::Credit, 1_000, Currency::JPY),
        ],
        1_700_000_000_000_000,
    );

    let fy = FiscalYear::calendar_year(2026);
    let clock = FixedClock(Timestamp::from_unix_nanos(1_700_000_100_000_000));
    let reversal = original
        .reverse(
            EntryId::new(904),
            EntryNumber::new(2),
            AccountingDate::new(2026, 4, 2).unwrap(),
            "理由\0".to_string(),
            &fy,
            &chart(),
            &schema(),
            &AllOpen,
            &clock,
        )
        .unwrap();

    // NULバイトの検出はSQL発行前のRust側で行われるため、元仕訳が実際に
    // DBへ保存されているかどうかに関わらずここで拒否される。
    let result: Result<(), AppError> = with_tx(&store, |tx| {
        let reversal = reversal.clone();
        Box::pin(async move {
            tx.insert_entry(&reversal).await?;
            Ok(())
        })
    })
    .await;

    match result {
        Err(AppError::Repo(RepoError::Corrupt { .. })) => {}
        other => panic!("RepoError::Corrupt を期待しましたが {other:?} でした"),
    }
}

#[sqlx::test]
async fn insert_entry_rejects_tag_text_value_with_nul_byte(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let store = store_for(pool_opts, conn_opts).await;

    let mut tags = TagSet::new();
    tags.insert(
        TagKey::parse("memo_text").unwrap(),
        TagValue::Text("備考\0".to_string()),
    );
    let debit_line = JournalLine::new(
        AccountCode::parse("100").unwrap(),
        Side::Debit,
        Money::from_minor(1_000, Currency::JPY),
        tags,
        None,
    )
    .unwrap();
    let credit_line = line("500", Side::Credit, 1_000, Currency::JPY);

    let entry = build_entry(
        905,
        1,
        AccountingDate::new(2026, 4, 1).unwrap(),
        2026,
        "テスト仕訳",
        vec![debit_line, credit_line],
        1_700_000_000_000_000,
    );

    let result: Result<(), AppError> = with_tx(&store, |tx| {
        let entry = entry.clone();
        Box::pin(async move {
            tx.insert_entry(&entry).await?;
            Ok(())
        })
    })
    .await;

    match result {
        Err(AppError::Repo(RepoError::Corrupt { .. })) => {}
        other => panic!("RepoError::Corrupt を期待しましたが {other:?} でした"),
    }
}

#[sqlx::test]
async fn insert_entry_rejects_tag_code_value_with_nul_byte(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let store = store_for(pool_opts, conn_opts).await;

    let mut tags = TagSet::new();
    tags.insert(
        TagKey::parse("tax_code").unwrap(),
        TagValue::Code("A\0".to_string()),
    );
    let debit_line = JournalLine::new(
        AccountCode::parse("100").unwrap(),
        Side::Debit,
        Money::from_minor(1_000, Currency::JPY),
        tags,
        None,
    )
    .unwrap();
    let credit_line = line("500", Side::Credit, 1_000, Currency::JPY);

    let entry = build_entry(
        906,
        1,
        AccountingDate::new(2026, 4, 1).unwrap(),
        2026,
        "テスト仕訳",
        vec![debit_line, credit_line],
        1_700_000_000_000_000,
    );

    let result: Result<(), AppError> = with_tx(&store, |tx| {
        let entry = entry.clone();
        Box::pin(async move {
            tx.insert_entry(&entry).await?;
            Ok(())
        })
    })
    .await;

    match result {
        Err(AppError::Repo(RepoError::Corrupt { .. })) => {}
        other => panic!("RepoError::Corrupt を期待しましたが {other:?} でした"),
    }
}

// ---- proptest: save→find の全フィールド一致を明細本数・金額・タグ・メモの
//      組み合わせにわたって検証する ----
//
// `proptest!` マクロは同期関数のみを対象にするため、そのままでは非同期の
// DBアクセス（`#[sqlx::test]` が提供する接続）と組み合わせられない
// （`#[sqlx::test]` は current-thread の tokio ランタイム上で実行されるため、
// マクロ内から別ランタイムをネストして `block_on` すると
// "Cannot start a runtime from within a runtime" になる）。そのため、
// `proptest::strategy::Strategy::new_tree` / `TestRunner` を直接使い、
// 生成した値をそのまま `.await` する（`proptest!` マクロが内部で行っている
// ことと同じ生成処理を、非同期コンテキストの中で手動に行うだけ）。

/// 仕訳明細1組（貸方・借方が同額になるペア）の生成仕様。
#[derive(Debug, Clone)]
struct PairSpec {
    amount: i128,
    debit_account: &'static str,
    credit_account: &'static str,
    debit_memo: Option<String>,
    credit_memo: Option<String>,
    debit_tags: TagSet,
    credit_tags: TagSet,
}

fn any_amount() -> impl Strategy<Value = i128> {
    // 明細は最大50行（25組）まで生成されうるため、貸借判定の遅延制約トリガ
    // （`assert_entry_is_balanced`、`journal_lines.amount_minor` は `BIGINT`）が
    // `SUM` でオーバーフローしない範囲に上限を抑える。i64::MAX そのものの
    // 境界（明細2行）は `round_trip_preserves_amount_at_bigint_upper_boundary`
    // で別途検証済みなので、ここでは「i64::MAX に近い大きな値」を明示的に
    // 含める（Phase 0の教訓: 生成器のレンジを実務的な値に狭めない）。
    let large = i128::from(i64::MAX) / 64;
    prop_oneof![
        6 => 1i128..=1_000_000_000i128,
        2 => 1i128..=large,
        1 => Just(large),
        1 => Just(1i128),
    ]
}

fn any_account() -> impl Strategy<Value = &'static str> {
    prop_oneof![Just("100"), Just("500"), Just("600")]
}

fn any_memo() -> impl Strategy<Value = Option<String>> {
    prop_oneof![
        3 => Just(None),
        1 => "[a-zA-Z0-9 ぁ-んァ-ヶ一-龠]{0,16}".prop_map(Some),
    ]
}

/// `TagValue` の4バリアントのいずれか1つ（または「タグ無し」）を生成する。
fn any_tag_entry() -> impl Strategy<Value = Option<(TagKey, TagValue)>> {
    prop_oneof![
        3 => Just(None),
        1 => (0i64..=999_999i64, 0u32..=6u32).prop_map(|(mantissa, scale)| {
            Some((
                TagKey::parse("business_ratio").unwrap(),
                TagValue::Decimal(rust_decimal::Decimal::new(mantissa, scale)),
            ))
        }),
        1 => "[a-zA-Zぁ-んー0-9 ]{0,16}".prop_map(|s| {
            Some((TagKey::parse("memo_text").unwrap(), TagValue::Text(s)))
        }),
        1 => "[A-Z]{1,4}[0-9]{0,4}".prop_map(|s| {
            Some((TagKey::parse("tax_code").unwrap(), TagValue::Code(s)))
        }),
        1 => (2000i32..=2100i32, 1u8..=12u8, 1u8..=28u8).prop_map(|(y, m, d)| {
            Some((
                TagKey::parse("memo_date").unwrap(),
                TagValue::Date(AccountingDate::new(y, m, d).unwrap()),
            ))
        }),
    ]
}

/// 0〜2個のタグを組み合わせる（複数バリアントの同時使用をカバーする）。
fn any_tags() -> impl Strategy<Value = TagSet> {
    proptest::collection::vec(any_tag_entry(), 0..=2).prop_map(|picked| {
        let mut tags = TagSet::new();
        for tag in picked.into_iter().flatten() {
            tags.insert(tag.0, tag.1);
        }
        tags
    })
}

fn any_pair() -> impl Strategy<Value = PairSpec> {
    (
        any_amount(),
        any_account(),
        any_account(),
        any_memo(),
        any_memo(),
        any_tags(),
        any_tags(),
    )
        .prop_map(
            |(
                amount,
                debit_account,
                credit_account,
                debit_memo,
                credit_memo,
                debit_tags,
                credit_tags,
            )| {
                PairSpec {
                    amount,
                    debit_account,
                    credit_account,
                    debit_memo,
                    credit_memo,
                    debit_tags,
                    credit_tags,
                }
            },
        )
}

/// 明細本数2〜50行（1〜25組）を生成する。
fn any_entry_spec() -> impl Strategy<Value = Vec<PairSpec>> {
    proptest::collection::vec(any_pair(), 1..=25)
}

fn build_lines_from_pairs(pairs: &[PairSpec]) -> Vec<JournalLine> {
    let mut lines = Vec::with_capacity(pairs.len() * 2);
    for pair in pairs {
        lines.push(
            JournalLine::new(
                AccountCode::parse(pair.debit_account).unwrap(),
                Side::Debit,
                Money::from_minor(pair.amount, Currency::JPY),
                pair.debit_tags.clone(),
                pair.debit_memo.clone(),
            )
            .unwrap(),
        );
        lines.push(
            JournalLine::new(
                AccountCode::parse(pair.credit_account).unwrap(),
                Side::Credit,
                Money::from_minor(pair.amount, Currency::JPY),
                pair.credit_tags.clone(),
                pair.credit_memo.clone(),
            )
            .unwrap(),
        );
    }
    lines
}

#[sqlx::test]
async fn round_trip_property_preserves_arbitrary_valid_entries(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let store = store_for(pool_opts, conn_opts).await;

    let strategy = any_entry_spec();
    let mut runner = TestRunner::new(ProptestConfig::with_cases(40));

    for case in 0u32..40 {
        let pairs = strategy
            .new_tree(&mut runner)
            .expect("proptest: 値の生成に失敗しました")
            .current();
        let lines = build_lines_from_pairs(&pairs);

        let entry = build_entry(
            2_000_000 + u128::from(case),
            case + 1,
            AccountingDate::new(2026, 4, 1).unwrap(),
            2026,
            "proptestで生成した仕訳",
            lines,
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
        .unwrap_or_else(|e| panic!("case={case} pairs={pairs:?}: with_tx が失敗しました: {e}"));

        let found = found.unwrap_or_else(|| {
            panic!("case={case} pairs={pairs:?}: 保存した仕訳が見つかりませんでした")
        });
        assert_entries_equal(&entry, &found);
    }
}
