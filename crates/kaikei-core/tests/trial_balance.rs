//! `trial_balance.rs`（read model）の統合テスト。
//!
//! `docs/02-test-cases.md` の `## trial_balance`（B-01〜B-33）と
//! `## プロパティテスト`（PT-01〜PT-03）を全件カバーする。
//! テストをここ（`tests/` 配下）に置く理由は `tests/common/mod.rs` の冒頭コメントを
//! 参照（`journal.rs` と同様、公開APIだけで完結するため統合テストに寄せる）。
//!
//! `docs/02-test-cases.md` R-10（「元仕訳と逆仕訳を合算すると全科目ゼロ」）の完全な検証も
//! ここで行う（`tests/journal.rs` の R-10 は `TrialBalance` 未実装時点での部分検証だった）。

mod common;

use kaikei_core::{
    AccountCode, AccountingDate, ChartOfAccounts, CoreError, Currency, EntryId, EntryNumber,
    FiscalYear, JournalEntry, JournalLine, Money, NewEntry, Side, TagKey, TagSchema, TagSet,
    TagValue, TrialBalance,
};
use proptest::prelude::*;

// ---- 汎用ヘルパー ----

fn jpy(minor: i128) -> Money {
    Money::from_minor(minor, Currency::JPY)
}

fn acct(code: &str) -> AccountCode {
    AccountCode::parse(code).expect("テスト用科目コードは常に有効")
}

fn debit(code: &str, minor: i128) -> JournalLine {
    JournalLine::new(acct(code), Side::Debit, jpy(minor), TagSet::new(), None)
        .expect("テスト用の借方明細は常に構築できる")
}

fn credit(code: &str, minor: i128) -> JournalLine {
    JournalLine::new(acct(code), Side::Credit, jpy(minor), TagSet::new(), None)
        .expect("テスト用の貸方明細は常に構築できる")
}

fn tags_with(pairs: &[(&str, TagValue)]) -> TagSet {
    let mut set = TagSet::new();
    for (key, value) in pairs {
        set.insert(
            TagKey::parse(key).expect("テスト用タグキーは常に有効"),
            value.clone(),
        );
    }
    set
}

fn debit_with_tags(code: &str, minor: i128, tags: TagSet) -> JournalLine {
    JournalLine::new(acct(code), Side::Debit, jpy(minor), tags, None)
        .expect("テスト用の借方明細（タグ付き）は常に構築できる")
}

fn credit_with_tags(code: &str, minor: i128, tags: TagSet) -> JournalLine {
    JournalLine::new(acct(code), Side::Credit, jpy(minor), tags, None)
        .expect("テスト用の貸方明細（タグ付き）は常に構築できる")
}

/// 経費・収益科目に必須の `tax_category` タグ（`test_schema()` の要件）を満たすためのタグ。
fn tax_tags() -> TagSet {
    tags_with(&[("tax_category", TagValue::Code("10".to_string()))])
}

fn tag_key(s: &str) -> TagKey {
    TagKey::parse(s).expect("テスト用タグキーは常に有効")
}

fn date(year: i32, month: u8, day: u8) -> AccountingDate {
    AccountingDate::new(year, month, day).expect("テスト用日付は常に有効")
}

fn fy_2026() -> FiscalYear {
    FiscalYear::calendar_year(2026)
}

fn new_entry(
    id: u128,
    entry_no: u32,
    entry_date: AccountingDate,
    description: &str,
    lines: Vec<JournalLine>,
) -> Result<JournalEntry, CoreError> {
    JournalEntry::new(
        NewEntry {
            id: EntryId::new(id),
            entry_no: EntryNumber::new(entry_no),
            entry_date,
            description: description.to_string(),
            lines,
            document_refs: Vec::new(),
        },
        &fy_2026(),
        &common::test_chart(),
        &common::test_schema(),
        &common::open_guard(),
        &common::fixed_clock(),
    )
}

/// 標準の日付（2026-04-15）・摘要で仕訳を組み立てる。特別な日付が不要な大半のテストで使う。
fn entry(id: u128, lines: Vec<JournalLine>) -> JournalEntry {
    new_entry(id, id as u32, date(2026, 4, 15), "テスト仕訳", lines)
        .expect("テスト用の仕訳は常に貸借一致する構成で組み立てる")
}

fn reverse_of(original: &JournalEntry, id: u128, reason: &str, on: AccountingDate) -> JournalEntry {
    original
        .reverse(
            EntryId::new(id),
            EntryNumber::new(id as u32),
            on,
            reason.to_string(),
            &fy_2026(),
            &common::test_chart(),
            &common::test_schema(),
            &common::open_guard(),
            &common::fixed_clock(),
        )
        .expect("テスト用の逆仕訳は常に成立する")
}

fn trial_balance<'a>(
    entries: impl Iterator<Item = &'a JournalEntry>,
    chart: &ChartOfAccounts,
    schema: &TagSchema,
    group_by: &[TagKey],
) -> Result<TrialBalance, CoreError> {
    TrialBalance::from_entries(entries, chart, schema, group_by)
}

// =====================================================================
// 基本（B-01〜B-04）
// =====================================================================

// B-01
#[test]
fn trial_balance_no_entries_is_empty_and_balanced() {
    let entries: Vec<JournalEntry> = Vec::new();
    let tb = trial_balance(
        entries.iter(),
        &common::test_chart(),
        &common::test_schema(),
        &[],
    )
    .expect("空の仕訳集合でも構築できる");
    assert!(tb.rows().is_empty());
    assert!(tb.is_balanced());
}

// B-02
#[test]
fn trial_balance_single_entry_produces_two_rows_with_matching_totals() {
    let e = entry(1, vec![debit("100", 100), credit("400", 100)]);
    let tb = trial_balance(
        [&e].into_iter(),
        &common::test_chart(),
        &common::test_schema(),
        &[],
    )
    .expect("貸借一致した仕訳から構築できる");
    assert_eq!(tb.rows().len(), 2);
    let (debit_total, credit_total) = tb.totals();
    assert_eq!(debit_total.minor(), 100);
    assert_eq!(credit_total.minor(), 100);
}

// B-03
#[test]
fn trial_balance_same_account_posted_multiple_times_is_aggregated_into_one_row() {
    let e1 = entry(1, vec![debit("100", 100), credit("400", 100)]);
    let e2 = entry(2, vec![debit("100", 50), credit("400", 50)]);
    let tb = trial_balance(
        [&e1, &e2].into_iter(),
        &common::test_chart(),
        &common::test_schema(),
        &[],
    )
    .expect("構築できる");

    assert_eq!(tb.rows().len(), 2);
    let cash_row = tb
        .rows()
        .iter()
        .find(|r| r.account.as_str() == "100")
        .expect("100 の行が存在する");
    assert_eq!(cash_row.debit_total.minor(), 150);
    assert_eq!(tb.balance_of(&acct("100")).unwrap().minor(), 150);
}

// B-04
#[test]
fn trial_balance_is_balanced_is_always_true_for_valid_entries() {
    let e1 = entry(1, vec![debit("100", 100), credit("400", 100)]);
    let e2 = entry(
        2,
        vec![debit("135", 50), debit("100", 50), credit("310", 100)],
    );
    let e3 = entry(
        3,
        vec![
            debit_with_tags("609", 30_000, tax_tags()),
            credit("100", 30_000),
        ],
    );
    let tb = trial_balance(
        [&e1, &e2, &e3].into_iter(),
        &common::test_chart(),
        &common::test_schema(),
        &[],
    )
    .expect("構築できる");
    assert!(tb.is_balanced());
}

// =====================================================================
// 残高の向き（DOMAIN.md §2 との一致）（B-10〜B-16）
// =====================================================================

// B-10: 資産科目に借方100 => +100
#[test]
fn trial_balance_asset_debit_balance_is_positive() {
    let e = entry(1, vec![debit("100", 100), credit("400", 100)]);
    let tb = trial_balance(
        [&e].into_iter(),
        &common::test_chart(),
        &common::test_schema(),
        &[],
    )
    .unwrap();
    assert_eq!(tb.balance_of(&acct("100")).unwrap().minor(), 100);
}

// B-11: 資産科目に貸方100 => -100
#[test]
fn trial_balance_asset_credit_balance_is_negative() {
    let e = entry(1, vec![debit("400", 100), credit("100", 100)]);
    let tb = trial_balance(
        [&e].into_iter(),
        &common::test_chart(),
        &common::test_schema(),
        &[],
    )
    .unwrap();
    assert_eq!(tb.balance_of(&acct("100")).unwrap().minor(), -100);
}

// B-12: 負債科目に貸方100 => +100
#[test]
fn trial_balance_liability_credit_balance_is_positive() {
    let e = entry(1, vec![debit("100", 100), credit("310", 100)]);
    let tb = trial_balance(
        [&e].into_iter(),
        &common::test_chart(),
        &common::test_schema(),
        &[],
    )
    .unwrap();
    assert_eq!(tb.balance_of(&acct("310")).unwrap().minor(), 100);
}

// B-13: 負債科目に借方100 => -100
#[test]
fn trial_balance_liability_debit_balance_is_negative() {
    let e = entry(1, vec![debit("310", 100), credit("100", 100)]);
    let tb = trial_balance(
        [&e].into_iter(),
        &common::test_chart(),
        &common::test_schema(),
        &[],
    )
    .unwrap();
    assert_eq!(tb.balance_of(&acct("310")).unwrap().minor(), -100);
}

// B-14: 収益科目に貸方100 => +100
#[test]
fn trial_balance_revenue_credit_balance_is_positive() {
    let e = entry(
        1,
        vec![debit("100", 100), credit_with_tags("500", 100, tax_tags())],
    );
    let tb = trial_balance(
        [&e].into_iter(),
        &common::test_chart(),
        &common::test_schema(),
        &[],
    )
    .unwrap();
    assert_eq!(tb.balance_of(&acct("500")).unwrap().minor(), 100);
}

// B-15: 費用科目に借方100 => +100
#[test]
fn trial_balance_expense_debit_balance_is_positive() {
    let e = entry(
        1,
        vec![debit_with_tags("609", 100, tax_tags()), credit("100", 100)],
    );
    let tb = trial_balance(
        [&e].into_iter(),
        &common::test_chart(),
        &common::test_schema(),
        &[],
    )
    .unwrap();
    assert_eq!(tb.balance_of(&acct("609")).unwrap().minor(), 100);
}

// B-16: 純資産科目に貸方100 => +100
#[test]
fn trial_balance_equity_credit_balance_is_positive() {
    let e = entry(1, vec![debit("100", 100), credit("400", 100)]);
    let tb = trial_balance(
        [&e].into_iter(),
        &common::test_chart(),
        &common::test_schema(),
        &[],
    )
    .unwrap();
    assert_eq!(tb.balance_of(&acct("400")).unwrap().minor(), 100);
}

// =====================================================================
// group_by（B-20〜B-25）
// =====================================================================

fn counterparty_tag(name: &str) -> TagSet {
    tags_with(&[
        ("tax_category", TagValue::Code("10".to_string())),
        ("counterparty", TagValue::Code(name.to_string())),
    ])
}

// B-20
#[test]
fn trial_balance_group_by_empty_aggregates_by_account_only() {
    let e1 = entry(
        1,
        vec![
            debit("100", 100),
            credit_with_tags("500", 100, counterparty_tag("A")),
        ],
    );
    let e2 = entry(
        2,
        vec![
            debit("100", 200),
            credit_with_tags("500", 200, counterparty_tag("B")),
        ],
    );
    let tb = trial_balance(
        [&e1, &e2].into_iter(),
        &common::test_chart(),
        &common::test_schema(),
        &[],
    )
    .unwrap();

    let rows_500: Vec<_> = tb
        .rows()
        .iter()
        .filter(|r| r.account.as_str() == "500")
        .collect();
    assert_eq!(rows_500.len(), 1);
    assert_eq!(rows_500[0].credit_total.minor(), 300);
}

// B-21
#[test]
fn trial_balance_group_by_one_key_splits_account_by_tag() {
    let e1 = entry(
        1,
        vec![
            debit("100", 100),
            credit_with_tags("500", 100, counterparty_tag("A")),
        ],
    );
    let e2 = entry(
        2,
        vec![
            debit("100", 200),
            credit_with_tags("500", 200, counterparty_tag("B")),
        ],
    );
    let group_by = [tag_key("counterparty")];
    let tb = trial_balance(
        [&e1, &e2].into_iter(),
        &common::test_chart(),
        &common::test_schema(),
        &group_by,
    )
    .unwrap();

    let rows_500: Vec<_> = tb
        .rows()
        .iter()
        .filter(|r| r.account.as_str() == "500")
        .collect();
    assert_eq!(rows_500.len(), 2);
}

// B-22
#[test]
fn trial_balance_group_by_two_keys_splits_by_combination() {
    let e1 = entry(
        1,
        vec![
            debit("100", 100),
            credit_with_tags(
                "500",
                100,
                tags_with(&[
                    ("tax_category", TagValue::Code("10".to_string())),
                    ("counterparty", TagValue::Code("A".to_string())),
                ]),
            ),
        ],
    );
    let e2 = entry(
        2,
        vec![
            debit("100", 200),
            credit_with_tags(
                "500",
                200,
                tags_with(&[
                    ("tax_category", TagValue::Code("8".to_string())),
                    ("counterparty", TagValue::Code("A".to_string())),
                ]),
            ),
        ],
    );
    let e3 = entry(
        3,
        vec![
            debit("100", 300),
            credit_with_tags(
                "500",
                300,
                tags_with(&[
                    ("tax_category", TagValue::Code("10".to_string())),
                    ("counterparty", TagValue::Code("B".to_string())),
                ]),
            ),
        ],
    );
    let group_by = [tag_key("counterparty"), tag_key("tax_category")];
    let tb = trial_balance(
        [&e1, &e2, &e3].into_iter(),
        &common::test_chart(),
        &common::test_schema(),
        &group_by,
    )
    .unwrap();

    let rows_500 = tb
        .rows()
        .iter()
        .filter(|r| r.account.as_str() == "500")
        .count();
    assert_eq!(rows_500, 3);
}

// B-23
#[test]
fn trial_balance_line_without_the_group_by_tag_falls_into_the_empty_group() {
    let untagged = entry(1, vec![debit("100", 100), credit("400", 100)]);

    let tb_no_group_by = trial_balance(
        [&untagged].into_iter(),
        &common::test_chart(),
        &common::test_schema(),
        &[],
    )
    .unwrap();
    let empty_group = tb_no_group_by.rows()[0].group.clone();

    let group_by = [tag_key("counterparty")];
    let tb_grouped = trial_balance(
        [&untagged].into_iter(),
        &common::test_chart(),
        &common::test_schema(),
        &group_by,
    )
    .unwrap();

    for row in tb_grouped.rows() {
        assert_eq!(row.group, empty_group);
    }
}

// B-24
#[test]
fn trial_balance_group_by_non_aggregatable_key_is_error() {
    let e = entry(1, vec![debit("100", 100), credit("400", 100)]);
    let group_by = [tag_key("business_ratio")];
    let err = trial_balance(
        [&e].into_iter(),
        &common::test_chart(),
        &common::test_schema(),
        &group_by,
    )
    .unwrap_err();
    assert!(matches!(err, CoreError::NotAggregatable { .. }));
}

// B-25
#[test]
fn trial_balance_totals_match_regardless_of_group_by() {
    let e1 = entry(
        1,
        vec![
            debit("100", 100),
            credit_with_tags("500", 100, counterparty_tag("A")),
        ],
    );
    let e2 = entry(
        2,
        vec![
            debit("100", 200),
            credit_with_tags("500", 200, counterparty_tag("B")),
        ],
    );
    let entries = [&e1, &e2];
    let chart = common::test_chart();
    let schema = common::test_schema();

    let tb_no_group_by = trial_balance(entries.into_iter(), &chart, &schema, &[]).unwrap();
    let group_by = [tag_key("counterparty")];
    let tb_grouped = trial_balance(entries.into_iter(), &chart, &schema, &group_by).unwrap();

    assert_eq!(tb_no_group_by.totals(), tb_grouped.totals());
}

// =====================================================================
// 検算シナリオ（統合テスト）（B-30〜B-33）
// =====================================================================

// B-30: 売上計上 → 入金 → 経費支払
#[test]
fn trial_balance_sales_receipt_and_expense_scenario_matches_manual_calculation() {
    let sales = entry(
        1,
        vec![
            debit("135", 100_000),
            credit_with_tags("500", 100_000, tax_tags()),
        ],
    );
    let receipt = entry(2, vec![debit("100", 100_000), credit("135", 100_000)]);
    let expense = entry(
        3,
        vec![
            debit_with_tags("609", 30_000, tax_tags()),
            credit("100", 30_000),
        ],
    );

    let tb = trial_balance(
        [&sales, &receipt, &expense].into_iter(),
        &common::test_chart(),
        &common::test_schema(),
        &[],
    )
    .unwrap();

    assert!(tb.is_balanced());
    assert_eq!(tb.balance_of(&acct("100")).unwrap().minor(), 70_000);
    assert_eq!(tb.balance_of(&acct("135")).unwrap().minor(), 0);
    assert_eq!(tb.balance_of(&acct("500")).unwrap().minor(), 100_000);
    assert_eq!(tb.balance_of(&acct("609")).unwrap().minor(), 30_000);
}

// B-31: 家事按分仕訳
#[test]
fn trial_balance_household_ratio_entry_scenario_matches_manual_calculation() {
    let household = entry(
        1,
        vec![
            debit_with_tags("615", 30_000, tax_tags()),
            debit("410", 70_000),
            credit("100", 100_000),
        ],
    );

    let tb = trial_balance(
        [&household].into_iter(),
        &common::test_chart(),
        &common::test_schema(),
        &[],
    )
    .unwrap();

    assert!(tb.is_balanced());
    assert_eq!(tb.balance_of(&acct("615")).unwrap().minor(), 30_000);
    assert_eq!(tb.balance_of(&acct("410")).unwrap().minor(), -70_000);
    assert_eq!(tb.balance_of(&acct("100")).unwrap().minor(), -100_000);
}

// B-32: 誤った仕訳 → 逆仕訳 → 正しい仕訳 の最終残高が「正しい仕訳のみ」の場合と一致する
#[test]
fn trial_balance_wrong_entry_reversal_and_correct_entry_matches_correct_only() {
    let wrong = entry(
        1,
        vec![
            debit_with_tags("609", 40_000, tax_tags()),
            credit("100", 40_000),
        ],
    );
    let reversal = reverse_of(&wrong, 2, "金額誤り", date(2026, 4, 16));
    let correct = entry(
        3,
        vec![
            debit_with_tags("609", 50_000, tax_tags()),
            credit("100", 50_000),
        ],
    );

    let chart = common::test_chart();
    let schema = common::test_schema();
    let tb_full = trial_balance(
        [&wrong, &reversal, &correct].into_iter(),
        &chart,
        &schema,
        &[],
    )
    .unwrap();

    let correct_only = entry(
        10,
        vec![
            debit_with_tags("609", 50_000, tax_tags()),
            credit("100", 50_000),
        ],
    );
    let tb_correct_only = trial_balance([&correct_only].into_iter(), &chart, &schema, &[]).unwrap();

    // 借方合計・貸方合計そのもの（取引の総流通額）は、誤仕訳と逆仕訳の分だけ
    // 「正しい仕訳のみ」の場合より大きくなる（40,000の記帳が2回分余計に乗る）。
    // ここで一致すべきなのは総流通額ではなく、最終的な科目残高である。
    assert!(tb_full.is_balanced());
    assert_eq!(
        tb_full.balance_of(&acct("609")),
        tb_correct_only.balance_of(&acct("609"))
    );
    assert_eq!(
        tb_full.balance_of(&acct("100")),
        tb_correct_only.balance_of(&acct("100"))
    );
}

// B-33: 100件の仕訳をランダム生成して集計しても常に is_balanced()
#[test]
fn trial_balance_100_randomly_generated_entries_is_always_balanced() {
    use proptest::strategy::{Strategy as _, ValueTree as _};
    use proptest::test_runner::TestRunner;

    let strategy = common::balanced_lines_strategy();
    let mut runner = TestRunner::default();

    let mut entries = Vec::with_capacity(100);
    for i in 0..100u32 {
        let lines = strategy
            .new_tree(&mut runner)
            .expect("戦略からのサンプリングは失敗しない")
            .current();
        let entry = new_entry(
            u128::from(i) + 1,
            i + 1,
            date(2026, 4, 15),
            "ランダム生成テスト仕訳",
            lines,
        )
        .expect("balanced_lines_strategy が生成する明細は常に貸借一致する");
        entries.push(entry);
    }

    let tb = trial_balance(
        entries.iter(),
        &common::test_chart(),
        &common::test_schema(),
        &[],
    )
    .unwrap();
    assert!(tb.is_balanced());
}

// =====================================================================
// R-10（trial_balance.rs での完全な検証）
// =====================================================================

// R-10
//
// `tests/journal.rs` の R-10 は `TrialBalance` 未実装時点での部分検証
// （借方合計・貸方合計が入れ替わっていることのみ）だった。ここでは
// `TrialBalance` を使い、「合算すると全科目の残高がゼロになる」ことを
// 直接検証する（決定的な具体例）。汎用的な性質としての検証は
// `pt03_entry_plus_its_reversal_has_zero_balance_for_every_account` で行う。
#[test]
fn reverse_and_original_combined_have_zero_balance_for_every_account() {
    let original = entry(
        1,
        vec![
            debit("100", 70),
            debit("135", 30),
            credit("310", 60),
            credit("400", 40),
        ],
    );
    let reversed = reverse_of(&original, 2, "入力誤り", date(2026, 4, 16));

    let tb = trial_balance(
        [&original, &reversed].into_iter(),
        &common::test_chart(),
        &common::test_schema(),
        &[],
    )
    .unwrap();

    assert!(tb.is_balanced());
    for row in tb.rows() {
        assert!(
            row.balance.is_zero(),
            "account={} balance={}",
            row.account.as_str(),
            row.balance.to_display_string()
        );
    }
}

// =====================================================================
// プロパティテスト（PT-01〜PT-03）
// =====================================================================

proptest! {
    // PT-01: 任意の貸借一致明細で JournalEntry::new が成功する
    #[test]
    fn pt01_journal_entry_new_succeeds_for_any_balanced_lines(
        lines in common::balanced_lines_strategy()
    ) {
        let result = new_entry(1, 1, date(2026, 4, 15), "プロパティテスト仕訳", lines);
        prop_assert!(result.is_ok());
    }

    // PT-02: 任意の仕訳集合で TrialBalance::is_balanced() が true
    #[test]
    fn pt02_trial_balance_is_balanced_for_any_entry_set(
        line_sets in prop::collection::vec(common::balanced_lines_strategy(), 1..=20)
    ) {
        let entries: Vec<JournalEntry> = line_sets
            .into_iter()
            .enumerate()
            .map(|(i, lines)| {
                new_entry(
                    (i as u128) + 1,
                    (i as u32) + 1,
                    date(2026, 4, 15),
                    "プロパティテスト仕訳",
                    lines,
                )
                .expect("balanced_lines_strategy が生成する明細は常に貸借一致する")
            })
            .collect();

        let tb = trial_balance(
            entries.iter(),
            &common::test_chart(),
            &common::test_schema(),
            &[],
        )
        .unwrap();
        prop_assert!(tb.is_balanced());
    }

    // PT-03: 任意の仕訳とその逆仕訳の合算で全科目残高がゼロ（R-10 の性質としての完全版）
    #[test]
    fn pt03_entry_plus_its_reversal_has_zero_balance_for_every_account(
        lines in common::balanced_lines_strategy()
    ) {
        let original = new_entry(1, 1, date(2026, 4, 15), "元仕訳", lines)
            .expect("balanced_lines_strategy が生成する明細は常に貸借一致する");
        let reversed = reverse_of(&original, 2, "検証用の逆仕訳", date(2026, 4, 16));

        let tb = trial_balance(
            [&original, &reversed].into_iter(),
            &common::test_chart(),
            &common::test_schema(),
            &[],
        )
        .unwrap();

        prop_assert!(tb.is_balanced());
        for row in tb.rows() {
            prop_assert!(row.balance.is_zero());
        }
    }
}
