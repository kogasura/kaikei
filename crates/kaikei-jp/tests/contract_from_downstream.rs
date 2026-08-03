//! ★契約凍結点★ の検証（`kaikei-jp` 側）: `docs/07-mcp-server.md` §4 の
//! **呼び出し経路 (c)**——`kaikei-app` を経由せず、合成ルートが保持する
//! `kaikei-jp` の値から直接組み立てるツール（`list_tax_categories` /
//! `post_journal_entry` の `tags` 変換）——を、実際に書いてみるプローブ。
//!
//! `crates/kaikei-app/tests/contract_from_downstream.rs` の `kaikei-jp` 版で
//! あり、同じ限界を持つ（後述）。
//!
//! # このテストが検査できること
//!
//! 統合テストは `kaikei-jp` を**外部 crate としてリンクする**ので、
//! `pub` でないものは見えない。PR-B 2巡目の消費側が実測した2つの
//! コンパイルエラーが、公開 API だけで解消されていることを踏む:
//!
//! 1. **線上（JSON）の `tags`（文字列 → 文字列）から `TagSet` を作れること。**
//!    2巡目は `TagSchema` からキーの `TagValueType` を引き戻せず
//!    （`no method named value_type found for reference &TagSchema`）、
//!    `TagValue::Decimal` を作るには小数の crate が要った
//!    （`cannot find module or crate rust_decimal`）。
//!    → `tags::TagCatalog` に集約した（`kaikei-core` は無変更）
//! 2. **`TaxRuleSets` が保持するマスタを列挙できること。**
//!    2巡目は公開メソッドが `from_embedded` / `new` / `for_date` の3つだけで、
//!    「どの期間なら有効か」を答えられなかった
//!    （`no method named iter found for struct TaxRuleSets`）。
//!    → `iter` / `available_ranges_display` / `require_for_date` を足した
//!
//! # このテストが検査**できない**こと
//!
//! 下流（`kaikei-mcp`）の `Cargo.toml` に依存が増えないことは、ここでは
//! 検査できない（統合テストには `kaikei-jp` の依存がそのままリンクされる）。
//! [`the_probe_itself_does_not_reach_for_crates_outside_kaikei_jp`] が
//! ソース上の自制を機械的に固定するだけである。本物の検査は PR-D
//! （`kaikei-mcp` 新設）で CI に置く。理由の詳細は
//! `crates/kaikei-app/tests/contract_from_downstream.rs` のモジュールdoc。

use kaikei_core::{AccountCode, AccountType, AccountingDate, TagValue};
use kaikei_jp::compose::{compose, ComposeOptions, Composition};
use kaikei_jp::error::JpError;
use kaikei_jp::tags::tag_value_to_string;
use kaikei_jp::tax::{JpSettingsOverrides, TaxRuleSets};

// ---- 合成ルート（kaikei-mcp の config.rs）が書く形 ----

fn date(year: i32, month: u8, day: u8) -> AccountingDate {
    AccountingDate::new(year, month, day).unwrap()
}

/// 起動時の組み立て。**同梱データを指す定数（`kaikei-jp-data`）を下流が
/// 名指しする必要が無い**ことも、ここで踏んでいる（`compose` と
/// `TaxRuleSets::from_embedded` が内部で読む）。
fn composition() -> Composition {
    compose(ComposeOptions {
        rule_sets: TaxRuleSets::from_embedded().unwrap(),
        settings_overrides: JpSettingsOverrides {
            tax_mode: None,
            rounding: None,
            rounding_unit: None,
            is_taxable_business: true,
            simplified_taxation: false,
        },
        defaults_as_of: date(2026, 4, 1),
        closing_accounts: kaikei_jp::closing::ClosingAccounts {
            capital: AccountCode::parse("400").unwrap(),
            owner_drawings: AccountCode::parse("410").unwrap(),
            owner_contributions: AccountCode::parse("420").unwrap(),
        },
        closing_tax_category: Some("NOT_APPLICABLE".to_string()),
    })
    .unwrap()
}

// ---- 0. プローブ自身の制約 ----

/// このプローブが `kaikei-jp` の外の crate に手を伸ばしていないことを
/// ソースを読んで検査する（限界はモジュールdocを参照）。
#[test]
fn the_probe_itself_does_not_reach_for_crates_outside_kaikei_jp() {
    const SOURCE: &str = include_str!("contract_from_downstream.rs");

    // `rust_decimal`: タグの小数値を作る／読むのに下流が要らないこと
    //   （2巡目の消費側が実際に踏んだコンパイルエラー）
    // `kaikei_jp_data`: 同梱データの定数を下流が名指しせずに済むこと
    // `uuid` / `serde_json` / `sqlx` / `chrono`: kaikei-app 側のプローブと同じ理由
    for name in [
        "rust_decimal",
        "kaikei_jp_data",
        "uuid",
        "serde_json",
        "sqlx",
        "chrono",
    ] {
        let path_prefix = format!("{name}{}", "::");
        let use_stmt = format!("use {name}");
        assert!(
            !SOURCE.contains(&path_prefix),
            "このプローブが {name} を直接使っている。\
             kaikei-jp の公開 API に入口が無いなら、それは契約の穴である\
             （下流は同じ行を書けない）"
        );
        assert!(
            !SOURCE.contains(&use_stmt),
            "このプローブが {name} を use している（同上）"
        );
    }
}

// ---- 1. post_journal_entry の `tags`（文字列マップ）→ TagSet ----

/// `docs/07-mcp-server.md` §3 の入力例そのままの形を変換する。
#[test]
fn the_wire_tags_map_can_be_converted_to_a_tag_set_downstream() {
    let composition = composition();
    let catalog = &composition.tag_catalog;

    // MCP の DTO（`BTreeMap<String, String>` 相当）から素直に渡せること。
    let tags = catalog
        .parse_tag_set([
            ("tax_category", "SALES_10"),
            ("counterparty", "CP0001"),
            ("business_ratio", "0.30"),
        ])
        .unwrap();

    // 型付きの値になっている（下流は型を知らないまま渡してよい）。
    let tax_category = tags
        .iter()
        .find(|(key, _)| key.as_str() == "tax_category")
        .map(|(_, value)| value)
        .unwrap();
    assert!(matches!(tax_category, TagValue::Code(code) if code == "SALES_10"));

    // 小数の crate に依存せずに小数タグを組み立てられ、応答にも載せられる。
    let business_ratio = tags
        .iter()
        .find(|(key, _)| key.as_str() == "business_ratio")
        .map(|(_, value)| value)
        .unwrap();
    assert!(matches!(business_ratio, TagValue::Decimal(_)));
    assert_eq!(tag_value_to_string(business_ratio), "0.30");

    // 組み立てた TagSet はそのまま core の検証に渡せる（同じ値から引ける）。
    catalog
        .schema()
        .validate(&tags, AccountType::Expense)
        .unwrap();
}

/// 未登録キーは黙って通らず、AI が次の手を取れる文言になる
/// （`CLAUDE.md` §4・§11）。
#[test]
fn an_unregistered_tag_key_is_rejected_with_the_valid_keys_downstream() {
    let composition = composition();
    let err = composition
        .tag_catalog
        .parse_tag_set([("tax_cat", "SALES_10")])
        .unwrap_err();

    assert!(matches!(err, JpError::UnregisteredTagKey { .. }));
    let message = err.to_string();
    assert!(message.contains("tax_cat"), "message = {message}");
    assert!(message.contains("tax_category"), "message = {message}");
}

/// 型に合わない値も同様（「小数として解釈できない」ことと期待する書式が出る）。
#[test]
fn a_value_that_does_not_match_the_declared_type_is_rejected_downstream() {
    let composition = composition();
    let err = composition
        .tag_catalog
        .parse_tag_set([("business_ratio", "3割")])
        .unwrap_err();

    assert!(matches!(err, JpError::InvalidTagValue { .. }));
    let message = err.to_string();
    assert!(message.contains("business_ratio"), "message = {message}");
    assert!(message.contains("小数"), "message = {message}");
}

// ---- 2. list_tax_categories ----

/// 指定日時点で有効な区分を、`kaikei-jp` の公開 API だけで列挙できること。
#[test]
fn list_tax_categories_can_be_built_from_kaikei_jp_alone() {
    let rule_sets = TaxRuleSets::from_embedded().unwrap();
    let table = rule_sets.require_for_date(date(2026, 4, 1)).unwrap();

    // 応答に載せる形（区分コードと表示名、マスタの有効期間）。
    let codes: Vec<&str> = table.categories().map(|c| c.code.as_str()).collect();
    assert!(codes.contains(&"SALES_10"), "codes = {codes:?}");
    assert!(table.range_display().contains('〜'));

    // 同梱されているマスタ自体も列挙できる（`get_settings` の診断用）。
    assert_eq!(rule_sets.len(), rule_sets.iter().count());
    assert!(!rule_sets.is_empty());
}

/// 該当する年度マスタが無い日付では、**空配列ではなくエラー**を返せること
/// （`docs/07-mcp-server.md` §2）。有効期間がメッセージに含まれる。
#[test]
fn a_date_without_a_master_yields_an_error_that_names_the_valid_ranges_downstream() {
    let rule_sets = TaxRuleSets::from_embedded().unwrap();

    // `for_date` の None は正常な戻り値（D-055）。
    assert!(rule_sets.for_date(date(1990, 1, 1)).is_none());

    // その None を「エラーとして扱う」呼び出し元のための入口。
    let err = rule_sets.require_for_date(date(1990, 1, 1)).unwrap_err();
    assert!(matches!(err, JpError::NoApplicableTaxRuleSet { .. }));

    let message = err.to_string();
    assert!(message.contains("1990-01-01"), "message = {message}");
    assert!(message.contains("取引日"), "message = {message}");
    assert!(
        message.contains(&rule_sets.available_ranges_display()),
        "有効期間が示されていない: {message}"
    );
}
