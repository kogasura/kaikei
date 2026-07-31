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

    // 2. 純関数: タグ検証（税額行の導出より前、元の明細に対して行う）。
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
    use crate::context::FiscalYearRule;
    use crate::testing::{InMemoryStore, SequentialIdGenerator};
    use crate::tx::with_tx;
    use kaikei_core::{
        AccountCode, AccountDef, AccountType, ChartOfAccounts, Currency, FixedClock, Money, Side,
        TagSet, Timestamp,
    };
    use kaikei_policy::testing::{FlatRateTaxPolicy, NoTaxPolicy};

    fn sample_chart() -> ChartOfAccounts {
        ChartOfAccounts::new(vec![
            AccountDef {
                code: AccountCode::parse("100").unwrap(),
                name: "現金".to_string(),
                account_type: AccountType::Asset,
                parent: None,
                postable: true,
            },
            AccountDef {
                code: AccountCode::parse("500").unwrap(),
                name: "売上高".to_string(),
                account_type: AccountType::Revenue,
                parent: None,
                postable: true,
            },
            AccountDef {
                code: AccountCode::parse("330").unwrap(),
                name: "仮受消費税".to_string(),
                account_type: AccountType::Liability,
                parent: None,
                postable: true,
            },
        ])
        .unwrap()
    }

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

    fn settings() -> BookSettings {
        BookSettings {
            fiscal_year_rule: FiscalYearRule::CalendarYear,
        }
    }

    fn fixed_clock() -> FixedClock {
        FixedClock(Timestamp::from_unix_nanos(0))
    }

    // C-1: 正常系。貸借一致した明細が記帳され、insert_entry まで到達する。
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

    // C-2: 貸借不一致は JournalEntry::new（core）で弾かれ、AppError::Core に写像される。
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

    // C-3: 締められた期間への記帳は PeriodClosed になる。
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

    // C-4: auto_tax_lines により税額行が自動生成され、貸借が保たれる。
    #[tokio::test]
    async fn post_entry_auto_generates_tax_line_and_keeps_balance() {
        let store = InMemoryStore::with_chart(sample_chart());
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

    // C-5: auto_tax_lines = false のときは税額行を生成しない。
    #[tokio::test]
    async fn post_entry_does_not_generate_tax_line_when_disabled() {
        let store = InMemoryStore::with_chart(sample_chart());
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

    // C-6: 未知の勘定科目コードを指定すると UnknownAccount になる
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

    // C-7: next_entry_no は検証失敗時に消費されない
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
}
