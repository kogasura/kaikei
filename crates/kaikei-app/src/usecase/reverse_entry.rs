//! 訂正ユースケース（[`execute`]）。逆仕訳（赤伝）による訂正のみ。
//!
//! `TaxPolicy` を引数に取らない。明細は [`kaikei_core::JournalEntry::reverse`]
//! が貸借を反転して複製するだけであり、税額行を再度導出すると二重計上になる。
//!
//! 二重訂正（既に赤伝済みの仕訳を再度訂正すること）は既定で拒否する
//! （[`AppError::AlreadyReversed`]）。`allow_double_reversal: true` を明示した
//! 場合のみ許可する。
//!
//! # 実行順序
//!
//! 1. **I/O**: 訂正対象の仕訳を読み込む（無ければ `RepoError::NotFound`）
//! 2. **I/O**: 二重訂正の検出。`allow_double_reversal` が `false`（既定）なら
//!    既存の逆仕訳の有無を確認し、あれば拒否する
//! 3. **I/O**: 勘定科目表・締め状態を読み込む。会計年度は `reverse_date`
//!    （逆仕訳の取引日）で決まる。**元仕訳が別年度でも逆仕訳は指定日付の
//!    年度に属する**
//! 4. **I/O**: 仕訳番号を採番する。失敗しうる検証を全て終えた直後・INSERT
//!    の直前に置く
//! 5. **domain**: [`kaikei_core::JournalEntry::reverse`] で逆仕訳を構築する
//! 6. **I/O**: 仕訳を追加する

use crate::context::{load_posting_context, BookSettings, PostingContext};
use crate::error::{AppError, RepoError};
use crate::ports::{AppClock, IdGenerator, TxOps};
use kaikei_core::{AccountingDate, EntryId, JournalEntry, TagSchema};

/// [`execute`] への入力。
#[derive(Debug, Clone)]
pub struct ReverseEntryInput {
    /// 訂正対象の仕訳ID。
    pub original_id: EntryId,
    /// 逆仕訳の取引日。会計年度はこの日付から決定する。
    pub reverse_date: AccountingDate,
    /// 訂正理由。
    pub reason: String,
    /// `true` の場合、既に赤伝済みの仕訳を再度訂正することを明示的に許可する。
    /// 既定（`false`）では二重訂正は拒否される。
    pub allow_double_reversal: bool,
}

/// 仕訳を訂正する（逆仕訳を作る）。
///
/// トランザクションの開始・確定・破棄は行わない（呼び出し側が
/// [`crate::tx::with_tx`] で管理する）。実行順序は本モジュール doc を参照。
pub async fn execute<Tx>(
    tx: &mut Tx,
    tag_schema: &TagSchema,
    id_gen: &dyn IdGenerator,
    clock: &dyn AppClock,
    settings: &BookSettings,
    input: ReverseEntryInput,
) -> Result<JournalEntry, AppError>
where
    Tx: TxOps,
{
    // 1. I/O: 訂正対象の仕訳を読み込む。
    let original = tx.find_entry(input.original_id).await?.ok_or_else(|| {
        AppError::Repo(RepoError::NotFound {
            reason: format!(
                "仕訳が見つかりません（仕訳ID: {}）",
                input.original_id.as_u128()
            ),
        })
    })?;

    // 2. I/O: 二重訂正の検出。既定では拒否する。
    if !input.allow_double_reversal {
        if let Some((_, reversal_no)) = tx.find_reversal_of(input.original_id).await? {
            return Err(AppError::AlreadyReversed {
                entry_no: original.entry_no(),
                reversal_no,
            });
        }
    }

    // 3. I/O: 逆仕訳の会計年度は reverse_date で決まる（元仕訳が別年度でも）。
    let PostingContext {
        fiscal_year,
        chart,
        guard,
        ..
    } = load_posting_context(tx, input.reverse_date, settings).await?;

    // 4. I/O: 失敗しうる検証を全て終えた直後・INSERT の直前で採番する。
    let entry_no = tx.next_entry_no(fiscal_year.label()).await?;

    // 5. domain
    let entry = original.reverse(
        id_gen.new_entry_id(),
        entry_no,
        input.reverse_date,
        input.reason,
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
    use crate::ports::{JournalRepo, NumberingRepo, Store, TxScope};
    use crate::testing::{InMemoryStore, SequentialIdGenerator};
    use crate::tx::with_tx;
    use kaikei_core::{
        AccountCode, AccountDef, AccountType, ChartOfAccounts, Currency, FixedClock, JournalLine,
        Money, NewEntry, PeriodGuard, PeriodStatus, Side, TagSet, Timestamp,
    };

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
        ])
        .unwrap()
    }

    fn settings() -> BookSettings {
        BookSettings {
            fiscal_year_rule: FiscalYearRule::CalendarYear,
        }
    }

    fn fixed_clock() -> FixedClock {
        FixedClock(Timestamp::from_unix_nanos(0))
    }

    struct AllOpen;
    impl PeriodGuard for AllOpen {
        fn status(&self, _date: AccountingDate) -> PeriodStatus {
            PeriodStatus::Open
        }
    }

    /// 貸借が一致した最小限の仕訳を1件、`store` にコミット済みの状態で作る。
    ///
    /// 仕訳番号は `store` の採番（[`NumberingRepo::next_entry_no`]）を通して
    /// 払い出す。ハードコードした番号を直接 `insert_entry` すると、`store` 側の
    /// 採番カウンタが進まないまま以後の `next_entry_no` が同じ番号を返し、
    /// 後続の記帳・訂正が `(fiscal_year, entry_no)` の一意制約違反になる。
    async fn seed_entry(store: &InMemoryStore, id: u128, date: AccountingDate) {
        let chart = sample_chart();
        let fy = kaikei_core::FiscalYear::calendar_year(date.year());
        let clock = fixed_clock();
        let lines = vec![
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
        ];

        let mut tx = store.begin().await.unwrap();
        let entry_no = tx.next_entry_no(fy.label()).await.unwrap();
        let entry = JournalEntry::new(
            NewEntry {
                id: kaikei_core::EntryId::new(id),
                entry_no,
                entry_date: date,
                description: "元仕訳".to_string(),
                lines,
                document_refs: Vec::new(),
            },
            &fy,
            &chart,
            &TagSchema::empty(),
            &AllOpen,
            &clock,
        )
        .unwrap();
        tx.insert_entry(&entry).await.unwrap();
        tx.commit().await.unwrap();
    }

    /// `store` に対して1回分の `execute` を実行する。`with_tx` のクロージャは
    /// 依存を所有値として `move` するため、同じ変数を複数回の `with_tx`
    /// 呼び出しにまたがって使い回せない（`crate::tx::with_tx` の doc を参照）。
    /// 依存をこの関数内で毎回組み立て直すことで、テストが同じ問題を踏まないようにする。
    async fn run_reverse_entry(
        store: &InMemoryStore,
        id_gen_start: u128,
        input: ReverseEntryInput,
    ) -> Result<JournalEntry, AppError> {
        let schema = TagSchema::empty();
        let id_gen = SequentialIdGenerator::starting_at(id_gen_start);
        let clock = fixed_clock();
        let settings = settings();

        with_tx(store, |tx| {
            Box::pin(async move { execute(tx, &schema, &id_gen, &clock, &settings, input).await })
        })
        .await
    }

    // R-1: 正常系。逆仕訳が貸借反転して作られ、insert_entry まで到達する。
    #[tokio::test]
    async fn reverse_entry_succeeds_and_flips_sides() {
        let store = InMemoryStore::with_chart(sample_chart());
        let original_date = AccountingDate::new(2026, 4, 1).unwrap();
        seed_entry(&store, 1, original_date).await;

        let input = ReverseEntryInput {
            original_id: EntryId::new(1),
            reverse_date: AccountingDate::new(2026, 4, 5).unwrap(),
            reason: "入力誤り".to_string(),
            allow_double_reversal: false,
        };

        let result = run_reverse_entry(&store, 100, input).await;

        let reversal = result.unwrap();
        assert_eq!(reversal.reverses(), Some(EntryId::new(1)));
        assert!(reversal.lines()[0].side() == Side::Credit);
        assert_eq!(store.committed_entries().len(), 2);
    }

    // R-2: 二重訂正は既定で AlreadyReversed になり、既存逆仕訳の番号が入る。
    #[tokio::test]
    async fn reverse_entry_rejects_double_reversal_by_default() {
        let store = InMemoryStore::with_chart(sample_chart());
        let original_date = AccountingDate::new(2026, 4, 1).unwrap();
        seed_entry(&store, 1, original_date).await;

        // 1回目の訂正（成功する）。
        let first_input = ReverseEntryInput {
            original_id: EntryId::new(1),
            reverse_date: AccountingDate::new(2026, 4, 5).unwrap(),
            reason: "1回目".to_string(),
            allow_double_reversal: false,
        };
        let first_reversal = run_reverse_entry(&store, 100, first_input).await.unwrap();

        // 2回目の訂正（同じ元仕訳を再度訂正しようとする）。
        let second_input = ReverseEntryInput {
            original_id: EntryId::new(1),
            reverse_date: AccountingDate::new(2026, 4, 10).unwrap(),
            reason: "2回目".to_string(),
            allow_double_reversal: false,
        };
        let second_result = run_reverse_entry(&store, 200, second_input).await;

        match second_result {
            Err(AppError::AlreadyReversed {
                entry_no,
                reversal_no,
            }) => {
                assert_eq!(entry_no.as_u32(), 1);
                assert_eq!(reversal_no, first_reversal.entry_no());
            }
            other => panic!("AlreadyReversed を期待したが: {other:?}"),
        }
    }

    // R-3: allow_double_reversal: true を指定すれば二重訂正が許可される。
    #[tokio::test]
    async fn reverse_entry_allows_double_reversal_when_explicitly_enabled() {
        let store = InMemoryStore::with_chart(sample_chart());
        let original_date = AccountingDate::new(2026, 4, 1).unwrap();
        seed_entry(&store, 1, original_date).await;

        let first_input = ReverseEntryInput {
            original_id: EntryId::new(1),
            reverse_date: AccountingDate::new(2026, 4, 5).unwrap(),
            reason: "1回目".to_string(),
            allow_double_reversal: false,
        };
        run_reverse_entry(&store, 100, first_input).await.unwrap();

        let second_input = ReverseEntryInput {
            original_id: EntryId::new(1),
            reverse_date: AccountingDate::new(2026, 4, 10).unwrap(),
            reason: "2回目（明示的に許可）".to_string(),
            allow_double_reversal: true,
        };
        let second_result = run_reverse_entry(&store, 200, second_input).await;

        assert!(second_result.is_ok());
        assert_eq!(store.committed_entries().len(), 3);
    }

    // R-4: 元仕訳が別年度でも、逆仕訳は指定日付（reverse_date）の年度に属する。
    #[tokio::test]
    async fn reverse_entry_belongs_to_the_fiscal_year_of_reverse_date() {
        let store = InMemoryStore::with_chart(sample_chart());
        let original_date = AccountingDate::new(2025, 12, 20).unwrap();
        seed_entry(&store, 1, original_date).await;

        let input = ReverseEntryInput {
            original_id: EntryId::new(1),
            reverse_date: AccountingDate::new(2026, 1, 10).unwrap(),
            reason: "年度をまたぐ訂正".to_string(),
            allow_double_reversal: false,
        };

        let result = run_reverse_entry(&store, 100, input).await;

        let reversal = result.unwrap();
        assert_eq!(reversal.fiscal_year(), 2026);
        assert_eq!(
            reversal.entry_date(),
            AccountingDate::new(2026, 1, 10).unwrap()
        );
    }

    // R-5: 存在しない仕訳IDを訂正しようとすると RepoError::NotFound になる。
    #[tokio::test]
    async fn reverse_entry_rejects_unknown_original_id() {
        let store = InMemoryStore::with_chart(sample_chart());

        let input = ReverseEntryInput {
            original_id: EntryId::new(999),
            reverse_date: AccountingDate::new(2026, 4, 5).unwrap(),
            reason: "存在しない".to_string(),
            allow_double_reversal: false,
        };

        let result = run_reverse_entry(&store, 100, input).await;

        assert!(matches!(
            result,
            Err(AppError::Repo(RepoError::NotFound { .. }))
        ));
    }

    // R-6: 締められた期間への訂正は PeriodClosed になる
    // （逆仕訳の取引日＝reverse_date に対して締め状態が判定される）。
    #[tokio::test]
    async fn reverse_entry_rejects_reversal_in_closed_period() {
        let store = InMemoryStore::with_chart(sample_chart());
        let original_date = AccountingDate::new(2026, 4, 1).unwrap();
        seed_entry(&store, 1, original_date).await;
        store.set_closed_through(2026, AccountingDate::new(2026, 3, 31).unwrap());

        let input = ReverseEntryInput {
            original_id: EntryId::new(1),
            reverse_date: AccountingDate::new(2026, 1, 15).unwrap(),
            reason: "締められた期間への訂正".to_string(),
            allow_double_reversal: false,
        };

        let result = run_reverse_entry(&store, 100, input).await;

        assert!(matches!(
            result,
            Err(AppError::Core(kaikei_core::CoreError::PeriodClosed { .. }))
        ));
    }
}
