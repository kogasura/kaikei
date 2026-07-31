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
    // このモジュールのダミー（`SequentialNumbering`）は `testing.rs` の
    // 公開ダミーとほぼ同じ形をしている。これは重複ではなく意図的な分離:
    // `testing.rs` は `test-doubles` feature 配下でのみコンパイルされるため、
    // feature を付けない既定の `cargo test -p kaikei-policy` では存在しない。
    // dyn 互換性（object safety）は feature の有無に関わらず常に保証したいので、
    // ここに feature 非依存の最小ダミーを個別に用意している。
    // ロジックを変更する際は両方（このファイルと `testing.rs`）の同期を
    // 忘れないこと（`checked_add` への修正で一度この同期を怠った）。
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
            let next = match issued {
                None => 1,
                // `+ 1` を無検証で行うと `EntryNumber(u32::MAX)` のとき debug では
                // panic、release では無言に `0`（＝「未払い出し」を表す `None` と
                // 実質衝突する値）へラップする（D-018/D-020 と同じ欠陥クラス）。
                Some(n) => {
                    n.as_u32()
                        .checked_add(1)
                        .ok_or_else(|| PolicyError::InvalidPolicyData {
                            reason: "仕訳番号が u32 の上限に達しました".to_string(),
                        })?
                }
            };
            Ok(EntryNumber::new(next))
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

    // 回帰テスト: `EntryNumber(u32::MAX)` の次を求めるとエラーになり、
    // 無検証の `+ 1` のように `0` へ無言でラップしないことを確認する。
    #[test]
    fn numbering_at_u32_max_returns_error_instead_of_wrapping() {
        let numbering = SequentialNumbering;
        let fy = FiscalYear::calendar_year(2026);
        let result = numbering.peek(&fy, Some(EntryNumber::new(u32::MAX)));
        assert!(
            matches!(result, Err(PolicyError::InvalidPolicyData { .. })),
            "u32::MAX の次はエラーになるべき（無言のラップは禁止）: {result:?}"
        );
    }
}
