//! read model 専用の DTO（[`BalanceRowView`] / [`TrialBalanceView`] /
//! [`EntrySummaryView`] / [`LedgerPageView`]）。
//!
//! `kaikei_core::GroupKey` には `impl` ブロックが1つも無く、公開コンストラクタも
//! アクセサも存在しない（実測確認済み）。したがって `kaikei_core::BalanceRow` /
//! `TrialBalance` は core の外から構築できない。SQL 集計（`kaikei-store::query`、
//! PR-6）から直接組み立てられる read model 専用の DTO をここに定義する
//! （`DECISIONS.md` D-031）。
//!
//! 金額は文字列ではなく [`kaikei_core::Money`] のまま保持する。`DECISIONS.md`
//! D-013「JSON では金額を文字列で扱う」は presentation 層（HTTP/MCP 応答）が
//! 外部にシリアライズする形式についての決定であり、この DTO は
//! `kaikei-app` の呼び出し元にプロセス内でそのまま渡す中間表現なので対象外。
//!
//! # 取り消された仕訳は「消えない」（`DECISIONS.md` D-088）
//!
//! 帳簿は追記のみなので、赤伝で訂正された仕訳も、その赤伝も、どちらも
//! 検索結果・元帳に残り続ける。**読み手がそれを「取り消し済み」と判別
//! できなければ、AI は同じ仕訳をもう一度訂正しようとする。**
//! そこで [`EntrySummaryView`] / [`LedgerRowView`] は次の2方向を必ず持つ:
//!
//! | 欄 | 意味 |
//! |---|---|
//! | `reverses` | この仕訳が**赤伝**であり、訂正対象がどれか |
//! | `reversed_by` | この仕訳が**赤伝で取り消されている**こと（[`ReversalRef`]） |
//!
//! どちらも `Option` であり、`None` は「そうではない」を意味する。

use kaikei_core::{
    AccountCode, AccountType, AccountingDate, CoreError, Currency, EntryId, EntryNumber,
    JournalLine, Money, Side, TagSet,
};
use std::collections::BTreeMap;

/// `group_by` のグループキー。指定したタグキー文字列と値文字列の組。
///
/// キーの型を `kaikei_core::TagKey` ではなく `String` にしているのは、この
/// DTO が SQL の集計結果（例: `jsonb_object_agg`）から直接組み立てられることを
/// 想定しているため（`TagKey::parse` による再検証を read model の構築の
/// たびに強制しない）。
pub type GroupKeyView = BTreeMap<String, String>;

/// 試算表の1行（read model 版）。
///
/// フィールド構成は `kaikei_core::BalanceRow` に対応するが、`group` の型が
/// `kaikei_core::GroupKey`（構築不能）ではなく [`GroupKeyView`] になっている
/// 点が異なる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BalanceRowView {
    /// 勘定科目コード。
    pub account: AccountCode,
    /// 科目種別。残高の符号を決めるために保持する。
    pub account_type: AccountType,
    /// `group_by` によるグループキー。`group_by` を指定しなければ常に空。
    pub group: GroupKeyView,
    /// 借方合計。
    pub debit_total: Money,
    /// 貸方合計。
    pub credit_total: Money,
    /// `account_type.is_debit_normal()` に従った符号付き残高。
    pub balance: Money,
}

/// 試算表（read model 版）。
///
/// [`crate::ports::TrialBalanceQuery::trial_balance`] が返す行一覧をラップし、
/// 検算等の補助メソッドを提供する。
///
/// # 通貨は行から推論せず、明示的に受け取る（PR-B 2巡目）
///
/// 1巡目は行から通貨を推論していたため、**0行の期間では通貨が決まらず**
/// `totals()` が `Ok(None)` を返していた。その結果、`get_trial_balance` の
/// 応答は空期間で通貨を名乗れず、合計欄も出せなかった
/// （「集計対象の通貨が単一であることを要求する」という `DECISIONS.md`
/// D-042 の要件も、行が無いと検査できない）。
///
/// [`TrialBalanceView::new`] は帳簿通貨
/// （[`crate::context::BookSettings::book_currency`]）を必須の引数として
/// 受け取る。これにより:
///
/// - **0行でも通貨を名乗れる**（合計は `0`）。
/// - 行の通貨が帳簿通貨と食い違えば `totals()` が
///   `CoreError::CurrencyMismatch` を返す（D-042 の実効的な検査）。
/// - `totals()` の戻り値から `Option` が消え、呼び出し側の分岐が減る。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrialBalanceView {
    rows: Vec<BalanceRowView>,
    currency: Currency,
}

impl TrialBalanceView {
    /// 行一覧と**この試算表の通貨**から試算表ビューを作る。
    ///
    /// `currency` は帳簿通貨（[`crate::context::BookSettings::book_currency`]）
    /// を渡す。行が0件でもこの値が応答の通貨になる。
    pub fn new(rows: Vec<BalanceRowView>, currency: Currency) -> Self {
        TrialBalanceView { rows, currency }
    }

    /// 全行を返す。
    pub fn rows(&self) -> &[BalanceRowView] {
        &self.rows
    }

    /// この試算表の通貨を返す。**行が0件でも決まる。**
    ///
    /// 応答の `currency` フィールドはこの値を使う
    /// （`kaikei_core::Currency` はコードと小数桁数の組なので、
    /// 金額文字列の解釈にも必要になる。`docs/07-mcp-server.md` §5）。
    pub fn currency(&self) -> Currency {
        self.currency
    }

    /// 借方合計・貸方合計を返す。**行が0件なら両方ゼロ**
    /// （[`TrialBalanceView::currency`] 建て）。
    ///
    /// # Errors
    ///
    /// 行の通貨がこの試算表の通貨と食い違う場合は
    /// `CoreError::CurrencyMismatch`（`DECISIONS.md` D-042「集計対象の通貨が
    /// 単一であること」の検査）。合算がオーバーフローする場合は
    /// `CoreError::InvalidAmount`。
    pub fn totals(&self) -> Result<(Money, Money), CoreError> {
        let mut debit = Money::zero(self.currency);
        let mut credit = Money::zero(self.currency);
        for row in &self.rows {
            // `Money::add` が通貨不一致を弾くため、ここで明示的な比較は要らない。
            debit = debit.add(&row.debit_total)?;
            credit = credit.add(&row.credit_total)?;
        }
        Ok((debit, credit))
    }

    /// 借方合計と貸方合計が一致するかどうかを検算する。行が無ければ自明に `true`。
    pub fn is_balanced(&self) -> Result<bool, CoreError> {
        let (debit, credit) = self.totals()?;
        Ok(debit == credit)
    }
}

// ---------------------------------------------------------------------------
// 仕訳検索 / 総勘定元帳（Phase 3 PR-H。`DECISIONS.md` D-088 / D-089）
// ---------------------------------------------------------------------------

/// この仕訳を取り消している赤伝への参照。
///
/// 「取り消された」ことが読み手に分かる形にするための欄
/// （モジュール doc「取り消された仕訳は『消えない』」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReversalRef {
    /// 赤伝の仕訳ID。
    pub entry_id: EntryId,
    /// 赤伝の仕訳番号。
    pub entry_no: EntryNumber,
    /// 赤伝の取引日。
    pub entry_date: AccountingDate,
}

/// 検索結果の仕訳1件（明細を含む）。
///
/// 明細を含めるのは、含めないと呼び出し元が件数分の追加問い合わせを
/// することになるためである（1件ずつ引き直す形にすると、AI は検索1回に
/// 対して `get_entry` を件数分呼ぶ）。含める代わりに1ページの上限を
/// 小さく取る（[`crate::usecase::search_entries::MAX_LIMIT`]）。
///
/// `PartialEq` を導出していないのは `kaikei_core::JournalLine` が実装して
/// いないためである（`kaikei-core` は凍結層であり、この DTO の都合で
/// derive を足さない。`CLAUDE.md` §1）。
#[derive(Debug, Clone)]
pub struct EntrySummaryView {
    /// 仕訳ID。
    pub entry_id: EntryId,
    /// 仕訳番号（会計年度内の連番）。
    pub entry_no: EntryNumber,
    /// 会計年度。
    pub fiscal_year: i32,
    /// 取引日（記帳日ではない。`CLAUDE.md` §7）。
    pub entry_date: AccountingDate,
    /// 摘要。
    pub description: String,
    /// 明細（`line_no` の昇順）。
    pub lines: Vec<JournalLine>,
    /// この仕訳が赤伝なら、訂正対象の仕訳ID。
    pub reverses: Option<EntryId>,
    /// この仕訳が赤伝なら、訂正理由（記帳時の入力のまま）。
    pub reverse_reason: Option<String>,
    /// この仕訳を取り消している赤伝（あれば）。
    pub reversed_by: Option<ReversalRef>,
}

/// 仕訳検索の続きを指す位置（keyset ページング。`DECISIONS.md` D-089）。
///
/// **オフセットではない。** 取引日は過去日でも記帳できるため、ページを
/// またぐ間に**前のページより前へ挿入される**仕訳がありうる。オフセットで
/// 送ると、その瞬間に行が1つずれて**黙って読み飛ばされる**。
/// この位置は `(entry_date, entry_no, entry_id)` の全順序で、次のページは
/// 「この位置より厳密に後ろ」から始まる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryCursor {
    /// 直前のページの最後の仕訳の取引日。
    pub entry_date: AccountingDate,
    /// 同・仕訳番号。
    pub entry_no: EntryNumber,
    /// 同・仕訳ID（同一日・同一番号を割る最終手段）。
    pub entry_id: EntryId,
}

/// 仕訳検索の1ページ分。
#[derive(Debug, Clone)]
pub struct EntrySearchPageView {
    /// このページの仕訳（取引日 → 仕訳番号 → 仕訳ID の昇順）。
    pub entries: Vec<EntrySummaryView>,
    /// **条件に一致した総件数**（このページの件数ではない）。
    ///
    /// 上限で切ったことを呼び出し元が判別できるようにするために返す。
    /// これが無いと「返ってきた件数＝全件」と読める（`PROGRESS.md`
    /// 「無言の truncation は『全部見た』と読める」）。
    pub total_matches: u64,
    /// 続きがある場合の次の開始位置。`None` なら**このページで全部**である。
    pub next_cursor: Option<EntryCursor>,
}

/// 総勘定元帳の続きを指す位置（keyset ページング）。
///
/// 元帳の行は明細単位なので、[`EntryCursor`] に `line_no` を足した
/// `(entry_date, entry_no, entry_id, line_no)` の全順序になる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LedgerCursor {
    /// 仕訳側の位置。
    pub entry: EntryCursor,
    /// 直前のページの最後の明細の行番号（1 始まり）。
    pub line_no: u16,
}

/// 総勘定元帳の1行（＝ 対象科目の明細1行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerRowView {
    /// 仕訳ID。
    pub entry_id: EntryId,
    /// 仕訳番号。
    pub entry_no: EntryNumber,
    /// 取引日。
    pub entry_date: AccountingDate,
    /// 明細の行番号（1 始まり）。
    pub line_no: u16,
    /// 仕訳の摘要。
    pub description: String,
    /// 借方・貸方。
    pub side: Side,
    /// 金額（常に正。向きは [`LedgerRowView::side`] が持つ）。
    pub amount: Money,
    /// 明細のタグ。
    pub tags: TagSet,
    /// 明細の備考。
    pub memo: Option<String>,
    /// 相手科目（同じ仕訳の反対側にある科目コード。重複を除いた昇順）。
    pub counter_accounts: Vec<AccountCode>,
    /// この行までの残高（期首残高を含む。科目種別に従った符号付き）。
    pub running_balance: Money,
    /// この仕訳が赤伝なら、訂正対象の仕訳ID。
    pub reverses: Option<EntryId>,
    /// この仕訳が赤伝なら、訂正理由（記帳時の入力のまま）。
    ///
    /// [`EntrySummaryView::reverse_reason`] と同じもの。元帳の行にも持たせるのは、
    /// **赤伝の行を見た読み手が「なぜ取り消されたか」を引き直さずに読める**
    /// ようにするためである（`DECISIONS.md` D-088 の表は `reverses` と
    /// `reverse_reason` を赤伝に付く欄として並べている。片方だけ落とすと、
    /// 元帳から検索へ移らないと理由が読めない）。
    pub reverse_reason: Option<String>,
    /// この仕訳を取り消している赤伝（あれば）。
    pub reversed_by: Option<ReversalRef>,
}

/// 総勘定元帳の1ページ分。
///
/// # 合計はページではなく**期間全体**のもの
///
/// `opening_balance` / `debit_total` / `credit_total` / `closing_balance` /
/// `total_lines` は、ページングに関係なく指定期間の全明細から求める。
/// ページ内の行を合計しても `debit_total` にならない（そういう読み方を
/// させないために、行側には合計を置かない）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerPageView {
    /// 対象の勘定科目コード。
    pub account: AccountCode,
    /// 対象科目の名称（`accounts` テーブルの現在の値）。
    pub account_name: String,
    /// 対象科目の5要素分類。残高の符号を決める。
    pub account_type: AccountType,
    /// 期首残高（`from` **より前**の全明細から求めた符号付き残高）。
    pub opening_balance: Money,
    /// 期間中の借方合計。
    pub debit_total: Money,
    /// 期間中の貸方合計。
    pub credit_total: Money,
    /// 期末残高（`opening_balance` に期間中の増減を加えた符号付き残高）。
    pub closing_balance: Money,
    /// 期間中の明細行数（このページの行数ではない）。
    pub total_lines: u64,
    /// このページの行（取引日 → 仕訳番号 → 仕訳ID → 行番号の昇順）。
    pub rows: Vec<LedgerRowView>,
    /// 続きがある場合の次の開始位置。`None` なら**このページで全部**である。
    pub next_cursor: Option<LedgerCursor>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(account: &str, account_type: AccountType, debit: i128, credit: i128) -> BalanceRowView {
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
            group: GroupKeyView::new(),
            debit_total,
            credit_total,
            balance,
        }
    }

    // TBV-1（PR-B 2巡目）: 0行でも通貨を名乗れ、合計はゼロになる。
    #[test]
    fn empty_trial_balance_view_still_names_its_currency() {
        let tb = TrialBalanceView::new(Vec::new(), Currency::JPY);
        assert_eq!(tb.currency(), Currency::JPY);
        let (debit, credit) = tb.totals().unwrap();
        assert_eq!(debit.minor(), 0);
        assert_eq!(credit.minor(), 0);
        assert_eq!(debit.currency(), Currency::JPY);
        assert_eq!(credit.currency(), Currency::JPY);
        assert!(tb.is_balanced().unwrap());
    }

    // TBV-2: 空でも通貨は行から推論していない（USD の空期間は USD を名乗る）。
    #[test]
    fn empty_trial_balance_view_uses_the_declared_currency_not_a_default() {
        let tb = TrialBalanceView::new(Vec::new(), Currency::USD);
        assert_eq!(tb.currency(), Currency::USD);
        assert_eq!(tb.totals().unwrap().0.currency(), Currency::USD);
    }

    #[test]
    fn balanced_rows_are_balanced() {
        let tb = TrialBalanceView::new(
            vec![
                row("100", AccountType::Asset, 1_000, 0),
                row("500", AccountType::Revenue, 0, 1_000),
            ],
            Currency::JPY,
        );
        let (debit, credit) = tb.totals().unwrap();
        assert_eq!(debit.minor(), 1_000);
        assert_eq!(credit.minor(), 1_000);
        assert!(tb.is_balanced().unwrap());
    }

    #[test]
    fn unbalanced_rows_are_not_balanced() {
        let tb = TrialBalanceView::new(
            vec![
                row("100", AccountType::Asset, 1_000, 0),
                row("500", AccountType::Revenue, 0, 900),
            ],
            Currency::JPY,
        );
        assert!(!tb.is_balanced().unwrap());
    }

    // TBV-3（PR-B 2巡目 / `DECISIONS.md` D-042）: 行の通貨が試算表の通貨と
    // 食い違えば、合計を出す時点で検出される。
    #[test]
    fn rows_in_another_currency_are_rejected_when_totalling() {
        let usd_row = BalanceRowView {
            account: AccountCode::parse("100").unwrap(),
            account_type: AccountType::Asset,
            group: GroupKeyView::new(),
            debit_total: Money::from_minor(1_000, Currency::USD),
            credit_total: Money::zero(Currency::USD),
            balance: Money::from_minor(1_000, Currency::USD),
        };
        let tb = TrialBalanceView::new(vec![usd_row], Currency::JPY);
        assert!(matches!(
            tb.totals(),
            Err(CoreError::CurrencyMismatch { .. })
        ));
    }
}

/// 証憑1件（`docs/06-documents.md` §3）。
///
/// ファイルの中身は持たない（内容は `kaikei-blob` が SHA-256 で管理する）。
/// **メタデータだけを運ぶ**ので、一覧を返しても量が跳ねない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentView {
    /// 証憑ID。
    pub id: String,
    /// 内容の SHA-256（16進64文字・小文字）。
    pub blob_hash: String,
    /// 元のファイル名。
    pub original_name: String,
    /// MIME タイプ。
    pub mime_type: String,
    /// バイト数。
    pub byte_size: i64,
    /// 取引年月日（検索要件）。
    pub doc_date: AccountingDate,
    /// 取引金額（検索要件）。**契約書のように金額が無い証憑があるので
    /// `None` を許す。0 で埋めない。**
    pub amount_minor: Option<i64>,
    /// 取引先（検索要件）。
    pub counterparty: Option<String>,
    /// 種別（invoice / receipt / contract / other）。
    pub doc_type: String,
    /// 授受の経路（email / download / scan / manual）。
    pub received_via: String,
    /// 備考。
    pub note: Option<String>,
}

/// 証憑の検索条件（`docs/06-documents.md` §4）。
///
/// **取引年月日・取引金額・取引先の3項目**の組み合わせと範囲指定に対応する。
/// これが電子取引データの検索要件の内容である。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocumentQuery {
    /// 取引年月日の下限（この日を含む）。
    pub date_from: Option<AccountingDate>,
    /// 取引年月日の上限（この日を含む）。
    pub date_to: Option<AccountingDate>,
    /// 取引金額の下限（この額を含む）。
    pub amount_min: Option<i64>,
    /// 取引金額の上限（この額を含む）。
    pub amount_max: Option<i64>,
    /// 取引先（完全一致）。
    pub counterparty: Option<String>,
    /// 種別。
    pub doc_type: Option<String>,
}
