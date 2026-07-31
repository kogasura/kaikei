//! `journal.rs`（集約）の統合テスト。
//!
//! `docs/02-test-cases.md` の `## journal — JournalLine`（L-01〜L-04）、
//! `## journal — JournalEntry::new`（J-01〜J-81）、`## journal — reverse`（R-01〜R-12）
//! を全件カバーする。テストをここ（`tests/` 配下）に置く理由は
//! `tests/common/mod.rs` の冒頭コメントを参照。
//!
//! 各テストケースの直前に `// J-01` のようなケースIDコメントを付ける。

mod common;

use kaikei_core::{
    AccountCode, AccountingDate, ChartOfAccounts, CoreError, Currency, DocumentRef, EntryId,
    EntryNumber, FiscalYear, JournalEntry, JournalLine, Money, NewEntry, PeriodGuard, Side, TagKey,
    TagSchema, TagSet, TagValue, Timestamp,
};

// ---- 汎用ヘルパー ----

fn jpy(minor: i128) -> Money {
    Money::from_minor(minor, Currency::JPY)
}

fn usd(minor: i128) -> Money {
    Money::from_minor(minor, Currency::USD)
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

fn debit_usd(code: &str, minor: i128) -> JournalLine {
    JournalLine::new(acct(code), Side::Debit, usd(minor), TagSet::new(), None)
        .expect("テスト用の借方明細（USD）は常に構築できる")
}

fn credit_usd(code: &str, minor: i128) -> JournalLine {
    JournalLine::new(acct(code), Side::Credit, usd(minor), TagSet::new(), None)
        .expect("テスト用の貸方明細（USD）は常に構築できる")
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

fn date(year: i32, month: u8, day: u8) -> AccountingDate {
    AccountingDate::new(year, month, day).expect("テスト用日付は常に有効")
}

fn fy_2026() -> FiscalYear {
    FiscalYear::calendar_year(2026)
}

/// [`JournalEntry::new`] の全引数をカスタマイズできる汎用ヘルパー。
#[allow(clippy::too_many_arguments)]
fn try_new(
    lines: Vec<JournalLine>,
    entry_date: AccountingDate,
    description: &str,
    fy: &FiscalYear,
    chart: &ChartOfAccounts,
    schema: &TagSchema,
    guard: &dyn PeriodGuard,
) -> Result<JournalEntry, CoreError> {
    JournalEntry::new(
        NewEntry {
            id: EntryId::new(1),
            entry_no: EntryNumber::new(1),
            entry_date,
            description: description.to_string(),
            lines,
            document_refs: Vec::new(),
        },
        fy,
        chart,
        schema,
        guard,
        &common::fixed_clock(),
    )
}

/// 標準の会計年度（2026年暦年）・勘定科目表・タグスキーマ・Open期間・固定摘要日付で
/// 仕訳を組み立てる。特別なカスタマイズが不要な大半のテストで使う。
fn build(lines: Vec<JournalLine>) -> Result<JournalEntry, CoreError> {
    try_new(
        lines,
        date(2026, 4, 15),
        "テスト仕訳",
        &fy_2026(),
        &common::test_chart(),
        &common::test_schema(),
        &common::open_guard(),
    )
}

// =====================================================================
// journal — JournalLine（L-01〜L-04）
// =====================================================================

// L-01
#[test]
fn journal_line_new_valid_debit_line_succeeds() {
    let line = JournalLine::new(acct("100"), Side::Debit, jpy(1000), TagSet::new(), None);
    assert!(line.is_ok());
}

// L-02
#[test]
fn journal_line_new_negative_amount_is_error() {
    let line = JournalLine::new(acct("100"), Side::Debit, jpy(-1), TagSet::new(), None);
    assert!(matches!(line, Err(CoreError::InvalidAmount { .. })));
}

// L-03
#[test]
fn journal_line_new_zero_amount_is_error() {
    let line = JournalLine::new(acct("100"), Side::Debit, jpy(0), TagSet::new(), None);
    assert!(matches!(line, Err(CoreError::InvalidAmount { .. })));
}

// L-04
#[test]
fn journal_line_is_debit_matches_side() {
    let debit_line = JournalLine::new(acct("100"), Side::Debit, jpy(100), TagSet::new(), None)
        .expect("正常な明細");
    let credit_line = JournalLine::new(acct("100"), Side::Credit, jpy(100), TagSet::new(), None)
        .expect("正常な明細");
    assert!(debit_line.is_debit());
    assert!(!credit_line.is_debit());
}

// =====================================================================
// journal — JournalEntry::new（J-01〜J-81）
// =====================================================================

// ---- 正常系 ----

// J-01
#[test]
fn journal_entry_new_two_lines_balanced_succeeds() {
    let entry = build(vec![debit("100", 100), credit("400", 100)]);
    assert!(entry.is_ok());
}

// J-02
#[test]
fn journal_entry_new_three_lines_balanced_succeeds() {
    let entry = build(vec![
        debit("100", 100),
        credit("310", 60),
        credit("400", 40),
    ]);
    assert!(entry.is_ok());
}

// J-03
#[test]
fn journal_entry_new_four_lines_balanced_succeeds() {
    let entry = build(vec![
        debit("100", 70),
        debit("135", 30),
        credit("310", 60),
        credit("400", 40),
    ]);
    assert!(entry.is_ok());
}

// J-04
//
// 設計書の例示「売掛110 / 売上100 + 仮受10」は3行だが、ケース見出しは
// 「消費税ありの4行」となっている。ここでは借方を現金+売掛金の2科目に分割し、
// 借方2行・貸方2行の実質4行構成として消費税区分タグ付きの仕訳を再現する。
#[test]
fn journal_entry_new_four_lines_with_tax_succeeds() {
    let tax_tags = tags_with(&[("tax_category", TagValue::Code("10".to_string()))]);
    let entry = build(vec![
        debit("100", 55_000),
        debit("135", 55_000),
        credit_with_tags("500", 100_000, tax_tags),
        credit("330", 10_000),
    ]);
    assert!(entry.is_ok());
}

// J-05
#[test]
fn journal_entry_new_debit_total_and_credit_total_match() {
    let entry = build(vec![debit("100", 100), credit("400", 100)]).expect("貸借一致");
    assert_eq!(entry.debit_total(), entry.credit_total());
    assert_eq!(entry.debit_total().minor(), 100);
}

// J-06
#[test]
fn journal_entry_new_recorded_at_matches_clock() {
    let entry = build(vec![debit("100", 100), credit("400", 100)]).expect("貸借一致");
    assert_eq!(entry.recorded_at(), common::fixed_clock().0);
}

// ---- 貸借不一致 ----

// J-10
#[test]
fn journal_entry_new_debit_greater_than_credit_is_unbalanced() {
    let err = build(vec![debit("100", 100), credit("400", 90)]).unwrap_err();
    assert!(matches!(
        err,
        CoreError::Unbalanced { ref diff, .. } if diff == "10"
    ));
}

// J-11
#[test]
fn journal_entry_new_credit_greater_than_debit_is_unbalanced() {
    let err = build(vec![debit("100", 100), credit("400", 110)]).unwrap_err();
    assert!(matches!(
        err,
        CoreError::Unbalanced { ref diff, .. } if diff == "10"
    ));
}

// J-12
#[test]
fn journal_entry_new_unbalanced_error_message_contains_debit_credit_and_diff() {
    let err = build(vec![debit("100", 110_000), credit("400", 100_000)]).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("110,000"), "message = {message}");
    assert!(message.contains("100,000"), "message = {message}");
    assert!(message.contains("10,000"), "message = {message}");
    assert!(message.contains("貸借不一致"), "message = {message}");
}

// J-13
#[test]
fn journal_entry_new_all_debit_lines_is_unbalanced() {
    let err = build(vec![debit("100", 50), debit("135", 50)]).unwrap_err();
    assert!(matches!(err, CoreError::Unbalanced { .. }));
}

// ---- 明細数 ----

// J-20
#[test]
fn journal_entry_new_zero_lines_is_too_few() {
    let err = build(vec![]).unwrap_err();
    assert!(matches!(err, CoreError::TooFewLines { found: 0 }));
}

// J-21
#[test]
fn journal_entry_new_one_line_is_too_few() {
    let err = build(vec![debit("100", 100)]).unwrap_err();
    assert!(matches!(err, CoreError::TooFewLines { found: 1 }));
}

// ---- 勘定科目 ----

// J-30
#[test]
fn journal_entry_new_unknown_account_is_error() {
    let err = build(vec![debit("700", 100), credit("400", 100)]).unwrap_err();
    assert!(matches!(err, CoreError::UnknownAccount { .. }));
}

// J-31
#[test]
fn journal_entry_new_not_postable_account_is_error() {
    let err = build(vec![debit("999", 100), credit("400", 100)]).unwrap_err();
    assert!(matches!(err, CoreError::NotPostable { .. }));
}

// J-32
#[test]
fn journal_entry_new_same_account_both_sides_succeeds() {
    let entry = build(vec![debit("100", 100), credit("100", 100)]);
    assert!(entry.is_ok());
}

// ---- 通貨 ----

// J-40
#[test]
fn journal_entry_new_currency_mismatch_is_error() {
    let err = build(vec![debit("100", 100), credit_usd("400", 100)]).unwrap_err();
    assert!(matches!(err, CoreError::CurrencyMismatch { .. }));
}

// J-41
#[test]
fn journal_entry_new_all_usd_balanced_succeeds() {
    let entry = build(vec![debit_usd("100", 100), credit_usd("400", 100)]);
    assert!(entry.is_ok());
}

// J-42
#[test]
fn journal_entry_new_currency_returns_line_currency() {
    let jpy_entry = build(vec![debit("100", 100), credit("400", 100)]).expect("貸借一致");
    assert_eq!(jpy_entry.currency(), Currency::JPY);

    let usd_entry = build(vec![debit_usd("100", 100), credit_usd("400", 100)]).expect("貸借一致");
    assert_eq!(usd_entry.currency(), Currency::USD);
}

// ---- タグ ----

// J-50
#[test]
fn journal_entry_new_schema_conforming_tags_succeeds() {
    let tax_tags = tags_with(&[("tax_category", TagValue::Code("10".to_string()))]);
    let entry = build(vec![
        debit("100", 100),
        credit_with_tags("500", 100, tax_tags),
    ]);
    assert!(entry.is_ok());
}

// J-51
#[test]
fn journal_entry_new_unknown_tag_key_is_error() {
    let bad_tags = tags_with(&[("unregistered_key", TagValue::Text("x".to_string()))]);
    let err = build(vec![
        debit_with_tags("100", 100, bad_tags),
        credit("400", 100),
    ])
    .unwrap_err();
    assert!(matches!(err, CoreError::UnknownTagKey { .. }));
}

// J-52
#[test]
fn journal_entry_new_missing_required_tag_for_expense_is_error() {
    let err = build(vec![debit("609", 100), credit("100", 100)]).unwrap_err();
    assert!(matches!(err, CoreError::MissingRequiredTag { .. }));
}

// ---- 日付と期間 ----

// J-60
#[test]
fn journal_entry_new_date_outside_fiscal_year_is_error() {
    let err = try_new(
        vec![debit("100", 100), credit("400", 100)],
        date(2027, 1, 1),
        "テスト仕訳",
        &fy_2026(),
        &common::test_chart(),
        &common::test_schema(),
        &common::open_guard(),
    )
    .unwrap_err();
    assert!(matches!(err, CoreError::DateOutOfFiscalYear { .. }));
}

// J-61
#[test]
fn journal_entry_new_date_at_fiscal_year_start_succeeds() {
    let fy = fy_2026();
    let entry = try_new(
        vec![debit("100", 100), credit("400", 100)],
        fy.start(),
        "テスト仕訳",
        &fy,
        &common::test_chart(),
        &common::test_schema(),
        &common::open_guard(),
    );
    assert!(entry.is_ok());
}

// J-62
#[test]
fn journal_entry_new_date_at_fiscal_year_end_succeeds() {
    let fy = fy_2026();
    let entry = try_new(
        vec![debit("100", 100), credit("400", 100)],
        fy.end(),
        "テスト仕訳",
        &fy,
        &common::test_chart(),
        &common::test_schema(),
        &common::open_guard(),
    );
    assert!(entry.is_ok());
}

// J-63
#[test]
fn journal_entry_new_closed_period_is_error() {
    let err = try_new(
        vec![debit("100", 100), credit("400", 100)],
        date(2026, 4, 15),
        "テスト仕訳",
        &fy_2026(),
        &common::test_chart(),
        &common::test_schema(),
        &common::closed_guard(),
    )
    .unwrap_err();
    assert!(matches!(err, CoreError::PeriodClosed { .. }));
}

// ---- 摘要 ----

// J-70
#[test]
fn journal_entry_new_empty_description_is_error() {
    let err = try_new(
        vec![debit("100", 100), credit("400", 100)],
        date(2026, 4, 15),
        "",
        &fy_2026(),
        &common::test_chart(),
        &common::test_schema(),
        &common::open_guard(),
    )
    .unwrap_err();
    assert!(matches!(err, CoreError::EmptyDescription));
}

// J-71
#[test]
fn journal_entry_new_blank_description_is_error() {
    let err = try_new(
        vec![debit("100", 100), credit("400", 100)],
        date(2026, 4, 15),
        "   ",
        &fy_2026(),
        &common::test_chart(),
        &common::test_schema(),
        &common::open_guard(),
    )
    .unwrap_err();
    assert!(matches!(err, CoreError::EmptyDescription));
}

// ---- 不変性の保証（構造テスト） ----

// J-80
//
// `JournalEntry` に `update`/`delete`/`edit`/`modify`/`set_*` メソッドが無いことは
// コンパイル時の設計テストであり、ランタイムテストは存在しない。
// `crates/kaikei-core/src/journal.rs` のモジュール doc コメントと、
// `.github/workflows/architecture.yml` の「JournalEntry に変更系メソッドが無い」
// ステップ（`fn (update|delete|set_|edit|modify)` の grep 検査）がこれを担保する。

// J-81
#[test]
fn journal_entry_lines_returns_read_only_view() {
    let entry = build(vec![debit("100", 100), credit("400", 100)]).expect("貸借一致");

    // `lines()` の戻り値は `&[JournalLine]` であり、`JournalEntry` には
    // `lines_mut()` 相当のメソッドが存在しないため、呼び出し側が内部の
    // `Vec<JournalLine>` を直接書き換えることはコンパイルできない
    // （これ自体はコンパイル時の型システムによる保証）。
    // ここでは実行時の観点として、`lines()` の結果を複製して変更しても
    // 元の `entry` の状態に影響しないことを確認する。
    let mut copied: Vec<JournalLine> = entry.lines().to_vec();
    copied.clear();

    assert_eq!(entry.lines().len(), 2);
    assert_eq!(entry.lines()[0].amount().minor(), 100);
}

// =====================================================================
// journal — rehydrate
// =====================================================================

// `docs/02-test-cases.md` に専用のケースIDは無いが、`rehydrate` は10引数の
// `#[allow(clippy::too_many_arguments)]` なコンストラクタであり、将来フィールドを
// 追加した際の配線ミス（引数の取り違え）を検出する安全網が無かったため追加する。
// 各引数に他のどのフィールドとも異なる値を与え、`rehydrate` で構築した
// `JournalEntry` の全ゲッターが元の値と一致することを確認する。
#[test]
fn rehydrate_round_trips_all_fields_with_distinct_values() {
    let id = EntryId::new(111);
    let fiscal_year = 2025;
    let entry_no = EntryNumber::new(222);
    let entry_date = date(2025, 6, 20);
    let description = "手動で組み立てた仕訳".to_string();
    let line_tags = tags_with(&[("counterparty", TagValue::Code("A社".to_string()))]);
    let lines = vec![
        debit_with_tags("100", 300, line_tags.clone()),
        credit("400", 300),
    ];
    let document_refs = vec![DocumentRef {
        document_id: 999,
        label: "領収書".to_string(),
    }];
    let reverses = Some(EntryId::new(50));
    let reverse_reason = Some("金額誤り".to_string());
    let recorded_at = Timestamp::from_unix_nanos(123_456_789);

    let entry = JournalEntry::rehydrate(
        id,
        fiscal_year,
        entry_no,
        entry_date,
        description.clone(),
        lines.clone(),
        document_refs.clone(),
        reverses,
        reverse_reason.clone(),
        recorded_at,
    );

    assert_eq!(entry.id(), id);
    assert_eq!(entry.fiscal_year(), fiscal_year);
    assert_eq!(entry.entry_no(), entry_no);
    assert_eq!(entry.entry_date(), entry_date);
    assert_eq!(entry.description(), description);

    assert_eq!(entry.lines().len(), lines.len());
    assert_eq!(entry.lines()[0].account().as_str(), "100");
    assert!(entry.lines()[0].is_debit());
    assert_eq!(entry.lines()[0].amount().minor(), 300);
    assert_eq!(entry.lines()[0].tags(), &line_tags);
    assert_eq!(entry.lines()[1].account().as_str(), "400");
    assert!(!entry.lines()[1].is_debit());

    assert_eq!(entry.document_refs().len(), 1);
    assert_eq!(entry.document_refs()[0].document_id, 999);
    assert_eq!(entry.document_refs()[0].label, "領収書");

    assert_eq!(entry.reverses(), reverses);
    assert_eq!(entry.reverse_reason(), reverse_reason.as_deref());
    assert_eq!(entry.recorded_at(), recorded_at);
}

// =====================================================================
// journal — reverse（R-01〜R-12）
// =====================================================================

fn reverse_with(
    original: &JournalEntry,
    date_value: AccountingDate,
    reason: &str,
    fy: &FiscalYear,
    guard: &dyn PeriodGuard,
) -> Result<JournalEntry, CoreError> {
    original.reverse(
        EntryId::new(2),
        EntryNumber::new(2),
        date_value,
        reason.to_string(),
        fy,
        &common::test_chart(),
        &common::test_schema(),
        guard,
        &common::fixed_clock(),
    )
}

// R-01
#[test]
fn reverse_two_line_entry_flips_sides() {
    let original = build(vec![debit("100", 100), credit("400", 100)]).expect("貸借一致");
    let fy = fy_2026();
    let reversed = reverse_with(
        &original,
        date(2026, 4, 16),
        "入力誤り",
        &fy,
        &common::open_guard(),
    )
    .expect("逆仕訳の生成に成功する");

    assert_eq!(reversed.lines()[0].account().as_str(), "100");
    assert!(!reversed.lines()[0].is_debit());
    assert_eq!(reversed.lines()[1].account().as_str(), "400");
    assert!(reversed.lines()[1].is_debit());
}

// R-02
#[test]
fn reverse_four_line_entry_flips_all_sides() {
    let original = build(vec![
        debit("100", 70),
        debit("135", 30),
        credit("310", 60),
        credit("400", 40),
    ])
    .expect("貸借一致");
    let fy = fy_2026();
    let reversed = reverse_with(
        &original,
        date(2026, 4, 16),
        "入力誤り",
        &fy,
        &common::open_guard(),
    )
    .expect("逆仕訳の生成に成功する");

    for (original_line, reversed_line) in original.lines().iter().zip(reversed.lines()) {
        assert_ne!(original_line.is_debit(), reversed_line.is_debit());
    }
}

// R-03
#[test]
fn reverse_preserves_amounts() {
    let original = build(vec![debit("100", 12_345), credit("400", 12_345)]).expect("貸借一致");
    let fy = fy_2026();
    let reversed = reverse_with(
        &original,
        date(2026, 4, 16),
        "入力誤り",
        &fy,
        &common::open_guard(),
    )
    .expect("逆仕訳の生成に成功する");

    for (original_line, reversed_line) in original.lines().iter().zip(reversed.lines()) {
        assert_eq!(original_line.amount(), reversed_line.amount());
        assert_eq!(original_line.account(), reversed_line.account());
    }
}

// R-04
#[test]
fn reverse_duplicates_tags() {
    let tax_tags = tags_with(&[("tax_category", TagValue::Code("10".to_string()))]);
    let original = build(vec![
        debit("100", 100),
        credit_with_tags("500", 100, tax_tags.clone()),
    ])
    .expect("貸借一致");
    let fy = fy_2026();
    let reversed = reverse_with(
        &original,
        date(2026, 4, 16),
        "入力誤り",
        &fy,
        &common::open_guard(),
    )
    .expect("逆仕訳の生成に成功する");

    assert_eq!(reversed.lines()[1].tags(), &tax_tags);
}

// R-05
#[test]
fn reverse_sets_reverses_to_original_id() {
    let original = build(vec![debit("100", 100), credit("400", 100)]).expect("貸借一致");
    let fy = fy_2026();
    let reversed = reverse_with(
        &original,
        date(2026, 4, 16),
        "入力誤り",
        &fy,
        &common::open_guard(),
    )
    .expect("逆仕訳の生成に成功する");

    assert_eq!(reversed.reverses(), Some(original.id()));
}

// R-06
#[test]
fn reverse_sets_reverse_reason() {
    let original = build(vec![debit("100", 100), credit("400", 100)]).expect("貸借一致");
    let fy = fy_2026();
    let reversed = reverse_with(
        &original,
        date(2026, 4, 16),
        "入力誤り",
        &fy,
        &common::open_guard(),
    )
    .expect("逆仕訳の生成に成功する");

    assert_eq!(reversed.reverse_reason(), Some("入力誤り"));
}

// R-07
#[test]
fn reverse_description_has_correction_prefix() {
    let original = try_new(
        vec![debit("100", 100), credit("400", 100)],
        date(2026, 4, 15),
        "元の摘要",
        &fy_2026(),
        &common::test_chart(),
        &common::test_schema(),
        &common::open_guard(),
    )
    .expect("貸借一致");
    let fy = fy_2026();
    let reversed = reverse_with(
        &original,
        date(2026, 4, 16),
        "入力誤り",
        &fy,
        &common::open_guard(),
    )
    .expect("逆仕訳の生成に成功する");

    assert_eq!(reversed.description(), "【訂正】元の摘要");
}

// R-08
#[test]
fn reverse_is_reversal_returns_true() {
    let original = build(vec![debit("100", 100), credit("400", 100)]).expect("貸借一致");
    let fy = fy_2026();
    let reversed = reverse_with(
        &original,
        date(2026, 4, 16),
        "入力誤り",
        &fy,
        &common::open_guard(),
    )
    .expect("逆仕訳の生成に成功する");

    assert!(!original.is_reversal());
    assert!(reversed.is_reversal());
}

// R-09
#[test]
fn reverse_of_reverse_is_allowed() {
    let original = build(vec![debit("100", 100), credit("400", 100)]).expect("貸借一致");
    let fy = fy_2026();
    let reversed = reverse_with(
        &original,
        date(2026, 4, 16),
        "入力誤り",
        &fy,
        &common::open_guard(),
    )
    .expect("逆仕訳の生成に成功する");

    let re_reversed = reversed
        .reverse(
            EntryId::new(3),
            EntryNumber::new(3),
            date(2026, 4, 17),
            "逆仕訳の訂正".to_string(),
            &fy,
            &common::test_chart(),
            &common::test_schema(),
            &common::open_guard(),
            &common::fixed_clock(),
        )
        .expect("逆仕訳の逆仕訳も許可される");

    // reverses は直前の仕訳（reversed）を指し、元の original は指さない
    assert_eq!(re_reversed.reverses(), Some(reversed.id()));
    assert_ne!(re_reversed.reverses(), Some(original.id()));
}

// R-10
//
// 「元仕訳と逆仕訳を合算すると全科目ゼロ」の完全な検証は `tests/trial_balance.rs` の
// `reverse_and_original_combined_have_zero_balance_for_every_account`（決定的な具体例）と
// `pt03_entry_plus_its_reversal_has_zero_balance_for_every_account`（任意の貸借一致明細に
// 対する性質としての検証）で行う。ここでは `JournalEntry` レベルで確認できる範囲、
// すなわち「逆仕訳の借方合計・貸方合計が元仕訳のそれと入れ替わっている」ことのみ検証する。
#[test]
fn reverse_and_original_totals_are_swapped() {
    let original = build(vec![
        debit("100", 70),
        debit("135", 30),
        credit("310", 60),
        credit("400", 40),
    ])
    .expect("貸借一致");
    let fy = fy_2026();
    let reversed = reverse_with(
        &original,
        date(2026, 4, 16),
        "入力誤り",
        &fy,
        &common::open_guard(),
    )
    .expect("逆仕訳の生成に成功する");

    assert_eq!(original.debit_total(), reversed.credit_total());
    assert_eq!(original.credit_total(), reversed.debit_total());
}

// R-11
#[test]
fn reverse_into_closed_period_is_error() {
    let original = build(vec![debit("100", 100), credit("400", 100)]).expect("貸借一致");
    let fy = fy_2026();
    let err = reverse_with(
        &original,
        date(2026, 4, 16),
        "入力誤り",
        &fy,
        &common::closed_guard(),
    )
    .unwrap_err();

    assert!(matches!(err, CoreError::PeriodClosed { .. }));
}

// R-12
#[test]
fn reverse_belongs_to_the_fiscal_year_of_the_specified_date_even_if_original_is_different_year() {
    let fy_2025 = FiscalYear::calendar_year(2025);
    let original = try_new(
        vec![debit("100", 100), credit("400", 100)],
        date(2025, 6, 15),
        "前年度の仕訳",
        &fy_2025,
        &common::test_chart(),
        &common::test_schema(),
        &common::open_guard(),
    )
    .expect("前年度の仕訳として成立する");
    assert_eq!(original.fiscal_year(), 2025);

    let fy_2026 = fy_2026();
    let reversed = reverse_with(
        &original,
        date(2026, 1, 10),
        "前年度分の訂正",
        &fy_2026,
        &common::open_guard(),
    )
    .expect("今年度の日付を指定すれば逆仕訳は成立する");

    assert_eq!(reversed.fiscal_year(), 2026);
    assert_eq!(reversed.entry_date().year(), 2026);
}
