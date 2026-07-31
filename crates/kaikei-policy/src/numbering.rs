//! 仕訳番号の払い出し規則（`Numbering`）。
//!
//! 実際の払い出し（カウンタの読み書き）は store の I/O が担う。ここでは
//! 「次にどの番号を払い出すべきか」という規則のみを定義する。

use crate::error::PolicyError;
use kaikei_core::{EntryNumber, FiscalYear};

/// 仕訳番号の採番規則。
pub trait Numbering: Send + Sync {
    /// 指定した会計年度で次に払い出すべき仕訳番号を返す。
    ///
    /// `issued` は直近で払い出し済みの番号（未払い出しなら `None`）。
    fn peek(
        &self,
        fy: &FiscalYear,
        issued: Option<EntryNumber>,
    ) -> Result<EntryNumber, PolicyError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// 直近の番号の次を返すだけの最小実装。dyn 互換性の検査専用。
    struct SequentialNumbering;

    impl Numbering for SequentialNumbering {
        fn peek(
            &self,
            _fy: &FiscalYear,
            issued: Option<EntryNumber>,
        ) -> Result<EntryNumber, PolicyError> {
            Ok(EntryNumber::new(issued.map_or(1, |n| n.as_u32() + 1)))
        }
    }

    // dyn 互換性の静的検査。
    fn _dyn(_: &dyn Numbering) {}

    #[test]
    fn numbering_is_object_safe() {
        let numbering = SequentialNumbering;
        _dyn(&numbering);
    }

    #[test]
    fn numbering_can_be_used_as_arc_dyn() {
        let numbering: Arc<dyn Numbering> = Arc::new(SequentialNumbering);
        let fy = FiscalYear::calendar_year(2026);
        assert_eq!(numbering.peek(&fy, None).unwrap().as_u32(), 1);
        assert_eq!(
            numbering
                .peek(&fy, Some(EntryNumber::new(5)))
                .unwrap()
                .as_u32(),
            6
        );
    }
}
