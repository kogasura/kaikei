//! 決算振替仕訳の生成（`ClosingPolicy`）。
//!
//! 全メソッドは純関数。採番（`EntryId` / `EntryNumber`）は store の I/O のため、
//! ここでは仕訳を提案する [`ProposedEntry`] のみを返す（`DECISIONS.md` D-027）。

use crate::error::PolicyError;
use crate::proposal::ProposedEntry;
use kaikei_core::{FiscalYear, TrialBalance};

/// 決算振替仕訳の生成規則。
pub trait ClosingPolicy: Send + Sync {
    /// 収益・費用のゼロ化、元入金への振替等の決算振替仕訳を提案する。
    fn closing_entries(
        &self,
        tb: &TrialBalance,
        fy: &FiscalYear,
    ) -> Result<Vec<ProposedEntry>, PolicyError>;

    /// 期首の振替仕訳（前年度繰越等）を提案する。
    ///
    /// 個人事業主の元入金振替を当年度末と翌年期首のどちらに計上するかは
    /// 未確定（`docs/04-jp-tax.md` §9、税理士確認事項）。既定では何も
    /// 生成しない実装にしておき、両対応できるよう trait の形だけ用意する。
    fn opening_entries(
        &self,
        _tb: &TrialBalance,
        _fy: &FiscalYear,
    ) -> Result<Vec<ProposedEntry>, PolicyError> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_core::{ChartOfAccounts, TagSchema};
    use std::sync::Arc;

    struct NoClosing;

    impl ClosingPolicy for NoClosing {
        fn closing_entries(
            &self,
            _tb: &TrialBalance,
            _fy: &FiscalYear,
        ) -> Result<Vec<ProposedEntry>, PolicyError> {
            Ok(Vec::new())
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
    fn _dyn(_: &dyn ClosingPolicy) {}

    #[test]
    fn closing_policy_is_object_safe() {
        let policy = NoClosing;
        _dyn(&policy);
    }

    #[test]
    fn closing_policy_can_be_used_as_arc_dyn() {
        let policy: Arc<dyn ClosingPolicy> = Arc::new(NoClosing);
        let fy = FiscalYear::calendar_year(2026);
        let tb = empty_trial_balance();
        assert!(policy.closing_entries(&tb, &fy).unwrap().is_empty());
    }

    #[test]
    fn opening_entries_default_impl_returns_empty() {
        let policy = NoClosing;
        let fy = FiscalYear::calendar_year(2026);
        let tb = empty_trial_balance();
        assert!(policy.opening_entries(&tb, &fy).unwrap().is_empty());
    }
}
