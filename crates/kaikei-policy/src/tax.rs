//! 税額計算と税区分の妥当性（`TaxPolicy`）。
//!
//! 全メソッドは純関数。`async fn` にしない・`async_trait` を使わない
//! （`CLAUDE.md` §3 の中核）。必要なデータは呼び出し側（`kaikei-app`）が
//! 事前にロードし、[`TaxContext`] に詰めて渡す。

use crate::context::TaxContext;
use crate::error::PolicyError;
use crate::note::PolicyNote;
use kaikei_core::{AccountDef, JournalLine, Money, Ratio, RoundMode, TagSet};

/// [`TaxPolicy::derive_tax_lines`] の戻り値。
///
/// `lines` は**確定後の明細一覧**（入力された明細＋生成された税額行）であり、
/// 追加行だけではない。呼び出し側はこの `lines` で元の明細一覧を**置き換える**
/// （`extend` してはいけない）。税額が 0 になる行は含まない
/// （`JournalLine::new` が 0 円を `CoreError::InvalidAmount` として拒否するため）。
#[derive(Debug, Clone)]
pub struct TaxDerivation {
    /// 確定後の明細一覧。
    pub lines: Vec<JournalLine>,
    /// 断定的でない補足情報（非適格の経過措置の扱い等）。確定は人間に残す
    /// （`CLAUDE.md` §10）。
    pub notes: Vec<PolicyNote>,
}

/// 税額計算と税区分の妥当性を判定する。
///
/// 必要なデータは呼び出し側が [`TaxContext`] に詰めて渡すため、このトレイトの
/// メソッドは全て同期の純関数である。
pub trait TaxPolicy: Send + Sync {
    /// 明細のタグ（税区分等）が、その科目・その日付時点のルールに照らして
    /// 妥当かどうかを検証する。
    fn validate_tag(
        &self,
        ctx: &TaxContext<'_>,
        tags: &TagSet,
        account: &AccountDef,
    ) -> Result<(), PolicyError>;

    /// 税抜経理での消費税行を導出する。
    ///
    /// 戻り値は**確定後の明細一覧**（入力＋税額行）。追加行だけではない。
    /// 税額 0 の行は生成しない。冪等性は保証されないため、1回の記帳につき
    /// 1回だけ呼び出すこと。
    fn derive_tax_lines(
        &self,
        ctx: &TaxContext<'_>,
        lines: &[JournalLine],
    ) -> Result<TaxDerivation, PolicyError>;

    /// 端数処理の方式を返す。事業者が選択できる設定は実装（例: `JpTaxPolicy`）が
    /// 構築時に保持する。
    fn round_mode(&self, ctx: &TaxContext<'_>) -> RoundMode;

    /// 按分・税額計算に使う。既定実装は [`TaxPolicy::round_mode`] に従って丸める。
    ///
    /// `Money` は最小通貨単位の整数（`i128`）で端数を保持できないため、
    /// `round(Money) -> Money` は事実上の恒等関数にしかならない。代わりに
    /// 「金額 × 比率」の時点で丸めるこの形にする（`DECISIONS.md` D-026）。
    fn apply_ratio(
        &self,
        ctx: &TaxContext<'_>,
        base: Money,
        ratio: Ratio,
    ) -> Result<Money, PolicyError> {
        Ok(base.mul_ratio(ratio, self.round_mode(ctx))?)
    }
}

#[cfg(test)]
mod tests {
    // このモジュールのダミー（`AlwaysValidNoTax`）は `testing.rs` の
    // `NoTaxPolicy` とほぼ同じ形をしている。これは重複ではなく意図的な分離:
    // `testing.rs` は `test-doubles` feature 配下でのみコンパイルされるため、
    // feature を付けない既定の `cargo test -p kaikei-policy` では存在しない。
    // dyn 互換性（object safety）は feature の有無に関わらず常に保証したいので、
    // ここに feature 非依存の最小ダミーを個別に用意している。
    // trait のメソッドシグネチャを変更する際は両方の同期を忘れないこと。
    use super::*;
    use kaikei_core::{AccountingDate, ChartOfAccounts, Currency, TagSchema};
    use std::sync::Arc;

    /// 税行を一切生成しない最小の `TaxPolicy`。dyn 互換性の検査専用。
    struct AlwaysValidNoTax;

    impl TaxPolicy for AlwaysValidNoTax {
        fn validate_tag(
            &self,
            _ctx: &TaxContext<'_>,
            _tags: &TagSet,
            _account: &AccountDef,
        ) -> Result<(), PolicyError> {
            Ok(())
        }

        fn derive_tax_lines(
            &self,
            _ctx: &TaxContext<'_>,
            lines: &[JournalLine],
        ) -> Result<TaxDerivation, PolicyError> {
            Ok(TaxDerivation {
                lines: lines.to_vec(),
                notes: Vec::new(),
            })
        }

        fn round_mode(&self, _ctx: &TaxContext<'_>) -> RoundMode {
            RoundMode::Floor
        }
    }

    fn sample_context<'a>(
        chart: &'a ChartOfAccounts,
        schema: &'a TagSchema,
        counterparties: &'a crate::counterparty::CounterpartyIndex,
    ) -> TaxContext<'a> {
        TaxContext {
            as_of: AccountingDate::new(2026, 4, 1).unwrap(),
            chart,
            tag_schema: schema,
            counterparties,
        }
    }

    // dyn 互換性の静的検査。コンパイルが通ること自体がテスト
    // （ARCHITECTURE.md §6 の State 設計が成立することの証明）。
    fn _dyn(_: &dyn TaxPolicy) {}

    #[test]
    fn tax_policy_is_object_safe() {
        let policy = AlwaysValidNoTax;
        _dyn(&policy);
    }

    #[test]
    fn tax_policy_can_be_used_as_arc_dyn() {
        let policy: Arc<dyn TaxPolicy> = Arc::new(AlwaysValidNoTax);
        let chart = ChartOfAccounts::new(vec![]).unwrap();
        let schema = TagSchema::empty();
        let counterparties = crate::counterparty::CounterpartyIndex::empty();
        let ctx = sample_context(&chart, &schema, &counterparties);
        assert_eq!(policy.round_mode(&ctx), RoundMode::Floor);
    }

    #[test]
    fn apply_ratio_default_impl_uses_round_mode() {
        let policy = AlwaysValidNoTax;
        let chart = ChartOfAccounts::new(vec![]).unwrap();
        let schema = TagSchema::empty();
        let counterparties = crate::counterparty::CounterpartyIndex::empty();
        let ctx = sample_context(&chart, &schema, &counterparties);

        let base = Money::from_minor(100, Currency::JPY);
        let ratio = Ratio::parse_fraction("0.333").unwrap();
        let result = policy.apply_ratio(&ctx, base, ratio).unwrap();
        assert_eq!(result.minor(), 33); // Floor
    }

    #[test]
    fn derive_tax_lines_returns_lines_unchanged_when_no_tax_applies() {
        let policy = AlwaysValidNoTax;
        let chart = ChartOfAccounts::new(vec![]).unwrap();
        let schema = TagSchema::empty();
        let counterparties = crate::counterparty::CounterpartyIndex::empty();
        let ctx = sample_context(&chart, &schema, &counterparties);

        let lines = vec![JournalLine::new(
            kaikei_core::AccountCode::parse("100").unwrap(),
            kaikei_core::Side::Debit,
            Money::from_minor(1_000, Currency::JPY),
            TagSet::new(),
            None,
        )
        .unwrap()];

        let derivation = policy.derive_tax_lines(&ctx, &lines).unwrap();
        assert_eq!(derivation.lines.len(), 1);
        assert!(derivation.notes.is_empty());
    }
}
