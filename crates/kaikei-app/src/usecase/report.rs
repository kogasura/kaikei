//! 試算表ユースケース（[`execute`]）。
//!
//! `Tx` を通さず [`crate::ports::TrialBalanceQuery`] に直行する（read model は
//! 物理的に分離する。`CLAUDE.md` §6「Repository を通さず SQL から DTO へ直行する」）。
//! ここでの責務は3つだけ:
//!
//! 1. `from`/`to` の妥当性検証（`from > to` は入力ミスとして拒否する）
//! 2. `group_by` の `is_aggregatable` ホワイトリスト検証（SQL に到達する前に弾く）。
//!    検証を通した後、重複したキーを出現順を保ったまま除去する
//! 3. 検算（借方合計 ≠ 貸方合計なら [`AppError::Inconsistent`]）
//!
//! テストID（`RPT-1` 等）はこのファイル内でのみ一意な連番であり、
//! `docs/02-test-cases.md` のID体系とは独立している。

use crate::context::BookSettings;
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
    /// 集計軸。空ならグルーピングなし（科目のみで集計する）。重複したキーは
    /// [`execute`] が出現順を保ったまま除去する。
    pub group_by: Vec<TagKey>,
}

/// 試算表を取得する。
///
/// `query` は `Tx` を経由しない read model 専用のクエリ（[`TrialBalanceQuery`]）。
///
/// `settings` から使うのは [`BookSettings::book_currency`] だけだが、
/// 引数の型は書き込み系ユースケース（`post_entry` / `reverse_entry`）と
/// 揃えて `&BookSettings` にしている。呼び出し側が保持しているのは
/// この構造体であり、フィールドを1つ取り出して渡す形にすると
/// 「どの設定を使うユースケースか」が呼び出し側の書き方に依存してしまう。
///
/// # Errors
///
/// - `input.from > input.to`（集計期間の開始日が終了日より後）の場合は
///   [`AppError::Rejected`]
/// - `input.group_by` に `tag_schema.is_aggregatable` が `false` を返す
///   キー（未登録のキーを含む）がある場合は
///   [`AppError::Core`]（[`CoreError::NotAggregatable`]）
/// - `query.trial_balance` が失敗した場合は [`AppError::Repo`]
/// - 借方合計と貸方合計が一致しない場合は [`AppError::Inconsistent`]
/// - 行の通貨が帳簿通貨と食い違う場合・合算のオーバーフロー等、
///   [`TrialBalanceView::totals`] が失敗した場合は [`AppError::Core`]
pub async fn execute(
    query: &dyn TrialBalanceQuery,
    tag_schema: &TagSchema,
    settings: &BookSettings,
    input: ReportInput,
) -> Result<TrialBalanceView, AppError> {
    // 1. from/to の妥当性検証。入力ミスを「0件の空の試算表」として
    //    静かに成功させない（特に MCP 経由で AI が見たとき危険なため）。
    if input.from > input.to {
        return Err(AppError::Rejected {
            reason: format!(
                "集計期間の開始日が終了日より後です: from={} to={}。\
                 from と to を入れ替えるか、正しい期間を指定してください",
                input.from.to_iso_string(),
                input.to.to_iso_string()
            ),
        });
    }

    // 2. group_by のホワイトリスト検証。SQL に到達する前に弾く。
    for key in &input.group_by {
        if !tag_schema.is_aggregatable(key) {
            return Err(AppError::Core(CoreError::NotAggregatable {
                key: key.as_str().to_string(),
            }));
        }
    }

    // 検証を通った後、重複したキーを出現順を保ったまま除去する。
    // `BTreeSet` 等で並べ替えると group キーの順序が変わり、read model
    // （PR-6）側の出力順の期待とズレる可能性があるため、線形探索で
    // 出現順を保つ（`group_by` は数個程度の想定なので O(n^2) でも問題ない）。
    let mut group_by: Vec<TagKey> = Vec::with_capacity(input.group_by.len());
    for key in input.group_by {
        if !group_by.contains(&key) {
            group_by.push(key);
        }
    }

    let rows = query.trial_balance(input.from, input.to, &group_by).await?;
    // 通貨は行から推論せず帳簿通貨を明示する。0行の期間でも応答で通貨を
    // 名乗れるようにするため（`view.rs` の doc）。
    let view = TrialBalanceView::new(rows, settings.book_currency);

    // 3. 検算。借方合計と貸方合計が食い違えばデータ破損・実装バグの兆候。
    //    行が0件なら両方ゼロなので自明に一致する。
    let (debit, credit) = view.totals()?;
    if debit != credit {
        return Err(AppError::Inconsistent {
            debit: debit.to_display_string(),
            credit: credit.to_display_string(),
        });
    }

    Ok(view)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::RepoError;
    use crate::test_support::settings;
    use crate::view::BalanceRowView;
    use async_trait::async_trait;
    use kaikei_core::{AccountCode, AccountType, Currency, Money, TagDef, TagValueType};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// テスト用の固定応答を返す [`TrialBalanceQuery`] フェイク。
    ///
    /// `received_group_by` に、直近の呼び出しで実際に渡された `group_by`
    /// を記録する（app 層での重複除去を検証するため）。
    struct FakeTrialBalanceQuery {
        rows: Vec<BalanceRowView>,
        received_group_by: Mutex<Option<Vec<TagKey>>>,
        call_count: AtomicUsize,
    }

    impl FakeTrialBalanceQuery {
        fn with_rows(rows: Vec<BalanceRowView>) -> Self {
            FakeTrialBalanceQuery {
                rows,
                received_group_by: Mutex::new(None),
                call_count: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl TrialBalanceQuery for FakeTrialBalanceQuery {
        async fn trial_balance(
            &self,
            _from: AccountingDate,
            _to: AccountingDate,
            group_by: &[TagKey],
        ) -> Result<Vec<BalanceRowView>, RepoError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            *self
                .received_group_by
                .lock()
                .expect("テスト用フェイクの Mutex は毒されない前提") = Some(group_by.to_vec());
            Ok(self.rows.clone())
        }
    }

    /// 常にエラーを返す [`TrialBalanceQuery`] フェイク（read model 側の
    /// 失敗が `execute` に正しく伝播することを確認するために使う）。
    struct FailingTrialBalanceQuery;

    #[async_trait]
    impl TrialBalanceQuery for FailingTrialBalanceQuery {
        async fn trial_balance(
            &self,
            _from: AccountingDate,
            _to: AccountingDate,
            _group_by: &[TagKey],
        ) -> Result<Vec<BalanceRowView>, RepoError> {
            Err(RepoError::Backend {
                reason: "テスト用の意図的な接続断".to_string(),
            })
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

    // RPT-1: 正常系。貸借一致した行から試算表ビューが返る。
    #[tokio::test]
    async fn report_returns_balanced_trial_balance() {
        let query = FakeTrialBalanceQuery::with_rows(vec![
            balanced_row("100", AccountType::Asset, 1_000, 0),
            balanced_row("500", AccountType::Revenue, 0, 1_000),
        ]);
        let schema = TagSchema::empty();

        let result = execute(&query, &schema, &settings(), sample_input()).await;

        let view = result.unwrap();
        assert_eq!(view.rows().len(), 2);
    }

    // RPT-2: group_by に is_aggregatable: false のキーを指定すると SQL 到達前に弾かれる。
    #[tokio::test]
    async fn report_rejects_non_aggregatable_group_by_key_before_reaching_query() {
        let query = FakeTrialBalanceQuery::with_rows(Vec::new());
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

        let result = execute(&query, &schema, &settings(), input).await;

        assert!(matches!(
            result,
            Err(AppError::Core(CoreError::NotAggregatable { .. }))
        ));
        assert_eq!(
            query.call_count.load(Ordering::SeqCst),
            0,
            "SQL 到達前に弾かれるべき"
        );
    }

    // RPT-3: 未登録のタグキーを group_by に指定した場合も
    // （is_aggregatable が false を返すため）SQL 到達前に弾かれる。
    #[tokio::test]
    async fn report_rejects_unregistered_group_by_key() {
        let query = FakeTrialBalanceQuery::with_rows(Vec::new());
        let schema = TagSchema::empty();

        let mut input = sample_input();
        input.group_by = vec![kaikei_core::TagKey::parse("unregistered").unwrap()];

        let result = execute(&query, &schema, &settings(), input).await;

        assert!(matches!(
            result,
            Err(AppError::Core(CoreError::NotAggregatable { .. }))
        ));
    }

    // RPT-4: 借方合計と貸方合計が食い違う場合は Inconsistent になる
    // （read model が正しく検算していることの確認）。
    #[tokio::test]
    async fn report_detects_debit_credit_mismatch_as_inconsistent() {
        let query = FakeTrialBalanceQuery::with_rows(vec![
            balanced_row("100", AccountType::Asset, 1_000, 0),
            balanced_row("500", AccountType::Revenue, 0, 900),
        ]);
        let schema = TagSchema::empty();

        let result = execute(&query, &schema, &settings(), sample_input()).await;

        assert!(matches!(result, Err(AppError::Inconsistent { .. })));
    }

    // RPT-5: 行が1つも無い場合は自明に貸借一致とみなす。
    #[tokio::test]
    async fn report_with_no_rows_is_trivially_balanced() {
        let query = FakeTrialBalanceQuery::with_rows(Vec::new());
        let schema = TagSchema::empty();

        let result = execute(&query, &schema, &settings(), sample_input()).await;

        assert!(result.unwrap().rows().is_empty());
    }

    // RPT-9（PR-B 2巡目）: 0行の期間でも試算表は**帳簿通貨を名乗り**、
    // 合計をゼロとして返せる（応答で通貨を出せない状態を解消した。`view.rs` の doc）。
    #[tokio::test]
    async fn report_with_no_rows_still_names_the_book_currency() {
        let query = FakeTrialBalanceQuery::with_rows(Vec::new());
        let schema = TagSchema::empty();

        let view = execute(&query, &schema, &settings(), sample_input())
            .await
            .unwrap();

        assert_eq!(view.currency(), Currency::JPY);
        let (debit, credit) = view.totals().unwrap();
        assert_eq!(debit.minor(), 0);
        assert_eq!(credit.minor(), 0);
        assert_eq!(debit.currency(), Currency::JPY);
    }

    // RPT-10（PR-B 2巡目 / `DECISIONS.md` D-042）: read model が帳簿通貨と
    // 異なる通貨の行を返したら、空の成功ではなくエラーになる。
    #[tokio::test]
    async fn report_rejects_rows_in_a_currency_other_than_the_book_currency() {
        let usd_row = BalanceRowView {
            account: AccountCode::parse("100").unwrap(),
            account_type: AccountType::Asset,
            group: Default::default(),
            debit_total: Money::from_minor(1_000, Currency::USD),
            credit_total: Money::zero(Currency::USD),
            balance: Money::from_minor(1_000, Currency::USD),
        };
        let query = FakeTrialBalanceQuery::with_rows(vec![usd_row]);
        let schema = TagSchema::empty();

        let result = execute(&query, &schema, &settings(), sample_input()).await;

        assert!(matches!(
            result,
            Err(AppError::Core(CoreError::CurrencyMismatch { .. }))
        ));
    }

    // RPT-6（修正1）: from > to は「0件の空の試算表」として静かに成功させず、
    // SQL に到達する前に拒否する。
    #[tokio::test]
    async fn report_rejects_from_after_to() {
        let query = FakeTrialBalanceQuery::with_rows(Vec::new());
        let schema = TagSchema::empty();

        let mut input = sample_input();
        input.from = AccountingDate::new(2026, 12, 31).unwrap();
        input.to = AccountingDate::new(2026, 1, 1).unwrap();

        let result = execute(&query, &schema, &settings(), input).await;

        match result {
            Err(AppError::Rejected { reason }) => {
                assert!(reason.contains("2026-12-31"), "reason={reason}");
                assert!(reason.contains("2026-01-01"), "reason={reason}");
            }
            other => panic!("AppError::Rejected を期待したが: {other:?}"),
        }
        assert_eq!(
            query.call_count.load(Ordering::SeqCst),
            0,
            "SQL 到達前に弾かれるべき"
        );
    }

    // RPT-7（修正2）: 重複した group_by キーは、is_aggregatable の検証を
    // 通した後、出現順を保ったまま除去されて query に渡る。
    #[tokio::test]
    async fn report_deduplicates_group_by_keys_preserving_order() {
        let query = FakeTrialBalanceQuery::with_rows(Vec::new());
        let schema = TagSchema::new(vec![
            (
                kaikei_core::TagKey::parse("counterparty").unwrap(),
                TagDef {
                    value_type: TagValueType::Code,
                    aggregatable: true,
                    required_for: Vec::new(),
                },
            ),
            (
                kaikei_core::TagKey::parse("project").unwrap(),
                TagDef {
                    value_type: TagValueType::Code,
                    aggregatable: true,
                    required_for: Vec::new(),
                },
            ),
        ]);

        let mut input = sample_input();
        input.group_by = vec![
            kaikei_core::TagKey::parse("counterparty").unwrap(),
            kaikei_core::TagKey::parse("project").unwrap(),
            kaikei_core::TagKey::parse("counterparty").unwrap(),
        ];

        execute(&query, &schema, &settings(), input).await.unwrap();

        let received = query
            .received_group_by
            .lock()
            .unwrap()
            .clone()
            .expect("trial_balance が呼ばれているはず");
        assert_eq!(
            received.iter().map(TagKey::as_str).collect::<Vec<_>>(),
            vec!["counterparty", "project"],
            "重複を除去しつつ出現順を保つこと"
        );
    }

    // RPT-8（修正5-4）: read model のクエリが失敗した場合、その失敗が
    // そのまま AppError::Repo として伝播する。
    #[tokio::test]
    async fn report_propagates_query_failure() {
        let query = FailingTrialBalanceQuery;
        let schema = TagSchema::empty();

        let result = execute(&query, &schema, &settings(), sample_input()).await;

        assert!(matches!(
            result,
            Err(AppError::Repo(RepoError::Backend { .. }))
        ));
    }
}
