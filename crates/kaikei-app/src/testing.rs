//! テスト用のインメモリ実装。`#[cfg(any(test, feature = "testing"))]`
//! （lib.rs 側の `mod` 宣言で切っている）。
//!
//! 他 crate（`kaikei-store` のテスト、後続 PR の usecase テスト等）から使う
//! ことを想定するため、`#[cfg(test)]` だけでなく `testing` feature でも
//! 有効になるようにしている（`#[cfg(test)]` は自クレート内のテストからしか
//! 見えず、他 crate からは参照できないため）。
//!
//! ここに置くのは「DB 無しでユースケースをテストできる」ための最小限の
//! フェイクのみ。実際の永続化・トランザクション分離（ロールバック等）の
//! 完全な模倣ではない。

use crate::error::RepoError;
use crate::ports::{ChartRepo, JournalRepo, NumberingRepo, PeriodRepo, Store, TxOps, TxScope};
use async_trait::async_trait;
use kaikei_core::{AccountingDate, ChartOfAccounts, EntryId, EntryNumber, JournalEntry};
use kaikei_policy::CounterpartyIndex;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

/// `InMemoryStore` が保持する共有状態。
struct InMemoryState {
    /// `EntryId::as_u128()` をキーにした、commit 済みの仕訳一覧。
    entries: HashMap<u128, JournalEntry>,
    chart: ChartOfAccounts,
    counterparties: CounterpartyIndex,
    /// 会計年度ラベル → 締められている期間の終端日。
    closed_through: BTreeMap<i32, AccountingDate>,
    /// 会計年度ラベル → 直近に払い出し済みの仕訳番号。
    next_no: BTreeMap<i32, u32>,
}

impl InMemoryState {
    fn new() -> Self {
        InMemoryState {
            entries: HashMap::new(),
            chart: ChartOfAccounts::new(Vec::new()).expect("空の勘定科目表は必ず構築できる"),
            counterparties: CounterpartyIndex::empty(),
            closed_through: BTreeMap::new(),
            next_no: BTreeMap::new(),
        }
    }
}

/// DB を使わないインメモリの [`Store`] 実装。
///
/// `kaikei-app` 自身のテスト、および後続 PR のユースケーステストが DB 無しで
/// 動作するために提供する。実際の DB トランザクションの分離レベルを厳密に
/// 再現するものではないが、`with_tx` の commit/rollback 経路を検証するには
/// 十分な忠実度を持たせている（[`InMemoryTx`] の doc を参照）。
#[derive(Clone)]
pub struct InMemoryStore {
    state: Arc<Mutex<InMemoryState>>,
}

impl InMemoryStore {
    /// 空の状態（勘定科目表なし・取引先なし・締め期間なし）で store を作る。
    pub fn new() -> Self {
        InMemoryStore {
            state: Arc::new(Mutex::new(InMemoryState::new())),
        }
    }

    /// 勘定科目表を差し替えたテスト用 store を作る。
    pub fn with_chart(chart: ChartOfAccounts) -> Self {
        let store = Self::new();
        store.lock().chart = chart;
        store
    }

    /// 取引先索引を差し替える（テスト用）。
    pub fn set_counterparties(&self, counterparties: CounterpartyIndex) {
        self.lock().counterparties = counterparties;
    }

    /// 指定した会計年度の締め終端日を設定する（テスト用）。
    pub fn set_closed_through(&self, fiscal_year: i32, date: AccountingDate) {
        self.lock().closed_through.insert(fiscal_year, date);
    }

    /// commit 済みの仕訳一覧を取得する（テストのアサーション用）。
    pub fn committed_entries(&self) -> Vec<JournalEntry> {
        self.lock().entries.values().cloned().collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, InMemoryState> {
        self.state
            .lock()
            .expect("InMemoryStore の Mutex はテスト用フェイクなので毒されない前提")
    }
}

impl Default for InMemoryStore {
    fn default() -> Self {
        InMemoryStore::new()
    }
}

#[async_trait]
impl Store for InMemoryStore {
    type Tx = InMemoryTx;

    async fn begin(&self) -> Result<Self::Tx, RepoError> {
        Ok(InMemoryTx {
            shared: Arc::clone(&self.state),
            inserted_entries: Vec::new(),
            issued_numbers: BTreeMap::new(),
        })
    }
}

/// [`InMemoryStore::begin`] が返すフェイクトランザクション。
///
/// 書き込み（仕訳の追加・採番の払い出し）はこの構造体が持つローカルな
/// バッファに溜め、[`TxScope::commit`] された時点でのみ共有状態へ反映する。
/// [`TxScope::rollback`]（またはバッファを破棄する）と何も反映されない。
/// これにより `with_tx` の commit/rollback 経路の違いをテストで観測できる。
pub struct InMemoryTx {
    shared: Arc<Mutex<InMemoryState>>,
    inserted_entries: Vec<JournalEntry>,
    /// 会計年度ラベル → このトランザクション内で払い出した最後の番号。
    issued_numbers: BTreeMap<i32, u32>,
}

impl InMemoryTx {
    fn lock_shared(&self) -> std::sync::MutexGuard<'_, InMemoryState> {
        self.shared
            .lock()
            .expect("InMemoryStore の Mutex はテスト用フェイクなので毒されない前提")
    }
}

#[async_trait]
impl JournalRepo for InMemoryTx {
    async fn find_entry(&mut self, id: EntryId) -> Result<Option<JournalEntry>, RepoError> {
        if let Some(entry) = self.inserted_entries.iter().find(|e| e.id() == id) {
            return Ok(Some(entry.clone()));
        }
        Ok(self.lock_shared().entries.get(&id.as_u128()).cloned())
    }

    async fn find_reversal_of(
        &mut self,
        id: EntryId,
    ) -> Result<Option<(EntryId, EntryNumber)>, RepoError> {
        if let Some(entry) = self
            .inserted_entries
            .iter()
            .find(|e| e.reverses() == Some(id))
        {
            return Ok(Some((entry.id(), entry.entry_no())));
        }
        Ok(self
            .lock_shared()
            .entries
            .values()
            .find(|e| e.reverses() == Some(id))
            .map(|e| (e.id(), e.entry_no())))
    }

    async fn insert_entry(&mut self, entry: &JournalEntry) -> Result<(), RepoError> {
        self.inserted_entries.push(entry.clone());
        Ok(())
    }
}

#[async_trait]
impl ChartRepo for InMemoryTx {
    async fn load_chart(&mut self) -> Result<ChartOfAccounts, RepoError> {
        Ok(self.lock_shared().chart.clone())
    }

    async fn load_counterparties(&mut self) -> Result<CounterpartyIndex, RepoError> {
        Ok(self.lock_shared().counterparties.clone())
    }
}

#[async_trait]
impl PeriodRepo for InMemoryTx {
    async fn closed_through(
        &mut self,
        fiscal_year: i32,
    ) -> Result<Option<AccountingDate>, RepoError> {
        Ok(self.lock_shared().closed_through.get(&fiscal_year).copied())
    }
}

#[async_trait]
impl NumberingRepo for InMemoryTx {
    async fn next_entry_no(&mut self, fiscal_year: i32) -> Result<EntryNumber, RepoError> {
        let baseline = self
            .lock_shared()
            .next_no
            .get(&fiscal_year)
            .copied()
            .unwrap_or(0);
        let current = self
            .issued_numbers
            .get(&fiscal_year)
            .copied()
            .unwrap_or(baseline);
        let next = current
            .checked_add(1)
            .ok_or_else(|| RepoError::OutOfRange {
                reason: "仕訳番号が u32 の上限に達しました".to_string(),
            })?;
        self.issued_numbers.insert(fiscal_year, next);
        Ok(EntryNumber::new(next))
    }
}

#[async_trait]
impl TxScope for InMemoryTx {
    async fn commit(self) -> Result<(), RepoError> {
        // 先にフィールドを分解してから `shared` をロックする。
        // `self.lock_shared()`（`&self` を借用）を呼んだ後に `self.inserted_entries`
        // を move しようとすると E0505（借用と move の衝突）になる。
        let InMemoryTx {
            shared,
            inserted_entries,
            issued_numbers,
        } = self;
        let mut guard = shared
            .lock()
            .expect("InMemoryStore の Mutex はテスト用フェイクなので毒されない前提");
        for entry in inserted_entries {
            guard.entries.insert(entry.id().as_u128(), entry);
        }
        for (fiscal_year, next) in issued_numbers {
            guard.next_no.insert(fiscal_year, next);
        }
        Ok(())
    }

    async fn rollback(self) -> Result<(), RepoError> {
        // ローカルバッファ（`inserted_entries` / `issued_numbers`）を
        // そのまま破棄するだけで、共有状態には一切触れない。
        Ok(())
    }
}

/// 呼び出しごとに1ずつ増える決定的な仕訳IDを返す `IdGenerator` フェイク。
///
/// [`crate::id::UuidV7IdGenerator`] は呼び出しごとに非決定的な値になるため、
/// 期待する ID を固定したいテストにはこちらを使う。
pub struct SequentialIdGenerator {
    next: Mutex<u128>,
}

impl SequentialIdGenerator {
    /// 指定した値から採番を始めるフェイクを作る。
    pub fn starting_at(first: u128) -> Self {
        SequentialIdGenerator {
            next: Mutex::new(first),
        }
    }
}

impl crate::ports::IdGenerator for SequentialIdGenerator {
    fn new_entry_id(&self) -> EntryId {
        let mut guard = self
            .next
            .lock()
            .expect("SequentialIdGenerator の Mutex はテスト用フェイクなので毒されない前提");
        let id = EntryId::new(*guard);
        *guard += 1;
        id
    }
}

/// `Tx` が [`TxOps`] を満たすこと（束ね trait のブランケット実装が
/// `InMemoryTx` にも効くこと）の静的検査。
fn _assert_in_memory_tx_satisfies_tx_ops<T: TxOps>() {}
const _: fn() = _assert_in_memory_tx_satisfies_tx_ops::<InMemoryTx>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AppError;
    use crate::ports::IdGenerator;
    use crate::tx::with_tx;

    #[tokio::test]
    async fn insert_entry_is_visible_after_commit_but_not_before() {
        let store = InMemoryStore::new();
        assert!(store.committed_entries().is_empty());

        // commit 前は共有状態に反映されない（rollback と区別するための前提確認）。
        let mut tx = store.begin().await.unwrap();
        let entry = tests_support::sample_entry(1, 1);
        tx.insert_entry(&entry).await.unwrap();
        assert!(store.committed_entries().is_empty());

        tx.commit().await.unwrap();
        assert_eq!(store.committed_entries().len(), 1);
    }

    #[tokio::test]
    async fn rollback_discards_buffered_writes() {
        let store = InMemoryStore::new();

        let result: Result<(), AppError> = with_tx(&store, |tx| {
            Box::pin(async move {
                let entry = tests_support::sample_entry(1, 1);
                tx.insert_entry(&entry).await?;
                Err(AppError::Rejected {
                    reason: "意図的な失敗（rollback を発生させる）".to_string(),
                })
            })
        })
        .await;

        assert!(result.is_err());
        assert!(store.committed_entries().is_empty());
    }

    #[test]
    fn sequential_id_generator_increments_from_the_given_start() {
        let generator = SequentialIdGenerator::starting_at(10);
        assert_eq!(generator.new_entry_id().as_u128(), 10);
        assert_eq!(generator.new_entry_id().as_u128(), 11);
    }

    /// テスト専用の仕訳ビルダー。`tests` モジュールの複数のテストから
    /// 使うための共通ヘルパ。
    mod tests_support {
        use kaikei_core::{
            AccountCode, AccountDef, AccountType, AccountingDate, ChartOfAccounts, Currency,
            EntryId, EntryNumber, FiscalYear, FixedClock, JournalEntry, JournalLine, Money,
            NewEntry, PeriodGuard, PeriodStatus, Side, TagSchema, TagSet, Timestamp,
        };

        struct AllOpen;
        impl PeriodGuard for AllOpen {
            fn status(&self, _date: AccountingDate) -> PeriodStatus {
                PeriodStatus::Open
            }
        }

        /// テストで使う、貸借が一致した最小限の仕訳を1件組み立てる。
        pub(super) fn sample_entry(id: u128, entry_no: u32) -> JournalEntry {
            let chart = ChartOfAccounts::new(vec![
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
            .unwrap();
            let schema = TagSchema::empty();
            let fy = FiscalYear::calendar_year(2026);
            let clock = FixedClock(Timestamp::from_unix_nanos(0));

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

            JournalEntry::new(
                NewEntry {
                    id: EntryId::new(id),
                    entry_no: EntryNumber::new(entry_no),
                    entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
                    description: "テスト仕訳".to_string(),
                    lines,
                    document_refs: Vec::new(),
                },
                &fy,
                &chart,
                &schema,
                &AllOpen,
                &clock,
            )
            .unwrap()
        }
    }
}
