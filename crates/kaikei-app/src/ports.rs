//! ★契約凍結点★ ドメインが要求する穴（ポート）の trait 定義。
//!
//! # トランザクション境界の設計
//!
//! 3案（UnitOfWork / トランザクションを引数で引き回す / クロージャで実行させる）を
//! 独立に設計・採点した結果、3人の審査員が満場一致で「引数で引き回す」案を
//! 選んだ（`DECISIONS.md` D-029）。要点:
//!
//! - [`Store::Tx`] はライフタイム引数を持たない関連型（GAT を使わない）。
//!   `sqlx::PgPool::begin()` が `Transaction<'static, Postgres>` を返すため
//!   （sqlx 0.8.6 で実測確認済み）、この制約を無理なく満たせる
//! - ユースケース本体は `begin` も `commit` も呼ばない。`&mut Tx` を引数で
//!   受け取り、[`JournalRepo`] 等のメソッドを直接呼ぶだけ
//! - `begin`/`commit`/`rollback` は [`crate::tx::with_tx`] に一本化する
//!
//! # dyn 互換性（`&dyn` / `Arc<dyn>` で使えるか）
//!
//! - [`Store`] は関連型 `Tx` を明示すれば `dyn Store<Tx = 具象型>` として
//!   扱える（`begin` が `&self` を取るため）。ただし実運用では `Arc<PgStore>`
//!   のような**具象型**を axum の `State` に積む設計を推奨する
//!   （`DECISIONS.md` D-029）。`Tx` を dyn 化の時点で具象型に固定する必要が
//!   あり、抽象化の利点が薄いうえ、`with_tx<S: Store>` はジェネリックのまま
//!   使える
//! - [`TxScope`] は `commit`/`rollback` が `self` を値で取る（コミット/
//!   ロールバック後に再利用できないようにするための意図的な設計）ため、
//!   **`dyn TxScope` は原理的に構成できない**（`Self: Sized` を要求する
//!   メソッドを持つ trait は object-safe にならない）
//! - [`TxOps`]・[`JournalRepo`]・[`ChartRepo`]・[`PeriodRepo`]・
//!   [`NumberingRepo`] はいずれも `&mut self` のメソッドのみを持つため
//!   `dyn` 互換（`&mut dyn TxOps` 等として使える）。下部の `dyn_safety`
//!   テストで実際にコンパイルすることを確認している
//! - [`TrialBalanceQuery`] は `Tx` を通さない read model 用のクエリなので
//!   `Arc<dyn TrialBalanceQuery>` を axum の `State` に積む設計が自然
//! - [`AppClock`] / [`IdGenerator`] も `&self`/`&dyn` で問題なく使える

use crate::error::RepoError;
use crate::view::BalanceRowView;
use async_trait::async_trait;
use kaikei_core::{
    AccountingDate, ChartOfAccounts, Clock, EntryId, EntryNumber, JournalEntry, TagKey,
};
use kaikei_policy::CounterpartyIndex;

/// トランザクションを開始する起点。
///
/// 実装は `kaikei-store::PgStore` 等の永続化層が持つ（`kaikei-app` 自身は
/// 実装を持たない。[`crate::testing::InMemoryStore`] はテスト専用の例外）。
#[async_trait]
pub trait Store: Send + Sync + 'static {
    /// この store が返すトランザクション型。ライフタイム引数を持たない
    /// （関連型に `'_` を持たせると GAT が必要になり trait 定義が複雑化する。
    /// `PgPool::begin()` が `Transaction<'static, Postgres>` を返すため
    /// GAT を使わずにこの制約を満たせる）。
    type Tx: TxScope + TxOps;

    /// トランザクションを開始する。
    ///
    /// **直接呼ばないこと。** commit を書き忘れると、エラーも警告も出ずに
    /// 何も保存されない（会計データでは致命的。`DECISIONS.md` D-029）。
    /// 必ず [`crate::tx::with_tx`] を経由すること。
    #[doc(hidden)]
    async fn begin(&self) -> Result<Self::Tx, RepoError>;
}

/// トランザクションの確定・破棄。
///
/// `commit`/`rollback` が `self` を値で取るのは意図的な設計であり、
/// 確定・破棄後に同じトランザクションを再利用するコードを型で防ぐ
/// （このため `dyn TxScope` は構成できない。上部モジュール doc を参照）。
#[async_trait]
pub trait TxScope: Send + Sized {
    /// このトランザクションで行った変更を確定する。
    async fn commit(self) -> Result<(), RepoError>;

    /// このトランザクションで行った変更を破棄する。
    async fn rollback(self) -> Result<(), RepoError>;
}

/// [`JournalRepo`] / [`ChartRepo`] / [`PeriodRepo`] / [`NumberingRepo`] を
/// まとめた束ね trait。ブランケット実装があるため個別に実装する必要はない。
///
/// ユースケースの境界を `where Tx: JournalRepo + ChartRepo + PeriodRepo +
/// NumberingRepo` と4本書く代わりに `where Tx: TxOps` の1本で済ませるために
/// 存在する。読み取り専用のユースケースは、必要な個別 trait だけを
/// `where` 句に書いてよい（この束ね trait を使う義務はない）。
pub trait TxOps: JournalRepo + ChartRepo + PeriodRepo + NumberingRepo + Send {}

impl<T> TxOps for T where T: JournalRepo + ChartRepo + PeriodRepo + NumberingRepo + Send {}

/// 仕訳の読み書き。
///
/// `update_entry` / `delete_entry` は意図的に定義しない（`CLAUDE.md` §2。
/// 帳簿の訂正は [`kaikei_core::JournalEntry::reverse`] による逆仕訳のみ）。
#[async_trait]
pub trait JournalRepo: Send {
    /// 仕訳IDから仕訳を1件取得する。存在しなければ `Ok(None)`。
    async fn find_entry(&mut self, id: EntryId) -> Result<Option<JournalEntry>, RepoError>;

    /// 指定した仕訳を訂正している逆仕訳が既にあれば、その `(EntryId, EntryNumber)`
    /// を返す。無ければ `Ok(None)`。
    ///
    /// 二重取消（既に赤伝済みの仕訳を再度赤伝すること）を検出してユースケース側で
    /// 拒否するために使う。
    async fn find_reversal_of(
        &mut self,
        id: EntryId,
    ) -> Result<Option<(EntryId, EntryNumber)>, RepoError>;

    /// 仕訳を1件追加する。
    ///
    /// 渡す `entry` は [`kaikei_core::JournalEntry::new`] または
    /// [`kaikei_core::JournalEntry::reverse`] を経て構築済みであること
    /// （不変条件の検証済みデータのみを渡す）。
    ///
    /// # Errors
    ///
    /// 既存の仕訳と `id`（仕訳ID）が重複する場合、または既存の仕訳と
    /// `(fiscal_year, entry_no)` の組が重複する場合は
    /// [`RepoError::Conflict`] を返す（採番は同一トランザクション内で
    /// 行うため通常は起こらないが、実装（`PgTx` 等）は DB の一意制約
    /// 違反として検出できる必要がある）。
    async fn insert_entry(&mut self, entry: &JournalEntry) -> Result<(), RepoError>;
}

/// 勘定科目表と取引先索引の読み込み。
///
/// どちらも「その時点のスナップショット」を返す読み取り専用の操作であり、
/// この trait には書き込みメソッドを定義しない。
///
/// 取引先マスタの編集は Phase 4 以降。勘定科目マスタの**投入**は Phase 3 で
/// 専用のユースケース（`usecase/import_chart.rs` 等）と対応するポートを新設して
/// 行う（`DECISIONS.md` D-070）。書き込み用の trait をこの `ChartRepo` に足すのか
/// 別 trait として分けるのかは、その PR で決める。
///
/// `CounterpartyIndex` は `kaikei-policy` の型だが、`kaikei-app`
/// （このファイルの `lib.rs`）が再エクスポートしている。実装者
/// （`kaikei-store` 等）は `kaikei_policy::CounterpartyIndex` を直接
/// `use` せず、`kaikei_app::CounterpartyIndex`（同 `kaikei_app::Counterparty`）
/// 経由で参照すること。`kaikei-store` から `kaikei-policy` への直接依存は
/// CI（`.github/workflows/architecture.yml`）が禁じている。
#[async_trait]
pub trait ChartRepo: Send {
    /// 勘定科目表を読み込む。
    async fn load_chart(&mut self) -> Result<ChartOfAccounts, RepoError>;

    /// 取引先索引を読み込む。
    async fn load_counterparties(&mut self) -> Result<CounterpartyIndex, RepoError>;
}

/// 会計期間の締め状態（の生データ）の読み込み。
///
/// [`kaikei_core::PeriodGuard::status`] は同期の純関数なので、DB を引く
/// 実装は原理的に書けない。この trait は「どこまで締まっているか」という
/// 生データだけを返し、呼び出し側（`kaikei-app`）が
/// [`crate::period_guard::ClosedPeriodGuard`] に固めて `PeriodGuard` として使う。
#[async_trait]
pub trait PeriodRepo: Send {
    /// 指定した会計年度について、締められている期間の終端日を返す。
    /// 締められている期間が無ければ `Ok(None)`。
    async fn closed_through(
        &mut self,
        fiscal_year: i32,
    ) -> Result<Option<AccountingDate>, RepoError>;
}

/// 仕訳番号の払い出し。
///
/// 採番規則そのもの（次はどの番号か）は `kaikei_policy::Numbering` が定めるが、
/// カウンタの実際の読み書きは I/O なのでこの trait が担う。仕訳 INSERT と
/// 同一トランザクションで採番することで、検証失敗時にカウンタの増分も
/// 一緒に巻き戻り、欠番が原理的に発生しない（`DECISIONS.md` の該当決定）。
#[async_trait]
pub trait NumberingRepo: Send {
    /// 指定した会計年度で次に払い出す仕訳番号を採番する。
    async fn next_entry_no(&mut self, fiscal_year: i32) -> Result<EntryNumber, RepoError>;
}

/// 試算表の read model クエリ。
///
/// `Tx` を通さない（`CLAUDE.md` §6「read model は物理的に分離する」）。
/// 書き込みはドメインモデル経由（[`JournalRepo`] 等）、読み取りは SQL 集計に
/// 直行させ、両者を混ぜない。
///
/// 実装者（`kaikei-store`、PR-6）向けの申し送り: 金額の `SUM` を SQL で
/// 集計する際の型の扱いは `DECISIONS.md` D-033 を参照（`SUM(amount_minor)`
/// を `::BIGINT` へ明示キャストし、桁あふれは `RepoError::OutOfRange` に
/// 写像する）。
#[async_trait]
pub trait TrialBalanceQuery: Send + Sync {
    /// `from`〜`to`（取引日、両端を含む）の仕訳明細を `group_by` で集計する。
    ///
    /// `group_by` は `TagSchema::is_aggregatable` が `true` を返すキーに
    /// 限定されるべきだが、その検証はユースケース側の責務（この trait の
    /// 実装は SQL 集計に徹する）。
    async fn trial_balance(
        &self,
        from: AccountingDate,
        to: AccountingDate,
        group_by: &[TagKey],
    ) -> Result<Vec<BalanceRowView>, RepoError>;
}

/// `kaikei_core::Clock` を `Send + Sync` に限定したブランケット trait。
///
/// `kaikei-core::Clock` 自体には `Send`/`Sync` 境界が無く、`&dyn Clock` は
/// `!Send` になる（実測確認済み）。ユースケースの引数を `&dyn Clock` にすると
/// await 跨ぎで保持される際に future 全体が `!Send` となり、axum のハンドラや
/// `tokio::spawn` で「future cannot be sent between threads safely」という
/// 読みにくいエラーになる。この trait を境界として最初から使うことで、
/// 原因（core の trait に境界が無いこと）に触れずに済む。
///
/// `&dyn AppClock` は `&dyn Clock` へ unsize coercion されるため、
/// `kaikei_core::JournalEntry::new` 等の呼び出しは無変更で済む。
pub trait AppClock: Clock + Send + Sync {}

impl<C: Clock + Send + Sync> AppClock for C {}

/// 仕訳IDの生成。
///
/// 生成規則自体（UUID v7 等）は実装（[`crate::id`] の関数、または
/// `kaikei-store` 側の実装）が決める。ユースケースはこの trait を通してのみ
/// 新しい ID を得る。
pub trait IdGenerator: Send + Sync {
    /// 新しい仕訳IDを生成する。
    fn new_entry_id(&self) -> EntryId;
}

#[cfg(test)]
mod dyn_safety {
    use super::*;
    use async_trait::async_trait;
    use kaikei_core::{Clock, Timestamp};
    use std::sync::Arc;

    /// `&mut dyn JournalRepo` 等として使えることの静的検査に使う最小実装。
    /// 実際の永続化は行わない。
    struct NoopTx;

    #[async_trait]
    impl JournalRepo for NoopTx {
        async fn find_entry(&mut self, _id: EntryId) -> Result<Option<JournalEntry>, RepoError> {
            Ok(None)
        }

        async fn find_reversal_of(
            &mut self,
            _id: EntryId,
        ) -> Result<Option<(EntryId, EntryNumber)>, RepoError> {
            Ok(None)
        }

        async fn insert_entry(&mut self, _entry: &JournalEntry) -> Result<(), RepoError> {
            Ok(())
        }
    }

    #[async_trait]
    impl ChartRepo for NoopTx {
        async fn load_chart(&mut self) -> Result<ChartOfAccounts, RepoError> {
            Ok(ChartOfAccounts::new(Vec::new()).expect("空の勘定科目表は必ず構築できる"))
        }

        async fn load_counterparties(&mut self) -> Result<CounterpartyIndex, RepoError> {
            Ok(CounterpartyIndex::empty())
        }
    }

    #[async_trait]
    impl PeriodRepo for NoopTx {
        async fn closed_through(
            &mut self,
            _fiscal_year: i32,
        ) -> Result<Option<AccountingDate>, RepoError> {
            Ok(None)
        }
    }

    #[async_trait]
    impl NumberingRepo for NoopTx {
        async fn next_entry_no(&mut self, _fiscal_year: i32) -> Result<EntryNumber, RepoError> {
            Ok(EntryNumber::new(1))
        }
    }

    #[async_trait]
    impl TxScope for NoopTx {
        async fn commit(self) -> Result<(), RepoError> {
            Ok(())
        }

        async fn rollback(self) -> Result<(), RepoError> {
            Ok(())
        }
    }

    /// `Store` が `dyn Store<Tx = 具象型>` として構成できることの検査専用。
    struct NoopStore;

    #[async_trait]
    impl Store for NoopStore {
        type Tx = NoopTx;

        async fn begin(&self) -> Result<Self::Tx, RepoError> {
            Ok(NoopTx)
        }
    }

    fn _dyn_journal_repo(_: &mut dyn JournalRepo) {}
    fn _dyn_chart_repo(_: &mut dyn ChartRepo) {}
    fn _dyn_period_repo(_: &mut dyn PeriodRepo) {}
    fn _dyn_numbering_repo(_: &mut dyn NumberingRepo) {}
    fn _dyn_tx_ops(_: &mut dyn TxOps) {}

    #[test]
    fn journal_chart_period_numbering_repos_and_tx_ops_are_dyn_compatible() {
        let mut tx = NoopTx;
        _dyn_journal_repo(&mut tx);
        _dyn_chart_repo(&mut tx);
        _dyn_period_repo(&mut tx);
        _dyn_numbering_repo(&mut tx);
        _dyn_tx_ops(&mut tx);
    }

    #[test]
    fn store_can_be_used_as_dyn_with_a_concrete_tx_type() {
        // `Store` 自体は `&self` を取る `begin` のみを持つため
        // `dyn Store<Tx = 具象型>` として構成できる。ただし `TxScope` の
        // `commit`/`rollback` は `self` を値で取るため `dyn TxScope` は
        // 構成できない（コンパイルエラーになるためここでは試みない。
        // 上部モジュール doc を参照）。
        let store: Box<dyn Store<Tx = NoopTx>> = Box::new(NoopStore);
        let _ = store;
    }

    struct NoopClock;

    impl Clock for NoopClock {
        fn now(&self) -> Timestamp {
            Timestamp::from_unix_nanos(0)
        }
    }

    struct NoopIdGenerator;

    impl IdGenerator for NoopIdGenerator {
        fn new_entry_id(&self) -> EntryId {
            EntryId::new(0)
        }
    }

    struct NoopTrialBalanceQuery;

    #[async_trait]
    impl TrialBalanceQuery for NoopTrialBalanceQuery {
        async fn trial_balance(
            &self,
            _from: AccountingDate,
            _to: AccountingDate,
            _group_by: &[TagKey],
        ) -> Result<Vec<BalanceRowView>, RepoError> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn app_clock_id_generator_and_trial_balance_query_can_be_used_as_arc_dyn() {
        let clock: Arc<dyn AppClock> = Arc::new(NoopClock);
        assert_eq!(clock.now().as_unix_nanos(), 0);

        let id_gen: Arc<dyn IdGenerator> = Arc::new(NoopIdGenerator);
        assert_eq!(id_gen.new_entry_id().as_u128(), 0);

        let query: Arc<dyn TrialBalanceQuery> = Arc::new(NoopTrialBalanceQuery);
        let _ = query;
    }
}
