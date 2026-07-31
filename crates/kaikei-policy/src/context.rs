//! 税務判定・按分計算等に必要な文脈をまとめた読み取り専用ビュー（`TaxContext`）。
//!
//! **I/O を持つものを絶対に入れない。** これが `CLAUDE.md` §3「policy trait は
//! 純関数を保つ」の構造的な担保になる。
//!
//! 年度別税区分マスタ（`TaxCategoryTable`）や事業者設定（`JpSettings`）は
//! `kaikei-jp` の型であり、ここには含めない。含めると policy → jp の
//! 循環依存になり、`kaikei-app` も jp の型を知る必要が生じて依存方向
//! （`CLAUDE.md` §1）が崩れる。年度別マスタと事業者設定は、国別の実装
//! （`JpTaxPolicy` 等）が**構築時**に保持し、年度の選択は
//! [`TaxContext::as_of`]（取引日）で行う（`DECISIONS.md` D-025）。

use crate::counterparty::CounterpartyIndex;
use kaikei_core::{AccountingDate, ChartOfAccounts, TagSchema};

/// 税務判定に必要な文脈。`kaikei-app` が構築し、`TaxPolicy` 等の各メソッドに
/// 引数として渡す。
#[derive(Debug, Clone, Copy)]
pub struct TaxContext<'a> {
    /// 取引日。年度別データの選択基準（`CLAUDE.md` §7: 記帳日ではなく取引日）。
    pub as_of: AccountingDate,
    /// 勘定科目表。
    pub chart: &'a ChartOfAccounts,
    /// タグスキーマ。
    pub tag_schema: &'a TagSchema,
    /// 取引先の索引（DB から読み込んだスナップショット）。
    pub counterparties: &'a CounterpartyIndex,
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_core::{AccountCode, AccountDef, AccountType};

    #[test]
    fn tax_context_holds_references_without_owning_data() {
        let chart = ChartOfAccounts::new(vec![AccountDef {
            code: AccountCode::parse("100").unwrap(),
            name: "現金".to_string(),
            account_type: AccountType::Asset,
            parent: None,
            postable: true,
        }])
        .unwrap();
        let schema = TagSchema::empty();
        let counterparties = CounterpartyIndex::empty();
        let as_of = AccountingDate::new(2026, 4, 1).unwrap();

        let ctx = TaxContext {
            as_of,
            chart: &chart,
            tag_schema: &schema,
            counterparties: &counterparties,
        };

        // Copy であること（呼び出しのたびに再構築せず使い回せることの確認）。
        let ctx_copy = ctx;
        assert_eq!(ctx_copy.as_of, as_of);
        assert!(ctx.chart.get(&AccountCode::parse("100").unwrap()).is_some());
        assert!(ctx.counterparties.is_empty());
    }
}
