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
use crate::ports::{
    ChartRepo, ChartWriteRepo, JournalRepo, NumberingRepo, PeriodRepo, Store, TxOps, TxScope,
};
use async_trait::async_trait;
use kaikei_core::{
    AccountDef, AccountingDate, ChartOfAccounts, EntryId, EntryNumber, JournalEntry,
};
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
            inserted_accounts: Vec::new(),
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
    /// このトランザクション内で追加した勘定科目
    /// （[`ChartWriteRepo::insert_accounts`]）。commit 時に共有状態へ反映する。
    inserted_accounts: Vec<AccountDef>,
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
        // 実 DB では仕訳ID・`(fiscal_year, entry_no)` のいずれも一意制約
        // 違反として拒否される（`crates/kaikei-store/migrations/0003_journal.sql`
        // の `PRIMARY KEY` / `UNIQUE (fiscal_year, entry_no)`）。この fake が
        // 重複を無言で上書きすると、実装が同じ状況で `RepoError::Conflict`
        // を返す経路をテストで再現できなくなる（`JournalRepo::insert_entry`
        // の `# Errors` を参照）。
        let local_id_conflict = self.inserted_entries.iter().any(|e| e.id() == entry.id());
        let local_fy_no_conflict = self
            .inserted_entries
            .iter()
            .any(|e| e.fiscal_year() == entry.fiscal_year() && e.entry_no() == entry.entry_no());

        let (shared_id_conflict, shared_fy_no_conflict) = {
            let shared = self.lock_shared();
            let id_conflict = shared.entries.contains_key(&entry.id().as_u128());
            let fy_no_conflict = shared.entries.values().any(|e| {
                e.fiscal_year() == entry.fiscal_year() && e.entry_no() == entry.entry_no()
            });
            (id_conflict, fy_no_conflict)
        };

        if local_id_conflict || shared_id_conflict {
            return Err(RepoError::Conflict {
                reason: format!("仕訳ID {} は既に存在します", entry.id().as_u128()),
            });
        }
        if local_fy_no_conflict || shared_fy_no_conflict {
            return Err(RepoError::Conflict {
                reason: format!(
                    "会計年度 {} の仕訳番号 {} は既に存在します",
                    entry.fiscal_year(),
                    entry.entry_no().as_u32()
                ),
            });
        }

        self.inserted_entries.push(entry.clone());
        Ok(())
    }
}

#[async_trait]
impl ChartRepo for InMemoryTx {
    async fn load_chart(&mut self) -> Result<ChartOfAccounts, RepoError> {
        let committed = self.lock_shared().chart.clone();
        if self.inserted_accounts.is_empty() {
            return Ok(committed);
        }
        // 同一トランザクション内の追加が見えること（read-your-writes）を
        // 実 DB と揃える。`import_chart::execute` は load → insert の順なので
        // 現状これに依存していないが、依存しない実装であることをテストで
        // 保証しているわけではないので、忠実度の高い方に寄せる。
        let mut defs: Vec<AccountDef> = committed.iter().cloned().collect();
        defs.extend(self.inserted_accounts.iter().cloned());
        ChartOfAccounts::new(defs).map_err(|e| RepoError::Corrupt {
            reason: format!("勘定科目表が整合しません: {e}"),
        })
    }

    async fn load_counterparties(&mut self) -> Result<CounterpartyIndex, RepoError> {
        Ok(self.lock_shared().counterparties.clone())
    }
}

#[async_trait]
impl ChartWriteRepo for InMemoryTx {
    async fn insert_accounts(&mut self, defs: &[AccountDef]) -> Result<usize, RepoError> {
        // 実装の契約（`ports::ChartWriteRepo`）どおり、既にあるコードは
        // **何もしない**（上書きしない）。PostgreSQL 実装の
        // `ON CONFLICT (code) DO NOTHING` に対応する。
        let committed = self.lock_shared().chart.clone();
        let mut inserted = 0usize;
        for def in defs {
            let already_committed = committed.get(&def.code).is_some();
            let already_buffered = self
                .inserted_accounts
                .iter()
                .any(|existing| existing.code == def.code);
            if already_committed || already_buffered {
                continue;
            }
            self.inserted_accounts.push(def.clone());
            inserted += 1;
        }
        Ok(inserted)
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
            inserted_accounts,
            issued_numbers,
        } = self;
        let mut guard = shared
            .lock()
            .expect("InMemoryStore の Mutex はテスト用フェイクなので毒されない前提");
        for entry in inserted_entries {
            guard.entries.insert(entry.id().as_u128(), entry);
        }
        if !inserted_accounts.is_empty() {
            let mut defs: Vec<AccountDef> = guard.chart.iter().cloned().collect();
            defs.extend(inserted_accounts);
            guard.chart = ChartOfAccounts::new(defs).map_err(|e| RepoError::Corrupt {
                reason: format!("勘定科目表が整合しません: {e}"),
            })?;
        }
        for (fiscal_year, next) in issued_numbers {
            guard.next_no.insert(fiscal_year, next);
        }
        Ok(())
    }

    async fn rollback(self) -> Result<(), RepoError> {
        // ローカルバッファ（`inserted_entries` / `inserted_accounts` /
        // `issued_numbers`）をそのまま破棄するだけで、共有状態には一切触れない。
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

/// [`crate::audit::with_audit`] のテスト用に、書かれた監査ログの行を
/// メモリに溜める [`AuditSink`] フェイク。
///
/// **実際の永続化層の代わりにはならない。** このフェイクは
/// 「帳簿とは別のコネクションで書く」という D-070 の要件を検証できない
/// （そもそもトランザクションが無い）。それを実証するのは実 PostgreSQL の
/// テスト（`crates/kaikei-store/tests/audit_log.rs`）の役目であり、
/// ここで検証するのは **fail-closed / fail-open の手順**だけである。
pub struct RecordingAuditSink {
    rows: Mutex<Vec<RecordedAuditRow>>,
    fail_start: bool,
    fail_result: bool,
}

/// [`RecordingAuditSink`] が記録した1行（`audit_log` の行に相当）。
#[derive(Debug, Clone)]
pub struct RecordedAuditRow {
    /// `request_id` 列。
    pub request_id: crate::audit::RequestId,
    /// `occurred_at` 列。
    pub occurred_at: kaikei_core::Timestamp,
    /// `actor` 列。
    pub actor: String,
    /// `tool` 列。
    pub tool: String,
    /// `status` 列（`crate::audit::status` の定数）。
    pub status: &'static str,
    /// `input` 列（開始レコードのみ）。
    pub input_json: Option<String>,
    /// `output` 列に載せる JSON（成功の結果レコードのみ）。
    pub output_json: Option<String>,
    /// 失敗の結果レコードで AI に返した本文（`public_message()`）。
    pub public_message: Option<String>,
    /// `error_code` 列。
    pub error_code: Option<String>,
    /// `entry_id` 列。
    pub entry_id: Option<EntryId>,
}

impl RecordingAuditSink {
    /// すべての書き込みに成功するフェイクを作る。
    pub fn new() -> Self {
        RecordingAuditSink {
            rows: Mutex::new(Vec::new()),
            fail_start: false,
            fail_result: false,
        }
    }

    /// **開始レコードの書き込みが必ず失敗する**フェイク（fail-closed の検証用）。
    ///
    /// 返す [`RepoError`] は、実際に `REVOKE INSERT ON audit_log FROM
    /// kaikei_app` した場合と同じ形（SQLSTATE 42501 →
    /// `AppendOnlyViolation`）にしてある。この文言（「訂正は逆仕訳で」）が
    /// **応答に漏れないこと**の回帰テストを書けるようにするため。
    pub fn failing_on_start() -> Self {
        RecordingAuditSink {
            rows: Mutex::new(Vec::new()),
            fail_start: true,
            fail_result: false,
        }
    }

    /// **結果レコードの書き込みだけが失敗する**フェイク（fail-open の検証用）。
    pub fn failing_on_result() -> Self {
        RecordingAuditSink {
            rows: Mutex::new(Vec::new()),
            fail_start: false,
            fail_result: true,
        }
    }

    /// 記録された行を古い順に返す。
    pub fn rows(&self) -> Vec<RecordedAuditRow> {
        self.rows
            .lock()
            .expect("RecordingAuditSink の Mutex はテスト用フェイクなので毒されない前提")
            .clone()
    }

    fn denied() -> RepoError {
        RepoError::AppendOnlyViolation {
            reason: "権限エラーです（SQLSTATE 42501: insufficient_privilege）: \
                     permission denied for table audit_log"
                .to_string(),
        }
    }
}

impl Default for RecordingAuditSink {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl crate::ports::AuditSink for RecordingAuditSink {
    async fn record_start(&self, record: &crate::audit::AuditStart<'_>) -> Result<(), RepoError> {
        if self.fail_start {
            return Err(Self::denied());
        }
        self.rows
            .lock()
            .expect("RecordingAuditSink の Mutex はテスト用フェイクなので毒されない前提")
            .push(RecordedAuditRow {
                request_id: record.request_id,
                occurred_at: record.occurred_at,
                actor: record.actor.to_string(),
                tool: record.tool.to_string(),
                status: crate::audit::status::STARTED,
                input_json: record.input_json.map(str::to_string),
                output_json: None,
                public_message: None,
                error_code: None,
                entry_id: None,
            });
        Ok(())
    }

    async fn record_result(&self, record: &crate::audit::AuditResult<'_>) -> Result<(), RepoError> {
        if self.fail_result {
            return Err(Self::denied());
        }
        let (output_json, public_message) = match record.outcome {
            crate::audit::AuditOutcome::Succeeded { output_json } => {
                (output_json.map(str::to_string), None)
            }
            // 失敗時も応答本文（あれば）を残す。永続化層
            // （`kaikei_store::audit`）と同じ非対称の解消
            // （PR-F レビュー C-4）。
            crate::audit::AuditOutcome::Failed {
                public_message,
                output_json,
                ..
            } => (
                output_json.map(str::to_string),
                Some(public_message.to_string()),
            ),
        };
        self.rows
            .lock()
            .expect("RecordingAuditSink の Mutex はテスト用フェイクなので毒されない前提")
            .push(RecordedAuditRow {
                request_id: record.request_id,
                occurred_at: record.occurred_at,
                actor: record.actor.to_string(),
                tool: record.tool.to_string(),
                status: record.outcome.status_code(),
                input_json: None,
                output_json,
                public_message,
                error_code: record.outcome.error_code().map(str::to_string),
                entry_id: record.entry_id,
            });
        Ok(())
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

    #[tokio::test]
    async fn insert_entry_rejects_duplicate_id_within_the_same_transaction() {
        let store = InMemoryStore::new();
        let mut tx = store.begin().await.unwrap();
        let first = tests_support::sample_entry(1, 1);
        tx.insert_entry(&first).await.unwrap();

        // 同じトランザクション内でも、同じ仕訳IDを二重に挿入しようとすると
        // 拒否される（DB の PRIMARY KEY 違反を模している）。
        let duplicate_id = tests_support::sample_entry(1, 2);
        let result = tx.insert_entry(&duplicate_id).await;
        assert!(matches!(result, Err(RepoError::Conflict { .. })));
    }

    #[tokio::test]
    async fn insert_entry_rejects_duplicate_id_after_commit() {
        let store = InMemoryStore::new();
        let entry = tests_support::sample_entry(1, 1);

        let mut tx = store.begin().await.unwrap();
        tx.insert_entry(&entry).await.unwrap();
        tx.commit().await.unwrap();

        let mut tx = store.begin().await.unwrap();
        let result = tx.insert_entry(&entry).await;
        assert!(matches!(result, Err(RepoError::Conflict { .. })));
    }

    #[tokio::test]
    async fn insert_entry_rejects_duplicate_fiscal_year_and_entry_no() {
        let store = InMemoryStore::new();
        let first = tests_support::sample_entry(1, 1);

        let mut tx = store.begin().await.unwrap();
        tx.insert_entry(&first).await.unwrap();
        tx.commit().await.unwrap();

        // 別の仕訳ID・同じ (fiscal_year, entry_no) は UNIQUE 制約違反に相当する。
        let second = tests_support::sample_entry(2, 1);
        let mut tx = store.begin().await.unwrap();
        let result = tx.insert_entry(&second).await;
        assert!(matches!(result, Err(RepoError::Conflict { .. })));
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
        use crate::test_support::{sample_chart, AllOpen};
        use kaikei_core::{
            AccountCode, AccountingDate, Currency, EntryId, EntryNumber, FiscalYear, JournalEntry,
            JournalLine, Money, NewEntry, Side, TagSchema, TagSet,
        };

        /// テストで使う、貸借が一致した最小限の仕訳を1件組み立てる。
        pub(super) fn sample_entry(id: u128, entry_no: u32) -> JournalEntry {
            let chart = sample_chart();
            let schema = TagSchema::empty();
            let fy = FiscalYear::calendar_year(2026);
            let clock = crate::test_support::fixed_clock();

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
