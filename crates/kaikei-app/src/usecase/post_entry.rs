//! 記帳ユースケース（[`execute`]）。
//!
//! # 実行順序（仕様。入れ替えると壊れる）
//!
//! 1. **I/O**: 勘定科目表・締め状態（会計期間ガード）・取引先索引を読み込む
//!    （[`crate::context::load_posting_context`]）
//! 2. **純関数**: 各明細のタグを `tax.validate_tag` で検証する（元の明細に対して行う。
//!    自動生成される税額行はまだ存在しない）
//! 3. **純関数**: `input.auto_tax_lines` が `true` の場合のみ、`tax.derive_tax_lines`
//!    で消費税行を導出する。戻り値は**確定後の明細一覧**なので、追加ではなく
//!    置き換える。冪等性は保証されないため、この呼び出しは1回のみ行う
//! 4. **I/O**: 仕訳番号を採番する。失敗しうる検証（2・3）を全て終えた直後・
//!    INSERT の直前に置く
//! 5. **domain**: [`JournalEntry::new`] で仕訳を構築する（明細数・科目の存在と
//!    記帳可否・通貨・貸借・タグスキーマ・会計年度・締め状態・摘要を検証する）。
//!    これより後は `lines` に一切触れない（触れると貸借検証の迂回になる）
//! 6. **I/O**: 仕訳を追加する
//!
//! テストID（`PE-1` 等）はこのファイル内でのみ一意な連番であり、
//! `docs/02-test-cases.md` のID体系とは独立している。

use crate::context::{load_posting_context, BookSettings, PostingContext};
use crate::error::AppError;
use crate::ports::{AppClock, IdGenerator, TxOps};
use kaikei_core::{AccountingDate, CoreError, JournalEntry, JournalLine, NewEntry, TagSchema};
use kaikei_policy::{TaxContext, TaxPolicy};

/// [`execute`] への入力。
#[derive(Debug, Clone)]
pub struct PostEntryInput {
    /// 取引日。年度別データの選択基準・会計年度の決定に使う（記帳日ではない）。
    pub entry_date: AccountingDate,
    /// 摘要。
    pub description: String,
    /// 仕訳明細（2行以上）。税抜経理で `auto_tax_lines` を使う場合、税額行を
    /// 含まない元の明細を渡す。
    pub lines: Vec<JournalLine>,
    /// `true` の場合、`tax.derive_tax_lines` で消費税行を自動生成する。
    pub auto_tax_lines: bool,
}

/// 仕訳を記帳する。
///
/// トランザクションの開始・確定・破棄は行わない（呼び出し側が
/// [`crate::tx::with_tx`] で管理する）。実行順序は本モジュール doc を参照。
///
/// # Errors
///
/// - `tx` からの読み込み（勘定科目表・締め状態・取引先索引・採番）・書き込み
///   （`insert_entry`）が失敗した場合は [`AppError::Repo`]
/// - `input.lines` のいずれかの科目が `chart` に存在しない場合は
///   [`AppError::Core`]（[`CoreError::UnknownAccount`]）
/// - `tax.validate_tag` / `tax.derive_tax_lines` が失敗した場合は
///   [`AppError::Policy`]
/// - [`JournalEntry::new`] の検証（明細数・科目の記帳可否・通貨・貸借・
///   タグスキーマ・会計年度・締め状態・摘要）に失敗した場合は
///   [`AppError::Core`]
pub async fn execute<Tx>(
    tx: &mut Tx,
    tax: &dyn TaxPolicy,
    tag_schema: &TagSchema,
    id_gen: &dyn IdGenerator,
    clock: &dyn AppClock,
    settings: &BookSettings,
    input: PostEntryInput,
) -> Result<JournalEntry, AppError>
where
    Tx: TxOps,
{
    // 1. I/O
    let PostingContext {
        fiscal_year,
        chart,
        counterparties,
        guard,
    } = load_posting_context(tx, input.entry_date, settings).await?;

    let tax_ctx = TaxContext {
        as_of: input.entry_date,
        chart: &chart,
        tag_schema,
        counterparties: &counterparties,
    };

    // 2. 純関数: タグ検証(税額行の導出より前、元の明細に対して行う)。
    for line in &input.lines {
        let account_def = chart.get(line.account()).ok_or_else(|| {
            AppError::Core(CoreError::UnknownAccount {
                code: line.account().as_str().to_string(),
            })
        })?;
        tax.validate_tag(&tax_ctx, line.tags(), account_def)?;
    }

    // 3. 純関数: 税額行の導出（1回だけ）。derive_tax_lines は「確定後の明細
    //    一覧」を返すため、追加ではなく置き換える。
    let lines = if input.auto_tax_lines {
        tax.derive_tax_lines(&tax_ctx, &input.lines)?.lines
    } else {
        input.lines
    };

    // 4. I/O: 失敗しうる検証を全て終えた直後・INSERT の直前で採番する。
    let entry_no = tx.next_entry_no(fiscal_year.label()).await?;

    // 5. domain: これより後は lines に触れない。
    let entry = JournalEntry::new(
        NewEntry {
            id: id_gen.new_entry_id(),
            entry_no,
            entry_date: input.entry_date,
            description: input.description,
            lines,
            document_refs: Vec::new(),
        },
        &fiscal_year,
        &chart,
        tag_schema,
        &guard,
        clock,
    )?;

    // 6. I/O
    tx.insert_entry(&entry).await?;

    Ok(entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{fixed_clock, sample_chart, sample_chart_with_tax_account, settings};
    use crate::testing::{InMemoryStore, SequentialIdGenerator};
    use crate::tx::with_tx;
    use kaikei_core::{AccountCode, Currency, Money, Side, TagSet};
    use kaikei_policy::testing::{FlatRateTaxPolicy, NoTaxPolicy};

    fn balanced_lines() -> Vec<JournalLine> {
        vec![
            JournalLine::new(
                AccountCode::parse("100").unwrap(),
                Side::Debit,
                Money::from_minor(1_000, Currency::JPY),
                TagSet::new(),
                None,
            )
            .unwrap(),
            JournalLine::new(
                AccountCode::parse("500").unwrap(),
                Side::Credit,
                Money::from_minor(1_000, Currency::JPY),
                TagSet::new(),
                None,
            )
            .unwrap(),
        ]
    }

    // PE-1: 正常系。貸借一致した明細が記帳され、insert_entry まで到達する。
    #[tokio::test]
    async fn post_entry_succeeds_with_balanced_lines() {
        let store = InMemoryStore::with_chart(sample_chart());
        let tax = NoTaxPolicy;
        let schema = TagSchema::empty();
        let id_gen = SequentialIdGenerator::starting_at(1);
        let clock = fixed_clock();
        let settings = settings();

        let input = PostEntryInput {
            entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
            description: "現金売上".to_string(),
            lines: balanced_lines(),
            auto_tax_lines: false,
        };

        let result: Result<JournalEntry, AppError> = with_tx(&store, |tx| {
            Box::pin(
                async move { execute(tx, &tax, &schema, &id_gen, &clock, &settings, input).await },
            )
        })
        .await;

        let entry = result.unwrap();
        assert_eq!(entry.entry_no().as_u32(), 1);
        assert_eq!(store.committed_entries().len(), 1);
    }

    // PE-2: 貸借不一致は JournalEntry::new（core）で弾かれ、AppError::Core に写像される。
    #[tokio::test]
    async fn post_entry_rejects_unbalanced_lines() {
        let store = InMemoryStore::with_chart(sample_chart());
        let tax = NoTaxPolicy;
        let schema = TagSchema::empty();
        let id_gen = SequentialIdGenerator::starting_at(1);
        let clock = fixed_clock();
        let settings = settings();

        let unbalanced = vec![
            JournalLine::new(
                AccountCode::parse("100").unwrap(),
                Side::Debit,
                Money::from_minor(1_100, Currency::JPY),
                TagSet::new(),
                None,
            )
            .unwrap(),
            JournalLine::new(
                AccountCode::parse("500").unwrap(),
                Side::Credit,
                Money::from_minor(1_000, Currency::JPY),
                TagSet::new(),
                None,
            )
            .unwrap(),
        ];
        let input = PostEntryInput {
            entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
            description: "不一致".to_string(),
            lines: unbalanced,
            auto_tax_lines: false,
        };

        let result: Result<JournalEntry, AppError> = with_tx(&store, |tx| {
            Box::pin(
                async move { execute(tx, &tax, &schema, &id_gen, &clock, &settings, input).await },
            )
        })
        .await;

        assert!(matches!(
            result,
            Err(AppError::Core(CoreError::Unbalanced { .. }))
        ));
        assert!(store.committed_entries().is_empty());
    }

    // PE-3: 締められた期間への記帳は PeriodClosed になる。
    #[tokio::test]
    async fn post_entry_rejects_entry_in_closed_period() {
        let store = InMemoryStore::with_chart(sample_chart());
        let closed_through = AccountingDate::new(2026, 3, 31).unwrap();
        store.set_closed_through(2026, closed_through);
        let tax = NoTaxPolicy;
        let schema = TagSchema::empty();
        let id_gen = SequentialIdGenerator::starting_at(1);
        let clock = fixed_clock();
        let settings = settings();

        let input = PostEntryInput {
            entry_date: AccountingDate::new(2026, 1, 15).unwrap(),
            description: "締め後の記帳".to_string(),
            lines: balanced_lines(),
            auto_tax_lines: false,
        };

        let result: Result<JournalEntry, AppError> = with_tx(&store, |tx| {
            Box::pin(
                async move { execute(tx, &tax, &schema, &id_gen, &clock, &settings, input).await },
            )
        })
        .await;

        assert!(matches!(
            result,
            Err(AppError::Core(CoreError::PeriodClosed { .. }))
        ));
    }

    // PE-4: auto_tax_lines により税額行が自動生成され、貸借が保たれる。
    #[tokio::test]
    async fn post_entry_auto_generates_tax_line_and_keeps_balance() {
        let store = InMemoryStore::with_chart(sample_chart_with_tax_account());
        let tax = FlatRateTaxPolicy {
            rate: kaikei_core::Ratio::parse_rate("0.10").unwrap(),
            tax_account: "330",
        };
        let schema = TagSchema::empty();
        let id_gen = SequentialIdGenerator::starting_at(1);
        let clock = fixed_clock();
        let settings = settings();

        // 借方 現金 100,000 / 貸方 売上高 100,000（貸借一致した元の明細）を入力する。
        // FlatRateTaxPolicy は側（借方・貸方）ごとの合計に一律の税率を掛けるため、
        // 両側に 10,000 円の税額行が追加され（計4行）、貸借は 110,000 で一致し続ける。
        let lines = vec![
            JournalLine::new(
                AccountCode::parse("100").unwrap(),
                Side::Debit,
                Money::from_minor(100_000, Currency::JPY),
                TagSet::new(),
                None,
            )
            .unwrap(),
            JournalLine::new(
                AccountCode::parse("500").unwrap(),
                Side::Credit,
                Money::from_minor(100_000, Currency::JPY),
                TagSet::new(),
                None,
            )
            .unwrap(),
        ];
        let input = PostEntryInput {
            entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
            description: "税抜経理".to_string(),
            lines,
            auto_tax_lines: true,
        };

        let result: Result<JournalEntry, AppError> = with_tx(&store, |tx| {
            Box::pin(
                async move { execute(tx, &tax, &schema, &id_gen, &clock, &settings, input).await },
            )
        })
        .await;

        let entry = result.unwrap();
        assert_eq!(entry.lines().len(), 4);
        assert_eq!(entry.debit_total().minor(), entry.credit_total().minor());
        assert_eq!(entry.credit_total().minor(), 110_000);
    }

    // PE-5: auto_tax_lines = false のときは税額行を生成しない。
    #[tokio::test]
    async fn post_entry_does_not_generate_tax_line_when_disabled() {
        let store = InMemoryStore::with_chart(sample_chart_with_tax_account());
        let tax = FlatRateTaxPolicy {
            rate: kaikei_core::Ratio::parse_rate("0.10").unwrap(),
            tax_account: "330",
        };
        let schema = TagSchema::empty();
        let id_gen = SequentialIdGenerator::starting_at(1);
        let clock = fixed_clock();
        let settings = settings();

        let input = PostEntryInput {
            entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
            description: "税額行なし".to_string(),
            lines: balanced_lines(),
            auto_tax_lines: false,
        };

        let result: Result<JournalEntry, AppError> = with_tx(&store, |tx| {
            Box::pin(
                async move { execute(tx, &tax, &schema, &id_gen, &clock, &settings, input).await },
            )
        })
        .await;

        assert_eq!(result.unwrap().lines().len(), 2);
    }

    // PE-6: 未知の勘定科目コードを指定すると UnknownAccount になる
    // （validate_tag に渡す account_def が引けない時点で早期に検出する）。
    #[tokio::test]
    async fn post_entry_rejects_unknown_account() {
        let store = InMemoryStore::with_chart(sample_chart());
        let tax = NoTaxPolicy;
        let schema = TagSchema::empty();
        let id_gen = SequentialIdGenerator::starting_at(1);
        let clock = fixed_clock();
        let settings = settings();

        let lines = vec![
            JournalLine::new(
                AccountCode::parse("999").unwrap(),
                Side::Debit,
                Money::from_minor(1_000, Currency::JPY),
                TagSet::new(),
                None,
            )
            .unwrap(),
            JournalLine::new(
                AccountCode::parse("500").unwrap(),
                Side::Credit,
                Money::from_minor(1_000, Currency::JPY),
                TagSet::new(),
                None,
            )
            .unwrap(),
        ];
        let input = PostEntryInput {
            entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
            description: "未知科目".to_string(),
            lines,
            auto_tax_lines: false,
        };

        let result: Result<JournalEntry, AppError> = with_tx(&store, |tx| {
            Box::pin(
                async move { execute(tx, &tax, &schema, &id_gen, &clock, &settings, input).await },
            )
        })
        .await;

        assert!(matches!(
            result,
            Err(AppError::Core(CoreError::UnknownAccount { .. }))
        ));
    }

    /// `store` に対して1回分の `execute` を実行する。`with_tx` のクロージャは
    /// 依存を所有値として `move` するため、同じ変数を複数回の `with_tx`
    /// 呼び出しにまたがって使い回せない（`crate::tx::with_tx` の doc を参照）。
    /// 依存をこの関数内で毎回組み立て直すことで、テストが同じ問題を踏まないようにする。
    async fn run_post_entry(
        store: &InMemoryStore,
        id_gen_start: u128,
        input: PostEntryInput,
    ) -> Result<JournalEntry, AppError> {
        let tax = NoTaxPolicy;
        let schema = TagSchema::empty();
        let id_gen = SequentialIdGenerator::starting_at(id_gen_start);
        let clock = fixed_clock();
        let settings = settings();

        with_tx(store, |tx| {
            Box::pin(
                async move { execute(tx, &tax, &schema, &id_gen, &clock, &settings, input).await },
            )
        })
        .await
    }

    // PE-7: next_entry_no は検証失敗時に消費されない
    // （失敗しうる検証（貸借不一致）を終えてから採番する設計の検証）。
    #[tokio::test]
    async fn post_entry_does_not_consume_entry_number_when_validation_fails_first() {
        let store = InMemoryStore::with_chart(sample_chart());

        let unbalanced = vec![
            JournalLine::new(
                AccountCode::parse("100").unwrap(),
                Side::Debit,
                Money::from_minor(1_100, Currency::JPY),
                TagSet::new(),
                None,
            )
            .unwrap(),
            JournalLine::new(
                AccountCode::parse("500").unwrap(),
                Side::Credit,
                Money::from_minor(1_000, Currency::JPY),
                TagSet::new(),
                None,
            )
            .unwrap(),
        ];
        let failing_input = PostEntryInput {
            entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
            description: "不一致".to_string(),
            lines: unbalanced,
            auto_tax_lines: false,
        };

        let failing_result = run_post_entry(&store, 1, failing_input).await;
        assert!(failing_result.is_err());

        // 失敗しても採番は進んでいないため、次に成功する記帳は entry_no = 1 になる。
        let succeeding_input = PostEntryInput {
            entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
            description: "正常".to_string(),
            lines: balanced_lines(),
            auto_tax_lines: false,
        };
        let succeeding_result = run_post_entry(&store, 2, succeeding_input).await;

        assert_eq!(succeeding_result.unwrap().entry_no().as_u32(), 1);
    }

    // PE-8（修正5-1）: 明細が0行だと TooFewLines で弾かれる。
    #[tokio::test]
    async fn post_entry_rejects_empty_lines() {
        let store = InMemoryStore::with_chart(sample_chart());

        let input = PostEntryInput {
            entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
            description: "明細なし".to_string(),
            lines: Vec::new(),
            auto_tax_lines: false,
        };

        let result = run_post_entry(&store, 1, input).await;

        assert!(matches!(
            result,
            Err(AppError::Core(CoreError::TooFewLines { found: 0 }))
        ));
    }

    // PE-9（修正5-1）: 明細が1行のみだと TooFewLines で弾かれる。
    #[tokio::test]
    async fn post_entry_rejects_single_line() {
        let store = InMemoryStore::with_chart(sample_chart());

        let input = PostEntryInput {
            entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
            description: "明細1行のみ".to_string(),
            lines: vec![JournalLine::new(
                AccountCode::parse("100").unwrap(),
                Side::Debit,
                Money::from_minor(1_000, Currency::JPY),
                TagSet::new(),
                None,
            )
            .unwrap()],
            auto_tax_lines: false,
        };

        let result = run_post_entry(&store, 1, input).await;

        assert!(matches!(
            result,
            Err(AppError::Core(CoreError::TooFewLines { found: 1 }))
        ));
    }

    // PE-10（修正5-2）: derive_tax_lines がエラーを返した場合、
    // next_entry_no（採番）は消費されない。`FlatRateTaxPolicy` に
    // `AccountCode::parse` が拒否する不正な科目コードを与えて意図的に
    // `derive_tax_lines` を失敗させる（`?` が採番より前にあることの担保）。
    #[tokio::test]
    async fn post_entry_does_not_consume_entry_number_when_derive_tax_lines_fails() {
        let store = InMemoryStore::with_chart(sample_chart_with_tax_account());
        let tax = FlatRateTaxPolicy {
            rate: kaikei_core::Ratio::parse_rate("0.10").unwrap(),
            // 英数字とハイフン以外を含むため AccountCode::parse が拒否する。
            tax_account: "不正科目",
        };
        let schema = TagSchema::empty();
        let id_gen = SequentialIdGenerator::starting_at(1);
        let clock = fixed_clock();
        let settings = settings();

        let input = PostEntryInput {
            entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
            description: "税額行導出が失敗するケース".to_string(),
            lines: balanced_lines(),
            auto_tax_lines: true,
        };

        let result: Result<JournalEntry, AppError> = with_tx(&store, |tx| {
            Box::pin(
                async move { execute(tx, &tax, &schema, &id_gen, &clock, &settings, input).await },
            )
        })
        .await;

        assert!(matches!(result, Err(AppError::Policy(_))));

        // 採番が進んでいないため、次に成功する記帳は entry_no = 1 になる。
        let succeeding_input = PostEntryInput {
            entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
            description: "正常".to_string(),
            lines: balanced_lines(),
            auto_tax_lines: false,
        };
        let succeeding_result = run_post_entry(&store, 2, succeeding_input).await;
        assert_eq!(succeeding_result.unwrap().entry_no().as_u32(), 1);
    }

    // ---- プロパティテスト（修正6-a） ----
    //
    // 「任意の貸借一致明細 + NoTaxPolicy なら post_entry は常に成功し、
    // 結果の debit_total == credit_total」という不変条件を、行数・金額の
    // 組み合わせを広く散らして検証する。Phase 0 の教訓（生成器のレンジが
    // 仕様の許容範囲より狭いと実バグを見逃す）を踏まえ、`kaikei-policy` の
    // `testing.rs` にある同種の proptest（貸借一致の不変条件）と同じ
    // `positive_partition` の考え方を使う。
    mod balance_invariant {
        use super::*;
        use proptest::prelude::*;
        use proptest::strategy::BoxedStrategy;

        /// `total` 円を、最小1円ずつを持つ `k` 個の正の整数に分割する。
        fn positive_partition(total: i128, k: usize) -> BoxedStrategy<Vec<i128>> {
            if k <= 1 {
                return Just(vec![total]).boxed();
            }
            (1i128..=(total - (k as i128 - 1)))
                .prop_flat_map(move |first| {
                    positive_partition(total - first, k - 1).prop_map(move |mut rest| {
                        let mut amounts = vec![first];
                        amounts.append(&mut rest);
                        amounts
                    })
                })
                .boxed()
        }

        /// 借方・貸方それぞれが同じ `total` に分割された、行数も金額も
        /// 様々な明細の組を生成する。`total` は「実務的にありそうな金額」では
        /// なく、1円程度の極小値から数百万円台までを広く踏む。
        fn balanced_split_strategy() -> impl Strategy<Value = (Vec<i128>, Vec<i128>)> {
            let total_strategy = prop_oneof![
                3 => 1i128..=3i128,
                7 => 4i128..=5_000_000i128,
            ];
            total_strategy
                .prop_flat_map(|total| {
                    let max_k = total.min(6) as u8;
                    (Just(total), 1u8..=max_k, 1u8..=max_k)
                })
                .prop_flat_map(|(total, k_debit, k_credit)| {
                    (
                        positive_partition(total, k_debit as usize),
                        positive_partition(total, k_credit as usize),
                    )
                })
        }

        proptest! {
            #[test]
            fn post_entry_succeeds_and_keeps_balance_for_arbitrary_balanced_splits(
                (debit_amounts, credit_amounts) in balanced_split_strategy(),
            ) {
                // 生成器自体が入力の貸借一致を保証していることの自己検証。
                let input_debit: i128 = debit_amounts.iter().sum();
                let input_credit: i128 = credit_amounts.iter().sum();
                prop_assert_eq!(input_debit, input_credit);

                let mut lines = Vec::new();
                for amount in &debit_amounts {
                    lines.push(
                        JournalLine::new(
                            AccountCode::parse("100").unwrap(),
                            Side::Debit,
                            Money::from_minor(*amount, Currency::JPY),
                            TagSet::new(),
                            None,
                        )
                        .unwrap(),
                    );
                }
                for amount in &credit_amounts {
                    lines.push(
                        JournalLine::new(
                            AccountCode::parse("500").unwrap(),
                            Side::Credit,
                            Money::from_minor(*amount, Currency::JPY),
                            TagSet::new(),
                            None,
                        )
                        .unwrap(),
                    );
                }

                let input = PostEntryInput {
                    entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
                    description: "proptest".to_string(),
                    lines,
                    auto_tax_lines: false,
                };

                let store = InMemoryStore::with_chart(sample_chart());
                let tax = NoTaxPolicy;
                let schema = TagSchema::empty();
                let id_gen = SequentialIdGenerator::starting_at(1);
                let clock = fixed_clock();
                let settings = settings();

                // proptest の #[test] は同期関数のため、専用ランタイムで
                // async な execute を実行する（#[tokio::test] は使えない）。
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let result: Result<JournalEntry, AppError> = runtime.block_on(with_tx(&store, |tx| {
                    Box::pin(async move {
                        execute(tx, &tax, &schema, &id_gen, &clock, &settings, input).await
                    })
                }));

                let entry = result.unwrap();
                prop_assert_eq!(entry.debit_total().minor(), entry.credit_total().minor());
            }
        }
    }
}
