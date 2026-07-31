//! `journal.rs` の統合テスト用共通ヘルパー（`docs/02-test-cases.md` 「テスト補助」）。
//!
//! ## テスト配置の方針
//!
//! `kaikei-core` の Rust 統合テスト（`tests/` 配下）は公開APIを経由してしか
//! `JournalEntry` / `JournalLine` の private フィールドに触れられない。
//! `journal.rs` のテストケース（L-01〜L-04, J-01〜J-81, R-01〜R-12）は、
//! `JournalLine::new` / `JournalEntry::new` / `JournalEntry::reverse` という
//! 公開コンストラクタと公開ゲッターだけで組み立て・検証できる
//! （private フィールドへの直接アクセスが必要なケースは無い）。
//!
//! そのため本プロジェクトでは、journal.rs 関連のテストは
//! `src/journal.rs` 内の `#[cfg(test)] mod tests` ではなく、
//! この `tests/` 配下の統合テストに全面的に寄せる方針を採る
//! （`money.rs` 等の他モジュールは private フィールドを直接触るテストが
//! 必要になりやすいためインラインの `#[cfg(test)]` で書かれているが、
//! 集約1モジュールに閉じた `journal.rs` は逆に「公開APIだけで完結する」度合いが
//! 最も高いモジュールなので、統合テストに寄せた方が
//! `tests/common/mod.rs` のフィクスチャ（勘定科目表・タグスキーマ・期間ガード・
//! 時計）を全テストケースで使い回せて重複が減る）。
//! 将来 private フィールドの直接検証が必要になったら、そのテストだけ
//! `src/journal.rs` 内のユニットテストに切り替える。

use kaikei_core::{
    AccountCode, AccountDef, AccountType, AccountingDate, ChartOfAccounts, FixedClock, PeriodGuard,
    PeriodStatus, TagDef, TagKey, TagSchema, TagValueType, Timestamp,
};

/// テスト用の最小勘定科目表。
///
/// 科目コードは `crates/kaikei-jp-data/chart/sole_proprietor.yaml` と一致させる。
/// `999` のみ実データに存在しない、`postable = false`（見出し科目）検証専用のダミー科目。
pub fn test_chart() -> ChartOfAccounts {
    fn account(code: &str, name: &str, account_type: AccountType, postable: bool) -> AccountDef {
        AccountDef {
            code: AccountCode::parse(code).expect("テスト用科目コードは常に有効"),
            name: name.to_string(),
            account_type,
            parent: None,
            postable,
        }
    }

    ChartOfAccounts::new(vec![
        account("100", "現金", AccountType::Asset, true),
        account("135", "売掛金", AccountType::Asset, true),
        account("180", "仮払消費税等", AccountType::Asset, true),
        account("310", "買掛金", AccountType::Liability, true),
        account("330", "仮受消費税等", AccountType::Liability, true),
        account("400", "元入金", AccountType::Equity, true),
        account("410", "事業主貸", AccountType::Equity, true),
        account("420", "事業主借", AccountType::Equity, true),
        account("500", "売上高", AccountType::Revenue, true),
        account("609", "消耗品費", AccountType::Expense, true),
        account("615", "地代家賃", AccountType::Expense, true),
        account("999", "見出し科目", AccountType::Expense, false),
    ])
    .expect("テスト用勘定科目表は常に構築できる")
}

/// テスト用のタグスキーマ。
///
/// - `tax_category`: `Code`、集計軸として使用可、`Revenue`/`Expense` の明細で必須
/// - `counterparty`: `Code`、集計軸として使用可
/// - `business_ratio`: `Decimal`、集計軸としては使用不可
pub fn test_schema() -> TagSchema {
    fn key(s: &str) -> TagKey {
        TagKey::parse(s).expect("テスト用タグキーは常に有効")
    }

    TagSchema::new(vec![
        (
            key("tax_category"),
            TagDef {
                value_type: TagValueType::Code,
                aggregatable: true,
                required_for: vec![AccountType::Revenue, AccountType::Expense],
            },
        ),
        (
            key("counterparty"),
            TagDef {
                value_type: TagValueType::Code,
                aggregatable: true,
                required_for: vec![],
            },
        ),
        (
            key("business_ratio"),
            TagDef {
                value_type: TagValueType::Decimal,
                aggregatable: false,
                required_for: vec![],
            },
        ),
    ])
}

struct AlwaysOpen;

impl PeriodGuard for AlwaysOpen {
    fn status(&self, _date: AccountingDate) -> PeriodStatus {
        PeriodStatus::Open
    }
}

struct AlwaysClosed;

impl PeriodGuard for AlwaysClosed {
    fn status(&self, _date: AccountingDate) -> PeriodStatus {
        PeriodStatus::Closed
    }
}

/// 常に `PeriodStatus::Open` を返す `PeriodGuard`。
pub fn open_guard() -> impl PeriodGuard {
    AlwaysOpen
}

/// 常に `PeriodStatus::Closed` を返す `PeriodGuard`。
pub fn closed_guard() -> impl PeriodGuard {
    AlwaysClosed
}

/// テスト用の固定時刻 `Clock`。
pub fn fixed_clock() -> FixedClock {
    FixedClock(Timestamp::from_unix_nanos(1_700_000_000_000_000_000))
}
