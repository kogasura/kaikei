//! `kaikei_store::query::PgTrialBalanceQuery`（SQL集計）と
//! `kaikei_core::TrialBalance::from_entries`（read modelの対照実装）が一致する
//! ことを検証する差分テスト。
//!
//! # R9（`GroupKey`を直接比較できない制約）への対処
//!
//! `kaikei_core::GroupKey` には`impl`ブロックが1つも無く、公開コンストラクタも
//! アクセサも存在しないため、SQL集計側の結果（`GroupKeyView` =
//! `BTreeMap<String, String>`）とcore側の`GroupKey`を直接比較することは
//! できない（phase1計画 §0-7 / R9）。このファイルでは2段構えで対処する:
//!
//! 1. `group_by = &[]`（グループ化なし）を主戦場にする。この場合両実装とも
//!    `GroupKey`/グループは空になるため、行単位で完全に比較できる
//!    （[`trial_balance_matches_core_for_empty_group_by`]）。5科目種別すべての
//!    残高の向き（`DOMAIN.md` §2）もここで検証する。
//! 2. `group_by`ありのケースは、SQL側の結果を科目ごとにロールアップして
//!    coreの`TrialBalance::balance_of`と突き合わせ（間接的な差分検証）、
//!    かつ各グループの内容そのものはテストが構築した既知のタグ割り当てに
//!    対する期待値と直接比較する（[`trial_balance_group_by_rolls_up_to_the_same_balance_as_core`]）。
//!
//! # 通貨が混在した場合の扱い（`DECISIONS.md` D-042）
//!
//! `journal_lines`は行ごとに`currency`/`currency_minor_unit`を持つ。Phase 1は
//! JPY単一通貨を想定するが、coreの`TrialBalance::from_entries`は対象の仕訳
//! 集合全体で通貨が単一であることを要求するため（`CoreError::CurrencyMismatch`）、
//! この実装も同じ粒度で`RepoError::Unsupported`を返すことを検証する
//! （[`trial_balance_rejects_mixed_currencies_like_core_does`]）。

#![cfg(feature = "pg-tests")]

mod common;

use common::AllOpen;
use kaikei_app::error::RepoError;
use kaikei_app::ports::TrialBalanceQuery;
use kaikei_app::view::GroupKeyView;
use kaikei_core::{
    AccountCode, AccountDef, AccountType, AccountingDate, ChartOfAccounts, CoreError, Currency,
    EntryId, EntryNumber, FiscalYear, FixedClock, JournalEntry, JournalLine, Money, NewEntry, Side,
    TagDef, TagKey, TagSchema, TagSet, TagValue, TagValueType, Timestamp, TrialBalance,
};
use kaikei_store::convert::{
    account_type_to_i16, accounting_date_to_naive_date, entry_no_to_i32, money_to_columns,
    side_to_i16, timestamp_to_datetime,
};
use kaikei_store::query::PgTrialBalanceQuery;
use kaikei_store::tags::tag_set_to_json;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::PgPool;
use uuid::Uuid;

fn code(s: &str) -> AccountCode {
    AccountCode::parse(s).unwrap()
}

fn key(s: &str) -> TagKey {
    TagKey::parse(s).unwrap()
}

fn date(y: i32, m: u8, d: u8) -> AccountingDate {
    AccountingDate::new(y, m, d).unwrap()
}

fn account(code_str: &str, name: &str, account_type: AccountType) -> AccountDef {
    AccountDef {
        code: code(code_str),
        name: name.to_string(),
        account_type,
        parent: None,
        postable: true,
    }
}

fn line(account_code: &str, side: Side, amount: i128, tags: TagSet) -> JournalLine {
    JournalLine::new(
        code(account_code),
        side,
        Money::from_minor(amount, Currency::JPY),
        tags,
        None,
    )
    .unwrap()
}

fn entry(
    id: u128,
    entry_no: u32,
    entry_date: AccountingDate,
    lines: Vec<JournalLine>,
    fy: &FiscalYear,
    chart: &ChartOfAccounts,
    schema: &TagSchema,
) -> JournalEntry {
    JournalEntry::new(
        NewEntry {
            id: EntryId::new(id),
            entry_no: EntryNumber::new(entry_no),
            entry_date,
            description: "テスト仕訳".to_string(),
            lines,
            document_refs: Vec::new(),
        },
        fy,
        chart,
        schema,
        &AllOpen,
        &FixedClock(Timestamp::from_unix_nanos(0)),
    )
    .unwrap()
}

/// 勘定科目表を`accounts`テーブルに反映する（差分テストが構築した
/// `ChartOfAccounts`とDB上の`accounts`を一致させるため）。
async fn insert_account(pool: &PgPool, def: &AccountDef) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO accounts (code, name, account_type, parent_code, postable) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(def.code.as_str())
    .bind(&def.name)
    .bind(account_type_to_i16(def.account_type))
    .bind(def.parent.as_ref().map(AccountCode::as_str))
    .bind(def.postable)
    .execute(pool)
    .await?;
    Ok(())
}

/// `kaikei_core::JournalEntry::new`で構築済みの仕訳を、`kaikei-store`の共有
/// 変換関数（`convert.rs`/`tags.rs`。`DECISIONS.md` D-034でPR-5本体と共有する
/// 基盤として先に固めたもの）を使ってDBへINSERTする。
async fn insert_entry(pool: &PgPool, entry: &JournalEntry) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;

    let entry_no = entry_no_to_i32(entry.entry_no()).expect("テストのentry_noは範囲内");
    let entry_date =
        accounting_date_to_naive_date(entry.entry_date()).expect("テストの日付は範囲内");
    let recorded_at = timestamp_to_datetime(entry.recorded_at()).expect("テストの記帳時刻は範囲内");
    let id = Uuid::from_u128(entry.id().as_u128());

    sqlx::query(
        "INSERT INTO journal_entries \
         (id, fiscal_year, entry_no, entry_date, description, recorded_at) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(id)
    .bind(entry.fiscal_year())
    .bind(entry_no)
    .bind(entry_date)
    .bind(entry.description())
    .bind(recorded_at)
    .execute(&mut *tx)
    .await?;

    for (i, line) in entry.lines().iter().enumerate() {
        let line_no = i16::try_from(i + 1).expect("テストの明細行数はi16に収まる");
        let (amount_minor, currency, currency_minor_unit) =
            money_to_columns(line.amount()).expect("テストの金額は範囲内");

        sqlx::query(
            "INSERT INTO journal_lines \
             (entry_id, line_no, account_code, side, amount_minor, currency, \
              currency_minor_unit, tags, memo) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(id)
        .bind(line_no)
        .bind(line.account().as_str())
        .bind(side_to_i16(line.side()))
        .bind(amount_minor)
        .bind(currency)
        .bind(currency_minor_unit)
        .bind(tag_set_to_json(line.tags()))
        .bind(line.memo())
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await
}

/// 主戦場（R9対処①）: `group_by = &[]`なら`GroupKey`/`GroupKeyView`は必ず
/// 空になるため、SQL集計とcoreの結果を行単位で完全に比較できる。
/// 5科目種別すべてで残高の向き（`DOMAIN.md` §2:
/// Asset/Expenseは借方-貸方、Liability/Equity/Revenueは貸方-借方）が
/// 一致することも同時に検証する。
#[sqlx::test]
async fn trial_balance_matches_core_for_empty_group_by(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;

    let chart_defs = vec![
        account("100", "現金", AccountType::Asset),
        account("200", "買掛金", AccountType::Liability),
        account("300", "元入金", AccountType::Equity),
        account("500", "売上高", AccountType::Revenue),
        account("600", "消耗品費", AccountType::Expense),
    ];
    for def in &chart_defs {
        insert_account(&roles.migrator, def).await.unwrap();
    }
    let chart = ChartOfAccounts::new(chart_defs).unwrap();
    let schema = TagSchema::empty();
    let fy = FiscalYear::calendar_year(2026);

    let entries = vec![
        entry(
            1,
            1,
            date(2026, 1, 10),
            vec![
                line("100", Side::Debit, 10_000, TagSet::new()),
                line("600", Side::Debit, 5_000, TagSet::new()),
                line("200", Side::Credit, 3_000, TagSet::new()),
                line("300", Side::Credit, 2_000, TagSet::new()),
                line("500", Side::Credit, 10_000, TagSet::new()),
            ],
            &fy,
            &chart,
            &schema,
        ),
        entry(
            2,
            2,
            date(2026, 6, 1),
            vec![
                line("200", Side::Debit, 800, TagSet::new()),
                line("100", Side::Credit, 800, TagSet::new()),
            ],
            &fy,
            &chart,
            &schema,
        ),
    ];
    for e in &entries {
        insert_entry(&roles.migrator, e).await.unwrap();
    }

    let core_tb = TrialBalance::from_entries(entries.iter(), &chart, &schema, &[]).unwrap();
    assert!(core_tb.is_balanced());

    let query = PgTrialBalanceQuery::new(roles.app.clone());
    let sql_rows = query
        .trial_balance(date(2026, 1, 1), date(2026, 12, 31), &[])
        .await
        .unwrap();

    assert_eq!(sql_rows.len(), core_tb.rows().len());
    for sql_row in &sql_rows {
        assert!(sql_row.group.is_empty());
        let core_balance = core_tb.balance_of(&sql_row.account).unwrap();
        assert_eq!(sql_row.balance, core_balance);
        assert_eq!(
            sql_row.account_type,
            chart.get(&sql_row.account).unwrap().account_type
        );
    }

    // DOMAIN.md §2: 5科目種別すべてで残高の向きを明示的に検証する。
    let balance_of = |c: &str| -> i128 {
        sql_rows
            .iter()
            .find(|r| r.account == code(c))
            .unwrap()
            .balance
            .minor()
    };
    assert_eq!(balance_of("100"), 10_000 - 800); // Asset: 借方-貸方
    assert_eq!(balance_of("600"), 5_000); // Expense: 借方-貸方
    assert_eq!(balance_of("200"), 3_000 - 800); // Liability: 貸方-借方
    assert_eq!(balance_of("300"), 2_000); // Equity: 貸方-借方
    assert_eq!(balance_of("500"), 10_000); // Revenue: 貸方-借方

    let (core_debit_total, core_credit_total) = core_tb.totals();
    let sql_debit_total: i128 = sql_rows.iter().map(|r| r.debit_total.minor()).sum();
    let sql_credit_total: i128 = sql_rows.iter().map(|r| r.credit_total.minor()).sum();
    assert_eq!(sql_debit_total, core_debit_total.minor());
    assert_eq!(sql_credit_total, core_credit_total.minor());
}

/// R9対処②: `group_by`ありのケース。`GroupKey`同士は直接比較できないため、
/// (a) SQL側の結果を科目ごとにロールアップしてcoreの`balance_of`と突き合わせ、
/// (b) 各グループの内容（`GroupKeyView`）はテストが構築した既知のタグ割り当てに
/// 対する期待値と直接比較する。
#[sqlx::test]
async fn trial_balance_group_by_rolls_up_to_the_same_balance_as_core(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;

    let chart_defs = vec![
        account("100", "現金", AccountType::Asset),
        account("600", "消耗品費", AccountType::Expense),
    ];
    for def in &chart_defs {
        insert_account(&roles.migrator, def).await.unwrap();
    }
    let chart = ChartOfAccounts::new(chart_defs).unwrap();
    let schema = TagSchema::new(vec![(
        key("counterparty"),
        TagDef {
            value_type: TagValueType::Code,
            aggregatable: true,
            required_for: vec![],
        },
    )]);
    let fy = FiscalYear::calendar_year(2026);

    let tags_of = |counterparty: &str| {
        let mut t = TagSet::new();
        t.insert(
            key("counterparty"),
            TagValue::Code(counterparty.to_string()),
        );
        t
    };

    let entries = vec![
        entry(
            10,
            1,
            date(2026, 2, 1),
            vec![
                line("600", Side::Debit, 1_000, tags_of("A")),
                line("100", Side::Credit, 1_000, TagSet::new()),
            ],
            &fy,
            &chart,
            &schema,
        ),
        entry(
            11,
            2,
            date(2026, 2, 2),
            vec![
                line("600", Side::Debit, 2_000, tags_of("B")),
                line("100", Side::Credit, 2_000, TagSet::new()),
            ],
            &fy,
            &chart,
            &schema,
        ),
        entry(
            12,
            3,
            date(2026, 2, 3),
            vec![
                line("600", Side::Debit, 500, tags_of("A")),
                line("100", Side::Credit, 500, TagSet::new()),
            ],
            &fy,
            &chart,
            &schema,
        ),
        entry(
            13,
            4,
            date(2026, 2, 4),
            vec![
                line("600", Side::Debit, 300, TagSet::new()),
                line("100", Side::Credit, 300, TagSet::new()),
            ],
            &fy,
            &chart,
            &schema,
        ),
    ];
    for e in &entries {
        insert_entry(&roles.migrator, e).await.unwrap();
    }

    let group_by = [key("counterparty")];
    let core_tb = TrialBalance::from_entries(entries.iter(), &chart, &schema, &group_by).unwrap();
    assert!(core_tb.is_balanced());

    let query = PgTrialBalanceQuery::new(roles.app.clone());
    let sql_rows = query
        .trial_balance(date(2026, 1, 1), date(2026, 12, 31), &group_by)
        .await
        .unwrap();
    assert_eq!(sql_rows.len(), 4);
    assert_eq!(sql_rows.len(), core_tb.rows().len());

    // (b) 既知のタグ割り当てに対する期待値との直接比較。
    let assert_row = |acct: &str, group: &[(&str, &str)], debit: i128, credit: i128| {
        let expected_group: GroupKeyView = group
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let row = sql_rows
            .iter()
            .find(|r| r.account == code(acct) && r.group == expected_group)
            .unwrap_or_else(|| {
                panic!(
                    "行が見つかりません: account={acct} group={expected_group:?} rows={sql_rows:?}"
                )
            });
        assert_eq!(row.debit_total.minor(), debit);
        assert_eq!(row.credit_total.minor(), credit);
    };
    assert_row("600", &[("counterparty", "A")], 1_500, 0);
    assert_row("600", &[("counterparty", "B")], 2_000, 0);
    assert_row("600", &[], 300, 0);
    assert_row("100", &[], 0, 3_800);

    // (a) 科目単位でロールアップし、coreの balance_of と突き合わせる。
    let rollup = |acct: &str| -> i128 {
        sql_rows
            .iter()
            .filter(|r| r.account == code(acct))
            .map(|r| r.balance.minor())
            .sum()
    };
    assert_eq!(
        rollup("600"),
        core_tb.balance_of(&code("600")).unwrap().minor()
    );
    assert_eq!(
        rollup("100"),
        core_tb.balance_of(&code("100")).unwrap().minor()
    );
}

/// `DECISIONS.md` D-042: 同一期間に複数通貨が混在する場合、coreの
/// `TrialBalance::from_entries`は`CoreError::CurrencyMismatch`で拒否する。
/// この実装も同じ粒度（集計対象全体）で`RepoError::Unsupported`を返す。
#[sqlx::test]
async fn trial_balance_rejects_mixed_currencies_like_core_does(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;

    let chart_defs = vec![
        account("100", "現金", AccountType::Asset),
        account("500", "売上高", AccountType::Revenue),
    ];
    for def in &chart_defs {
        insert_account(&roles.migrator, def).await.unwrap();
    }
    let chart = ChartOfAccounts::new(chart_defs).unwrap();
    let schema = TagSchema::empty();
    let fy = FiscalYear::calendar_year(2026);

    let jpy_entry = entry(
        20,
        1,
        date(2026, 1, 5),
        vec![
            line("100", Side::Debit, 1_000, TagSet::new()),
            line("500", Side::Credit, 1_000, TagSet::new()),
        ],
        &fy,
        &chart,
        &schema,
    );

    let usd_lines = vec![
        JournalLine::new(
            code("100"),
            Side::Debit,
            Money::from_minor(1_000, Currency::USD),
            TagSet::new(),
            None,
        )
        .unwrap(),
        JournalLine::new(
            code("500"),
            Side::Credit,
            Money::from_minor(1_000, Currency::USD),
            TagSet::new(),
            None,
        )
        .unwrap(),
    ];
    let usd_entry = entry(21, 2, date(2026, 1, 6), usd_lines, &fy, &chart, &schema);

    insert_entry(&roles.migrator, &jpy_entry).await.unwrap();
    insert_entry(&roles.migrator, &usd_entry).await.unwrap();

    let entries = [jpy_entry, usd_entry];
    let core_result = TrialBalance::from_entries(entries.iter(), &chart, &schema, &[]);
    assert!(matches!(
        core_result,
        Err(CoreError::CurrencyMismatch { .. })
    ));

    let query = PgTrialBalanceQuery::new(roles.app.clone());
    let sql_result = query
        .trial_balance(date(2026, 1, 1), date(2026, 12, 31), &[])
        .await;
    assert!(matches!(sql_result, Err(RepoError::Unsupported { .. })));
}

/// 対象期間に仕訳明細が1件も無ければ空の試算表を返す（coreの
/// `TrialBalance::from_entries`が空のイテレータに対して空の`rows()`を
/// 返すのと同じ振る舞い）。
#[sqlx::test]
async fn trial_balance_is_empty_when_no_lines_in_range(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    let query = PgTrialBalanceQuery::new(roles.app.clone());
    let rows = query
        .trial_balance(date(2026, 1, 1), date(2026, 12, 31), &[])
        .await
        .unwrap();
    assert!(rows.is_empty());
}

/// `journal_lines.account_code`に`accounts.code`へのFK制約は無いため
/// （`docs/03-database.md` §2）、対応する科目が存在しない行が混在しうる。
/// この実装は`accounts`へ`LEFT JOIN`し、該当科目が見つからない場合に
/// 黙って集計から除外せず`RepoError::Corrupt`を返すことを検証する
/// （`kaikei_core::JournalEntry::new`を経由しない、生SQLで直接構成した
/// 破損データのシミュレーション）。
#[sqlx::test]
async fn trial_balance_reports_corrupt_for_orphaned_account_code(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    let id = Uuid::now_v7();

    let mut tx = roles.migrator.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO journal_entries \
         (id, fiscal_year, entry_no, entry_date, description, recorded_at) \
         VALUES ($1, 2026, 1, '2026-04-01', 'テスト仕訳', now())",
    )
    .bind(id)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO journal_lines \
         (entry_id, line_no, account_code, side, amount_minor, currency, currency_minor_unit) \
         VALUES ($1, 1, '999', 1, 1000, 'JPY', 0), ($1, 2, '999', 2, 1000, 'JPY', 0)",
    )
    .bind(id)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let query = PgTrialBalanceQuery::new(roles.app.clone());
    let result = query
        .trial_balance(date(2026, 1, 1), date(2026, 12, 31), &[])
        .await;
    assert!(matches!(result, Err(RepoError::Corrupt { .. })));
}
