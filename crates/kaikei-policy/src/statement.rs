//! 財務諸表の様式（`StatementPolicy`）。

use kaikei_core::{AccountCode, Money, TrialBalance};

/// 財務諸表の1行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatementLine {
    /// 対応する勘定科目コード。
    pub account: AccountCode,
    /// 表示ラベル（個別の科目名とは限らず、集計区分名等になることもある）。
    pub label: String,
    /// 金額。
    pub amount: Money,
}

/// 財務諸表の区分（例: 流動資産、経常収益）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatementSection {
    /// 区分名。
    pub title: String,
    /// この区分に属する行。
    pub lines: Vec<StatementLine>,
    /// この区分の小計。
    pub subtotal: Money,
}

/// 財務諸表（貸借対照表・損益計算書等）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement {
    /// 表題（例: "貸借対照表"）。
    pub title: String,
    /// 区分の一覧。
    pub sections: Vec<StatementSection>,
    /// 合計。
    pub total: Money,
}

/// 財務諸表の様式を規定する。
///
/// 会計基準そのもの（③層）は実装しない（`DECISIONS.md` D-011）。この trait は
/// 個人事業主向けの1様式を実装するための穴でしかない。
pub trait StatementPolicy: Send + Sync {
    /// 貸借対照表を組み立てる。
    fn balance_sheet(&self, tb: &TrialBalance) -> Statement;

    /// 損益計算書を組み立てる。
    fn income_statement(&self, tb: &TrialBalance) -> Statement;
}

#[cfg(test)]
mod tests {
    // このモジュールのダミー（`EmptyStatement`）は `testing.rs` の
    // `ByAccountTypeStatement` と役割が重複して見えるが、これは意図的な分離:
    // `testing.rs` は `test-doubles` feature 配下でのみコンパイルされるため、
    // feature を付けない既定の `cargo test -p kaikei-policy` では存在しない。
    // dyn 互換性（object safety）は feature の有無に関わらず常に保証したいので、
    // ここに feature 非依存の最小ダミーを個別に用意している。
    // trait のメソッドシグネチャを変更する際は両方の同期を忘れないこと。
    use super::*;
    use kaikei_core::{ChartOfAccounts, Currency, TagSchema};
    use std::sync::Arc;

    /// 常に空の財務諸表を返す最小の `StatementPolicy`。dyn 互換性の検査専用。
    struct EmptyStatement;

    impl StatementPolicy for EmptyStatement {
        fn balance_sheet(&self, _tb: &TrialBalance) -> Statement {
            Statement {
                title: "貸借対照表".to_string(),
                sections: Vec::new(),
                total: Money::zero(Currency::JPY),
            }
        }

        fn income_statement(&self, _tb: &TrialBalance) -> Statement {
            Statement {
                title: "損益計算書".to_string(),
                sections: Vec::new(),
                total: Money::zero(Currency::JPY),
            }
        }
    }

    fn empty_trial_balance() -> TrialBalance {
        TrialBalance::from_entries(
            std::iter::empty(),
            &ChartOfAccounts::new(vec![]).unwrap(),
            &TagSchema::empty(),
            &[],
        )
        .unwrap()
    }

    // dyn 互換性の静的検査。
    fn _dyn(_: &dyn StatementPolicy) {}

    #[test]
    fn statement_policy_is_object_safe() {
        let policy = EmptyStatement;
        _dyn(&policy);
    }

    #[test]
    fn statement_policy_can_be_used_as_arc_dyn() {
        let policy: Arc<dyn StatementPolicy> = Arc::new(EmptyStatement);
        let tb = empty_trial_balance();
        let bs = policy.balance_sheet(&tb);
        assert_eq!(bs.title, "貸借対照表");
        let is = policy.income_statement(&tb);
        assert_eq!(is.title, "損益計算書");
    }
}
