//! Phase 1 の結線の証明（PR-8 単位B）。
//!
//! これまでの pg-tests（`round_trip.rs` 等）は `PgStore` のリポジトリ実装
//! （`JournalRepo`/`ChartRepo`/`PeriodRepo`/`NumberingRepo`）を直接叩いていた。
//! このファイルは一段上、**`kaikei-app` のユースケース関数**
//! （[`kaikei_app::usecase::post_entry::execute`] /
//! [`kaikei_app::usecase::reverse_entry::execute`] /
//! [`kaikei_app::usecase::report::execute`]）を、実 PostgreSQL に繋いだ
//! `kaikei_store::pool::PgStore` / `kaikei_store::query::PgTrialBalanceQuery`
//! に対して通す。
//!
//! # `kaikei-policy` への非依存（`CLAUDE.md` §1）
//!
//! `kaikei-store` の `Cargo.toml` には `kaikei-policy` を追加しない（dev-dependency
//! も含めて）。`.github/workflows/architecture.yml` の「kaikei-store は
//! kaikei-jp/kaikei-policy に依存しない」ステップが守っている境界であり、
//! `--edges normal` は dev-dependency を見ないため機械的には通ってしまうが、
//! それをやると CI の意味が消える。`TaxPolicy`/`TaxContext`/`TaxDerivation`/
//! `PolicyError` は `kaikei_app` からの再エクスポート（`kaikei_app::{TaxPolicy,
//! TaxContext, TaxDerivation, PolicyError}`）経由で使う。
//!
//! `kaikei_app::testing`（`InMemoryStore` 等の fake）も使わない。この E2E の
//! 目的は実 DB に通すことであり、fake が混ざると意味が薄れるため、必要な
//! テストダブル（`TaxPolicy` の実装・`IdGenerator` の実装）はこのファイル内に
//! ローカル定義する。
//!
//! # `ROADMAP.md` Phase 1 完了条件との対応
//!
//! | # | 完了条件 | 対応するテスト |
//! |---|---|---|
//! | 1 | 再起動してもデータが残る | `posted_entry_is_readable_from_a_freshly_reconnected_pool`（E2E-01。別プールから読めることによる代理検証。docker volume 自体の永続性は README 参照） |
//! | 2 | `kaikei_app` ロールで UPDATE を試みると失敗する | `tests/append_only.rs` が既に担保。ここでは重複させない |
//! | 3 | 逆仕訳が正しく記録される | `reversed_entry_offsets_original_in_trial_balance_and_persists_reversal_fields`（E2E-03）・`double_reversal_is_rejected_and_leaves_no_second_reversal_in_db`（E2E-04） |
//! | 4 | 試算表が SQL 集計で出る | E2E-02・E2E-03・E2E-09 |
//! | 5 | `UnitOfWork`（`&mut Tx`）と借用チェッカとの相性評価 | `with_tx_rolls_back_journal_and_numbering_together`（E2E-10） |

#![cfg(feature = "pg-tests")]

mod common;

use kaikei_app::context::{BookSettings, FiscalYearRule};
use kaikei_app::error::AppError;
use kaikei_app::ports::{ChartRepo, IdGenerator, JournalRepo};
use kaikei_app::tx::with_tx;
use kaikei_app::usecase::post_entry::{self, PostEntryInput};
use kaikei_app::usecase::report::{self, ReportInput};
use kaikei_app::usecase::reverse_entry::{self, ReverseEntryInput};
use kaikei_app::{PolicyError, TaxContext, TaxDerivation, TaxPolicy};
use kaikei_core::{
    AccountCode, AccountDef, AccountType, AccountingDate, CoreError, Currency, EntryId, FixedClock,
    JournalEntry, JournalLine, Money, Ratio, RoundMode, Side, TagDef, TagKey, TagSchema, TagSet,
    TagValueType, Timestamp,
};
use kaikei_store::convert::account_type_to_i16;
use kaikei_store::pool::PgStore;
use kaikei_store::query::PgTrialBalanceQuery;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::PgPool;
use std::sync::{Arc, Mutex};

// ---- ローカルのテストダブル ----
//
// `kaikei-store` からは `kaikei-policy` / `kaikei_app::testing` へ依存を足せない
// ため（モジュールdocの「`kaikei-policy` への非依存」を参照）、必要なテスト
// ダブルはここに定義する。
//
// 名前は `kaikei_policy::testing` / `kaikei_app::testing` の同種の fake と
// 揃えてあるが、**それらと挙動を一致させる義務は無い**。このファイルの
// テストを駆動するためだけの独立したフィクスチャであり、本家が変わっても
// 追随する必要はない（追随すべき対象なら、そもそも依存を足せない時点で
// 破綻している）。

/// 消費税行を一切生成しない `TaxPolicy`。
#[derive(Debug, Clone, Copy, Default)]
struct NoTaxPolicy;

impl TaxPolicy for NoTaxPolicy {
    fn validate_tag(
        &self,
        _ctx: &TaxContext<'_>,
        _tags: &TagSet,
        _account: &AccountDef,
    ) -> Result<(), PolicyError> {
        Ok(())
    }

    fn derive_tax_lines(
        &self,
        _ctx: &TaxContext<'_>,
        lines: &[JournalLine],
    ) -> Result<TaxDerivation, PolicyError> {
        Ok(TaxDerivation {
            lines: lines.to_vec(),
            notes: Vec::new(),
        })
    }

    fn round_mode(&self, _ctx: &TaxContext<'_>) -> RoundMode {
        RoundMode::Floor
    }
}

/// 側（借方・貸方）ごとの明細合計に外税を1行追加する `TaxPolicy`
/// （E2E-05専用。税区分ごとの判定は一切行わない最小実装）。
struct FlatRateTaxPolicy {
    rate: Ratio,
    tax_account: &'static str,
}

impl TaxPolicy for FlatRateTaxPolicy {
    fn validate_tag(
        &self,
        _ctx: &TaxContext<'_>,
        _tags: &TagSet,
        _account: &AccountDef,
    ) -> Result<(), PolicyError> {
        Ok(())
    }

    fn derive_tax_lines(
        &self,
        ctx: &TaxContext<'_>,
        lines: &[JournalLine],
    ) -> Result<TaxDerivation, PolicyError> {
        let tax_account = AccountCode::parse(self.tax_account)?;
        let mut result: Vec<JournalLine> = lines.to_vec();
        for side in [Side::Debit, Side::Credit] {
            let amounts = lines
                .iter()
                .filter(|l| l.side() == side)
                .map(|l| l.amount());
            let Some(total) = kaikei_core::sum_money(amounts)? else {
                continue;
            };
            let tax_amount = self.apply_ratio(ctx, total, self.rate)?;
            if tax_amount.is_zero() {
                continue;
            }
            result.push(JournalLine::new(
                tax_account.clone(),
                side,
                tax_amount,
                TagSet::new(),
                None,
            )?);
        }
        Ok(TaxDerivation {
            lines: result,
            notes: Vec::new(),
        })
    }

    fn round_mode(&self, _ctx: &TaxContext<'_>) -> RoundMode {
        RoundMode::Floor
    }
}

/// 呼び出しごとに1ずつ増える決定的な仕訳IDを返す `IdGenerator`。
struct SequentialIdGenerator {
    next: Mutex<u128>,
}

impl SequentialIdGenerator {
    fn starting_at(first: u128) -> Self {
        SequentialIdGenerator {
            next: Mutex::new(first),
        }
    }
}

impl IdGenerator for SequentialIdGenerator {
    fn new_entry_id(&self) -> EntryId {
        let mut guard = self
            .next
            .lock()
            .expect("SequentialIdGenerator の Mutex はテスト専用なので毒されない前提");
        let id = EntryId::new(*guard);
        *guard += 1;
        id
    }
}

// ---- 共通セットアップ ----

fn settings() -> BookSettings {
    BookSettings {
        fiscal_year_rule: FiscalYearRule::CalendarYear,
    }
}

fn clock() -> FixedClock {
    FixedClock(Timestamp::from_unix_nanos(1_700_000_000_000_000))
}

/// 貸借が一致した最小限の明細（現金/売上高）を組み立てる。
fn balanced_lines(amount: i128) -> Vec<JournalLine> {
    vec![
        JournalLine::new(
            AccountCode::parse("100").unwrap(),
            Side::Debit,
            Money::from_minor(amount, Currency::JPY),
            TagSet::new(),
            None,
        )
        .unwrap(),
        JournalLine::new(
            AccountCode::parse("500").unwrap(),
            Side::Credit,
            Money::from_minor(amount, Currency::JPY),
            TagSet::new(),
            None,
        )
        .unwrap(),
    ]
}

/// `accounts` テーブルに勘定科目を1件INSERTする（`migrator` ロール = 所有者で行う）。
///
/// `post_entry::execute` は `tx.load_chart()` でこのテーブルを読むため、
/// コード内で `ChartOfAccounts` を組み立てて渡す既存の pg-tests とは異なり、
/// 実際にテーブルへ書き込んでからユースケースを呼ぶ（ユースケース経由の
/// 経路であることの証明）。
async fn insert_account(pool: &PgPool, code: &str, name: &str, account_type: AccountType) {
    sqlx::query(
        "INSERT INTO accounts (code, name, account_type, parent_code, postable) \
         VALUES ($1, $2, $3, NULL, TRUE)",
    )
    .bind(code)
    .bind(name)
    .bind(account_type_to_i16(account_type))
    .execute(pool)
    .await
    .expect("accounts へのテスト用INSERTが失敗しないこと");
}

/// 現金(100・資産)と売上高(500・収益)を `accounts` に用意する。
async fn seed_basic_accounts(pool: &PgPool) {
    insert_account(pool, "100", "現金", AccountType::Asset).await;
    insert_account(pool, "500", "売上高", AccountType::Revenue).await;
}

/// 仮受消費税(330・負債)を追加で用意する（E2E-05: 税額行の自動生成用）。
async fn seed_tax_account(pool: &PgPool) {
    insert_account(pool, "330", "仮受消費税", AccountType::Liability).await;
}

/// `period_snapshots` に締めスナップショットを1件INSERTし、
/// `PeriodRepo::closed_through` を効かせる（`migrator` ロールで行う）。
async fn close_period(pool: &PgPool, fiscal_year: i32, period_end: &str) {
    sqlx::query(
        "INSERT INTO period_snapshots \
         (fiscal_year, period_end, closed_at, balances, currency, currency_minor_unit, \
          entry_count, last_entry_no, checksum) \
         VALUES ($1, $2::date, now(), '{}', 'JPY', 0, 0, 0, 'e2e-test-checksum')",
    )
    .bind(fiscal_year)
    .bind(period_end)
    .execute(pool)
    .await
    .expect("period_snapshots へのテスト用INSERTが失敗しないこと");
}

/// `journal_entries` の総行数を数える（副作用の有無をDBを見て確認するため）。
async fn count_journal_entries(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM journal_entries")
        .fetch_one(pool)
        .await
        .expect("journal_entries の件数取得が失敗しないこと")
}

/// `store` に対して1回分の `find_entry` を実行する。
async fn find_entry(store: &PgStore, id: EntryId) -> Option<JournalEntry> {
    with_tx(store, |tx| {
        Box::pin(async move { Ok(tx.find_entry(id).await?) })
    })
    .await
    .expect("find_entry を含むトランザクションが失敗しないこと")
}

/// `store` に対して1回分の `post_entry::execute` を実行する。
async fn run_post_entry<P>(
    store: &PgStore,
    tax: P,
    schema: TagSchema,
    id_gen: Arc<SequentialIdGenerator>,
    input: PostEntryInput,
) -> Result<JournalEntry, AppError>
where
    P: TaxPolicy + 'static,
{
    let clock = clock();
    let settings = settings();
    with_tx(store, |tx| {
        Box::pin(async move {
            post_entry::execute(tx, &tax, &schema, &*id_gen, &clock, &settings, input).await
        })
    })
    .await
}

/// `store` に対して1回分の `reverse_entry::execute` を実行する。
async fn run_reverse_entry(
    store: &PgStore,
    schema: TagSchema,
    id_gen: Arc<SequentialIdGenerator>,
    input: ReverseEntryInput,
) -> Result<JournalEntry, AppError> {
    let clock = clock();
    let settings = settings();
    with_tx(store, |tx| {
        Box::pin(async move {
            reverse_entry::execute(tx, &schema, &*id_gen, &clock, &settings, input).await
        })
    })
    .await
}

// ---- E2E-01 ----

/// E2E-01 / 完了条件1（再起動してもデータが残る）の代理検証。
///
/// `post_entry::execute` で記帳した直後の `store`（`roles.app` に基づくプール）
/// ではなく、**新規に張り直した別プール**（`common::app_pool` で得る別コネクション）
/// から `find_entry` で読めることを確認する。同一プールのメモリキャッシュでは
/// なく実際にDBへ保存されていることの証明であり、docker volume 自体の永続性
/// （コンテナ再起動）は本テストの対象外（`README.md`「ローカル開発環境」を参照）。
#[sqlx::test]
async fn posted_entry_is_readable_from_a_freshly_reconnected_pool(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts.clone()).await;
    seed_basic_accounts(&roles.migrator).await;
    let store = PgStore::new(roles.app);
    // 仕訳IDは1つのジェネレータを全呼び出しで共有する。呼び出しごとに新しい
    // ジェネレータを作ると、成功した記帳が同じIDを引いて journal_entries_pkey の
    // 一意制約違反になる（レビューで実際に踏んだ）。
    let ids = Arc::new(SequentialIdGenerator::starting_at(1));

    let input = PostEntryInput {
        entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
        description: "現金売上".to_string(),
        lines: balanced_lines(10_000),
        auto_tax_lines: false,
    };
    let posted = run_post_entry(&store, NoTaxPolicy, TagSchema::empty(), ids.clone(), input)
        .await
        .unwrap();

    // ここで `store`/`roles.app` を一切使わず、appロールのプールだけを張り直す。
    let fresh_store = PgStore::new(common::app_pool(conn_opts).await);

    let found = find_entry(&fresh_store, posted.id())
        .await
        .expect("新しく張り直したプールからも保存済みの仕訳が読めること");
    assert_eq!(
        found.id(),
        posted.id(),
        "張り直したプールから読んだ仕訳のIDが記帳時と異なる"
    );
    assert_eq!(
        found.entry_no().as_u32(),
        1,
        "張り直したプールから読んだ仕訳の entry_no が1でない: {found:?}"
    );
}

// ---- E2E-02 ----

/// E2E-02 / 完了条件4: `post_entry::execute` で記帳した仕訳が
/// `report::execute`（SQL集計のread model）に反映され、借方合計・貸方合計が
/// 一致すること。
#[sqlx::test]
async fn posted_entry_appears_in_trial_balance_with_matching_totals(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    seed_basic_accounts(&roles.migrator).await;
    let store = PgStore::new(roles.app.clone());
    // 仕訳IDは1つのジェネレータを全呼び出しで共有する。呼び出しごとに新しい
    // ジェネレータを作ると、成功した記帳が同じIDを引いて journal_entries_pkey の
    // 一意制約違反になる（レビューで実際に踏んだ）。
    let ids = Arc::new(SequentialIdGenerator::starting_at(1));

    let input = PostEntryInput {
        entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
        description: "現金売上".to_string(),
        lines: balanced_lines(50_000),
        auto_tax_lines: false,
    };
    run_post_entry(&store, NoTaxPolicy, TagSchema::empty(), ids.clone(), input)
        .await
        .unwrap();

    let query = PgTrialBalanceQuery::new(roles.app);
    let report_input = ReportInput {
        from: AccountingDate::new(2026, 1, 1).unwrap(),
        to: AccountingDate::new(2026, 12, 31).unwrap(),
        group_by: Vec::new(),
    };
    let view = report::execute(&query, &TagSchema::empty(), report_input)
        .await
        .unwrap();

    assert_eq!(view.rows().len(), 2);
    let (debit, credit) = view.totals().unwrap().unwrap();
    assert_eq!(debit.minor(), credit.minor());
    assert_eq!(debit.minor(), 50_000);
}

// ---- E2E-03 ----

/// E2E-03 / 完了条件3+4: `reverse_entry::execute` による逆仕訳が正しく
/// 記録され、`report::execute` で元仕訳と相殺されて残高が0になる。逆仕訳の
/// `reverses`/`reverse_reason` がDBに保存され、読み戻せることも確認する。
#[sqlx::test]
async fn reversed_entry_offsets_original_in_trial_balance_and_persists_reversal_fields(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    seed_basic_accounts(&roles.migrator).await;
    let store = PgStore::new(roles.app.clone());
    // 仕訳IDは1つのジェネレータを全呼び出しで共有する。呼び出しごとに新しい
    // ジェネレータを作ると、成功した記帳が同じIDを引いて journal_entries_pkey の
    // 一意制約違反になる（レビューで実際に踏んだ）。
    let ids = Arc::new(SequentialIdGenerator::starting_at(1));

    let post_input = PostEntryInput {
        entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
        description: "現金売上".to_string(),
        lines: balanced_lines(30_000),
        auto_tax_lines: false,
    };
    let original = run_post_entry(
        &store,
        NoTaxPolicy,
        TagSchema::empty(),
        ids.clone(),
        post_input,
    )
    .await
    .unwrap();

    let reverse_input = ReverseEntryInput {
        original_id: original.id(),
        reverse_date: AccountingDate::new(2026, 4, 5).unwrap(),
        reason: "入力誤りのため取消".to_string(),
        allow_double_reversal: false,
    };
    let reversal = run_reverse_entry(&store, TagSchema::empty(), ids.clone(), reverse_input)
        .await
        .unwrap();

    // 逆仕訳の reverses/reverse_reason がDBに保存され、読み戻せること。
    let found_reversal = find_entry(&store, reversal.id())
        .await
        .expect("保存した逆仕訳が見つかること");
    assert_eq!(found_reversal.reverses(), Some(original.id()));
    assert_eq!(found_reversal.reverse_reason(), Some("入力誤りのため取消"));

    let query = PgTrialBalanceQuery::new(roles.app);
    let report_input = ReportInput {
        from: AccountingDate::new(2026, 1, 1).unwrap(),
        to: AccountingDate::new(2026, 12, 31).unwrap(),
        group_by: Vec::new(),
    };
    let view = report::execute(&query, &TagSchema::empty(), report_input)
        .await
        .unwrap();

    // 元仕訳と逆仕訳が相殺し、各科目とも借方合計=貸方合計=残高0になる。
    for row in view.rows() {
        assert_eq!(
            row.debit_total.minor(),
            row.credit_total.minor(),
            "account={:?}",
            row.account
        );
        assert_eq!(row.balance.minor(), 0, "account={:?}", row.account);
    }
    let (debit, credit) = view.totals().unwrap().unwrap();
    assert_eq!(debit.minor(), credit.minor());
}

// ---- E2E-04 ----

/// E2E-04: 二重訂正の拒否。同じ仕訳に2回 `reverse_entry::execute` すると
/// `AppError::AlreadyReversed` になり、2本目の逆仕訳がDBに入っていないこと
/// （エラーで返るだけでなく、副作用が無いことをDBを見て確認する）。
/// エラーメッセージに次の一手（`allow_double_reversal` の指定方法）が
/// 含まれることも確認する（`CLAUDE.md` §11）。
#[sqlx::test]
async fn double_reversal_is_rejected_and_leaves_no_second_reversal_in_db(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    seed_basic_accounts(&roles.migrator).await;
    let store = PgStore::new(roles.app.clone());
    // 仕訳IDは1つのジェネレータを全呼び出しで共有する。呼び出しごとに新しい
    // ジェネレータを作ると、成功した記帳が同じIDを引いて journal_entries_pkey の
    // 一意制約違反になる（レビューで実際に踏んだ）。
    let ids = Arc::new(SequentialIdGenerator::starting_at(1));

    let post_input = PostEntryInput {
        entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
        description: "現金売上".to_string(),
        lines: balanced_lines(1_000),
        auto_tax_lines: false,
    };
    let original = run_post_entry(
        &store,
        NoTaxPolicy,
        TagSchema::empty(),
        ids.clone(),
        post_input,
    )
    .await
    .unwrap();

    let first_reverse = ReverseEntryInput {
        original_id: original.id(),
        reverse_date: AccountingDate::new(2026, 4, 5).unwrap(),
        reason: "1回目の取消".to_string(),
        allow_double_reversal: false,
    };
    run_reverse_entry(&store, TagSchema::empty(), ids.clone(), first_reverse)
        .await
        .unwrap();
    assert_eq!(count_journal_entries(&roles.app).await, 2);

    let second_reverse = ReverseEntryInput {
        original_id: original.id(),
        reverse_date: AccountingDate::new(2026, 4, 10).unwrap(),
        reason: "2回目の取消".to_string(),
        allow_double_reversal: false,
    };
    let second_result =
        run_reverse_entry(&store, TagSchema::empty(), ids.clone(), second_reverse).await;

    match &second_result {
        Err(AppError::AlreadyReversed { .. }) => {}
        other => panic!("AppError::AlreadyReversed を期待しましたが: {other:?}"),
    }
    let message = second_result.unwrap_err().to_string();
    assert!(
        message.contains("allow_double_reversal"),
        "次の一手（allow_double_reversalの指定）がメッセージに含まれること: {message}"
    );

    // 2本目の逆仕訳はDBに入っていない（件数が2のまま増えていない）。
    assert_eq!(count_journal_entries(&roles.app).await, 2);
}

// ---- E2E-05 ----

/// E2E-05: `auto_tax_lines: true` と、ローカル定義の `FlatRateTaxPolicy`
/// （外税10%を1行追加する最小実装）を使い、税額行が自動生成されてDBに
/// 保存され、貸借が一致したまま保存されることを確認する（税行を足しても
/// 貸借が壊れないこと）。
#[sqlx::test]
async fn auto_tax_lines_generates_balanced_tax_line_and_persists_it(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    seed_basic_accounts(&roles.migrator).await;
    seed_tax_account(&roles.migrator).await;
    let store = PgStore::new(roles.app);
    // 仕訳IDは1つのジェネレータを全呼び出しで共有する。呼び出しごとに新しい
    // ジェネレータを作ると、成功した記帳が同じIDを引いて journal_entries_pkey の
    // 一意制約違反になる（レビューで実際に踏んだ）。
    let ids = Arc::new(SequentialIdGenerator::starting_at(1));

    let tax = FlatRateTaxPolicy {
        rate: Ratio::parse_rate("0.10").unwrap(),
        tax_account: "330",
    };
    let input = PostEntryInput {
        entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
        description: "税抜経理".to_string(),
        lines: balanced_lines(100_000),
        auto_tax_lines: true,
    };
    let posted = run_post_entry(&store, tax, TagSchema::empty(), ids.clone(), input)
        .await
        .unwrap();

    assert_eq!(posted.lines().len(), 4);
    assert_eq!(posted.debit_total().minor(), posted.credit_total().minor());
    assert_eq!(posted.credit_total().minor(), 110_000);

    // DBに保存された後も、税額行を含めて貸借が一致したままであること。
    let found = find_entry(&store, posted.id())
        .await
        .expect("保存した仕訳が見つかること");
    assert_eq!(found.lines().len(), 4);
    assert_eq!(found.debit_total().minor(), found.credit_total().minor());
    assert_eq!(found.credit_total().minor(), 110_000);
}

// ---- E2E-06 ----

/// E2E-06: 締め済み期間への記帳が `AppError::Core`（`CoreError::PeriodClosed`）
/// で拒否され、DBに1件も入らないこと。`period_snapshots` に行を入れて
/// `closed_through` を効かせる。
#[sqlx::test]
async fn posting_into_a_closed_period_is_rejected_and_writes_nothing(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    seed_basic_accounts(&roles.migrator).await;
    close_period(&roles.migrator, 2026, "2026-03-31").await;
    let store = PgStore::new(roles.app.clone());
    // 仕訳IDは1つのジェネレータを全呼び出しで共有する。呼び出しごとに新しい
    // ジェネレータを作ると、成功した記帳が同じIDを引いて journal_entries_pkey の
    // 一意制約違反になる（レビューで実際に踏んだ）。
    let ids = Arc::new(SequentialIdGenerator::starting_at(1));

    let input = PostEntryInput {
        entry_date: AccountingDate::new(2026, 1, 15).unwrap(),
        description: "締め後の記帳".to_string(),
        lines: balanced_lines(1_000),
        auto_tax_lines: false,
    };
    let result = run_post_entry(&store, NoTaxPolicy, TagSchema::empty(), ids.clone(), input).await;

    assert!(matches!(
        result,
        Err(AppError::Core(CoreError::PeriodClosed { .. }))
    ));
    assert_eq!(
        count_journal_entries(&roles.app).await,
        0,
        "拒否された記帳が journal_entries に行を残している"
    );
}

// ---- E2E-07 ----

/// E2E-07: 貸借不一致の入力が `JournalEntry::new`（core）で弾かれ、DBに
/// 到達しないこと（`journal_entries` の件数が0のまま）。append-only トリガ
/// （P0011）に頼らず core 自身が止めていることの証明。エラーメッセージが
/// 具体的な借方・貸方・差額を含む（`CLAUDE.md` §11、`DECISIONS.md` D-019）
/// ことも確認する。
#[sqlx::test]
async fn unbalanced_input_is_rejected_by_core_and_writes_nothing(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    seed_basic_accounts(&roles.migrator).await;
    let store = PgStore::new(roles.app.clone());
    // 仕訳IDは1つのジェネレータを全呼び出しで共有する。呼び出しごとに新しい
    // ジェネレータを作ると、成功した記帳が同じIDを引いて journal_entries_pkey の
    // 一意制約違反になる（レビューで実際に踏んだ）。
    let ids = Arc::new(SequentialIdGenerator::starting_at(1));

    let unbalanced = vec![
        JournalLine::new(
            AccountCode::parse("100").unwrap(),
            Side::Debit,
            Money::from_minor(1_100, Currency::JPY),
            TagSet::new(),
            None,
        )
        .unwrap(),
        JournalLine::new(
            AccountCode::parse("500").unwrap(),
            Side::Credit,
            Money::from_minor(1_000, Currency::JPY),
            TagSet::new(),
            None,
        )
        .unwrap(),
    ];
    let input = PostEntryInput {
        entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
        description: "貸借不一致".to_string(),
        lines: unbalanced,
        auto_tax_lines: false,
    };
    let result = run_post_entry(&store, NoTaxPolicy, TagSchema::empty(), ids.clone(), input).await;

    match &result {
        Err(AppError::Core(CoreError::Unbalanced {
            debit,
            credit,
            diff,
        })) => {
            assert_eq!(debit, "1,100");
            assert_eq!(credit, "1,000");
            assert_eq!(diff, "100");
        }
        other => panic!("CoreError::Unbalanced を期待しましたが: {other:?}"),
    }
    assert_eq!(
        count_journal_entries(&roles.app).await,
        0,
        "拒否された記帳が journal_entries に行を残している"
    );
}

// ---- E2E-08 ----

/// E2E-08 / `DECISIONS.md` D-023: 採番の連続性。`post_entry::execute` を
/// 3回成功させると `entry_no` が 1, 2, 3 になる。さらに、締め済み期間・
/// 貸借不一致という2種類の失敗した記帳が採番を消費しないことを確認する
/// （採番と仕訳INSERTを同一トランザクションで行っている証明）。
#[sqlx::test]
async fn entry_numbers_are_contiguous_and_failed_postings_do_not_consume_numbers(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    seed_basic_accounts(&roles.migrator).await;
    close_period(&roles.migrator, 2026, "2026-01-31").await;
    let store = PgStore::new(roles.app);
    // 仕訳IDは1つのジェネレータを全呼び出しで共有する。呼び出しごとに新しい
    // ジェネレータを作ると、成功した記帳が同じIDを引いて journal_entries_pkey の
    // 一意制約違反になる（レビューで実際に踏んだ）。
    let ids = Arc::new(SequentialIdGenerator::starting_at(1));

    // 検証したいのは `entry_no`（DB側の採番）の連続性であって仕訳IDではない。

    // 1件目（成功）: entry_no = 1。
    let first = run_post_entry(
        &store,
        NoTaxPolicy,
        TagSchema::empty(),
        ids.clone(),
        PostEntryInput {
            entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
            description: "1件目".to_string(),
            lines: balanced_lines(1_000),
            auto_tax_lines: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(first.entry_no().as_u32(), 1, "1件目の entry_no が1でない");

    // 失敗1: 締め済み期間（同じ会計年度2026だが、採番を消費しないはず）。
    let closed_attempt = run_post_entry(
        &store,
        NoTaxPolicy,
        TagSchema::empty(),
        ids.clone(),
        PostEntryInput {
            entry_date: AccountingDate::new(2026, 1, 15).unwrap(),
            description: "締め後".to_string(),
            lines: balanced_lines(1_000),
            auto_tax_lines: false,
        },
    )
    .await;
    assert!(matches!(
        closed_attempt,
        Err(AppError::Core(CoreError::PeriodClosed { .. }))
    ));

    // 失敗2: 貸借不一致（採番を消費しないはず）。
    let unbalanced_attempt = run_post_entry(
        &store,
        NoTaxPolicy,
        TagSchema::empty(),
        ids.clone(),
        PostEntryInput {
            entry_date: AccountingDate::new(2026, 4, 2).unwrap(),
            description: "不一致".to_string(),
            lines: vec![
                JournalLine::new(
                    AccountCode::parse("100").unwrap(),
                    Side::Debit,
                    Money::from_minor(2_000, Currency::JPY),
                    TagSet::new(),
                    None,
                )
                .unwrap(),
                JournalLine::new(
                    AccountCode::parse("500").unwrap(),
                    Side::Credit,
                    Money::from_minor(1_000, Currency::JPY),
                    TagSet::new(),
                    None,
                )
                .unwrap(),
            ],
            auto_tax_lines: false,
        },
    )
    .await;
    assert!(matches!(
        unbalanced_attempt,
        Err(AppError::Core(CoreError::Unbalanced { .. }))
    ));

    // 2件目（成功）: 上記2つの失敗はどちらも採番を進めていないため、
    // entry_no は 4 ではなく 2 になる。
    let second = run_post_entry(
        &store,
        NoTaxPolicy,
        TagSchema::empty(),
        ids.clone(),
        PostEntryInput {
            entry_date: AccountingDate::new(2026, 4, 3).unwrap(),
            description: "2件目".to_string(),
            lines: balanced_lines(1_000),
            auto_tax_lines: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        second.entry_no().as_u32(),
        2,
        "失敗した記帳2件が採番を消費している（2件目の entry_no が2でない）"
    );

    // 3件目（成功）: entry_no = 3。
    let third = run_post_entry(
        &store,
        NoTaxPolicy,
        TagSchema::empty(),
        ids.clone(),
        PostEntryInput {
            entry_date: AccountingDate::new(2026, 4, 4).unwrap(),
            description: "3件目".to_string(),
            lines: balanced_lines(1_000),
            auto_tax_lines: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(third.entry_no().as_u32(), 3, "3件目の entry_no が3でない");
}

// ---- E2E-09 ----

/// E2E-09: `report::execute` の `group_by` に集計不可（`aggregatable: false`）
/// のタグキーを渡すと `CoreError::NotAggregatable` でSQL到達前に弾かれる。
///
/// **接続できないDBを指す `PgTrialBalanceQuery` を渡す**ことで「SQLに到達しない」
/// ことを証明する。もし検証が SQL の後に回っていれば、`NotAggregatable` ではなく
/// 接続エラー（`RepoError::Backend`）が返るため、このテストは失敗する。
/// `#[sqlx::test]` を使わないのは、使い捨てDBの作成とマイグレーション適用の
/// コストを払っても**このテストは一度もDBに触れない**ため（レビュー指摘）。
/// `sqlx` の遅延接続（`connect_lazy_with`）は最初のクエリまで接続を張らない。
#[tokio::test]
async fn report_rejects_non_aggregatable_group_by_before_reaching_sql() {
    // ポート1・存在しないDB名・存在しないロール。実際に繋ぎに行けば必ず失敗する。
    let unreachable: PgConnectOptions = "postgres://nobody:nobody@127.0.0.1:1/nonexistent"
        .parse()
        .expect("接続文字列のパースは成功すること");
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_lazy_with(unreachable);
    let query = PgTrialBalanceQuery::new(pool);

    let schema = TagSchema::new(vec![(
        TagKey::parse("business_ratio").unwrap(),
        TagDef {
            value_type: TagValueType::Decimal,
            aggregatable: false,
            required_for: Vec::new(),
        },
    )]);

    let input = ReportInput {
        from: AccountingDate::new(2026, 1, 1).unwrap(),
        to: AccountingDate::new(2026, 12, 31).unwrap(),
        group_by: vec![TagKey::parse("business_ratio").unwrap()],
    };

    let result = report::execute(&query, &schema, input).await;

    assert!(
        matches!(
            result,
            Err(AppError::Core(CoreError::NotAggregatable { .. }))
        ),
        "SQL到達前に NotAggregatable で弾かれること（接続エラーが返るなら検証の順序が誤っている）: {result:?}"
    );
}

// ---- E2E-10 ----

/// E2E-10 / 完了条件5の実証: `with_tx` のロールバック。トランザクション内で
/// `post_entry::execute`（`JournalRepo`/`ChartRepo`/`PeriodRepo`/
/// `NumberingRepo` を横断する）を成功させた後、同じ `&mut Tx` に対して
/// さらに別リポジトリ（`JournalRepo::find_entry`/`ChartRepo::load_chart`）を
/// 直接呼び、最後に意図的にエラーを返すと、仕訳も採番カウンタも巻き戻る
/// （`&mut Tx` を引数で引き回す設計＝借用チェッカとの相性が良いことの実証。
/// `DECISIONS.md` D-029）。
#[sqlx::test]
async fn with_tx_rolls_back_journal_and_numbering_together(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    seed_basic_accounts(&roles.migrator).await;
    let store = PgStore::new(roles.app.clone());
    // 仕訳IDは1つのジェネレータを全呼び出しで共有する。呼び出しごとに新しい
    // ジェネレータを作ると、成功した記帳が同じIDを引いて journal_entries_pkey の
    // 一意制約違反になる（レビューで実際に踏んだ）。
    let ids = Arc::new(SequentialIdGenerator::starting_at(1));

    let tax = NoTaxPolicy;
    let schema = TagSchema::empty();
    let id_gen = Arc::clone(&ids);
    let clock = clock();
    let settings = settings();
    let input = PostEntryInput {
        entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
        description: "ロールバック対象".to_string(),
        lines: balanced_lines(1_000),
        auto_tax_lines: false,
    };

    let result: Result<(), AppError> = with_tx(&store, |tx| {
        Box::pin(async move {
            let entry =
                post_entry::execute(tx, &tax, &schema, &*id_gen, &clock, &settings, input).await?;
            // 複数リポジトリを跨ぐ呼び出し（JournalRepo::find_entry /
            // ChartRepo::load_chart）が同じ `&mut Tx` に対して続けて書けることの確認。
            let _ = tx.find_entry(entry.id()).await?;
            let _ = tx.load_chart().await?;
            Err(AppError::Rejected {
                reason: "E2E-10: 意図的なロールバック".to_string(),
            })
        })
    })
    .await;
    assert!(result.is_err());

    // 仕訳がロールバックされている（DBに1件も残らない）。
    assert_eq!(
        count_journal_entries(&roles.app).await,
        0,
        "拒否された記帳が journal_entries に行を残している"
    );

    // 採番カウンタもロールバックされている（次の記帳は entry_no = 1 になる）。
    let retried = run_post_entry(
        &store,
        NoTaxPolicy,
        TagSchema::empty(),
        ids.clone(),
        PostEntryInput {
            entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
            description: "ロールバック後の記帳".to_string(),
            lines: balanced_lines(1_000),
            auto_tax_lines: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        retried.entry_no().as_u32(),
        1,
        "ロールバック後の記帳で採番カウンタが巻き戻っていない"
    );
}
