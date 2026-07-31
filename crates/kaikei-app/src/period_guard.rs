//! 会計期間の締め状態を判定する `PeriodGuard` の実装（[`ClosedPeriodGuard`]）。

use kaikei_core::{AccountingDate, PeriodGuard, PeriodStatus};

/// store から読み込んだ「どこまで締まっているか」を保持し、`PeriodGuard` として
/// 判定する。
///
/// [`PeriodGuard::status`] は同期の純関数なので DB を直接引く実装は原理的に
/// 書けない。store（[`crate::ports::PeriodRepo`]）は「締められている期間の
/// 終端日」という生データだけを返し、ここでスナップショットに固める。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClosedPeriodGuard {
    closed_through: Option<AccountingDate>,
}

impl ClosedPeriodGuard {
    /// store から読み込んだ「締められている期間の終端日」からガードを作る。
    /// `None` なら締められている期間が無い（全期間 Open）。
    pub fn new(closed_through: Option<AccountingDate>) -> Self {
        ClosedPeriodGuard { closed_through }
    }

    /// 締められている期間が無いガードを作る（テスト用）。
    ///
    /// `kaikei-core/examples/hello_kaikei.rs` の `AlwaysOpen` と役割が重複するが、
    /// example から `kaikei-app` に依存させないための意図的な重複である。
    pub fn all_open() -> Self {
        ClosedPeriodGuard {
            closed_through: None,
        }
    }
}

impl PeriodGuard for ClosedPeriodGuard {
    fn status(&self, date: AccountingDate) -> PeriodStatus {
        match self.closed_through {
            Some(end) if date <= end => PeriodStatus::Closed,
            _ => PeriodStatus::Open,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_open_guard_is_always_open() {
        let guard = ClosedPeriodGuard::all_open();
        let date = AccountingDate::new(2020, 1, 1).unwrap();
        assert_eq!(guard.status(date), PeriodStatus::Open);
    }

    #[test]
    fn dates_on_or_before_closed_through_are_closed() {
        let end = AccountingDate::new(2025, 12, 31).unwrap();
        let guard = ClosedPeriodGuard::new(Some(end));
        assert_eq!(guard.status(end), PeriodStatus::Closed);
        assert_eq!(
            guard.status(AccountingDate::new(2025, 1, 1).unwrap()),
            PeriodStatus::Closed
        );
    }

    #[test]
    fn dates_after_closed_through_are_open() {
        let end = AccountingDate::new(2025, 12, 31).unwrap();
        let guard = ClosedPeriodGuard::new(Some(end));
        assert_eq!(
            guard.status(AccountingDate::new(2026, 1, 1).unwrap()),
            PeriodStatus::Open
        );
    }
}
