//! 仕訳（`JournalEntry`）とその明細（`JournalLine`）。**このモジュールが最重要。**
//!
//! `JournalEntry` の private フィールドを直接触れるのはこのモジュール内のコードだけ
//! （`CLAUDE.md` §6「集約は1モジュールに収める」）。外部からは
//! [`JournalEntry::new`]（新規記帳）と [`JournalEntry::reverse`]（訂正）という
//! 2つの生成経路しか存在しない。
//!
//! ## append-only の構造的保証（`CLAUDE.md` §2）
//!
//! `JournalEntry` に `update` / `delete` / `edit` / `modify` / `set_*` に相当する
//! メソッドは意図的に実装しない。訂正が必要な場合は [`JournalEntry::reverse`] で
//! 逆仕訳（赤伝）を作る。これはコーディング規約ではなく、電子帳簿保存法の
//! 「訂正削除の履歴」要件を構造的に満たすための設計そのものである。
//!
//! この不変性は2つの方法で守られている。
//! - **コンパイル時**: 上記の名前を持つメソッドがこのファイルに存在しないこと自体が
//!   設計テストである（`docs/02-test-cases.md` J-80）。ランタイムテストは無い。
//! - **CI**: `.github/workflows/architecture.yml` が
//!   `fn (update|delete|set_|edit|modify)` をこのファイルに対して grep で検査し、
//!   検出されたらビルドを失敗させる。

use crate::account::{AccountCode, AccountType, ChartOfAccounts};
use crate::clock::{Clock, Timestamp};
use crate::error::CoreError;
use crate::money::{sum_money, Currency, Money};
use crate::period::{AccountingDate, FiscalYear, PeriodGuard, PeriodStatus};
use crate::tag::{TagSchema, TagSet};

/// 仕訳ID。UUID v7 相当の値を想定するが、生成方法自体は core が規定しない
/// （生成は永続化層など外部から渡される）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntryId(u128);

impl EntryId {
    /// 値から仕訳IDを作る。
    pub fn new(value: u128) -> Self {
        EntryId(value)
    }

    /// 内部の値を返す。
    pub fn as_u128(&self) -> u128 {
        self.0
    }
}

/// 仕訳番号。会計年度内での表示・整列に使う連番。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct EntryNumber(u32);

impl EntryNumber {
    /// 値から仕訳番号を作る。
    pub fn new(value: u32) -> Self {
        EntryNumber(value)
    }

    /// 内部の値を返す。
    pub fn as_u32(&self) -> u32 {
        self.0
    }
}

/// 借方・貸方の別。
///
/// 「借方 = プラス」ではない。意味は科目種別によって変わる（`DOMAIN.md` §2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// 借方。
    Debit,
    /// 貸方。
    Credit,
}

/// 仕訳明細。1行分の記帳（科目・貸借・金額・タグ・摘要）。
#[derive(Debug, Clone)]
pub struct JournalLine {
    account: AccountCode,
    side: Side,
    amount: Money,
    tags: TagSet,
    memo: Option<String>,
}

impl JournalLine {
    /// 仕訳明細を作る。
    ///
    /// `amount` は正の値でなければならない。符号は `side`（借方/貸方）で表現するため、
    /// 負値やゼロは `CoreError::InvalidAmount` を返す。
    pub fn new(
        account: AccountCode,
        side: Side,
        amount: Money,
        tags: TagSet,
        memo: Option<String>,
    ) -> Result<Self, CoreError> {
        if amount.is_negative() || amount.is_zero() {
            return Err(CoreError::InvalidAmount {
                reason: format!(
                    "仕訳明細の金額は正の値である必要があります（符号は貸方/借方で表現します）: {}",
                    amount.to_display_string()
                ),
            });
        }
        Ok(JournalLine {
            account,
            side,
            amount,
            tags,
            memo,
        })
    }

    /// 勘定科目コードを返す。
    pub fn account(&self) -> &AccountCode {
        &self.account
    }

    /// 借方・貸方の別を返す。
    pub fn side(&self) -> Side {
        self.side
    }

    /// 金額を返す。
    pub fn amount(&self) -> &Money {
        &self.amount
    }

    /// タグを返す。
    pub fn tags(&self) -> &TagSet {
        &self.tags
    }

    /// 明細メモを返す。
    pub fn memo(&self) -> Option<&str> {
        self.memo.as_deref()
    }

    /// 借方の明細かどうか。
    pub fn is_debit(&self) -> bool {
        matches!(self.side, Side::Debit)
    }
}

/// 証憑への参照。証憑実体（ファイル）は core が知らない（`kaikei-blob` の責務）。
#[derive(Debug, Clone)]
pub struct DocumentRef {
    /// 証憑ID。
    pub document_id: u128,
    /// 表示用ラベル（例: "領収書"）。
    pub label: String,
}

/// 仕訳。複式簿記の集約。**private フィールドを触るコードはこのモジュールに閉じる。**
///
/// 生成経路は [`JournalEntry::new`]（新規記帳、全検証あり）と
/// [`JournalEntry::reverse`]（訂正、逆仕訳を生成）の2つのみ。
/// [`JournalEntry::rehydrate`] は永続化層からの復元専用で検証を行わない。
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

/// [`JournalEntry::new`] に渡す引数。引数が多いため構造体にまとめる。
pub struct NewEntry {
    /// 仕訳ID。
    pub id: EntryId,
    /// 仕訳番号。
    pub entry_no: EntryNumber,
    /// 取引日。
    pub entry_date: AccountingDate,
    /// 摘要。
    pub description: String,
    /// 仕訳明細（2行以上）。
    pub lines: Vec<JournalLine>,
    /// 証憑への参照。
    pub document_refs: Vec<DocumentRef>,
}

/// 指定した `side` の明細金額を合算する。
///
/// `lines` が単一通貨であることは呼び出し側の責務
/// （`JournalEntry::new` の通貨検証、または `rehydrate` の呼び出し元の責任で保証される）。
/// 該当する明細が無ければゼロを返す。
///
/// 合算そのものは `money.rs` の [`sum_money`] に委譲する（縮約ロジックの重複を避ける）。
fn sum_side(lines: &[JournalLine], currency: Currency, side: Side) -> Result<Money, CoreError> {
    let amounts = lines
        .iter()
        .filter(|line| line.side == side)
        .map(|line| line.amount());
    Ok(sum_money(amounts)?.unwrap_or_else(|| Money::zero(currency)))
}

impl JournalEntry {
    /// 仕訳を新規作成する唯一の生成経路。以下を順に検証し、最初に見つかった違反を返す。
    ///
    /// 1. `lines` が2行以上であること（`TooFewLines`）
    /// 2. 全 `account` が `chart` に存在し `postable == true` であること
    ///    （`UnknownAccount` / `NotPostable`）
    /// 3. 全明細の通貨が同一であること（`CurrencyMismatch`）
    /// 4. 借方合計と貸方合計が一致すること（複式簿記の核心。`Unbalanced`）
    /// 5. 全明細の `tags` が `schema` に適合すること
    ///    （`UnknownTagKey` / `TagTypeMismatch` / `MissingRequiredTag`）
    /// 6. `entry_date` が `fy` の範囲内であること（`DateOutOfFiscalYear`）
    /// 7. `entry_date` の会計期間が `guard` 上で Open であること（`PeriodClosed`）
    /// 8. `description` が空でないこと（前後の空白を除いて判定。`EmptyDescription`）
    pub fn new(
        input: NewEntry,
        fy: &FiscalYear,
        chart: &ChartOfAccounts,
        schema: &TagSchema,
        guard: &dyn PeriodGuard,
        clock: &dyn Clock,
    ) -> Result<Self, CoreError> {
        // 1. 明細数
        if input.lines.len() < 2 {
            return Err(CoreError::TooFewLines {
                found: input.lines.len(),
            });
        }

        // 2. 勘定科目の存在と記帳可否
        let mut account_types: Vec<AccountType> = Vec::with_capacity(input.lines.len());
        for line in &input.lines {
            let def = chart
                .get(line.account())
                .ok_or_else(|| CoreError::UnknownAccount {
                    code: line.account().as_str().to_string(),
                })?;
            if !def.postable {
                return Err(CoreError::NotPostable {
                    code: line.account().as_str().to_string(),
                });
            }
            account_types.push(def.account_type);
        }

        // 3. 通貨の一致
        let currency = input.lines[0].amount().currency();
        for line in &input.lines {
            if line.amount().currency() != currency {
                return Err(CoreError::CurrencyMismatch {
                    a: currency.code().to_string(),
                    b: line.amount().currency().code().to_string(),
                });
            }
        }

        // 4. 貸借一致
        let debit_total = sum_side(&input.lines, currency, Side::Debit)?;
        let credit_total = sum_side(&input.lines, currency, Side::Credit)?;
        if debit_total.minor() != credit_total.minor() {
            let diff = debit_total.sub(&credit_total)?.abs();
            return Err(CoreError::Unbalanced {
                debit: debit_total.to_display_string(),
                credit: credit_total.to_display_string(),
                diff: diff.to_display_string(),
            });
        }

        // 5. タグのスキーマ適合
        for (line, account_type) in input.lines.iter().zip(&account_types) {
            schema.validate(line.tags(), *account_type)?;
        }

        // 6. 会計年度の範囲内
        if !fy.contains(input.entry_date) {
            return Err(CoreError::DateOutOfFiscalYear {
                date: input.entry_date.to_iso_string(),
                fy: fy.label(),
                start: fy.start().to_iso_string(),
                end: fy.end().to_iso_string(),
            });
        }

        // 7. 会計期間が Open
        if guard.status(input.entry_date) != PeriodStatus::Open {
            return Err(CoreError::PeriodClosed {
                date: input.entry_date.to_iso_string(),
            });
        }

        // 8. 摘要が空でない
        if input.description.trim().is_empty() {
            return Err(CoreError::EmptyDescription);
        }

        Ok(JournalEntry {
            id: input.id,
            fiscal_year: fy.label(),
            entry_no: input.entry_no,
            entry_date: input.entry_date,
            description: input.description,
            lines: input.lines,
            document_refs: input.document_refs,
            reverses: None,
            reverse_reason: None,
            recorded_at: clock.now(),
        })
    }

    /// 永続化層からの復元専用。**検証を一切行わない。**
    ///
    /// `pub` だが store 層以外から呼び出さないこと。DB 上の行は
    /// [`JournalEntry::new`] を経て記帳された時点で既に検証済みであるため、
    /// 復元時に同じ検証を繰り返す必要はない（`chart` や `schema` の現在版と
    /// 過去の記帳内容が食い違っていても、それは仕様変更の履歴であって
    /// エラーではない）。
    ///
    /// # Panics
    ///
    /// この関数自体は panic しないが、ここで検証しなかった不正なデータ
    /// （`lines` が空、`lines` 内の通貨が混在している等）を渡すと、
    /// 後から [`JournalEntry::currency`] / [`JournalEntry::debit_total`] /
    /// [`JournalEntry::credit_total`] を呼び出した時点で panic する。
    /// 呼び出し側（store層）は [`JournalEntry::new`] を経て永続化されたデータのみを
    /// 渡す責任を負う。
    #[allow(clippy::too_many_arguments)]
    pub fn rehydrate(
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
    ) -> Self {
        JournalEntry {
            id,
            fiscal_year,
            entry_no,
            entry_date,
            description,
            lines,
            document_refs,
            reverses,
            reverse_reason,
            recorded_at,
        }
    }

    /// この仕訳を訂正する逆仕訳（赤伝）を作る。**訂正はこの方法のみ。**
    ///
    /// - 全明細の `side` を反転する（借方⇄貸方）
    /// - `amount` / `account` / `tags` はそのまま複製する
    /// - `description` は `"【訂正】{元の摘要}"` になる
    /// - 生成された仕訳の `reverses()` に元の `id`、`reverse_reason()` に `reason` が入る
    /// - 逆仕訳の逆仕訳も許可される（`reverses` は常に直前の仕訳を指す）
    /// - 元仕訳が別年度でも、逆仕訳は `fy`（呼び出し側が指定した日付の年度）に属する
    ///
    /// `document_refs`（証憑への参照）は複製しない。仕様書の「reverse の仕様」には
    /// `document_refs` の扱いが明記されておらず、訂正の根拠となる証憑は元仕訳とは
    /// 別に用意されることが多いため、逆仕訳は証憑なしで作成する。
    ///
    /// 生成される明細は貸借が入れ替わるだけなので、元仕訳が貸借一致していれば
    /// 逆仕訳も必ず貸借一致する。それ以外の検証（科目・通貨・タグ・年度・期間・摘要）は
    /// [`JournalEntry::new`] と同一の基準で行われる。
    #[allow(clippy::too_many_arguments)]
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
    ) -> Result<Self, CoreError> {
        let reversed_lines: Vec<JournalLine> = self
            .lines
            .iter()
            .map(|line| JournalLine {
                account: line.account.clone(),
                side: match line.side {
                    Side::Debit => Side::Credit,
                    Side::Credit => Side::Debit,
                },
                amount: line.amount,
                tags: line.tags.clone(),
                memo: line.memo.clone(),
            })
            .collect();

        let input = NewEntry {
            id,
            entry_no,
            entry_date: date,
            description: format!("【訂正】{}", self.description),
            lines: reversed_lines,
            document_refs: Vec::new(),
        };

        let mut reversed = JournalEntry::new(input, fy, chart, schema, guard, clock)?;
        reversed.reverses = Some(self.id);
        reversed.reverse_reason = Some(reason);
        Ok(reversed)
    }

    /// 仕訳IDを返す。
    pub fn id(&self) -> EntryId {
        self.id
    }

    /// 仕訳番号を返す。
    pub fn entry_no(&self) -> EntryNumber {
        self.entry_no
    }

    /// この仕訳が属する会計年度ラベルを返す（`FiscalYear::label()` と同じ値）。
    ///
    /// `docs/01-core-types.md` のゲッター一覧には無いが、[`reverse_reason`]
    /// と同じ理由（フィールドとして存在するのに外部から確認する手段が無いのは
    /// 不整合であり、`docs/02-test-cases.md` R-12「逆仕訳は指定日付の年度に属する」を
    /// 検証するために必要）で追加する。
    ///
    /// [`reverse_reason`]: JournalEntry::reverse_reason
    pub fn fiscal_year(&self) -> i32 {
        self.fiscal_year
    }

    /// 取引日を返す。
    pub fn entry_date(&self) -> AccountingDate {
        self.entry_date
    }

    /// 摘要を返す。
    pub fn description(&self) -> &str {
        &self.description
    }

    /// 仕訳明細を返す。不変参照のみで、外部から変更できない
    /// （`JournalEntry` に `lines` を書き換える手段は無い）。
    pub fn lines(&self) -> &[JournalLine] {
        &self.lines
    }

    /// 証憑への参照一覧を返す。
    pub fn document_refs(&self) -> &[DocumentRef] {
        &self.document_refs
    }

    /// この仕訳が訂正している元仕訳のIDを返す（逆仕訳でなければ `None`）。
    pub fn reverses(&self) -> Option<EntryId> {
        self.reverses
    }

    /// この仕訳が逆仕訳（訂正仕訳）かどうか。
    pub fn is_reversal(&self) -> bool {
        self.reverses.is_some()
    }

    /// 逆仕訳の理由を返す（逆仕訳でなければ `None`）。
    ///
    /// `docs/01-core-types.md` のゲッター一覧には明示されていないが、
    /// `reverse_reason` フィールド自体は仕様に存在し（`reverse` の仕様の
    /// `reverse_reason に理由`）、外部から確認できなければ「なぜ訂正したか」が
    /// 参照不能になってしまう。`memo()` と同様の読み取り専用ゲッターとして追加する
    /// （`docs/02-test-cases.md` R-06 の検証に必要）。
    pub fn reverse_reason(&self) -> Option<&str> {
        self.reverse_reason.as_deref()
    }

    /// 記帳時刻を返す。
    pub fn recorded_at(&self) -> Timestamp {
        self.recorded_at
    }

    /// 借方合計を返す。
    ///
    /// # Panics
    ///
    /// [`JournalEntry::rehydrate`] 経由で `lines` が空、または通貨が混在した
    /// インスタンスが作られている場合に panic する（詳細は `rehydrate` の doc を参照）。
    pub fn debit_total(&self) -> Money {
        let currency = self.currency();
        debug_assert!(
            self.lines
                .iter()
                .all(|line| line.amount().currency() == currency),
            "JournalEntry::debit_total: 明細の通貨が混在しています。\
             rehydrate に通貨混在の lines を渡していないか確認してください（store層のバグ）"
        );
        sum_side(&self.lines, currency, Side::Debit).expect(
            "JournalEntry は new/reverse 経由なら常に単一通貨で構築されるため、\
             明細の合算は（オーバーフローしない限り）失敗しない。\
             rehydrate 経由でこれが成り立たない場合は呼び出し元（store層）のバグ",
        )
    }

    /// 貸方合計を返す。
    ///
    /// # Panics
    ///
    /// [`JournalEntry::rehydrate`] 経由で `lines` が空、または通貨が混在した
    /// インスタンスが作られている場合に panic する（詳細は `rehydrate` の doc を参照）。
    pub fn credit_total(&self) -> Money {
        let currency = self.currency();
        debug_assert!(
            self.lines
                .iter()
                .all(|line| line.amount().currency() == currency),
            "JournalEntry::credit_total: 明細の通貨が混在しています。\
             rehydrate に通貨混在の lines を渡していないか確認してください（store層のバグ）"
        );
        sum_side(&self.lines, currency, Side::Credit).expect(
            "JournalEntry は new/reverse 経由なら常に単一通貨で構築されるため、\
             明細の合算は（オーバーフローしない限り）失敗しない。\
             rehydrate 経由でこれが成り立たない場合は呼び出し元（store層）のバグ",
        )
    }

    /// この仕訳の通貨を返す（全明細で共通）。
    ///
    /// # Panics
    ///
    /// [`JournalEntry::rehydrate`] 経由で `lines` が空のインスタンスが作られている場合に
    /// panic する（詳細は `rehydrate` の doc を参照）。
    pub fn currency(&self) -> Currency {
        debug_assert!(
            !self.lines.is_empty(),
            "JournalEntry::currency: 明細が空です。\
             rehydrate に空の lines を渡していないか確認してください（store層のバグ）"
        );
        self.lines
            .first()
            .expect(
                "JournalEntry は new/reverse 経由なら常に2行以上の明細を持つ。\
                 rehydrate 経由でこれが成り立たない場合は呼び出し元（store層）のバグ",
            )
            .amount()
            .currency()
    }
}
