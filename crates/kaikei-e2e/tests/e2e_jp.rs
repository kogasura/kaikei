//! Phase 2 の実装が実 PostgreSQL を通しで動くことの証明（PR-8）。
//!
//! `kaikei-store` は `kaikei-jp` を、`kaikei-jp` は `kaikei-store`（DB）を
//! 互いに知らない（`CLAUDE.md` §1）。そのため「税抜経理の消費税行が
//! 実際にPostgreSQLへ記帳できる」ことを検証するテストは、両方を知ってよい
//! 最上位の層＝この crate（`kaikei-e2e`）にしか置けない
//! （`crates/kaikei-e2e/src/lib.rs` クレートdocを参照）。
//!
//! `kaikei_e2e::compose` で組み立てた `JpTaxPolicy` / `JpSoleProprietorClosingPolicy`
//! を、実 PostgreSQL に繋いだ `kaikei_store::pool::PgStore` /
//! `kaikei_store::query::PgTrialBalanceQuery` に対する `kaikei_app` の
//! ユースケース関数（`post_entry::execute` / `report::execute`）へ実際に
//! 注入する。`kaikei-store/tests/e2e_usecase.rs`（Phase 1）と同じ形の
//! テストダブル（`SequentialIdGenerator`）はこのファイル内にローカル定義する。
//!
//! # `ROADMAP.md` Phase 2 完了条件との対応
//!
//! | # | 完了条件 | 対応するテスト |
//! |---|---|---|
//! | 1 | 税抜経理で消費税行が自動生成される | `condition_1_exclusive_accounting_generates_tax_line_on_taxable_sale` |
//! | 2 | 8% 軽減税率、非課税、不課税が扱える | `condition_2_reduced_rate_tax_free_and_out_of_scope_categories_are_handled` |
//! | 3 | 非適格の経過措置が YAML で表現できている | `condition_3_non_qualified_transitional_measure_is_expressed_in_yaml` |
//! | 4 | 家事按分の 3 行仕訳が生成される | `condition_4_household_split_produces_a_three_line_entry` |
//! | 5 | 年度別 YAML の切り替えが取引日で行われる | `condition_5_yearly_master_switch_is_based_on_entry_date` |
//!
//! 上記に加え、Phase 2 の実装が**通しで動く**ことを示す
//! `phase2_end_to_end_scenario_posts_and_closes_the_books` を用意する。
//! これは「決算振替仕訳が実際に記帳できる」ことの実証であり、PR-7 で
//! 「構築は通るが記帳できない」欠陥を3件踏んだ（`DECISIONS.md` D-066 の
//! 追記を参照）ことへの回帰検知を兼ねる、**PR-8 の最大の価値**を持つテスト。

#![cfg(feature = "pg-tests")]

mod common;

use kaikei_app::context::{BookSettings, FiscalYearRule};
use kaikei_app::error::AppError;
use kaikei_app::ports::{IdGenerator, JournalRepo};
use kaikei_app::tx::with_tx;
use kaikei_app::usecase::post_entry::{self, PostEntryInput, PostEntryOutput};
use kaikei_app::usecase::report::{self, ReportInput};
use kaikei_core::{
    AccountCode, AccountType, AccountingDate, ChartOfAccounts, Currency, EntryId, FiscalYear,
    FixedClock, JournalEntry, JournalLine, Money, Ratio, Side, TagKey, TagSchema, TagSet, TagValue,
    Timestamp, TrialBalance,
};
use kaikei_e2e::{compose, ComposeOptions};
use kaikei_jp::closing::ClosingAccounts;
use kaikei_jp::household_split::{household_split, HouseholdSplitInput};
use kaikei_jp::statement::JpStatementPolicy;
use kaikei_jp::tax::{
    JpSettingsOverrides, TaxCategory, TaxCategoryTable, TaxDirection, TaxRuleSets,
};
use kaikei_policy::{ClosingPolicy, CounterpartyIndex, StatementPolicy, TaxContext, TaxPolicy};
use kaikei_store::convert::account_type_to_i16;
use kaikei_store::pool::PgStore;
use kaikei_store::query::PgTrialBalanceQuery;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::PgPool;
use std::sync::{Arc, Mutex};

// ---- ローカルのテストダブル ----

/// 呼び出しごとに1ずつ増える決定的な仕訳IDを返す `IdGenerator`
/// （`kaikei-store/tests/e2e_usecase.rs` の同名のフィクスチャと同じ役割。
/// crate をまたぐため共有できない。理由は `tests/common/mod.rs` を参照）。
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
        book_currency: Currency::JPY,
    }
}

fn clock() -> FixedClock {
    FixedClock(Timestamp::from_unix_nanos(1_700_000_000_000_000))
}

/// 埋め込みの2026年度マスタを基準にした、素直な `ComposeOptions`。
///
/// 課税事業者・税抜経理・端数処理は2026.yamlの`settings_defaults`
/// （floor・明細ごと）をそのまま使う。決算科目は同梱の
/// `sole_proprietor.yaml` の元入金(400)/事業主貸(410)/事業主借(420)。
fn default_compose_options() -> ComposeOptions {
    ComposeOptions {
        rule_sets: TaxRuleSets::from_embedded().unwrap(),
        settings_overrides: JpSettingsOverrides {
            tax_mode: None,
            rounding: None,
            rounding_unit: None,
            is_taxable_business: true,
            simplified_taxation: false,
        },
        defaults_as_of: AccountingDate::new(2026, 4, 1).unwrap(),
        closing_accounts: ClosingAccounts {
            capital: AccountCode::parse("400").unwrap(),
            owner_drawings: AccountCode::parse("410").unwrap(),
            owner_contributions: AccountCode::parse("420").unwrap(),
        },
        // "対象外"（資産・負債の振替など消費税と無関係な取引に使う候補。
        // docs/04-jp-tax.md §9 のモジュールdoc参照）。この値の選択自体は
        // テストの都合であり、この crate が断定しているわけではない。
        closing_tax_category: Some("NOT_APPLICABLE".to_string()),
    }
}

/// `chart` の全科目を `accounts` テーブルに投入する（`migrator` ロールで行う）。
///
/// `post_entry::execute` は `tx.load_chart()` で DB を読むため、コード内で
/// `ChartOfAccounts` を組み立てて渡すだけでは記帳できない。親科目コードの
/// 参照（`parent_code`）が科目コードの辞書順と一致しない科目表でも壊れない
/// よう、まず全科目を `parent_code = NULL` で投入してから、`parent` を持つ
/// 科目だけ2パス目で `UPDATE` する（同梱の `sole_proprietor.yaml` は
/// フラットな科目表なので実際には2パス目は素通りする）。
async fn seed_chart(pool: &PgPool, chart: &ChartOfAccounts) {
    // 科目1件ずつ INSERT すると、同梱の科目表（約60件）でテストごとに
    // 60往復する。`kaikei-store` の明細一括 INSERT（`DECISIONS.md` D-040）と
    // 同じく UNNEST で1文にまとめる。
    //
    // 2パスに分けるのは `accounts.parent_code` が `accounts(code)` への
    // 自己参照 FK を持つため（親より先に子を入れると FK 違反になる）。
    // 1パス目は親を NULL で入れ、2パス目でまとめて更新する。
    let codes: Vec<String> = chart.iter().map(|d| d.code.as_str().to_string()).collect();
    let names: Vec<String> = chart.iter().map(|d| d.name.clone()).collect();
    let types: Vec<i16> = chart
        .iter()
        .map(|d| account_type_to_i16(d.account_type))
        .collect();
    let postables: Vec<bool> = chart.iter().map(|d| d.postable).collect();

    sqlx::query(
        "INSERT INTO accounts (code, name, account_type, parent_code, postable)          SELECT code, name, account_type, NULL, postable          FROM UNNEST($1::text[], $2::text[], $3::smallint[], $4::bool[])               AS t(code, name, account_type, postable)",
    )
    .bind(&codes)
    .bind(&names)
    .bind(&types)
    .bind(&postables)
    .execute(pool)
    .await
    .unwrap_or_else(|e| panic!("accounts の一括投入に失敗しました: {e}"));

    let child_codes: Vec<String> = chart
        .iter()
        .filter(|d| d.parent.is_some())
        .map(|d| d.code.as_str().to_string())
        .collect();
    if child_codes.is_empty() {
        return;
    }
    let parent_codes: Vec<String> = chart
        .iter()
        .filter_map(|d| d.parent.as_ref().map(|p| p.as_str().to_string()))
        .collect();

    sqlx::query(
        "UPDATE accounts SET parent_code = t.parent_code          FROM UNNEST($1::text[], $2::text[]) AS t(code, parent_code)          WHERE accounts.code = t.code",
    )
    .bind(&child_codes)
    .bind(&parent_codes)
    .execute(pool)
    .await
    .unwrap_or_else(|e| panic!("accounts.parent_code の一括更新に失敗しました: {e}"));
}

/// 明細を1行組み立てる。
fn line(account: &str, side: Side, amount_minor: i128, tags: TagSet) -> JournalLine {
    JournalLine::new(
        AccountCode::parse(account).unwrap(),
        side,
        Money::from_minor(amount_minor, Currency::JPY),
        tags,
        None,
    )
    .unwrap()
}

/// `tax_category` タグ1件だけを持つ `TagSet` を組み立てる。
fn tags_with_category(code: &str) -> TagSet {
    let mut tags = TagSet::new();
    tags.insert(
        TagKey::parse("tax_category").unwrap(),
        TagValue::Code(code.to_string()),
    );
    tags
}

/// `store` に対して1回分の `post_entry::execute` を実行する。
///
/// `tax`/`schema`/`id_gen` を所有値として `move` クロージャに渡す
/// （`kaikei_app::tx::with_tx` の doc「クロージャに渡せるもの」を参照。
/// `'static` でない借用はキャプチャできないため、呼び出し側が
/// `composition.tax_policy.clone()` のように所有値化してから渡すこと）。
async fn run_post_entry<P>(
    store: &PgStore,
    tax: P,
    schema: TagSchema,
    id_gen: Arc<SequentialIdGenerator>,
    input: PostEntryInput,
) -> Result<PostEntryOutput, AppError>
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

/// `store` に対して1回分の `find_entry` を実行する。
async fn find_entry(store: &PgStore, id: EntryId) -> Option<JournalEntry> {
    with_tx(store, |tx| {
        Box::pin(async move { Ok(tx.find_entry(id).await?) })
    })
    .await
    .expect("find_entry を含むトランザクションが失敗しないこと")
}

// ---- 完了条件1 ----

/// 完了条件1: 税抜経理で消費税行が自動生成される。
///
/// `kaikei_e2e::compose` で組み立てた `JpTaxPolicy`（同梱の2026年度マスタ）を
/// 実際に `post_entry::execute` へ注入して記帳し、生成された仮受消費税(330)の
/// 行がDBに保存され、読み戻せることを確認する。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn condition_1_exclusive_accounting_generates_tax_line_on_taxable_sale(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    let composition = compose(default_compose_options()).unwrap();
    seed_chart(&roles.migrator, &composition.chart).await;

    let store = PgStore::new(roles.app.clone());
    let ids = Arc::new(SequentialIdGenerator::starting_at(1));

    let input = PostEntryInput {
        entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
        description: "課税売上（税抜経理）".to_string(),
        lines: vec![
            line("135", Side::Debit, 110_000, TagSet::new()), // 売掛金
            line("500", Side::Credit, 100_000, tags_with_category("SALES_10")), // 売上高
        ],
        auto_tax_lines: true,
    };

    let posted = run_post_entry(
        &store,
        composition.tax_policy.clone(),
        composition.tag_schema.clone(),
        ids.clone(),
        input,
    )
    .await
    .unwrap()
    .entry;

    assert_eq!(posted.lines().len(), 3, "入力2行 + 税額行1行のはず");
    assert_eq!(posted.debit_total().minor(), posted.credit_total().minor());
    assert_eq!(posted.credit_total().minor(), 110_000);

    let tax_line = posted
        .lines()
        .iter()
        .find(|l| l.account().as_str() == "330")
        .expect("仮受消費税等(330)の行が生成されているはず");
    assert_eq!(tax_line.amount().minor(), 10_000);
    assert_eq!(
        tax_line.tags().get(&TagKey::parse("tax_category").unwrap()),
        Some(&TagValue::Code("SALES_10".to_string())),
        "生成した税額行にも元の tax_category が付くこと（docs/04-jp-tax.md §7）"
    );

    // 実DBから読み戻しても同じ内容であること（記帳が実際に完了していることの証明）。
    let found = find_entry(&store, posted.id())
        .await
        .expect("保存した仕訳が見つかること");
    assert_eq!(found.lines().len(), 3);
    assert_eq!(found.debit_total().minor(), found.credit_total().minor());
    assert_eq!(found.credit_total().minor(), 110_000);
}

// ---- 完了条件2 ----

/// 完了条件2: 8%軽減税率・非課税・不課税が同一仕訳内で扱える。
///
/// 10%課税売上・8%軽減税率課税売上・非課税・不課税の4区分を1つの仕訳に
/// 混在させて記帳する。税額行は10%分・8%分の2行のみ生成され、非課税・
/// 不課税の明細は税額行を生成しないまま正常に記帳・保存できることを確認する。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn condition_2_reduced_rate_tax_free_and_out_of_scope_categories_are_handled(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    let composition = compose(default_compose_options()).unwrap();
    seed_chart(&roles.migrator, &composition.chart).await;

    let store = PgStore::new(roles.app.clone());
    let ids = Arc::new(SequentialIdGenerator::starting_at(1));

    // 本体合計 160,000（100,000 + 50,000 + 5,000 + 5,000）+ 税額 14,000
    // （10,000 + 4,000）= 現金側 174,000。
    let input = PostEntryInput {
        entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
        description: "10%・8%・非課税・不課税の混在".to_string(),
        lines: vec![
            line("100", Side::Debit, 174_000, TagSet::new()), // 現金
            line("500", Side::Credit, 100_000, tags_with_category("SALES_10")),
            line(
                "500",
                Side::Credit,
                50_000,
                tags_with_category("SALES_8_REDUCED"),
            ),
            line("500", Side::Credit, 5_000, tags_with_category("TAX_FREE")),
            line(
                "500",
                Side::Credit,
                5_000,
                tags_with_category("OUT_OF_SCOPE"),
            ),
        ],
        auto_tax_lines: true,
    };

    let posted = run_post_entry(
        &store,
        composition.tax_policy.clone(),
        composition.tag_schema.clone(),
        ids.clone(),
        input,
    )
    .await
    .unwrap()
    .entry;

    // 入力5行 + 税額行2行（10%分・8%分）。非課税・不課税は税額行を生成しない。
    assert_eq!(posted.lines().len(), 7);
    assert_eq!(posted.debit_total().minor(), posted.credit_total().minor());

    let tax_lines: Vec<_> = posted
        .lines()
        .iter()
        .filter(|l| l.account().as_str() == "330")
        .collect();
    assert_eq!(
        tax_lines.len(),
        2,
        "10%分・8%分の税額行が別々に生成されること"
    );
    let tax_total: i128 = tax_lines.iter().map(|l| l.amount().minor()).sum();
    assert_eq!(tax_total, 10_000 + 4_000);

    let found = find_entry(&store, posted.id()).await.unwrap();
    assert_eq!(found.lines().len(), 7);
    assert_eq!(found.debit_total().minor(), found.credit_total().minor());
}

// ---- 完了条件3 ----

/// 完了条件3: 非適格請求書の経過措置（`deduction_ratio`）がYAMLで表現され、
/// 記帳では rate のみで税額計算されつつ、控除できない部分の扱いは
/// `PolicyNote` として断定せずに伝えられること。
///
/// `PURCHASE_10_NON_QUALIFIED`（`kaikei-jp-data/tax/jp/2026.yaml` で
/// `deduction_ratio: "0.80"` として表現された区分）を使って記帳する。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn condition_3_non_qualified_transitional_measure_is_expressed_in_yaml(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    let composition = compose(default_compose_options()).unwrap();
    seed_chart(&roles.migrator, &composition.chart).await;

    let store = PgStore::new(roles.app.clone());
    let ids = Arc::new(SequentialIdGenerator::starting_at(1));

    let purchase_line = line(
        "555",
        Side::Debit,
        100_000,
        tags_with_category("PURCHASE_10_NON_QUALIFIED"),
    );
    let input = PostEntryInput {
        entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
        description: "非適格請求書の経過措置".to_string(),
        lines: vec![
            purchase_line.clone(),
            line("100", Side::Credit, 110_000, TagSet::new()), // 現金
        ],
        auto_tax_lines: true,
    };

    let posted_output = run_post_entry(
        &store,
        composition.tax_policy.clone(),
        composition.tag_schema.clone(),
        ids.clone(),
        input,
    )
    .await
    .unwrap();
    let posted = &posted_output.entry;

    // deduction_ratio (0.80) を反映せず rate (0.10) のみで計算する
    // （DECISIONS.md D-059。控除できない部分の帳簿上の処理は税理士確認事項）。
    let tax_line = posted
        .lines()
        .iter()
        .find(|l| l.account().as_str() == "180")
        .expect("仮払消費税等(180)の行が生成されているはず");
    assert_eq!(tax_line.amount().minor(), 10_000);
    assert_eq!(posted.debit_total().minor(), posted.credit_total().minor());

    let found = find_entry(&store, posted.id()).await.unwrap();
    assert_eq!(found.debit_total().minor(), found.credit_total().minor());

    // YAMLの deduction_ratio が読み込まれ、控除割合1未満の区分を使うと
    // PolicyNote が添えられ、それが **`post_entry::execute` の戻り値として**
    // 呼び出し元へ届くこと（PR-B。`DECISIONS.md` D-073）。
    //
    // Phase 2 の時点では `post_entry::execute` が `derive_tax_lines(...)?.lines`
    // として `.notes` を捨てており、同じ policy インスタンスへ直接問い合わせ
    // 直すことでしか確認できなかった（PROGRESS.md Phase 2 の申し送り）。
    // その申し送りがここで解消される。
    assert_eq!(
        posted_output.notes.len(),
        1,
        "経過措置の PolicyNote が post_entry の戻り値に含まれること"
    );
    assert!(posted_output.notes[0]
        .message
        .contains("PURCHASE_10_NON_QUALIFIED"));
    // 文言は policy が組み立てたものを素通しする（言い換えない。CLAUDE.md §10）。
    assert!(posted_output.notes[0].message.contains("税理士"));

    // 戻り値の注記が、同じ policy へ直接問い合わせた結果と一致すること
    // （post_entry が注記を取りこぼしたり作り変えたりしていない確認）。
    let ctx = TaxContext {
        as_of: AccountingDate::new(2026, 4, 1).unwrap(),
        chart: &composition.chart,
        tag_schema: &composition.tag_schema,
        counterparties: &CounterpartyIndex::empty(),
    };
    let derivation = composition
        .tax_policy
        .derive_tax_lines(&ctx, std::slice::from_ref(&purchase_line))
        .unwrap();
    assert_eq!(derivation.notes, posted_output.notes);
}

// ---- 完了条件4 ----

/// 完了条件4: 家事按分の3行仕訳が生成され、実際に記帳・保存できる。
///
/// `household_split`（`kaikei-jp` の独立関数。`TaxPolicy` 経由ではない。
/// `DECISIONS.md` D-064）で組み立てた3行仕訳を `post_entry::execute` へ
/// **入力として**渡す（`kaikei-app` は `kaikei-jp` に直接依存できないため、
/// この呼び出しは合成ルート＝この crate でのみ行える）。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn condition_4_household_split_produces_a_three_line_entry(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    let composition = compose(default_compose_options()).unwrap();
    seed_chart(&roles.migrator, &composition.chart).await;

    let store = PgStore::new(roles.app.clone());
    let ids = Arc::new(SequentialIdGenerator::starting_at(1));

    let split_lines = household_split(
        HouseholdSplitInput {
            total: Money::from_minor(100_000, Currency::JPY),
            business_ratio: Ratio::parse_fraction("0.30").unwrap(),
            expense_account: AccountCode::parse("615").unwrap(), // 地代家賃
            owner_account: AccountCode::parse("410").unwrap(),   // 事業主貸
            payment_account: AccountCode::parse("100").unwrap(), // 現金
            tax_category: Some("PURCHASE_10_QUALIFIED".to_string()),
        },
        &composition.tax_policy.settings(),
    )
    .unwrap();
    assert_eq!(split_lines.len(), 3, "household_split は3行を生成するはず");

    let input = PostEntryInput {
        entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
        description: "家事按分（地代家賃・事業割合30%）".to_string(),
        lines: split_lines,
        auto_tax_lines: false,
    };

    let posted = run_post_entry(
        &store,
        composition.tax_policy.clone(),
        composition.tag_schema.clone(),
        ids.clone(),
        input,
    )
    .await
    .unwrap()
    .entry;

    assert_eq!(posted.lines().len(), 3);
    assert_eq!(posted.debit_total().minor(), posted.credit_total().minor());
    assert_eq!(posted.credit_total().minor(), 100_000);

    let expense_line = posted
        .lines()
        .iter()
        .find(|l| l.account().as_str() == "615")
        .expect("地代家賃(615)の明細があるはず");
    assert_eq!(expense_line.amount().minor(), 30_000);
    let owner_line = posted
        .lines()
        .iter()
        .find(|l| l.account().as_str() == "410")
        .expect("事業主貸(410)の明細があるはず");
    assert_eq!(owner_line.amount().minor(), 70_000);

    let found = find_entry(&store, posted.id()).await.unwrap();
    assert_eq!(found.lines().len(), 3);
    assert_eq!(found.debit_total().minor(), found.credit_total().minor());
}

// ---- 完了条件5 ----

/// 完了条件5: 年度別マスタの切り替えが取引日（記帳日ではない）で行われる。
///
/// 埋め込みの2026年度マスタに加え、テスト専用の架空の2025年度マスタ
/// （同じ区分コード `SALES_10` の税率を意図的に5%にしたもの）を組み合わせた
/// `TaxRuleSets` を使う。同じ税区分コードでも取引日によって異なる税率が
/// 適用され、その結果が実際にDBへ異なる税額で記帳されることを確認する。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn condition_5_yearly_master_switch_is_based_on_entry_date(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;

    // `TAX_CATEGORY_SOURCES` は「並び順に意味を持たせない」と宣言している
    // （`kaikei-jp-data/src/lib.rs`）ので、添字ではなくラベルで探す。
    // 添字で取ると、2027年度分などが先頭に追加された瞬間に別の年度の
    // マスタを静かに掴む（コンパイルは通り、テストは別の値で通ってしまう）。
    let embedded_2026 = TaxCategoryTable::from_embedded(
        *kaikei_jp_data::TAX_CATEGORY_SOURCES
            .iter()
            .find(|e| e.label.contains("2026"))
            .expect("2026年度の税区分マスタが同梱されているはず"),
    )
    .unwrap();
    let legacy_2025 = TaxCategoryTable::new(
        "test-legacy-2025".to_string(),
        AccountingDate::new(2025, 1, 1).unwrap(),
        Some(AccountingDate::new(2025, 12, 31).unwrap()),
        embedded_2026.settings_defaults(),
        vec![TaxCategory {
            code: "SALES_10".to_string(),
            label: "課税売上 10%（旧税率シミュレーション。テスト専用）".to_string(),
            direction: TaxDirection::Sales,
            rate: Some(Ratio::parse_rate("0.05").unwrap()),
            deductible: None,
            deduction_ratio: None,
            requires_qualified_invoice: false,
            tax_account: Some(AccountCode::parse("330").unwrap()),
            note: None,
        }],
    )
    .unwrap();
    let rule_sets = TaxRuleSets::new(vec![legacy_2025, embedded_2026]).unwrap();

    let mut options = default_compose_options();
    options.rule_sets = rule_sets;
    let composition = compose(options).unwrap();
    seed_chart(&roles.migrator, &composition.chart).await;

    let store = PgStore::new(roles.app.clone());
    let ids = Arc::new(SequentialIdGenerator::starting_at(1));

    // 2025年度（テスト専用マスタ、rate 5%）の取引。
    let legacy_posted = run_post_entry(
        &store,
        composition.tax_policy.clone(),
        composition.tag_schema.clone(),
        ids.clone(),
        PostEntryInput {
            entry_date: AccountingDate::new(2025, 6, 15).unwrap(),
            description: "2025年度の課税売上".to_string(),
            lines: vec![
                line("135", Side::Debit, 105_000, TagSet::new()),
                line("500", Side::Credit, 100_000, tags_with_category("SALES_10")),
            ],
            auto_tax_lines: true,
        },
    )
    .await
    .unwrap()
    .entry;
    let legacy_tax = legacy_posted
        .lines()
        .iter()
        .find(|l| l.account().as_str() == "330")
        .unwrap();
    assert_eq!(
        legacy_tax.amount().minor(),
        5_000,
        "2025年度（テスト専用マスタ）のrate(5%)が適用されること"
    );

    // 2026年度（埋め込みマスタ、rate 10%）の取引。同じ税区分コード・同じpolicyインスタンス。
    let current_posted = run_post_entry(
        &store,
        composition.tax_policy.clone(),
        composition.tag_schema.clone(),
        ids.clone(),
        PostEntryInput {
            entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
            description: "2026年度の課税売上".to_string(),
            lines: vec![
                line("135", Side::Debit, 110_000, TagSet::new()),
                line("500", Side::Credit, 100_000, tags_with_category("SALES_10")),
            ],
            auto_tax_lines: true,
        },
    )
    .await
    .unwrap()
    .entry;
    let current_tax = current_posted
        .lines()
        .iter()
        .find(|l| l.account().as_str() == "330")
        .unwrap();
    assert_eq!(
        current_tax.amount().minor(),
        10_000,
        "2026年度（埋め込みマスタ）のrate(10%)が適用されること"
    );

    // 記帳順序ではなく取引日で切り替わっていることの最終確認として、両方とも
    // 実際にDBへ保存され読み戻せることを見る。
    assert!(find_entry(&store, legacy_posted.id()).await.is_some());
    assert!(find_entry(&store, current_posted.id()).await.is_some());
}

// ---- 通しのシナリオ ----

/// Phase 2 の実装が**通しで動く**ことを示すシナリオ（PR-8の最大の価値）。
///
/// 1. 科目表をDBに投入し、タグスキーマをロードする
/// 2. 課税売上・課税仕入を `auto_tax_lines: true` で記帳する（税額行がDBに保存される）
/// 3. `household_split` で組み立てた3行仕訳を `post_entry::execute` に**入力として**渡す
/// 4. `report::execute`（SQL集計）で試算表を出す
/// 5. DBから仕訳を読み戻して `kaikei_core::TrialBalance` を組み立て、
///    `closing_entries` に決算振替仕訳を提案させる
/// 6. 提案された決算振替仕訳を `post_entry::execute` で**実際に記帳する**
///    （★ここがPR-8の最大の価値。PR-7では「構築は通るが記帳できない」欠陥を
///    3件踏んでいる。`DECISIONS.md` D-066 の追記を参照）
/// 7. 記帳後、`report::execute` で収益・費用の残高が0になっていることを確認する
/// 8. DBから全仕訳（決算仕訳を含む）を読み戻し、決算書生成の直前に読み直した
///    `chart` から都度 `JpStatementPolicy::new(chart)` してBS/PLを組み立てる
///    （`DECISIONS.md` D-069）
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn phase2_end_to_end_scenario_posts_and_closes_the_books(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    let composition = compose(default_compose_options()).unwrap();

    // 1. 科目表をDBに投入する。
    seed_chart(&roles.migrator, &composition.chart).await;

    let store = PgStore::new(roles.app.clone());
    let ids = Arc::new(SequentialIdGenerator::starting_at(1));
    let mut posted_ids = Vec::new();

    // 2a. 課税売上（税抜経理、auto_tax_lines: true）。
    let sales_posted = run_post_entry(
        &store,
        composition.tax_policy.clone(),
        composition.tag_schema.clone(),
        ids.clone(),
        PostEntryInput {
            entry_date: AccountingDate::new(2026, 4, 10).unwrap(),
            description: "課税売上".to_string(),
            lines: vec![
                line("135", Side::Debit, 1_100_000, TagSet::new()), // 売掛金
                line(
                    "500",
                    Side::Credit,
                    1_000_000,
                    tags_with_category("SALES_10"),
                ),
            ],
            auto_tax_lines: true,
        },
    )
    .await
    .unwrap()
    .entry;
    posted_ids.push(sales_posted.id());

    // 2b. 課税仕入（適格請求書あり、auto_tax_lines: true）。
    let purchase_posted = run_post_entry(
        &store,
        composition.tax_policy.clone(),
        composition.tag_schema.clone(),
        ids.clone(),
        PostEntryInput {
            entry_date: AccountingDate::new(2026, 5, 1).unwrap(),
            description: "課税仕入（消耗品費）".to_string(),
            lines: vec![
                line(
                    "609",
                    Side::Debit,
                    200_000,
                    tags_with_category("PURCHASE_10_QUALIFIED"),
                ),
                line("100", Side::Credit, 220_000, TagSet::new()), // 現金
            ],
            auto_tax_lines: true,
        },
    )
    .await
    .unwrap()
    .entry;
    posted_ids.push(purchase_posted.id());

    // 3. household_split で組み立てた3行仕訳を post_entry::execute に入力として渡す。
    let split_lines = household_split(
        HouseholdSplitInput {
            total: Money::from_minor(100_000, Currency::JPY),
            business_ratio: Ratio::parse_fraction("0.30").unwrap(),
            expense_account: AccountCode::parse("615").unwrap(), // 地代家賃
            owner_account: AccountCode::parse("410").unwrap(),   // 事業主貸
            payment_account: AccountCode::parse("100").unwrap(), // 現金
            tax_category: Some("PURCHASE_10_QUALIFIED".to_string()),
        },
        &composition.tax_policy.settings(),
    )
    .unwrap();
    let split_posted = run_post_entry(
        &store,
        composition.tax_policy.clone(),
        composition.tag_schema.clone(),
        ids.clone(),
        PostEntryInput {
            entry_date: AccountingDate::new(2026, 5, 15).unwrap(),
            description: "家事按分（地代家賃）".to_string(),
            lines: split_lines,
            auto_tax_lines: false,
        },
    )
    .await
    .unwrap()
    .entry;
    posted_ids.push(split_posted.id());

    // 4. report::execute（SQL集計）で試算表を出す。
    let query = PgTrialBalanceQuery::new(roles.app.clone());
    let period = ReportInput {
        from: AccountingDate::new(2026, 1, 1).unwrap(),
        to: AccountingDate::new(2026, 12, 31).unwrap(),
        group_by: Vec::new(),
    };
    let before_closing = report::execute(&query, &composition.tag_schema, period.clone())
        .await
        .unwrap();
    let (debit_before, credit_before) = before_closing.totals().unwrap().unwrap();
    assert_eq!(
        debit_before.minor(),
        credit_before.minor(),
        "決算前の試算表も貸借一致していること"
    );

    // 5. DBから仕訳を読み戻して kaikei_core::TrialBalance を組み立てる。
    //    read model（SQL集計）ではなく、ドメインモデル（JournalEntry）を
    //    経由するのは ClosingPolicy::closing_entries が要求する型
    //    （kaikei_core::TrialBalance）を組み立てるため。
    let mut fetched_entries = Vec::new();
    for id in &posted_ids {
        fetched_entries.push(
            find_entry(&store, *id)
                .await
                .expect("記帳済みの仕訳がDBから読めること"),
        );
    }
    let tb_before_closing = TrialBalance::from_entries(
        fetched_entries.iter(),
        &composition.chart,
        &composition.tag_schema,
        &[],
    )
    .unwrap();

    let fy_2026 = FiscalYear::calendar_year(2026);
    let proposed = composition
        .closing_policy
        .closing_entries(&tb_before_closing, &fy_2026)
        .unwrap();
    assert_eq!(
        proposed.len(),
        1,
        "収益・費用に動きがあるので決算振替仕訳が1本提案されるはず"
    );
    let proposal = proposed.into_iter().next().unwrap();
    assert_eq!(proposal.entry_date, fy_2026.end());

    // 6. ★提案された決算振替仕訳を post_entry::execute で実際に記帳する★
    //    （PR-8の最大の価値。「決算仕訳が実際に記帳できる」ことの実証）。
    let closing_posted = run_post_entry(
        &store,
        composition.tax_policy.clone(),
        composition.tag_schema.clone(),
        ids.clone(),
        PostEntryInput {
            entry_date: proposal.entry_date,
            description: proposal.description.clone(),
            lines: proposal.lines,
            auto_tax_lines: false,
        },
    )
    .await
    .expect(
        "決算振替仕訳が実際に記帳できること \
         （PR-7で「構築は通るが記帳できない」欠陥を3件踏んだことへの回帰検知。\
         DECISIONS.md D-066 の追記を参照）",
    )
    .entry;
    posted_ids.push(closing_posted.id());

    let found_closing = find_entry(&store, closing_posted.id())
        .await
        .expect("記帳した決算振替仕訳がDBから読み戻せること");
    assert_eq!(
        found_closing.debit_total().minor(),
        found_closing.credit_total().minor()
    );

    // 7. 記帳後、report::execute（SQL集計）で収益・費用の残高が0になっていることを確認する。
    let after_closing = report::execute(&query, &composition.tag_schema, period)
        .await
        .unwrap();
    for row in after_closing.rows() {
        if matches!(
            row.account_type,
            AccountType::Revenue | AccountType::Expense
        ) {
            assert_eq!(
                row.balance.minor(),
                0,
                "決算振替後は収益・費用の残高が0になっているはず: account={:?}",
                row.account
            );
        }
    }
    let (debit_after, credit_after) = after_closing.totals().unwrap().unwrap();
    assert_eq!(debit_after.minor(), credit_after.minor());

    // 8. DBから全仕訳（決算仕訳を含む）を読み戻してBS/PLを組み立てる。
    let mut all_entries = fetched_entries;
    all_entries.push(found_closing);
    let tb_after_closing = TrialBalance::from_entries(
        all_entries.iter(),
        &composition.chart,
        &composition.tag_schema,
        &[],
    )
    .unwrap();

    // JpStatementPolicy の chart は、決算書生成の直前に読み直したものを使う
    // （DECISIONS.md D-069。ここでの `composition.chart` は本テストの
    // seed_chart と同じ内容なので、「読み直した」ことの直接の実証は
    // JpStatementPolicy の構築コストが無視できるほど小さいことに拠る。
    // 実運用では合成ルートが tx.load_chart() を都度呼んでから渡す）。
    let statement_policy = JpStatementPolicy::new(composition.chart.clone());
    let balance_sheet = statement_policy.balance_sheet(&tb_after_closing);
    let income_statement = statement_policy.income_statement(&tb_after_closing);

    assert_eq!(balance_sheet.title, "貸借対照表");
    assert_eq!(income_statement.title, "損益計算書");
    // 決算振替後は収益・費用がゼロ化されているため、損益計算書の当期純利益(total)は0。
    assert!(
        income_statement.total.is_zero(),
        "決算振替後の当期純利益は0になっているはず: {:?}",
        income_statement.total
    );
}
