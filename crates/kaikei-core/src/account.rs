//! 勘定科目と勘定科目表。
//!
//! `AccountCode` の中身（"500" が売上高であること等）に意味は core に持たせない。
//! 表示名や日本語の勘定科目名は `AccountDef.name` として外から渡されるだけ
//! （`CLAUDE.md` §1）。

use crate::error::CoreError;
use std::collections::{BTreeMap, BTreeSet};

/// 勘定科目コード。core は中身の意味を知らない。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AccountCode(String);

impl AccountCode {
    /// 英数字とハイフンのみ、1〜32文字の科目コードを解釈する。
    pub fn parse(s: &str) -> Result<Self, CoreError> {
        let invalid = || CoreError::InvalidValue {
            reason: format!(
                "勘定科目コードは英数字とハイフンのみ、1〜32文字である必要があります: \"{s}\""
            ),
        };
        if s.is_empty() || s.chars().count() > 32 {
            return Err(invalid());
        }
        if !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Err(invalid());
        }
        Ok(AccountCode(s.to_string()))
    }

    /// 科目コードの文字列表現を返す。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 5要素。世界共通の分類なので core に置く。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountType {
    /// 資産。
    Asset,
    /// 負債。
    Liability,
    /// 純資産。
    Equity,
    /// 収益。
    Revenue,
    /// 費用。
    Expense,
}

impl AccountType {
    /// 残高計算の向き。借方残高で正の値になる科目なら `true`。
    ///
    /// `Asset`・`Expense` は `true`。`Liability`・`Equity`・`Revenue` は `false`。
    pub fn is_debit_normal(&self) -> bool {
        matches!(self, AccountType::Asset | AccountType::Expense)
    }

    /// 5要素分類の日本語ラベルを返す（資産・負債・純資産・収益・費用）。
    ///
    /// `CLAUDE.md` §1 が禁止する「勘定科目の日本語名」は現金・売掛金等の
    /// 個別科目名を指す。`AccountType` は世界共通の5要素分類であり、
    /// `docs/01-core-types.md` が明示的に core に置くと定めているため対象外。
    pub fn label_ja(&self) -> &'static str {
        match self {
            AccountType::Asset => "資産",
            AccountType::Liability => "負債",
            AccountType::Equity => "純資産",
            AccountType::Revenue => "収益",
            AccountType::Expense => "費用",
        }
    }
}

/// 勘定科目の定義。
#[derive(Debug, Clone)]
pub struct AccountDef {
    /// 科目コード。
    pub code: AccountCode,
    /// 表示名。core は意味を持たせない。
    pub name: String,
    /// 5要素分類。
    pub account_type: AccountType,
    /// 親科目のコード（あれば）。
    pub parent: Option<AccountCode>,
    /// 記帳可能かどうか。`false` なら集計専用（見出し科目）。
    pub postable: bool,
}

/// 勘定科目表。
///
/// `new` の時点で親の存在・循環参照・コードの重複を検証するため、
/// 構築に成功した `ChartOfAccounts` は常に整合した木構造になっている。
#[derive(Debug, Clone)]
pub struct ChartOfAccounts {
    accounts: BTreeMap<AccountCode, AccountDef>,
}

impl ChartOfAccounts {
    /// 勘定科目表を構築する。
    ///
    /// 以下を検証し、違反があれば `CoreError::InvalidChart` を返す。
    /// - 科目コードの重複
    /// - 親科目コードが表内に存在すること
    /// - 親をたどって自身に戻る循環参照が無いこと
    pub fn new(defs: Vec<AccountDef>) -> Result<Self, CoreError> {
        let mut accounts = BTreeMap::new();
        for def in defs {
            if accounts.contains_key(&def.code) {
                return Err(CoreError::InvalidChart {
                    reason: format!("勘定科目コードが重複しています: {}", def.code.as_str()),
                });
            }
            accounts.insert(def.code.clone(), def);
        }

        for def in accounts.values() {
            if let Some(parent) = &def.parent {
                if !accounts.contains_key(parent) {
                    return Err(CoreError::InvalidChart {
                        reason: format!(
                            "科目 {} の親科目 {} が勘定科目表に存在しません",
                            def.code.as_str(),
                            parent.as_str()
                        ),
                    });
                }
            }
        }

        for def in accounts.values() {
            let mut visited = BTreeSet::new();
            let mut current = def;
            visited.insert(&current.code);
            while let Some(parent_code) = &current.parent {
                if visited.contains(parent_code) {
                    return Err(CoreError::InvalidChart {
                        reason: format!(
                            "循環参照があります: {} から辿ると自身に戻ります",
                            def.code.as_str()
                        ),
                    });
                }
                let parent = accounts
                    .get(parent_code)
                    .expect("直前のループで親の存在は検証済み");
                visited.insert(&parent.code);
                current = parent;
            }
        }

        Ok(ChartOfAccounts { accounts })
    }

    /// 科目コードから科目定義を取得する。
    pub fn get(&self, code: &AccountCode) -> Option<&AccountDef> {
        self.accounts.get(code)
    }

    /// 全科目定義を巡回する。
    ///
    /// 内部で `BTreeMap::values()` を使っているため、返す順序は科目コードの
    /// 昇順で決定的である（`descendants` とは異なり順序が規定されている）。
    pub fn iter(&self) -> impl Iterator<Item = &AccountDef> {
        self.accounts.values()
    }

    /// 指定した科目の子孫（子・孫・…）すべてを返す。指定科目自身は含まない。
    ///
    /// 戻り値の順序は未規定。呼び出し側は順序に依存しないこと。
    /// 指定コードが勘定科目表に存在しない場合は空の `Vec` を返す。
    pub fn descendants(&self, code: &AccountCode) -> Vec<&AccountDef> {
        let mut result = Vec::new();
        let mut frontier = vec![code.clone()];
        while let Some(current) = frontier.pop() {
            for def in self.accounts.values() {
                if def.parent.as_ref() == Some(&current) {
                    result.push(def);
                    frontier.push(def.code.clone());
                }
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(
        code: &str,
        account_type: AccountType,
        parent: Option<&str>,
        postable: bool,
    ) -> AccountDef {
        AccountDef {
            code: AccountCode::parse(code).unwrap(),
            name: format!("科目{code}"),
            account_type,
            parent: parent.map(|p| AccountCode::parse(p).unwrap()),
            postable,
        }
    }

    // A-01
    #[test]
    fn account_code_parse_numeric_succeeds() {
        assert!(AccountCode::parse("500").is_ok());
    }

    // A-02
    #[test]
    fn account_code_parse_empty_is_error() {
        assert!(AccountCode::parse("").is_err());
    }

    // A-03
    #[test]
    fn account_code_parse_non_alphanumeric_is_error() {
        assert!(AccountCode::parse("あいう").is_err());
    }

    // A-04
    #[test]
    fn account_code_parse_too_long_is_error() {
        let code = "a".repeat(33);
        assert!(AccountCode::parse(&code).is_err());
    }

    // A-05
    #[test]
    fn account_type_asset_is_debit_normal() {
        assert!(AccountType::Asset.is_debit_normal());
    }

    // A-06
    #[test]
    fn account_type_expense_is_debit_normal() {
        assert!(AccountType::Expense.is_debit_normal());
    }

    // A-07
    #[test]
    fn account_type_liability_is_not_debit_normal() {
        assert!(!AccountType::Liability.is_debit_normal());
    }

    // A-08
    #[test]
    fn account_type_equity_is_not_debit_normal() {
        assert!(!AccountType::Equity.is_debit_normal());
    }

    // A-09
    #[test]
    fn account_type_revenue_is_not_debit_normal() {
        assert!(!AccountType::Revenue.is_debit_normal());
    }

    // A-10
    #[test]
    fn chart_of_accounts_new_missing_parent_is_error() {
        let defs = vec![account("100", AccountType::Asset, Some("999"), true)];
        assert!(matches!(
            ChartOfAccounts::new(defs),
            Err(CoreError::InvalidChart { .. })
        ));
    }

    // A-11
    #[test]
    fn chart_of_accounts_new_two_node_cycle_is_error() {
        let defs = vec![
            account("A", AccountType::Asset, Some("B"), true),
            account("B", AccountType::Asset, Some("A"), true),
        ];
        assert!(matches!(
            ChartOfAccounts::new(defs),
            Err(CoreError::InvalidChart { .. })
        ));
    }

    // A-11 (自己参照)
    #[test]
    fn chart_of_accounts_new_self_reference_is_error() {
        let defs = vec![account("A", AccountType::Asset, Some("A"), true)];
        assert!(matches!(
            ChartOfAccounts::new(defs),
            Err(CoreError::InvalidChart { .. })
        ));
    }

    // A-11 (3階層以上の循環)
    #[test]
    fn chart_of_accounts_new_three_node_cycle_is_error() {
        let defs = vec![
            account("A", AccountType::Asset, Some("B"), true),
            account("B", AccountType::Asset, Some("C"), true),
            account("C", AccountType::Asset, Some("A"), true),
        ];
        assert!(matches!(
            ChartOfAccounts::new(defs),
            Err(CoreError::InvalidChart { .. })
        ));
    }

    // A-12
    #[test]
    fn chart_of_accounts_new_duplicate_code_is_error() {
        let defs = vec![
            account("100", AccountType::Asset, None, true),
            account("100", AccountType::Asset, None, true),
        ];
        assert!(matches!(
            ChartOfAccounts::new(defs),
            Err(CoreError::InvalidChart { .. })
        ));
    }

    // A-13
    #[test]
    fn descendants_returns_children_and_grandchildren() {
        let defs = vec![
            account("100", AccountType::Asset, None, false),
            account("110", AccountType::Asset, Some("100"), false),
            account("111", AccountType::Asset, Some("110"), true),
        ];
        let chart = ChartOfAccounts::new(defs).unwrap();
        let root = AccountCode::parse("100").unwrap();
        let descendants = chart.descendants(&root);
        let codes: BTreeSet<&str> = descendants.iter().map(|d| d.code.as_str()).collect();
        assert_eq!(codes.len(), 2);
        assert!(codes.contains("110"));
        assert!(codes.contains("111"));
    }

    // A-14
    #[test]
    fn descendants_of_leaf_node_is_empty() {
        let defs = vec![
            account("100", AccountType::Asset, None, false),
            account("110", AccountType::Asset, Some("100"), true),
        ];
        let chart = ChartOfAccounts::new(defs).unwrap();
        let leaf = AccountCode::parse("110").unwrap();
        assert!(chart.descendants(&leaf).is_empty());
    }

    #[test]
    fn chart_of_accounts_new_empty_is_ok() {
        assert!(ChartOfAccounts::new(vec![]).is_ok());
    }

    #[test]
    fn descendants_of_unknown_code_is_empty() {
        let defs = vec![account("100", AccountType::Asset, None, true)];
        let chart = ChartOfAccounts::new(defs).unwrap();
        let unknown = AccountCode::parse("999").unwrap();
        assert!(chart.descendants(&unknown).is_empty());
    }
}
