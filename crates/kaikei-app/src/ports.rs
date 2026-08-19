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
//! - [`TxOps`]・[`JournalRepo`]・[`ChartRepo`]・[`ChartWriteRepo`]・
//!   [`PeriodRepo`]・
//!   [`NumberingRepo`] はいずれも `&mut self` のメソッドのみを持つため
//!   `dyn` 互換（`&mut dyn TxOps` 等として使える）。下部の `dyn_safety`
//!   テストで実際にコンパイルすることを確認している
//! - [`TrialBalanceQuery`] は `Tx` を通さない read model 用のクエリなので
//!   `Arc<dyn TrialBalanceQuery>` を axum の `State` に積む設計が自然
//! - [`AuditSink`] も `Tx` を通さない（**通してはならない**。下記）。
//!   `&self` のメソッドのみなので `Arc<dyn AuditSink>` として持てる
//! - [`AppClock`] / [`IdGenerator`] も `&self`/`&dyn` で問題なく使える

use crate::audit::{AuditResult, AuditStart};
use crate::error::RepoError;
use crate::view::{BalanceRowView, EntryCursor, EntrySearchPageView, LedgerCursor, LedgerPageView};
use async_trait::async_trait;
use kaikei_core::{
    AccountCode, AccountDef, AccountingDate, ChartOfAccounts, Clock, Currency, EntryId,
    EntryNumber, JournalEntry, Money, TagKey, Timestamp,
};
use kaikei_policy::{Counterparty, CounterpartyIndex};

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

    /// 取引日が `from` 〜 `to`（両端を含む）の仕訳を**ドメインモデルとして**
    /// すべて返す。`(entry_date, entry_no)` の昇順。
    ///
    /// # なぜ read model ではなくドメインモデルなのか
    ///
    /// [`kaikei_core::TrialBalance::from_entries`] が `&JournalEntry` の
    /// イテレータを要求するためである。決算振替（`ClosingPolicy`）も
    /// 財務諸表（`StatementPolicy`）も `kaikei_core::TrialBalance` を入力に
    /// 取るので、read model の DTO（[`crate::view::TrialBalanceView`]。
    /// `DECISIONS.md` D-031）では代わりにならない。
    /// 画面に出すための試算表は read model、**帳簿から計算し直すもの**は
    /// この経路、という住み分けになる。
    ///
    /// # 取り消された仕訳も返す
    ///
    /// 赤伝で訂正された仕訳も、赤伝そのものも隠さない（`DECISIONS.md` D-088）。
    /// 試算表は両者を含めて集計することで相殺されるので、これが正しい。
    /// 隠すと決算書の金額が狂う。
    ///
    /// # 全件をメモリに載せる
    ///
    /// 上限もページングも設けない。**決算書の計算で件数を切ったら、
    /// 出てくるのは「途中まで正しい決算書」ではなく単に誤った決算書**で
    /// あり、切ったことを応答で伝える（D-089）という解決が使えない。
    /// 個人事業主の帳簿1年分（数千件）を前提とした割り切りであり、
    /// 試算表 read model の全件走査（D-046）と同じ性質の判断である。
    /// 体感できる遅さとして顕在化したら、その時点で分割を設計する。
    ///
    /// # 入力の妥当性検証はユースケース側の責務
    ///
    /// [`SearchEntriesQuery`] / [`LedgerQuery`] と同じ分担で、この実装は
    /// SQL に徹する。**`from > to` の拒否はユースケース側で済んでいる**
    /// （[`crate::usecase::ledger`] と同じ形。期間を逆に指定したときに
    /// 「貸借一致した空の決算書」が成功で返るのは、`PROGRESS.md` Phase 1 の
    /// 教訓3 が名指しした「誤診を招くエラー」そのものなので、**呼び出し側で
    /// 必ず弾いてから渡すこと**）。
    async fn list_entries_in_period(
        &mut self,
        from: AccountingDate,
        to: AccountingDate,
    ) -> Result<Vec<JournalEntry>, RepoError>;
}

/// 勘定科目表と取引先索引の読み込み。
///
/// どちらも「その時点のスナップショット」を返す読み取り専用の操作であり、
/// この trait には書き込みメソッドを定義しない。
///
/// 取引先マスタの編集は Phase 4 以降。**勘定科目マスタの投入は
/// [`ChartWriteRepo`] と [`crate::usecase::import_chart`] で行う**
/// （Phase 3 PR-E。`DECISIONS.md` D-070 / D-081）。読み書きを別 trait に
/// 分けたのは、読み取りしか要らないユースケース（記帳・試算表）が
/// 書き込み能力を持つ `Tx` を要求しないようにするため
/// （`TxOps` の束ねにも [`ChartWriteRepo`] を含めていない）。
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

/// 勘定科目マスタへの**追加**（新規科目の投入）。
///
/// # なぜ [`ChartRepo`] と分けるのか
///
/// `ChartRepo` は記帳・試算表の経路が使う読み取り専用のポートで、
/// [`TxOps`] の束ねに含まれている。ここに書き込みメソッドを足すと、
/// **記帳しかしないユースケースの `Tx` にも科目マスタを書き換える能力が
/// 付いて回る**。分けておけば、`where Tx: ChartWriteRepo` と書いた
/// ユースケース（現状 [`crate::usecase::import_chart`] だけ）以外は
/// マスタに触れないことが型で読み取れる。
///
/// # 実装の契約（★これを破ると会計上の実害が出る★）
///
/// - **既存行を `UPDATE` / `DELETE` してはならない。**
///   同じ科目コードが既に存在する場合は**何もしない**
///   （PostgreSQL 実装は `ON CONFLICT (code) DO NOTHING`）。
///   既に仕訳が参照している科目の名称・種別を投入経路が黙って書き換えると、
///   過去の仕訳の意味（試算表の符号・決算書の区分）が後から変わる
///   （`DECISIONS.md` D-081）。
/// - `accounts` は帳簿本体（`journal_entries` / `journal_lines`）とは違い
///   append-only ではなく、DB 権限としては `UPDATE` が許可されている
///   （`0002_accounts.sql`）。**それでもこのポートは追加しか行わない。**
///   科目の編集はユーザーの明示的な操作（Phase 4 以降）の領分である。
#[async_trait]
pub trait ChartWriteRepo: Send {
    /// まだ存在しない科目コードの定義を追加する。
    ///
    /// 実際に挿入された行数を返す（既存コードと重複した分は数えない）。
    /// 呼び出し側が事前に差分を取っていても、同時に起動した別プロセスが
    /// 先に入れている可能性があるため、戻り値は要求した件数と一致するとは
    /// 限らない。
    ///
    /// `defs` が空なら何もせず `Ok(0)` を返す。
    ///
    /// # Errors
    ///
    /// 挿入に失敗した場合は [`RepoError`]（親科目が存在しない場合など）。
    async fn insert_accounts(&mut self, defs: &[AccountDef]) -> Result<usize, RepoError>;
}

/// 取引先マスタへの**追加**（外部からの投入）。
///
/// # なぜ [`ChartRepo`] と分けるのか
///
/// [`ChartWriteRepo`] と同じ理由である。`ChartRepo::load_counterparties` は
/// 記帳の経路（消費税区分の検証）が使う読み取り専用のポートで、[`TxOps`] の
/// 束ねに含まれている。ここに書き込みを足すと、**記帳しかしないユースケースの
/// `Tx` にも取引先マスタを書き換える能力が付いて回る**。
///
/// # 実装の契約（★これを破ると会計上の実害が出る★）
///
/// - **既存行を `UPDATE` / `DELETE` してはならない。**
///   同じ取引先コードが既に存在する場合は**何もしない**
///   （PostgreSQL 実装は `ON CONFLICT (code) DO NOTHING`）。
/// - とくに `is_qualified`（適格請求書発行事業者か）を投入経路が黙って
///   書き換えてはならない。**この列は「ユーザーが確認した」という記録**で
///   あり、外部システムの値で上書きすると、確認していないものを確認済みに
///   見せることになる。`None`（未確認）と `Some(false)`（非適格と確認した）
///   の区別が消えるのは、`JpTaxPolicy` が `Some(false)` のときだけ記帳を
///   拒む設計（`QualifiedInvoiceUnverified`）を無意味にする。
/// - `counterparties` は帳簿本体と違い append-only ではなく、DB 権限としては
///   `UPDATE` が許可されている（`0005_counterparties.sql`）。
///   **それでもこのポートは追加しか行わない。** 編集はユーザーの明示的な
///   操作の領分である。
#[async_trait]
pub trait CounterpartyWriteRepo: Send {
    /// まだ存在しない取引先コードの定義を追加する。
    ///
    /// 実際に挿入された行数を返す（既存コードと重複した分は数えない）。
    /// `list` が空なら何もせず `Ok(0)` を返す。
    ///
    /// # Errors
    ///
    /// 挿入に失敗した場合は [`RepoError`]。
    async fn insert_counterparties(&mut self, list: &[Counterparty]) -> Result<usize, RepoError>;

    /// 既存の取引先の**適格請求書発行事業者の情報だけ**を更新する。
    ///
    /// # なぜ追加ではなく更新なのか
    ///
    /// `insert_counterparties` は `ON CONFLICT DO NOTHING` なので、既存の
    /// 取引先に登録番号を後から入れられない。**実帳簿の取引先31件はすべて
    /// 登録番号が空**で、CSV から入れ直そうとしても「既存を優先」で無視される
    /// （警告は出るが書き込まれない）。
    ///
    /// 相手が適格請求書発行事業者かどうかは**後から分かる**情報である。
    /// 取引を記帳した時点では未確認で、あとで先方に伺って埋める。
    /// 追加しかできないと、その運用が成り立たない。
    ///
    /// # 名前とコードは変えない
    ///
    /// 更新するのは `invoice_reg_no` / `is_qualified` / `verified_at` だけ。
    /// **名前を変えられると、過去の仕訳が指している相手が静かに別物になる。**
    /// 名前を直したいなら、それは別の操作として設計すること。
    ///
    /// 実際に更新された行数を返す（コードが存在しなければ 0）。
    ///
    /// # Errors
    ///
    /// 更新に失敗した場合は [`RepoError`]。
    async fn set_counterparty_invoice_status(
        &mut self,
        code: &str,
        registration_no: Option<&str>,
        is_qualified: Option<bool>,
        verified_on: AccountingDate,
    ) -> Result<usize, RepoError>;
}

/// 固定資産台帳の1件（`DECISIONS.md` D-103）。
///
/// **償却額の計算に要る入力だけを持つ。** 耐用年数も償却方法も人が決めて
/// 入れる値であり、このソフトは推定しない。
///
/// `kaikei-app` は `kaikei-jp` に依存できない（CI が禁じている）ので、
/// 償却方法は数値で持つ。`kaikei_jp::depreciation::DepreciationMethod` への
/// 翻訳は端（CLI / MCP）が行う。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixedAssetRow {
    /// 台帳のID。
    pub id: String,
    /// 決算書の「減価償却費の計算」欄に出す名前。
    pub name: String,
    /// 帳簿上どの科目に載っているか。
    pub account: AccountCode,
    /// 取得年月日。
    pub acquired_on: AccountingDate,
    /// 取得価額（常に正）。
    pub acquisition_cost: Money,
    /// 1=定額法 / 2=一括償却資産 / 3=少額減価償却資産。
    pub method: i16,
    /// 耐用年数。**定額法のときだけ意味がある。**
    pub useful_life_years: Option<i16>,
    /// 事業専用割合（表示用の10進文字列）。`None` は100%。
    ///
    /// 文字列で持つのは、`kaikei-app` が `Decimal` を公開の型に出さない
    /// ためである（`Ratio` の構築は端が行う）。
    pub business_ratio: Option<String>,
    /// 除却・売却した日。
    pub disposed_on: Option<AccountingDate>,
    /// 備考。
    pub note: Option<String>,
}

/// 固定資産台帳の読み書き。
///
/// # なぜ [`ChartRepo`] と分けるのか
///
/// [`ChartWriteRepo`] / [`CounterpartyWriteRepo`] と同じ理由。記帳しかしない
/// ユースケースの `Tx` に台帳を書き換える能力を付けて回らない。
///
/// # 実装の契約
///
/// - **DELETE を実装しない。** 資産を帳簿から外すのは除却（`disposed_on` を
///   埋める）であって、台帳から消すことではない。消せると、過去の年度の
///   償却費がどの資産のものだったか辿れなくなる
/// - `UPDATE` は許す（耐用年数の見直し・事業専用割合の変更が起きる）
#[async_trait]
pub trait FixedAssetRepo: Send {
    /// 台帳を全件読む（`acquired_on`、同日なら `id` の昇順）。
    async fn list_fixed_assets(&mut self) -> Result<Vec<FixedAssetRow>, RepoError>;

    /// 台帳に追加する。
    ///
    /// 実際に挿入された行数を返す。`list` が空なら `Ok(0)`。
    ///
    /// # Errors
    ///
    /// 科目が存在しない・制約に反する場合は [`RepoError`]。
    async fn insert_fixed_assets(&mut self, list: &[FixedAssetRow]) -> Result<usize, RepoError>;

    /// 資産を除却する（`disposed_on` を埋める）。
    ///
    /// **これが「台帳から資産を外す」唯一の手段である。** 行は消さない
    /// （`DECISIONS.md` D-104）。過去の年度の償却費がどの資産のものだったか
    /// 辿れなくなるため。
    ///
    /// 既に除却済みの資産は**上書きしない**。除却日を後から動かすのは、
    /// 過去の決算書の数字が変わるということである。
    ///
    /// 戻り値は実際に更新した行数（0 なら該当なし、または除却済み）。
    ///
    /// # Errors
    ///
    /// 除却日が取得日より前の場合など、制約に反すれば [`RepoError`]。
    async fn dispose_fixed_asset(
        &mut self,
        id: &str,
        disposed_on: AccountingDate,
    ) -> Result<usize, RepoError>;
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

/// 登録する証憑（`docs/06-documents.md` §3）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewDocument {
    /// 証憑ID（呼び出し側が採番する）。
    pub id: String,
    /// 内容の SHA-256（16進64文字・小文字）。
    pub blob_hash: String,
    /// 元のファイル名。
    pub original_name: String,
    /// MIME タイプ。
    pub mime_type: String,
    /// バイト数。
    pub byte_size: i64,
    /// 取引年月日。
    pub doc_date: AccountingDate,
    /// 取引金額。金額の無い証憑（契約書など）は `None`。
    pub amount_minor: Option<i64>,
    /// 取引先。
    pub counterparty: Option<String>,
    /// 種別（invoice / receipt / contract / other）。
    pub doc_type: String,
    /// 授受の経路（email / download / scan / manual）。
    pub received_via: String,
    /// 授受した日時。
    pub received_at: Timestamp,
    /// 備考。
    pub note: Option<String>,
}

/// 証憑の登録（Phase 4）。
///
/// **`Tx` を通す。** 仕訳と証憑の紐付けが半分だけ残ると、帳簿から証憑への
/// 道筋が壊れる。登録と紐付けは同じトランザクションで行う。
#[async_trait]
pub trait DocumentRepo: Send {
    /// 証憑を登録する。
    ///
    /// # Errors
    ///
    /// 同じIDが既にある場合は [`RepoError::Conflict`]、それ以外の失敗は
    /// [`RepoError`]。
    async fn insert_document(&mut self, document: &NewDocument) -> Result<(), RepoError>;

    /// 仕訳と証憑を紐付ける。
    ///
    /// **同じ組み合わせを2回登録しても失敗しない**（何度取り込んでも同じ結果に
    /// なるようにする）。
    async fn link_document(
        &mut self,
        entry_id: EntryId,
        document_id: &str,
    ) -> Result<(), RepoError>;
}

/// 証憑の read model クエリ（Phase 4。`docs/06-documents.md` §4）。
///
/// [`TrialBalanceQuery`] と同じく `Tx` を通さない
/// （`CLAUDE.md` §6「read model は物理的に分離する」）。
///
/// 書き込み側（証憑の登録）を分けているのは、**登録は帳簿と同じ
/// トランザクションで行う**ためである（仕訳と証憑の紐付けが半分だけ残ると、
/// 帳簿から証憑への道筋が壊れる）。
#[async_trait]
pub trait DocumentQueryPort: Send + Sync {
    /// 条件に一致する証憑を返す。
    ///
    /// 並びは取引年月日の降順、同じ日ならIDの昇順（決定的にする）。
    ///
    /// # Errors
    ///
    /// 問い合わせに失敗した場合、または保存されている値を復元できない場合は
    /// [`RepoError`]。
    async fn search_documents(
        &self,
        query: &crate::view::DocumentQuery,
        limit: u32,
    ) -> Result<Vec<crate::view::DocumentView>, RepoError>;

    /// 1つの仕訳に紐付いた証憑を返す。
    ///
    /// **帳簿から証憑へ辿れること**が電子帳簿保存法の相互関連性の要件である。
    async fn documents_of_entry(
        &self,
        entry_id: EntryId,
    ) -> Result<Vec<crate::view::DocumentView>, RepoError>;

    /// 帳簿に登録されている証憑の内容ハッシュを重複なく返す。
    ///
    /// 整合性検査（`docs/06-documents.md` §6）で、保存されているファイルの
    /// 中身が変わっていないかを確かめるために使う。**メタデータは返さない**
    /// ——検査に要るのはハッシュだけで、件数が多くても軽く済ませたい。
    async fn all_blob_hashes(&self) -> Result<Vec<String>, RepoError>;

    /// 帳簿に登録されている証憑の**件数**。
    ///
    /// **[`Self::all_blob_hashes`] の長さで代用しない。** あちらは
    /// `DISTINCT blob_hash` なので、**同じ内容の証憑が別の取引に付いていると
    /// 少なく出る**（内容は SHA-256 で1つに束ねて保存するので、同じ請求書を
    /// 2つの仕訳に紐付けると起きる）。
    ///
    /// 実際に 4 件登録されている帳簿で 3 と出た。件数を添えるのは「1件も
    /// 登録されていない」と「条件に合わなかった」を区別するためなので、
    /// **数が違うと区別そのものが狂う。**
    async fn count_documents(&self) -> Result<usize, RepoError>;

    /// 期間内の仕訳のうち、証憑が1件以上紐付いているものの数を返す。
    ///
    /// # なぜ数だけ返すのか
    ///
    /// **登録が進んでいるかを見るための数字である。** どの仕訳に付いているかは
    /// ここでは要らないし、件数が多くても軽く済ませたい
    /// （[`Self::all_blob_hashes`] と同じ考え方）。
    ///
    /// # なぜ要るのか
    ///
    /// 証憑が1件も登録されていないことは、帳簿を見ても分からない。**数字が
    /// 見えないと、登録が進んでいるかどうかも分からない。**
    ///
    /// # Errors
    ///
    /// 問い合わせに失敗した場合は [`RepoError`]。
    async fn entries_with_documents(
        &self,
        from: AccountingDate,
        to: AccountingDate,
    ) -> Result<u64, RepoError>;
}

/// 取り込んだ明細の向き（`docs/05-csv-import.md` §2）。
///
/// **借方/貸方ではない。** 口座から見た入金か出金かでしかなく、どちらの側に
/// どの科目が立つかは仕訳化のときに決まる（入金が必ず貸方とは限らない
/// ——返金の入金は費用の取消であり、貸方に費用が立つ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportDirection {
    /// 口座への入金。
    In,
    /// 口座からの出金。
    Out,
}

/// 取り込む明細（`docs/05-csv-import.md` §3）。
///
/// # なぜ `kaikei_import` の型をそのまま使わないか
///
/// `kaikei-app` は `kaikei-import` に依存しない（`ARCHITECTURE.md` §3。CI が
/// 検査する）。取込は帳簿とは別の文脈であり、繋ぐと「入金/出金」と
/// 「借方/貸方」が混ざる。**CSV の型からこの型への翻訳は、両方を知ってよい
/// 端（CLI / MCP）が行う。**
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewImportedTransaction {
    /// 明細ID（呼び出し側が採番する）。
    pub id: String,
    /// どの口座・カードから取り込んだか。
    pub source: String,
    /// 同じ明細を二度取り込まないための鍵（`docs/05-csv-import.md` §4）。
    pub external_key: String,
    /// 取引年月日。
    pub occurred_on: AccountingDate,
    /// 金額。**常に正**——向きは [`Self::direction`] が表す。
    pub amount_minor: i64,
    /// 入金か出金か。
    pub direction: ImportDirection,
    /// CSV の摘要（正規化後）。
    pub raw_description: String,
    /// 取引後残高。CSV に残高列が無ければ `None`。
    pub balance_after: Option<i64>,
    /// 元の CSV 行（JSON）。**捨てない**——解釈を間違えたと後で分かったとき、
    /// 元が無ければ直せない。
    pub raw_row: String,
    /// 取り込んだ日時。
    pub imported_at: Timestamp,
}

/// 明細を取り込んだ結果（`docs/05-csv-import.md` §4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportOutcome {
    /// 新しく取り込んだ。
    Inserted,
    /// 同じ明細が既にあったので何もしなかった。
    SkippedDuplicate,
}

/// 取り込んだ明細の登録と状態遷移（Phase 4。`docs/05-csv-import.md` §3・§6）。
///
/// # このテーブルだけ append-only ではない
///
/// 帳簿・監査ログ・証憑は追記のみだが、取込明細は「未処理 → 仕訳済み」と
/// 状態が変わる。分けることで両立する——取込を追記のみにすると状態が変わる
/// たび行が増えて「今どうなっているか」が読めなくなり、逆に帳簿を更新可に
/// すれば訂正の履歴が消える。
///
/// **`Tx` を通す。** 仕訳を作って明細を仕訳済みにするまでが一続きでないと、
/// 帳簿に仕訳だけが残って明細が未処理のまま——つまり二重計上の入口——になる。
#[async_trait]
pub trait ImportedTxRepo: Send {
    /// 明細を1件取り込む。
    ///
    /// **同じ `(source, external_key)` が既にあれば
    /// [`ImportOutcome::SkippedDuplicate`] を返し、エラーにしない。**
    /// 同じ CSV を2回流しても同じ結果になることが取込の要件である
    /// （`docs/05-csv-import.md` §4）。既存の行は書き換えない——取り込んだ後に
    /// 仕訳済みにした明細を、再取込が未処理へ巻き戻すと二重計上になる。
    ///
    /// # Errors
    ///
    /// 保存に失敗した場合は [`RepoError`]。
    async fn insert_imported(
        &mut self,
        imported: &NewImportedTransaction,
    ) -> Result<ImportOutcome, RepoError>;

    /// 明細を仕訳済みにする。
    ///
    /// **未処理の明細だけを移せる。** 既に仕訳済みの行を別の仕訳で塗り替える
    /// と、先に作った仕訳が帳簿に残ったまま誰からも指されなくなる（帳簿は
    /// 追記のみなので消せない）。取消は逆仕訳を起こしてから
    /// [`Self::revert_to_pending`] を使う。
    ///
    /// # Errors
    ///
    /// 明細が無い、または未処理でない場合は [`RepoError::NotFound`]。
    async fn mark_journalized(
        &mut self,
        imported_id: &str,
        entry_id: EntryId,
    ) -> Result<(), RepoError>;

    /// 明細を「仕訳しない」ものとして片付ける。
    ///
    /// 理由は必ず要る——理由の無い無視は、取りこぼしと見分けが付かない。
    ///
    /// # Errors
    ///
    /// 明細が無い、または未処理でない場合は [`RepoError::NotFound`]。
    async fn mark_ignored(&mut self, imported_id: &str, reason: &str) -> Result<(), RepoError>;

    /// 仕訳済みの明細を未処理へ戻す（`docs/05-csv-import.md` §6「状態遷移」）。
    ///
    /// **帳簿側の取消は呼び出し側の責任である。** この関数は明細の状態しか
    /// 戻さない。逆仕訳を起こさずにこれを呼ぶと、帳簿に仕訳が残ったまま明細が
    /// 未処理に戻り、もう一度仕訳化すると二重計上になる。
    ///
    /// # Errors
    ///
    /// 明細が無い、または仕訳済みでない場合は [`RepoError::NotFound`]。
    async fn revert_to_pending(&mut self, imported_id: &str) -> Result<(), RepoError>;
}

/// 取り込んだ明細の read model クエリ（Phase 4。`docs/05-csv-import.md` §3）。
///
/// [`ImportedTxRepo`] と分けているのは `CLAUDE.md` §6「read model は物理的に
/// 分離する」による。書き込みは `Tx` を通すが、こちらは通さない。
#[async_trait]
pub trait ImportedTxQuery: Send + Sync {
    /// 条件に一致する明細を返す。
    ///
    /// 並びは**取引年月日の昇順**、同じ日ならIDの昇順（決定的にする）。
    /// 証憑の検索が降順なのと逆なのは、用途が違うためである——未処理の明細は
    /// 古いものから順に片付けるものであり、新しい方から見せても手が付かない。
    ///
    /// # Errors
    ///
    /// 問い合わせに失敗した場合、または保存されている値を復元できない場合は
    /// [`RepoError`]。
    async fn list_imported(
        &self,
        query: &crate::view::ImportedTxQuerySpec,
        limit: u32,
    ) -> Result<Vec<crate::view::ImportedTxView>, RepoError>;

    /// 明細を1件、IDで引く。
    ///
    /// # なぜ一覧と別に用意するか
    ///
    /// [`Self::list_imported`] で引いてから絞ると、**上限を超えた分の明細が
    /// 「見つかりません」になる**。IDを持っているのに引けないのは、
    /// 帳簿が育つほど起きやすくなる種類の失敗である。
    ///
    /// # Errors
    ///
    /// 問い合わせに失敗した場合、または保存されている値を復元できない場合は
    /// [`RepoError`]。**見つからないことはエラーにしない**（`None` を返す）
    /// ——IDの打ち間違いは呼び出し側が文脈に応じて説明する。
    async fn find_imported(
        &self,
        imported_id: &str,
    ) -> Result<Option<crate::view::ImportedTxView>, RepoError>;

    /// 状態ごとの件数を返す。
    ///
    /// **一覧が空でも、取り込み済みかどうかが分かるようにする**
    /// （[`crate::view::ImportStatusCounts`] を参照）。
    ///
    /// # Errors
    ///
    /// 問い合わせに失敗した場合は [`RepoError`]。
    async fn import_status_counts(
        &self,
        source: Option<&str>,
    ) -> Result<crate::view::ImportStatusCounts, RepoError>;
}

/// 仕訳検索の read model クエリ（Phase 3 PR-H）。
///
/// [`TrialBalanceQuery`] と同じく `Tx` を通さない
/// （`CLAUDE.md` §6「read model は物理的に分離する」）。
///
/// **入力の妥当性検証はユースケース側の責務である**
/// （[`crate::usecase::search_entries`]）。この trait の実装は SQL に徹する:
/// `from > to` の拒否、`tags` のキーが `TagSchema::is_aggregatable` を
/// 満たすかどうか、`limit` の上限は SQL に到達する前に済んでいる。
#[async_trait]
pub trait SearchEntriesQuery: Send + Sync {
    /// 条件に一致する仕訳を1ページ分返す。
    ///
    /// # Errors
    ///
    /// 問い合わせに失敗した場合、または保存されている値を復元できない場合は
    /// [`RepoError`]。
    async fn search_entries(
        &self,
        params: &SearchEntriesParams,
    ) -> Result<EntrySearchPageView, RepoError>;
}

/// [`SearchEntriesQuery::search_entries`] の絞り込み条件。
///
/// フィールドを増やす形にしているのは、条件が7つあり位置引数では
/// 呼び出し側が読めなくなるためである（[`TrialBalanceQuery`] は3つなので
/// 位置引数のままにしてある）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchEntriesParams {
    /// 取引日の下限（両端を含む）。`None` なら下限なし。
    pub from: Option<AccountingDate>,
    /// 取引日の上限（両端を含む）。`None` なら上限なし。
    pub to: Option<AccountingDate>,
    /// この科目の明細を含む仕訳だけに絞る。
    pub account: Option<AccountCode>,
    /// 摘要にこの文字列を含む仕訳だけに絞る（部分一致・英字は大小を無視）。
    pub description_contains: Option<String>,
    /// 明細1行の金額がこの額**以上**である仕訳だけに絞る。
    pub min_amount: Option<Money>,
    /// 明細1行の金額がこの額**以下**である仕訳だけに絞る。
    pub max_amount: Option<Money>,
    /// タグの絞り込み（キーと**正準化済みの値文字列**の組）。
    ///
    /// 複数指定した場合は**すべてを満たす**仕訳だけが残る。判定は仕訳単位で
    /// あり、「キーごとにいずれかの明細が一致すればよい」（同じ1行が
    /// 全部のタグを持つ必要はない）。
    pub tags: Vec<(TagKey, String)>,
    /// 続きから読む場合の開始位置。`None` なら先頭から。
    pub cursor: Option<EntryCursor>,
    /// 1ページの上限件数。
    pub limit: u32,
}

/// 総勘定元帳の read model クエリ（Phase 3 PR-H）。
#[async_trait]
pub trait LedgerQuery: Send + Sync {
    /// 指定科目の元帳を1ページ分返す。
    ///
    /// # Errors
    ///
    /// - 指定した科目コードが `accounts` に無い場合は [`RepoError::NotFound`]
    ///   （**空の元帳を返さない**。科目コードの打ち間違いと「その期間に
    ///   取引が無い」は呼び出し元が取るべき次の手が違う）
    /// - 集計対象に複数の通貨が混在する場合は [`RepoError::Unsupported`]
    ///   （`DECISIONS.md` D-042。試算表の read model と同じ粒度）
    /// - そのほか問い合わせ・復元に失敗した場合は [`RepoError`]
    async fn ledger(&self, params: &LedgerParams) -> Result<LedgerPageView, RepoError>;
}

/// [`LedgerQuery::ledger`] の条件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerParams {
    /// 対象の勘定科目コード。
    pub account: AccountCode,
    /// 集計期間の開始日（取引日、両端を含む）。
    pub from: AccountingDate,
    /// 集計期間の終了日（取引日、両端を含む）。
    pub to: AccountingDate,
    /// 帳簿通貨（[`crate::context::BookSettings::book_currency`]）。
    ///
    /// **明細が1行も無い元帳でも通貨を名乗れるようにするために渡す。**
    /// `Money` は通貨なしでは構築できないので、0円の期首残高すら
    /// 通貨を決めなければ返せない（[`crate::view::TrialBalanceView::new`]
    /// が帳簿通貨を必須の引数として受け取るのと同じ理由。`DECISIONS.md` D-074）。
    /// 実装はこの値を**ゼロ値の通貨としてのみ**使い、行があるときは
    /// 保存されている通貨を使う（食い違いはユースケース側が検出する）。
    pub book_currency: Currency,
    /// 続きから読む場合の開始位置。`None` なら期間の先頭から。
    pub cursor: Option<LedgerCursor>,
    /// 1ページの上限行数。
    pub limit: u32,
}

/// 監査ログ（`audit_log`）の記録先。
///
/// # ★ [`TxOps`] に生やしてはならない★
///
/// リポジトリはすべて `&mut Tx` 経由（[`TxOps`]）で、[`crate::tx::with_tx`] が
/// commit/rollback を握っている。**監査ログを `TxOps` のメソッドにすると
/// 必ず帳簿と同一トランザクションになり、失敗した操作の記録が rollback で
/// 一緒に消える**（`DECISIONS.md` D-070）。「AI が何をしようとしたか」を
/// 最も知りたいのは失敗したときであり、その記録だけが残らない。
/// PostgreSQL に autonomous transaction は無いので、経路を分ける以外に
/// 手段が無い。
///
/// したがってこの trait は [`Store`] とも [`TxScope`] とも無関係で、
/// メソッドは `&self` を取る。実装（`kaikei-store::PgAuditSink`）は
/// **同じ `PgPool` から別の接続を acquire する**こと。接続プールの枯渇に
/// 注意（`connect_app` の `max_connections` は 10）。
///
/// # 呼ぶ場所
///
/// [`crate::tx::with_tx`] の**外側**（開始レコード → `with_tx(..)` →
/// 結果レコード）。手順そのものは [`crate::audit::with_audit`] に閉じて
/// あるので、通常はそちらを呼ぶ（fail-closed / fail-open の規律を
/// ツールごとに手で書かないため。`DECISIONS.md` D-076）。
#[async_trait]
pub trait AuditSink: Send + Sync {
    /// 開始レコード（`status='started'`）を書く。
    ///
    /// # Errors
    ///
    /// 記録に失敗したら [`RepoError`]。**呼び出し側は操作を実行しては
    /// ならない**（fail-closed。まだ何も起きていないので拒否して安全）。
    async fn record_start(&self, record: &AuditStart<'_>) -> Result<(), RepoError>;

    /// 結果レコード（`status='ok' | 'error'`）を書く。
    ///
    /// # Errors
    ///
    /// 記録に失敗したら [`RepoError`]。**呼び出し側は操作を成功として
    /// 返す**（fail-open。操作は既に確定しており、拒否しても取り消せない）。
    /// 開始レコードだけが残った行は「結果不明」として読む。
    async fn record_result(&self, record: &AuditResult<'_>) -> Result<(), RepoError>;
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

        async fn list_entries_in_period(
            &mut self,
            _from: AccountingDate,
            _to: AccountingDate,
        ) -> Result<Vec<JournalEntry>, RepoError> {
            Ok(Vec::new())
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
    impl ChartWriteRepo for NoopTx {
        async fn insert_accounts(&mut self, _defs: &[AccountDef]) -> Result<usize, RepoError> {
            Ok(0)
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
    fn _dyn_chart_write_repo(_: &mut dyn ChartWriteRepo) {}
    fn _dyn_period_repo(_: &mut dyn PeriodRepo) {}
    fn _dyn_numbering_repo(_: &mut dyn NumberingRepo) {}
    fn _dyn_tx_ops(_: &mut dyn TxOps) {}

    #[test]
    fn journal_chart_period_numbering_repos_and_tx_ops_are_dyn_compatible() {
        let mut tx = NoopTx;
        _dyn_journal_repo(&mut tx);
        _dyn_chart_repo(&mut tx);
        _dyn_chart_write_repo(&mut tx);
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

    /// `AuditSink` は `&self` のメソッドのみを持つため `Arc<dyn AuditSink>` に
    /// できる（`Store`/`TxScope` と無関係であることの静的な裏付けでもある）。
    #[test]
    fn audit_sink_can_be_used_as_arc_dyn() {
        let sink: Arc<dyn AuditSink> = Arc::new(crate::testing::RecordingAuditSink::new());
        let _ = sink;
    }
}
