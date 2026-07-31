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

// `tests/` 配下の各統合テストファイルはそれぞれ独立したバイナリとしてこのモジュールを
// 取り込む。ヘルパーによっては一部のテストファイルからしか使われないため、
// 使わないファイル側で `dead_code` 警告が出る。このモジュール全体を共通ライブラリと
// 位置づけ、警告を抑止する。
#![allow(dead_code)]

use kaikei_core::{
    AccountCode, AccountDef, AccountType, AccountingDate, ChartOfAccounts, Currency, FixedClock,
    JournalLine, Money, PeriodGuard, PeriodStatus, Side, TagDef, TagKey, TagSchema, TagSet,
    TagValueType, Timestamp,
};
use proptest::prelude::*;

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

/// `balanced_lines_strategy` が明細の科目として使う候補。
///
/// `test_schema()` でタグが必須（`required_for`）になっていない資産・負債・純資産の
/// 科目のみを選ぶ。ランダム生成のたびに `tax_category` タグを用意する必要がなくなり、
/// 生成戦略をシンプルに保てる（収益・費用科目の必須タグ検証は `journal.rs` の
/// J-52 等、他のテストで別途カバーされている）。
const BALANCED_LINE_ACCOUNTS: [&str; 8] = ["100", "135", "180", "310", "330", "400", "410", "420"];

/// `total` を正の整数 `count` 個に分割する `proptest` 戦略。
///
/// 各要素は `1..=(total / count).max(1)` の範囲で独立に選び、端数（切り捨てにより
/// 生じた余り）は最後の要素に足し込む。各要素の初期値が 1 以上であることと、
/// `count` 個の合計が `total` を超えないように上限を選んでいることから、
/// 端数を足し込んだ後も全要素が 1 以上のまま合計はちょうど `total` になる。
fn split_into_parts(total: i128, count: usize) -> impl Strategy<Value = Vec<i128>> {
    let max_weight = (total / count as i128).max(1);
    proptest::collection::vec(1..=max_weight, count).prop_map(move |mut parts| {
        let used: i128 = parts.iter().sum();
        let leftover = total - used;
        let last = parts.len() - 1;
        parts[last] += leftover;
        parts
    })
}

/// 貸借一致した `JournalLine` の集合（2〜6行）を生成する `proptest` 戦略。
///
/// 借方側・貸方側それぞれ独立に、同じ合計金額 `total` を 1〜3 行に分割することで
/// 貸借一致を構造的に保証する（後から金額を調整して合わせるのではなく、
/// 生成の時点で一致以外の値が作れない）。通貨は JPY 固定、タグは付けない
/// （`BALANCED_LINE_ACCOUNTS` の科目はタグ必須ではないため）。
pub fn balanced_lines_strategy() -> impl Strategy<Value = Vec<JournalLine>> {
    (1i128..=1_000_000, 1usize..=3, 1usize..=3)
        .prop_flat_map(|(total, debit_count_req, credit_count_req)| {
            let debit_count = debit_count_req.min(total as usize).max(1);
            let credit_count = credit_count_req.min(total as usize).max(1);
            (
                split_into_parts(total, debit_count),
                split_into_parts(total, credit_count),
                proptest::collection::vec(
                    0..BALANCED_LINE_ACCOUNTS.len(),
                    debit_count + credit_count,
                ),
            )
        })
        .prop_map(|(debit_amounts, credit_amounts, account_indices)| {
            let mut account_indices = account_indices.into_iter();
            let mut lines = Vec::with_capacity(debit_amounts.len() + credit_amounts.len());
            for amount in debit_amounts {
                lines.push(balanced_line(&mut account_indices, Side::Debit, amount));
            }
            for amount in credit_amounts {
                lines.push(balanced_line(&mut account_indices, Side::Credit, amount));
            }
            lines
        })
}

/// `balanced_lines_strategy` 内部専用。次の科目インデックスと金額から1明細を作る。
fn balanced_line(
    account_indices: &mut impl Iterator<Item = usize>,
    side: Side,
    amount: i128,
) -> JournalLine {
    let code = BALANCED_LINE_ACCOUNTS[account_indices
        .next()
        .expect("account_indices の要素数は明細数と一致するよう生成している")];
    JournalLine::new(
        AccountCode::parse(code).expect("BALANCED_LINE_ACCOUNTS は常に有効な科目コード"),
        side,
        Money::from_minor(amount, Currency::JPY),
        TagSet::new(),
        None,
    )
    .expect("balanced_lines_strategy が生成する金額は常に正の値")
}
