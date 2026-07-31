//! 試算表ユースケース（[`execute`]）。
//!
//! `Tx` を通さず [`crate::ports::TrialBalanceQuery`] に直行する（read model は
//! 物理的に分離する。`CLAUDE.md` §6「Repository を通さず SQL から DTO へ直行する」）。
//! ここでの責務は2つだけ:
//!
//! 1. `group_by` の `is_aggregatable` ホワイトリスト検証（SQL に到達する前に弾く）
//! 2. 検算（借方合計 ≠ 貸方合計なら [`AppError::Inconsistent`]）

use crate::error::AppError;
use crate::ports::TrialBalanceQuery;
use crate::view::TrialBalanceView;
use kaikei_core::{AccountingDate, CoreError, TagKey, TagSchema};

/// [`execute`] への入力。
#[derive(Debug, Clone)]
pub struct ReportInput {
    /// 集計対象期間の開始日（取引日、両端を含む）。
    pub from: AccountingDate,
    /// 集計対象期間の終了日（取引日、両端を含む）。
    pub to: AccountingDate,
    /// 集計軸。空ならグルーピングなし（科目のみで集計する）。
    pub group_by: Vec<TagKey>,
}

/// 試算表を取得する。
///
/// `query` は `Tx` を経由しない read model 専用のクエリ（[`TrialBalanceQuery`]）。
pub async fn execute(
    query: &dyn TrialBalanceQuery,
    tag_schema: &TagSchema,
    input: ReportInput,
) -> Result<TrialBalanceView, AppError> {
    // 1. group_by のホワイトリスト検証。SQL に到達する前に弾く。
    for key in &input.group_by {
        if !tag_schema.is_aggregatable(key) {
            return Err(AppError::Core(CoreError::NotAggregatable {
                key: key.as_str().to_string(),
            }));
        }
    }

    let rows = query
        .trial_balance(input.from, input.to, &input.group_by)
        .await?;
    let view = TrialBalanceView::new(rows);

    // 2. 検算。借方合計と貸方合計が食い違えばデータ破損・実装バグの兆候。
    if let Some((debit, credit)) = view.totals()? {
        if debit != credit {
            return Err(AppError::Inconsistent {
                debit: debit.to_display_string(),
                credit: credit.to_display_string(),
            });
        }
    }

    Ok(view)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::RepoError;
    use crate::view::BalanceRowView;
    use async_trait::async_trait;
    use kaikei_core::{AccountCode, AccountType, Currency, Money, TagDef, TagValueType};

    /// テスト用の固定応答を返す [`TrialBalanceQuery`] フェイク。
    struct FakeTrialBalanceQuery {
        rows: Vec<BalanceRowView>,
    }

    #[async_trait]
    impl TrialBalanceQuery for FakeTrialBalanceQuery {
        async fn trial_balance(
            &self,
            _from: AccountingDate,
            _to: AccountingDate,
            _group_by: &[TagKey],
        ) -> Result<Vec<BalanceRowView>, RepoError> {
            Ok(self.rows.clone())
        }
    }

    fn balanced_row(
        account: &str,
        account_type: AccountType,
        debit: i128,
        credit: i128,
    ) -> BalanceRowView {
        let debit_total = Money::from_minor(debit, Currency::JPY);
        let credit_total = Money::from_minor(credit, Currency::JPY);
        let balance = if account_type.is_debit_normal() {
            debit_total.sub(&credit_total).unwrap()
        } else {
            credit_total.sub(&debit_total).unwrap()
        };
        BalanceRowView {
            account: AccountCode::parse(account).unwrap(),
            account_type,
            group: Default::default(),
            debit_total,
            credit_total,
            balance,
        }
    }

    fn sample_input() -> ReportInput {
        ReportInput {
            from: AccountingDate::new(2026, 1, 1).unwrap(),
            to: AccountingDate::new(2026, 12, 31).unwrap(),
            group_by: Vec::new(),
        }
    }

    // T-1: 正常系。貸借一致した行から試算表ビューが返る。
    #[tokio::test]
    async fn report_returns_balanced_trial_balance() {
        let query = FakeTrialBalanceQuery {
            rows: vec![
                balanced_row("100", AccountType::Asset, 1_000, 0),
                balanced_row("500", AccountType::Revenue, 0, 1_000),
            ],
        };
        let schema = TagSchema::empty();

        let result = execute(&query, &schema, sample_input()).await;

        let view = result.unwrap();
        assert_eq!(view.rows().len(), 2);
    }

    // T-2: group_by に is_aggregatable: false のキーを指定すると SQL 到達前に弾かれる。
    #[tokio::test]
    async fn report_rejects_non_aggregatable_group_by_key_before_reaching_query() {
        let query = FakeTrialBalanceQuery { rows: Vec::new() };
        let schema = TagSchema::new(vec![(
            kaikei_core::TagKey::parse("business_ratio").unwrap(),
            TagDef {
                value_type: TagValueType::Decimal,
                aggregatable: false,
                required_for: Vec::new(),
            },
        )]);

        let mut input = sample_input();
        input.group_by = vec![kaikei_core::TagKey::parse("business_ratio").unwrap()];

        let result = execute(&query, &schema, input).await;

        assert!(matches!(
            result,
            Err(AppError::Core(CoreError::NotAggregatable { .. }))
        ));
    }

    // T-3: 未登録のタグキーを group_by に指定した場合も
    // （is_aggregatable が false を返すため）SQL 到達前に弾かれる。
    #[tokio::test]
    async fn report_rejects_unregistered_group_by_key() {
        let query = FakeTrialBalanceQuery { rows: Vec::new() };
        let schema = TagSchema::empty();

        let mut input = sample_input();
        input.group_by = vec![kaikei_core::TagKey::parse("unregistered").unwrap()];

        let result = execute(&query, &schema, input).await;

        assert!(matches!(
            result,
            Err(AppError::Core(CoreError::NotAggregatable { .. }))
        ));
    }

    // T-4: 借方合計と貸方合計が食い違う場合は Inconsistent になる
    // （read model が正しく検算していることの確認）。
    #[tokio::test]
    async fn report_detects_debit_credit_mismatch_as_inconsistent() {
        let query = FakeTrialBalanceQuery {
            rows: vec![
                balanced_row("100", AccountType::Asset, 1_000, 0),
                balanced_row("500", AccountType::Revenue, 0, 900),
            ],
        };
        let schema = TagSchema::empty();

        let result = execute(&query, &schema, sample_input()).await;

        assert!(matches!(result, Err(AppError::Inconsistent { .. })));
    }

    // T-5: 行が1つも無い場合は自明に貸借一致とみなす。
    #[tokio::test]
    async fn report_with_no_rows_is_trivially_balanced() {
        let query = FakeTrialBalanceQuery { rows: Vec::new() };
        let schema = TagSchema::empty();

        let result = execute(&query, &schema, sample_input()).await;

        assert!(result.unwrap().rows().is_empty());
    }
}
