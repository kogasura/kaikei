# 01 — kaikei-core 型定義仕様（Phase 0 実装対象）

この文書と `02-test-cases.md` だけで `kaikei-core` を実装できる粒度で書いてある。
**依存に追加してよいのは `rust_decimal` と `thiserror` のみ。**

---

## money.rs

```rust
/// 通貨。ISO 4217 コードと小数桁数
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Currency {
    code: [u8; 3],      // b"JPY"
    minor_unit: u8,     // JPY=0, USD=2, KWD=3
}

impl Currency {
    pub const JPY: Currency = Currency { code: *b"JPY", minor_unit: 0 };
    pub const USD: Currency = Currency { code: *b"USD", minor_unit: 2 };

    pub fn new(code: &str, minor_unit: u8) -> Result<Self, CoreError>;
    pub fn code(&self) -> &str;
    pub fn minor_unit(&self) -> u8;
}

/// 金額。最小通貨単位の整数で保持する。f64 は使わない
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Money {
    minor: i128,
    currency: Currency,
}

impl Money {
    pub fn from_minor(minor: i128, currency: Currency) -> Self;

    /// "1234.56" のような文字列から。桁数が minor_unit を超えたらエラー
    pub fn parse(s: &str, currency: Currency) -> Result<Self, CoreError>;

    pub fn zero(currency: Currency) -> Self;
    pub fn minor(&self) -> i128;
    pub fn currency(&self) -> Currency;
    pub fn is_zero(&self) -> bool;
    pub fn is_negative(&self) -> bool;

    /// 異通貨は CoreError::CurrencyMismatch
    pub fn add(&self, other: &Money) -> Result<Money, CoreError>;
    pub fn sub(&self, other: &Money) -> Result<Money, CoreError>;
    pub fn neg(&self) -> Money;
    pub fn abs(&self) -> Money;

    /// 按分等に使用。丸めは呼び出し側の責務（round_mode を明示的に渡す）
    pub fn mul_ratio(&self, ratio: Ratio, mode: RoundMode) -> Money;

    /// 表示用。"1,234.56" 形式
    pub fn to_display_string(&self) -> String;
}

/// std::ops::Add は実装しない（異通貨をパニックさせたくないため）
/// Iterator の合計は専用関数を使う
pub fn sum_money<'a>(items: impl Iterator<Item = &'a Money>)
    -> Result<Option<Money>, CoreError>;

/// 比率。按分率・税率に使用
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ratio(rust_decimal::Decimal);

impl Ratio {
    /// 0 以上 1 以下でなければエラー（按分率用）
    pub fn parse_fraction(s: &str) -> Result<Self, CoreError>;
    /// 0 以上（税率は 1 を超えないが制約は緩める）
    pub fn parse_rate(s: &str) -> Result<Self, CoreError>;
    pub fn as_decimal(&self) -> rust_decimal::Decimal;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundMode {
    Floor,      // 切捨
    Ceil,       // 切上
    HalfUp,     // 四捨五入
}
```

### 実装上の注意

- `Money` に `std::ops::Add` を実装しない。異通貨をパニックにしたくない
- `PartialOrd` は同一通貨でのみ意味を持つ。`Ord` は実装せず、比較は専用メソッドにする
- `to_display_string` は `minor_unit` に従って小数点を入れる（JPY は小数なし）

---

## account.rs

```rust
/// 勘定科目コード。core は中身の意味を知らない
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AccountCode(String);

impl AccountCode {
    /// 英数字とハイフン、1〜32文字
    pub fn parse(s: &str) -> Result<Self, CoreError>;
    pub fn as_str(&self) -> &str;
}

/// 5要素。世界共通なので core に置く
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountType {
    Asset,
    Liability,
    Equity,
    Revenue,
    Expense,
}

impl AccountType {
    /// 残高計算の向き。借方残なら true
    /// Asset, Expense => true / Liability, Equity, Revenue => false
    pub fn is_debit_normal(&self) -> bool;
}

#[derive(Debug, Clone)]
pub struct AccountDef {
    pub code: AccountCode,
    pub name: String,            // 表示名。core は意味を持たせない
    pub account_type: AccountType,
    pub parent: Option<AccountCode>,
    pub postable: bool,          // false なら集計専用（見出し科目）
}

#[derive(Debug, Clone)]
pub struct ChartOfAccounts {
    accounts: BTreeMap<AccountCode, AccountDef>,
}

impl ChartOfAccounts {
    /// 親の存在チェック、循環参照チェックを行う
    pub fn new(defs: Vec<AccountDef>) -> Result<Self, CoreError>;
    pub fn get(&self, code: &AccountCode) -> Option<&AccountDef>;
    pub fn iter(&self) -> impl Iterator<Item = &AccountDef>;
    /// 指定科目とその子孫すべて
    pub fn descendants(&self, code: &AccountCode) -> Vec<&AccountDef>;
}
```

---

## tag.rs

**設計の急所。** 消費税区分などを core に知らせずに運ぶための仕組み。

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TagKey(String);

impl TagKey {
    /// snake_case、1〜64文字
    pub fn parse(s: &str) -> Result<Self, CoreError>;
    pub fn as_str(&self) -> &str;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagValue {
    Code(String),
    Text(String),
    Decimal(rust_decimal::Decimal),
    Date(AccountingDate),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagValueType { Code, Text, Decimal, Date }

/// 仕訳明細に付く分類情報の袋。core は意味を解釈しない
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TagSet(BTreeMap<TagKey, TagValue>);

impl TagSet {
    pub fn new() -> Self;
    pub fn insert(&mut self, key: TagKey, value: TagValue) -> Option<TagValue>;
    pub fn get(&self, key: &TagKey) -> Option<&TagValue>;
    pub fn iter(&self) -> impl Iterator<Item = (&TagKey, &TagValue)>;
    pub fn is_empty(&self) -> bool;
}

/// タグのスキーマ。kaikei-jp が提供し、core が検証に使う
#[derive(Debug, Clone)]
pub struct TagSchema {
    defs: BTreeMap<TagKey, TagDef>,
}

#[derive(Debug, Clone)]
pub struct TagDef {
    pub value_type: TagValueType,
    /// 集計軸（group_by）として使えるか
    pub aggregatable: bool,
    /// この科目種別の明細では必須
    pub required_for: Vec<AccountType>,
}

impl TagSchema {
    pub fn new(defs: Vec<(TagKey, TagDef)>) -> Self;
    pub fn empty() -> Self;

    /// 未登録キー / 型不一致 / 必須欠落 を検出
    pub fn validate(&self, tags: &TagSet, account_type: AccountType)
        -> Result<(), CoreError>;

    pub fn is_aggregatable(&self, key: &TagKey) -> bool;
}
```

### 禁止事項

- **金額に影響する情報をタグに入れない。** 貸借一致の検証を迂回できてしまう
- 新しいキーは必ず `kaikei-jp-data/tags.yaml` に登録してから使う

---

## period.rs

```rust
/// 取引日。タイムゾーンを持たない純粋な日付
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct AccountingDate {
    year: i32,
    month: u8,
    day: u8,
}

impl AccountingDate {
    pub fn new(year: i32, month: u8, day: u8) -> Result<Self, CoreError>;
    /// "2026-04-15"
    pub fn parse(s: &str) -> Result<Self, CoreError>;
    pub fn year(&self) -> i32;
    pub fn month(&self) -> u8;
    pub fn day(&self) -> u8;
    pub fn to_iso_string(&self) -> String;
}

/// 会計年度。個人事業主は暦年だが、汎用形にしておく
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FiscalYear {
    label: i32,                 // 2026
    start: AccountingDate,
    end: AccountingDate,
}

impl FiscalYear {
    pub fn new(label: i32, start: AccountingDate, end: AccountingDate)
        -> Result<Self, CoreError>;
    /// 暦年（1/1〜12/31）。個人事業主用のショートカット
    pub fn calendar_year(year: i32) -> Self;
    pub fn contains(&self, date: AccountingDate) -> bool;
    pub fn label(&self) -> i32;
    pub fn start(&self) -> AccountingDate;
    pub fn end(&self) -> AccountingDate;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeriodStatus { Open, Closed }

/// 締め状態の判定に使う。実データは store が持つ
pub trait PeriodGuard {
    fn status(&self, date: AccountingDate) -> PeriodStatus;
}
```

---

## clock.rs

```rust
/// 記帳時刻の取得。core / policy 内で Utc::now() を直接呼ばない
pub trait Clock {
    fn now(&self) -> Timestamp;
}

/// UTCのUnix時刻（ナノ秒）。chrono を core に入れないため自前
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(i128);

impl Timestamp {
    pub fn from_unix_nanos(nanos: i128) -> Self;
    pub fn as_unix_nanos(&self) -> i128;
}

/// テスト用
pub struct FixedClock(pub Timestamp);
impl Clock for FixedClock { /* ... */ }
```

---

## journal.rs（集約）

**このモジュールが最重要。private フィールドを触るコードはここに閉じる。**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntryId(u128);   // UUID v7 相当の値。生成は外部から渡す

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct EntryNumber(u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side { Debit, Credit }

#[derive(Debug, Clone)]
pub struct JournalLine {
    account: AccountCode,
    side: Side,
    amount: Money,
    tags: TagSet,
    memo: Option<String>,
}

impl JournalLine {
    /// amount が負またはゼロならエラー（符号は side で表現する）
    pub fn new(
        account: AccountCode,
        side: Side,
        amount: Money,
        tags: TagSet,
        memo: Option<String>,
    ) -> Result<Self, CoreError>;

    pub fn account(&self) -> &AccountCode;
    pub fn side(&self) -> Side;
    pub fn amount(&self) -> &Money;
    pub fn tags(&self) -> &TagSet;
    pub fn memo(&self) -> Option<&str>;
    pub fn is_debit(&self) -> bool;
}

#[derive(Debug, Clone)]
pub struct DocumentRef {
    pub document_id: u128,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct JournalEntry {
    id: EntryId,
    fiscal_year: i32,
    entry_no: EntryNumber,
    entry_date: AccountingDate,
    description: String,
    lines: Vec<JournalLine>,
    document_refs: Vec<DocumentRef>,
    reverses: Option<EntryId>,
    reverse_reason: Option<String>,
    recorded_at: Timestamp,
}

/// new に渡す引数。引数が多いので構造体にする
pub struct NewEntry {
    pub id: EntryId,
    pub entry_no: EntryNumber,
    pub entry_date: AccountingDate,
    pub description: String,
    pub lines: Vec<JournalLine>,
    pub document_refs: Vec<DocumentRef>,
}

impl JournalEntry {
    /// 唯一の生成経路。以下を全て検証する
    ///   1. lines が 2 行以上
    ///   2. 全 account が chart に存在し postable == true
    ///   3. 全 line の通貨が同一
    ///   4. 借方合計 == 貸方合計
    ///   5. tags が schema に適合
    ///   6. entry_date が fy の範囲内
    ///   7. period が Open
    ///   8. description が空でない
    pub fn new(
        input: NewEntry,
        fy: &FiscalYear,
        chart: &ChartOfAccounts,
        schema: &TagSchema,
        guard: &dyn PeriodGuard,
        clock: &dyn Clock,
    ) -> Result<Self, CoreError>;

    /// 永続化層からの復元専用。検証を行わない
    /// pub(crate) にせず pub にするが、doc に「store 層のみが使う」と明記
    pub fn rehydrate(/* 全フィールド */) -> Self;

    /// 訂正は逆仕訳のみ。update / delete は存在しない
    pub fn reverse(
        &self,
        id: EntryId,
        entry_no: EntryNumber,
        date: AccountingDate,
        reason: String,
        fy: &FiscalYear,
        chart: &ChartOfAccounts,
        schema: &TagSchema,
        guard: &dyn PeriodGuard,
        clock: &dyn Clock,
    ) -> Result<Self, CoreError>;

    // getter のみ。setter は作らない
    pub fn id(&self) -> EntryId;
    pub fn entry_no(&self) -> EntryNumber;
    pub fn entry_date(&self) -> AccountingDate;
    pub fn description(&self) -> &str;
    pub fn lines(&self) -> &[JournalLine];
    pub fn document_refs(&self) -> &[DocumentRef];
    pub fn reverses(&self) -> Option<EntryId>;
    pub fn is_reversal(&self) -> bool;
    pub fn recorded_at(&self) -> Timestamp;

    pub fn debit_total(&self) -> Money;
    pub fn credit_total(&self) -> Money;
    pub fn currency(&self) -> Currency;
}
```

### reverse の仕様

- 全明細の `side` を反転（Debit ⇄ Credit）
- `amount`、`account`、`tags` はそのまま複製
- `description` は `format!("【訂正】{}", original.description)`
- `reverses` に元の `id`、`reverse_reason` に理由
- 逆仕訳の逆仕訳は許可する（が `reverses` は直前の仕訳を指す）
- 元仕訳が別年度でも、逆仕訳は指定された日付の年度に属する

---

## trial_balance.rs（read model）

```rust
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct GroupKey(Vec<(TagKey, String)>);   // group_by の結果キー。空なら全体

#[derive(Debug, Clone)]
pub struct BalanceRow {
    pub account: AccountCode,
    pub account_type: AccountType,
    pub group: GroupKey,
    pub debit_total: Money,
    pub credit_total: Money,
    /// account_type.is_debit_normal() に従った符号付き残高
    pub balance: Money,
}

#[derive(Debug, Clone)]
pub struct TrialBalance {
    rows: Vec<BalanceRow>,
    currency: Currency,
}

impl TrialBalance {
    /// group_by が空なら科目のみで集計。
    /// schema.is_aggregatable() が false のキーが渡されたらエラー
    pub fn from_entries<'a>(
        entries: impl Iterator<Item = &'a JournalEntry>,
        chart: &ChartOfAccounts,
        schema: &TagSchema,
        group_by: &[TagKey],
    ) -> Result<Self, CoreError>;

    pub fn rows(&self) -> &[BalanceRow];
    pub fn balance_of(&self, account: &AccountCode) -> Option<Money>;

    /// 借方合計と貸方合計。必ず一致する（不一致ならバグ）
    pub fn totals(&self) -> (Money, Money);

    /// 検算。totals が一致しなければ false
    pub fn is_balanced(&self) -> bool;

    /// 科目種別ごとの合計
    pub fn total_by_type(&self, t: AccountType) -> Money;
}
```

### 残高計算のルール（DOMAIN.md §2 と一致させる）

```
is_debit_normal() == true  (Asset, Expense)          => balance = debit - credit
is_debit_normal() == false (Liability, Equity, Revenue) => balance = credit - debit
```

---

## error.rs

```rust
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("貸借不一致: 借方 {debit} / 貸方 {credit}（差額 {diff}）")]
    Unbalanced { debit: String, credit: String, diff: String },

    #[error("明細が不足しています（{found} 行）。仕訳は 2 行以上必要です")]
    TooFewLines { found: usize },

    #[error("勘定科目が見つかりません: {code}")]
    UnknownAccount { code: String },

    #[error("記帳できない科目です（見出し科目）: {code}")]
    NotPostable { code: String },

    #[error("通貨が混在しています: {a} と {b}")]
    CurrencyMismatch { a: String, b: String },

    #[error("金額が不正です: {reason}")]
    InvalidAmount { reason: String },

    #[error("未登録のタグキーです: {key}。kaikei-jp-data/tags.yaml に登録してください")]
    UnknownTagKey { key: String },

    #[error("タグ {key} の型が不正です。期待={expected:?}")]
    TagTypeMismatch { key: String, expected: TagValueType },

    #[error("タグ {key} は {account_type:?} の明細では必須です")]
    MissingRequiredTag { key: String, account_type: AccountType },

    #[error("取引日 {date} は会計年度 {fy}（{start}〜{end}）の範囲外です")]
    DateOutOfFiscalYear { date: String, fy: i32, start: String, end: String },

    #[error("会計期間が締められています: {date}")]
    PeriodClosed { date: String },

    #[error("摘要が空です")]
    EmptyDescription,

    #[error("勘定科目表が不正です: {reason}")]
    InvalidChart { reason: String },

    #[error("集計軸に使えないタグキーです: {key}（aggregatable = false）")]
    NotAggregatable { key: String },

    #[error("値が不正です: {reason}")]
    InvalidValue { reason: String },
}
```

### エラーメッセージの方針

MCP 経由で AI が自己修正できる文言にする（`CLAUDE.md` §11）。
差額や期待値を必ず含める。「次に何をすべきか」が読み取れること。

---

## lib.rs

```rust
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod account;
mod clock;
mod error;
mod journal;
mod money;
mod period;
mod tag;
mod trial_balance;

pub use account::*;
pub use clock::*;
pub use error::*;
pub use journal::*;
pub use money::*;
pub use period::*;
pub use tag::*;
pub use trial_balance::*;
```

---

## core に入れないもの（再確認）

- 消費税、税率、軽減税率、インボイス
- 決算書の様式、青色申告
- 日本語の勘定科目名（`AccountDef.name` に外から入るだけ）
- DB、シリアライズ（serde derive も付けない）
- 現在時刻の直接取得
- 為替換算
- 家事按分
- 元入金振替
