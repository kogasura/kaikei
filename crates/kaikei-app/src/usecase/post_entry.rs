//! 記帳ユースケース（[`execute`]）と、その **dry-run**（[`preview`]）。
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
//! # dry-run（[`preview`]）— ★手順を二重に持たない★
//!
//! `docs/07-mcp-server.md` §3 の `hint.suggested_lines`（「税額行を追加すれば
//! 貸借が合う」という修正案）を上位層が組み立てるには、
//! **記帳せずに `derive_tax_lines` の結果を得る**入口が要る。
//!
//! 1巡目はこの入口が無かったため、消費側（`kaikei-mcp` を書く役）が
//! `with_tx` を自分で開き `load_posting_context` を呼び `TaxContext` を自作し
//! `sum_money` で検算する、という**同 §4「MCP はビジネスロジックを書かない」に
//! 真っ向から反するコードを書け、しかもコンパイルもテストも通ってしまった**。
//! 契約が「書けない」ではなく「**間違った書き方が通ってしまう**」形で
//! 凍結されかけていた。
//!
//! [`preview`] は [`execute`] と**同じ関数を通る**。
//!
//! | 手順 | [`execute`] | [`preview`] |
//! |---|---|---|
//! | 1〜3（I/O + policy） | [`prepare`] | [`prepare`]（同じ関数） |
//! | 4（採番） | 行う | **行わない** |
//! | 5（`JournalEntry::new` による検証） | [`build_entry`] | [`build_entry`]（同じ関数） |
//! | 6（INSERT） | 行う | **行わない** |
//!
//! 手順を独立に書き写した箇所は無いので、検証の順序が両者で乖離しえない。
//! 差分は「採番と INSERT をするかどうか」だけであり、それがまさに
//! dry-run の定義である。乖離が起きていないことは
//! `preview_and_execute_agree_on_*` の各テストが実行時にも突き合わせる。
//!
//! テストID（`PE-1` 等）はこのファイル内でのみ一意な連番であり、
//! `docs/02-test-cases.md` のID体系とは独立している。

use crate::context::{load_posting_context, BookSettings, PostingContext};
use crate::error::AppError;
use crate::period_guard::ClosedPeriodGuard;
use crate::ports::{AppClock, IdGenerator, TxOps};
use kaikei_core::{
    AccountingDate, ChartOfAccounts, CoreError, EntryId, EntryNumber, FiscalYear, JournalEntry,
    JournalLine, Money, NewEntry, TagSchema,
};
use kaikei_policy::{PolicyNote, TaxContext, TaxPolicy};

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

/// [`execute`] の戻り値。
///
/// `JournalEntry` 単体ではなくこの構造体を返すのは、**`PolicyNote` を
/// 呼び出し元まで届ける**ため（`DECISIONS.md` D-070 の決定3 / D-073。
/// Phase 2 の申し送り「`PolicyNote` が永続化されない」への回答）。
///
/// 非適格の経過措置（`deduction_ratio < 1`）や簡易課税のように、**税額計算
/// には反映されず注記にしか現れない情報**があり（D-059）、`notes` を捨てると
/// AI も監査ログも「控除割合の制限があった」ことを知る手段が無くなる。
///
/// フィールドを追加しても**読み取り側**は壊れない（構築するのはこの
/// ユースケースだけなので `#[non_exhaustive]` は付けていない。ただし
/// `let PostEntryOutput { .. } = out;` のような網羅的な分解を書いている
/// 呼び出し元はフィールド追加時に壊れる）。
#[derive(Debug, Clone)]
pub struct PostEntryOutput {
    /// 記帳された仕訳（確定後の明細を含む）。
    pub entry: JournalEntry,

    /// `tax.derive_tax_lines` が添えた注記。
    ///
    /// 文言は `kaikei-policy` の実装が組み立てたものを**そのまま**運ぶ。
    /// 上位層で税務判断を断定する言い換えをしないこと（`CLAUDE.md` §10）。
    ///
    /// **空であることは「注記が無い」を意味するとは限らない。**
    /// `input.auto_tax_lines` が `false` のときは `derive_tax_lines` 自体を
    /// 呼ばないので、常に空になる（`validate_tag` は注記を返さない）。
    /// 呼び出し元は自分が渡した `auto_tax_lines` を知っているので、
    /// この2つの区別が必要ならそちらで判断する。
    pub notes: Vec<PolicyNote>,
}

/// [`preview`]（dry-run）の戻り値。
///
/// **帳簿は一切変更されていない**（採番も INSERT も行われていない）。
/// [`PostEntryOutput`] と違って `JournalEntry` を持たないのは、
/// 記帳していない仕訳に仕訳ID・仕訳番号は存在しないからである
/// （内部では検証のために仮の値で [`JournalEntry`] を組み立てるが、
/// その値が外へ出ることは無い）。
#[derive(Debug, Clone)]
pub struct PreviewEntryOutput {
    /// `auto_tax_lines` を適用した後の**確定後の明細**。
    ///
    /// これをそのまま `PostEntryInput::lines` に入れ、
    /// `auto_tax_lines: false` で [`execute`] を呼べば、同じ仕訳が記帳される
    /// （`docs/07-mcp-server.md` §3 の `hint.suggested_lines` はこの値）。
    pub lines: Vec<JournalLine>,

    /// `tax.derive_tax_lines` が添えた注記（[`PostEntryOutput::notes`] と同じ扱い）。
    pub notes: Vec<PolicyNote>,

    /// 確定後の明細の借方合計。
    pub debit_total: Money,

    /// 確定後の明細の貸方合計。
    ///
    /// [`PreviewEntryOutput`] が返る時点で `JournalEntry::new` の検証を
    /// 通っているため、`debit_total` と必ず一致する（貸借不一致なら
    /// [`preview`] は `Err` を返す）。両方を持つのは、上位層が
    /// 応答に載せるときに再計算しなくて済むようにするため。
    pub credit_total: Money,
}

/// [`execute`] / [`preview`] の失敗。**失敗経路でも `PolicyNote` を運ぶ。**
///
/// # なぜ `AppError` を裸で返さないのか
///
/// `PolicyNote` が最も必要なのは**失敗したとき**である。
/// 1巡目の実装では、`derive_tax_lines` が
/// `PolicyNote{Info, "税込経理の設定のため税額行を生成していません"}` を
/// 返しても、直後の `JournalEntry::new` が `Unbalanced` で落ちると
/// `notes` はスコープごと捨てられていた。
///
/// AI に届くのは「貸借不一致: 借方 110,000 / 貸方 100,000」だけで、
/// **なぜ税額行が生成されなかったのか**（税込経理の設定だから）が失われる。
/// これでは `docs/07-mcp-server.md` §1③「エラーは自己修正可能な形で返す」が
/// 空文になり、AI は「金額を直す」という誤った方向に進む。
///
/// # 使い方
///
/// - 分類コードは [`PostEntryFailure::code`]、外部に出す本文は
///   [`PostEntryFailure::public_message`]（どちらも `error` へ委譲する）。
/// - `AppError` だけあればよい呼び出し元は `From` で剥がせる
///   （`let err: AppError = failure.into();`）。ただし**剥がすと `notes` は
///   落ちる**ので、応答を組み立てる層では剥がさないこと。
/// - `notes` は失敗時も**空とは限らない**が、失敗が起きた位置によっては空になる
///   （手順2・3 で落ちた場合は `derive_tax_lines` の注記がまだ存在しない）。
#[derive(Debug, thiserror::Error)]
#[error("{error}")]
pub struct PostEntryFailure {
    /// 失敗の理由。
    #[source]
    pub error: AppError,

    /// 失敗するまでに policy が添えた注記。
    ///
    /// **空であることは「注記が無い」を意味しない**（[`PostEntryOutput::notes`]
    /// と同じ非対称。`auto_tax_lines: false` なら常に空）。
    pub notes: Vec<PolicyNote>,
}

impl PostEntryFailure {
    /// 注記を伴わない失敗を作る。
    fn bare(error: impl Into<AppError>) -> Self {
        PostEntryFailure {
            error: error.into(),
            notes: Vec::new(),
        }
    }

    /// 分類コード（[`crate::error::codes`]）。`error` へ委譲する。
    pub fn code(&self) -> &'static str {
        self.error.code()
    }

    /// 外部に出してよい本文（[`AppError::public_message`]）。`error` へ委譲する。
    pub fn public_message(&self) -> String {
        self.error.public_message()
    }
}

impl From<AppError> for PostEntryFailure {
    fn from(error: AppError) -> Self {
        PostEntryFailure::bare(error)
    }
}

/// `with_tx` が `begin` / `commit` の失敗をこのエラー型で表現できるようにする
/// （`crate::tx::with_tx` の `E: From<RepoError>` 境界）。
impl From<crate::error::RepoError> for PostEntryFailure {
    fn from(error: crate::error::RepoError) -> Self {
        PostEntryFailure::bare(AppError::Repo(error))
    }
}

/// `AppError` だけを扱う呼び出し元のための取り出し口。**`notes` は落ちる。**
impl From<PostEntryFailure> for AppError {
    fn from(failure: PostEntryFailure) -> Self {
        failure.error
    }
}

/// 手順1〜3の成果物。[`execute`] と [`preview`] が共有する。
struct PreparedEntry {
    fiscal_year: FiscalYear,
    chart: ChartOfAccounts,
    guard: ClosedPeriodGuard,
    /// `auto_tax_lines` 適用後の確定明細。
    lines: Vec<JournalLine>,
    /// `derive_tax_lines` が添えた注記。
    notes: Vec<PolicyNote>,
}

/// 手順1〜3（I/O + policy）。**[`execute`] と [`preview`] の唯一の入口。**
///
/// ここで失敗した場合、`derive_tax_lines` はまだ成功していないか呼ばれて
/// いないので注記は存在しない（`PostEntryFailure::notes` は空になる）。
async fn prepare<Tx>(
    tx: &mut Tx,
    tax: &dyn TaxPolicy,
    tag_schema: &TagSchema,
    settings: &BookSettings,
    entry_date: AccountingDate,
    lines: Vec<JournalLine>,
    auto_tax_lines: bool,
) -> Result<PreparedEntry, PostEntryFailure>
where
    Tx: TxOps,
{
    // 1. I/O
    let PostingContext {
        fiscal_year,
        chart,
        counterparties,
        guard,
    } = load_posting_context(tx, entry_date, settings)
        .await
        .map_err(PostEntryFailure::bare)?;

    let tax_ctx = TaxContext {
        as_of: entry_date,
        chart: &chart,
        tag_schema,
        counterparties: &counterparties,
    };

    // 2. 純関数: タグ検証(税額行の導出より前、元の明細に対して行う)。
    for line in &lines {
        let account_def = chart.get(line.account()).ok_or_else(|| {
            PostEntryFailure::bare(AppError::Core(CoreError::UnknownAccount {
                code: line.account().as_str().to_string(),
            }))
        })?;
        tax.validate_tag(&tax_ctx, line.tags(), account_def)
            .map_err(PostEntryFailure::bare)?;
    }

    // 3. 純関数: 税額行の導出（1回だけ）。derive_tax_lines は「確定後の明細
    //    一覧」を返すため、追加ではなく置き換える。
    //    `notes` は捨てずに運ぶ（成功時は戻り値へ、失敗時は
    //    `PostEntryFailure::notes` へ。D-070 の決定3 / D-073）。
    let (lines, notes) = if auto_tax_lines {
        let derivation = tax
            .derive_tax_lines(&tax_ctx, &lines)
            .map_err(PostEntryFailure::bare)?;
        (derivation.lines, derivation.notes)
    } else {
        (lines, Vec::new())
    };

    Ok(PreparedEntry {
        fiscal_year,
        chart,
        guard,
        lines,
        notes,
    })
}

/// 手順5（domain）。**[`execute`] と [`preview`] の唯一の検証経路。**
///
/// [`JournalEntry::new`] を呼ぶのはこの関数だけであり、検証内容
/// （明細数・科目の記帳可否・通貨・貸借・タグスキーマ・会計年度・締め状態・
/// 摘要）が2つの経路で食い違うことは原理的に起きない。
#[allow(clippy::too_many_arguments)]
fn build_entry(
    prepared_chart: &ChartOfAccounts,
    fiscal_year: &FiscalYear,
    guard: &ClosedPeriodGuard,
    tag_schema: &TagSchema,
    clock: &dyn AppClock,
    id: EntryId,
    entry_no: EntryNumber,
    entry_date: AccountingDate,
    description: String,
    lines: Vec<JournalLine>,
) -> Result<JournalEntry, CoreError> {
    JournalEntry::new(
        NewEntry {
            id,
            entry_no,
            entry_date,
            description,
            lines,
            document_refs: Vec::new(),
        },
        fiscal_year,
        prepared_chart,
        tag_schema,
        guard,
        clock,
    )
}

/// 仕訳を記帳する。
///
/// トランザクションの開始・確定・破棄は行わない（呼び出し側が
/// [`crate::tx::with_tx`] で管理する）。実行順序は本モジュール doc を参照。
///
/// 戻り値は [`PostEntryOutput`]（記帳された仕訳 + `PolicyNote` の一覧）。
///
/// # Errors
///
/// [`PostEntryFailure`]（`error` に理由、`notes` にそこまでの注記）。
/// `error` の内訳:
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
) -> Result<PostEntryOutput, PostEntryFailure>
where
    Tx: TxOps,
{
    // 1〜3
    let PreparedEntry {
        fiscal_year,
        chart,
        guard,
        lines,
        notes,
    } = prepare(
        tx,
        tax,
        tag_schema,
        settings,
        input.entry_date,
        input.lines,
        input.auto_tax_lines,
    )
    .await?;

    // 4〜6。ここから先の失敗にも `notes` を添える（下の `match`）。
    let posted = post_prepared(
        tx,
        &chart,
        &fiscal_year,
        &guard,
        tag_schema,
        id_gen,
        clock,
        input.entry_date,
        input.description,
        lines,
    )
    .await;

    match posted {
        Ok(entry) => Ok(PostEntryOutput { entry, notes }),
        Err(error) => Err(PostEntryFailure { error, notes }),
    }
}

/// 手順4〜6（採番 → domain → INSERT）。[`execute`] だけが通る。
///
/// [`preview`] がこの関数を通らないことが「記帳しない」の実体である。
#[allow(clippy::too_many_arguments)]
async fn post_prepared<Tx>(
    tx: &mut Tx,
    chart: &ChartOfAccounts,
    fiscal_year: &FiscalYear,
    guard: &ClosedPeriodGuard,
    tag_schema: &TagSchema,
    id_gen: &dyn IdGenerator,
    clock: &dyn AppClock,
    entry_date: AccountingDate,
    description: String,
    lines: Vec<JournalLine>,
) -> Result<JournalEntry, AppError>
where
    Tx: TxOps,
{
    // 4. I/O: 失敗しうる検証を全て終えた直後・INSERT の直前で採番する。
    let entry_no = tx.next_entry_no(fiscal_year.label()).await?;

    // 5. domain: これより後は lines に触れない。
    let entry = build_entry(
        chart,
        fiscal_year,
        guard,
        tag_schema,
        clock,
        id_gen.new_entry_id(),
        entry_no,
        entry_date,
        description,
        lines,
    )?;

    // 6. I/O
    tx.insert_entry(&entry).await?;

    Ok(entry)
}

/// **記帳せずに**、記帳した場合の確定明細と注記を返す（dry-run）。
///
/// `docs/07-mcp-server.md` §3 の `hint.suggested_lines`（「税額行を追加すれば
/// 貸借が合う」という修正案）を上位層が組み立てるための入口。
/// 典型的な使い方は次のとおり:
///
/// 1. `auto_tax_lines: false` の [`execute`] が `unbalanced` で失敗する
/// 2. 同じ明細を `auto_tax_lines: true` にして [`preview`] を呼ぶ
/// 3. `Ok` なら [`PreviewEntryOutput::lines`] を `hint.suggested_lines` に載せる。
///    `Err` なら `hint` を返さない（税額行を足しても解決しないため）
///
/// # 帳簿に一切触れない
///
/// - `tx.next_entry_no` を呼ばない（**採番しない**）。したがって
///   dry-run を何回呼んでも仕訳番号は飛ばない。
/// - `tx.insert_entry` を呼ばない。
/// - `id_gen` を引数に取らない（生成した仕訳IDが使われないため。
///   `IdGenerator` の実装によっては呼ぶこと自体が状態を進める）。
///
/// 内部では検証のために [`JournalEntry`] を仮の仕訳ID・仕訳番号で組み立てるが、
/// その `JournalEntry` は関数外へ出ない（[`PreviewEntryOutput`] は
/// `JournalEntry` を持たない）。
///
/// # 読み取りだけだがトランザクションを取る
///
/// 引数は [`execute`] と同じく `&mut Tx` である。勘定科目表と締め状態を
/// 読むのに [`crate::ports::ChartRepo`] / [`crate::ports::PeriodRepo`] が要り、
/// かつ **`execute` と同じスナップショットで検証したい**ため
/// （`with_tx` の中で `preview` → `execute` と続けて呼べば、両者が同じ
/// 勘定科目表・締め状態を見ることが保証される）。
///
/// # Errors
///
/// [`execute`] と**同じ条件で同じ `error`** を返す（手順4・6 の I/O 起因の
/// ものを除く）。検証の順序も同じ（本モジュール doc の表を参照）。
pub async fn preview<Tx>(
    tx: &mut Tx,
    tax: &dyn TaxPolicy,
    tag_schema: &TagSchema,
    clock: &dyn AppClock,
    settings: &BookSettings,
    input: PostEntryInput,
) -> Result<PreviewEntryOutput, PostEntryFailure>
where
    Tx: TxOps,
{
    // 1〜3（execute と同じ関数）
    let PreparedEntry {
        fiscal_year,
        chart,
        guard,
        lines,
        notes,
    } = prepare(
        tx,
        tax,
        tag_schema,
        settings,
        input.entry_date,
        input.lines,
        input.auto_tax_lines,
    )
    .await?;

    // 5（execute と同じ関数）。4（採番）と 6（INSERT）は行わない。
    //
    // 仮の仕訳ID・仕訳番号を使う。`JournalEntry::new` はこの2つを検証せず
    // （`kaikei-core/src/journal.rs` の検証項目1〜8に含まれない）、
    // 組み立てた `JournalEntry` はこの関数の外に出ないため、
    // 「記帳していない仕訳に採番済みの番号が付いて見える」事故は起きない。
    let entry = build_entry(
        &chart,
        &fiscal_year,
        &guard,
        tag_schema,
        clock,
        EntryId::new(0),
        EntryNumber::new(0),
        input.entry_date,
        input.description,
        lines,
    );

    match entry {
        Ok(entry) => Ok(PreviewEntryOutput {
            lines: entry.lines().to_vec(),
            debit_total: entry.debit_total(),
            credit_total: entry.credit_total(),
            notes,
        }),
        Err(error) => Err(PostEntryFailure {
            error: AppError::Core(error),
            notes,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{fixed_clock, sample_chart, sample_chart_with_tax_account, settings};
    use crate::testing::{InMemoryStore, SequentialIdGenerator};
    use crate::tx::with_tx_err;
    use kaikei_core::{AccountCode, AccountDef, Currency, Money, RoundMode, Side, TagSet};
    use kaikei_policy::testing::{FlatRateTaxPolicy, NoTaxPolicy};
    use kaikei_policy::{NoteSeverity, PolicyError, TaxDerivation};

    /// 明細を一切変えずに `PolicyNote` だけを添える `TaxPolicy`。
    ///
    /// `kaikei-jp` の「非適格の経過措置」（`deduction_ratio < 1` の税区分）が
    /// 取る挙動そのものを最小化したもの: **税額計算には反映せず、注記にだけ
    /// 現れる**（`DECISIONS.md` D-059）。`kaikei-app` は `kaikei-jp` に依存
    /// できないため（`CLAUDE.md` §1）、その形をここで再現する。実際の区分
    /// （`PURCHASE_10_NON_QUALIFIED`）を使った検証は `kaikei-e2e` 側にある。
    struct DeductionRatioNotingTaxPolicy;

    impl TaxPolicy for DeductionRatioNotingTaxPolicy {
        fn validate_tag(
            &self,
            _ctx: &TaxContext<'_>,
            _tags: &TagSet,
            _account: &AccountDef,
        ) -> Result<(), PolicyError> {
            Ok(())
        }

        fn derive_tax_lines(
            &self,
            _ctx: &TaxContext<'_>,
            lines: &[JournalLine],
        ) -> Result<TaxDerivation, PolicyError> {
            Ok(TaxDerivation {
                lines: lines.to_vec(),
                notes: vec![PolicyNote {
                    severity: NoteSeverity::Warning,
                    message: "控除割合の制限がある税区分が含まれています。\
                              適用可否は税理士にご確認ください"
                        .to_string(),
                }],
            })
        }

        fn round_mode(&self, _ctx: &TaxContext<'_>) -> RoundMode {
            RoundMode::Floor
        }
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

        let result: Result<PostEntryOutput, PostEntryFailure> = with_tx_err(&store, |tx| {
            Box::pin(
                async move { execute(tx, &tax, &schema, &id_gen, &clock, &settings, input).await },
            )
        })
        .await;

        let output = result.unwrap();
        assert_eq!(output.entry.entry_no().as_u32(), 1);
        // auto_tax_lines: false のときは derive_tax_lines を呼ばないので notes は空。
        assert!(output.notes.is_empty());
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

        let result: Result<PostEntryOutput, PostEntryFailure> = with_tx_err(&store, |tx| {
            Box::pin(
                async move { execute(tx, &tax, &schema, &id_gen, &clock, &settings, input).await },
            )
        })
        .await;

        assert!(matches!(
            result,
            Err(PostEntryFailure {
                error: AppError::Core(CoreError::Unbalanced { .. }),
                ..
            })
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

        let result: Result<PostEntryOutput, PostEntryFailure> = with_tx_err(&store, |tx| {
            Box::pin(
                async move { execute(tx, &tax, &schema, &id_gen, &clock, &settings, input).await },
            )
        })
        .await;

        assert!(matches!(
            result,
            Err(PostEntryFailure {
                error: AppError::Core(CoreError::PeriodClosed { .. }),
                ..
            })
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

        let result: Result<PostEntryOutput, PostEntryFailure> = with_tx_err(&store, |tx| {
            Box::pin(
                async move { execute(tx, &tax, &schema, &id_gen, &clock, &settings, input).await },
            )
        })
        .await;

        let output = result.unwrap();
        let entry = &output.entry;
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

        let result: Result<PostEntryOutput, PostEntryFailure> = with_tx_err(&store, |tx| {
            Box::pin(
                async move { execute(tx, &tax, &schema, &id_gen, &clock, &settings, input).await },
            )
        })
        .await;

        assert_eq!(result.unwrap().entry.lines().len(), 2);
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

        let result: Result<PostEntryOutput, PostEntryFailure> = with_tx_err(&store, |tx| {
            Box::pin(
                async move { execute(tx, &tax, &schema, &id_gen, &clock, &settings, input).await },
            )
        })
        .await;

        assert!(matches!(
            result,
            Err(PostEntryFailure {
                error: AppError::Core(CoreError::UnknownAccount { .. }),
                ..
            })
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
    ) -> Result<PostEntryOutput, PostEntryFailure> {
        let tax = NoTaxPolicy;
        let schema = TagSchema::empty();
        let id_gen = SequentialIdGenerator::starting_at(id_gen_start);
        let clock = fixed_clock();
        let settings = settings();

        with_tx_err(store, |tx| {
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

        assert_eq!(succeeding_result.unwrap().entry.entry_no().as_u32(), 1);
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
            Err(PostEntryFailure {
                error: AppError::Core(CoreError::TooFewLines { found: 0 }),
                ..
            })
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
            Err(PostEntryFailure {
                error: AppError::Core(CoreError::TooFewLines { found: 1 }),
                ..
            })
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

        let result: Result<PostEntryOutput, PostEntryFailure> = with_tx_err(&store, |tx| {
            Box::pin(
                async move { execute(tx, &tax, &schema, &id_gen, &clock, &settings, input).await },
            )
        })
        .await;

        assert!(matches!(
            result,
            Err(PostEntryFailure {
                error: AppError::Policy(_),
                ..
            })
        ));

        // 採番が進んでいないため、次に成功する記帳は entry_no = 1 になる。
        let succeeding_input = PostEntryInput {
            entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
            description: "正常".to_string(),
            lines: balanced_lines(),
            auto_tax_lines: false,
        };
        let succeeding_result = run_post_entry(&store, 2, succeeding_input).await;
        assert_eq!(succeeding_result.unwrap().entry.entry_no().as_u32(), 1);
    }

    // PE-11（PR-B）: `derive_tax_lines` が返した `PolicyNote` が戻り値に含まれる。
    // Phase 2 の申し送り「`PolicyNote` が永続化されない（`.notes` を捨てている）」
    // への回答（`DECISIONS.md` D-070 の決定3 / D-073）。
    #[tokio::test]
    async fn post_entry_returns_policy_notes_from_derive_tax_lines() {
        let store = InMemoryStore::with_chart(sample_chart());
        let tax = DeductionRatioNotingTaxPolicy;
        let schema = TagSchema::empty();
        let id_gen = SequentialIdGenerator::starting_at(1);
        let clock = fixed_clock();
        let settings = settings();

        let input = PostEntryInput {
            entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
            description: "控除割合に制限のある仕入".to_string(),
            lines: balanced_lines(),
            auto_tax_lines: true,
        };

        let result: Result<PostEntryOutput, PostEntryFailure> = with_tx_err(&store, |tx| {
            Box::pin(
                async move { execute(tx, &tax, &schema, &id_gen, &clock, &settings, input).await },
            )
        })
        .await;

        let output = result.unwrap();
        assert_eq!(output.notes.len(), 1, "注記が戻り値から落ちている");
        assert_eq!(output.notes[0].severity, NoteSeverity::Warning);
        // policy が組み立てた文言をそのまま素通しする（言い換えない。CLAUDE.md §10）。
        assert!(output.notes[0].message.contains("税理士"));
        // 注記は税額計算に反映されない（明細は入力のまま2行）。
        assert_eq!(output.entry.lines().len(), 2);
    }

    // PE-12（PR-B）: `auto_tax_lines: false` では `derive_tax_lines` を呼ばないため
    // 注記は生じない（policy が注記を返す実装であっても空になる）。
    #[tokio::test]
    async fn post_entry_returns_no_notes_when_auto_tax_lines_is_disabled() {
        let store = InMemoryStore::with_chart(sample_chart());
        let tax = DeductionRatioNotingTaxPolicy;
        let schema = TagSchema::empty();
        let id_gen = SequentialIdGenerator::starting_at(1);
        let clock = fixed_clock();
        let settings = settings();

        let input = PostEntryInput {
            entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
            description: "税額行の自動生成なし".to_string(),
            lines: balanced_lines(),
            auto_tax_lines: false,
        };

        let result: Result<PostEntryOutput, PostEntryFailure> = with_tx_err(&store, |tx| {
            Box::pin(
                async move { execute(tx, &tax, &schema, &id_gen, &clock, &settings, input).await },
            )
        })
        .await;

        assert!(result.unwrap().notes.is_empty());
    }

    // ---- C-1: 失敗経路でも PolicyNote を運ぶ（PR-B 2巡目） ----

    /// 「税込経理の設定なので税額行を生成しなかった」ことだけを注記し、
    /// 明細は入力のまま返す `TaxPolicy`。
    ///
    /// `kaikei-jp` の `JpTaxPolicy` は `tax_mode: inclusive`（税込経理）や
    /// 免税事業者の設定のとき、まさにこの挙動を取る
    /// （`docs/07-mcp-server.md` §3「税込経理または免税事業者の設定では
    /// `derive_tax_lines` が入力明細をそのまま返すため、同じリクエストが
    /// 貸借不一致になる」）。`kaikei-app` は `kaikei-jp` に依存できないため
    /// （`CLAUDE.md` §1）、その形をここで最小再現する。
    struct InclusiveModeTaxPolicy;

    impl TaxPolicy for InclusiveModeTaxPolicy {
        fn validate_tag(
            &self,
            _ctx: &TaxContext<'_>,
            _tags: &TagSet,
            _account: &AccountDef,
        ) -> Result<(), PolicyError> {
            Ok(())
        }

        fn derive_tax_lines(
            &self,
            _ctx: &TaxContext<'_>,
            lines: &[JournalLine],
        ) -> Result<TaxDerivation, PolicyError> {
            Ok(TaxDerivation {
                lines: lines.to_vec(),
                notes: vec![PolicyNote {
                    severity: NoteSeverity::Info,
                    message: "税込経理の設定のため税額行を生成していません".to_string(),
                }],
            })
        }

        fn round_mode(&self, _ctx: &TaxContext<'_>) -> RoundMode {
            RoundMode::Floor
        }
    }

    /// 借方 110,000 / 貸方 100,000（税額行が無いと貸借不一致になる明細）。
    fn unbalanced_lines_missing_the_tax_line() -> Vec<JournalLine> {
        vec![
            JournalLine::new(
                AccountCode::parse("100").unwrap(),
                Side::Debit,
                Money::from_minor(110_000, Currency::JPY),
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
        ]
    }

    // PE-13（PR-B 2巡目 / C-1）: `derive_tax_lines` が成功して注記を返した後に
    // `JournalEntry::new` が Unbalanced で落ちても、注記は失敗値に載って届く。
    //
    // ★このテストが落ちたら、AI に届くのは「貸借不一致」だけになり、
    // 「税込経理の設定だから税額行が生成されなかった」という**唯一の手がかり**が
    // 失われる（`docs/07-mcp-server.md` §1③ が空文になる）。
    #[tokio::test]
    async fn post_entry_carries_policy_notes_on_the_failure_path() {
        let store = InMemoryStore::with_chart(sample_chart());
        let tax = InclusiveModeTaxPolicy;
        let schema = TagSchema::empty();
        let id_gen = SequentialIdGenerator::starting_at(1);
        let clock = fixed_clock();
        let settings = settings();

        let input = PostEntryInput {
            entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
            description: "税込経理の設定で税抜金額を渡した".to_string(),
            lines: unbalanced_lines_missing_the_tax_line(),
            auto_tax_lines: true,
        };

        let result: Result<PostEntryOutput, PostEntryFailure> = with_tx_err(&store, |tx| {
            Box::pin(
                async move { execute(tx, &tax, &schema, &id_gen, &clock, &settings, input).await },
            )
        })
        .await;

        let failure = result.unwrap_err();
        assert!(matches!(
            failure.error,
            AppError::Core(CoreError::Unbalanced { .. })
        ));
        assert_eq!(failure.code(), crate::error::codes::UNBALANCED);
        assert_eq!(failure.notes.len(), 1, "失敗経路で注記が落ちている");
        assert_eq!(failure.notes[0].severity, NoteSeverity::Info);
        assert!(failure.notes[0].message.contains("税込経理"));
        // 帳簿は変わっていない。
        assert!(store.committed_entries().is_empty());
    }

    // PE-14（PR-B 2巡目 / C-1）: 手順1〜3で落ちた場合、注記はまだ存在しないので空。
    // 「空＝注記が無い」ではないこと（doc に明記した非対称）の裏取り。
    #[tokio::test]
    async fn post_entry_failure_before_derivation_carries_no_notes() {
        let store = InMemoryStore::with_chart(sample_chart());
        let tax = InclusiveModeTaxPolicy;
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
            auto_tax_lines: true,
        };

        let result: Result<PostEntryOutput, PostEntryFailure> = with_tx_err(&store, |tx| {
            Box::pin(
                async move { execute(tx, &tax, &schema, &id_gen, &clock, &settings, input).await },
            )
        })
        .await;

        let failure = result.unwrap_err();
        assert_eq!(failure.code(), crate::error::codes::UNKNOWN_ACCOUNT);
        assert!(failure.notes.is_empty());
    }

    // PE-15（PR-B 2巡目 / C-1）: `AppError` だけを見たい呼び出し元は
    // `From` で剥がせる（剥がすと注記は落ちる、という契約の明示）。
    #[test]
    fn post_entry_failure_can_be_flattened_into_an_app_error() {
        let failure = PostEntryFailure {
            error: AppError::EmptyReverseReason,
            notes: vec![PolicyNote {
                severity: NoteSeverity::Info,
                message: "注記".to_string(),
            }],
        };
        assert_eq!(failure.code(), crate::error::codes::EMPTY_REVERSE_REASON);
        assert_eq!(failure.public_message(), failure.error.public_message());
        let flattened: AppError = failure.into();
        assert_eq!(flattened.code(), crate::error::codes::EMPTY_REVERSE_REASON);
    }

    // ---- C-2: dry-run（preview）（PR-B 2巡目） ----

    /// `store` に対して1回分の `preview` を実行する。
    async fn run_preview(
        store: &InMemoryStore,
        tax_account: &'static str,
        input: PostEntryInput,
    ) -> Result<PreviewEntryOutput, PostEntryFailure> {
        let tax = FlatRateTaxPolicy {
            rate: kaikei_core::Ratio::parse_rate("0.10").unwrap(),
            tax_account,
        };
        let schema = TagSchema::empty();
        let clock = fixed_clock();
        let settings = settings();

        with_tx_err(store, |tx| {
            Box::pin(async move { preview(tx, &tax, &schema, &clock, &settings, input).await })
        })
        .await
    }

    fn tax_exclusive_lines() -> Vec<JournalLine> {
        vec![
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
        ]
    }

    // PE-16: preview は導出された明細（税額行を含む）を返す。
    // これが `docs/07-mcp-server.md` §3 の `hint.suggested_lines` になる。
    #[tokio::test]
    async fn preview_returns_the_derived_lines_without_posting() {
        let store = InMemoryStore::with_chart(sample_chart_with_tax_account());

        let input = PostEntryInput {
            entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
            description: "税額行を足せば貸借が合う".to_string(),
            lines: tax_exclusive_lines(),
            auto_tax_lines: true,
        };

        let output = run_preview(&store, "330", input).await.unwrap();

        assert_eq!(output.lines.len(), 4, "税額行が2行追加される");
        assert_eq!(output.debit_total.minor(), 110_000);
        assert_eq!(output.credit_total.minor(), 110_000);
    }

    // PE-17: ★preview は記帳しない★（INSERT も採番もしない）。
    //
    // 「dry-run」を名乗る以上ここが崩れたら意味が無い。採番については、
    // preview を何回呼んでも次の記帳が entry_no = 1 になることで確認する
    // （番号が飛べば `next_entry_no` を呼んでいる）。
    #[tokio::test]
    async fn preview_neither_inserts_nor_consumes_an_entry_number() {
        let store = InMemoryStore::with_chart(sample_chart_with_tax_account());

        for _ in 0..3 {
            let input = PostEntryInput {
                entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
                description: "dry-run".to_string(),
                lines: tax_exclusive_lines(),
                auto_tax_lines: true,
            };
            run_preview(&store, "330", input).await.unwrap();
        }

        assert!(store.committed_entries().is_empty(), "記帳されている");

        // 採番も進んでいないので、次の記帳は entry_no = 1 になる。
        let posted = run_post_entry(
            &store,
            1,
            PostEntryInput {
                entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
                description: "本記帳".to_string(),
                lines: balanced_lines(),
                auto_tax_lines: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(posted.entry.entry_no().as_u32(), 1);
    }

    // PE-18: ★preview の結果をそのまま post すると同じ仕訳になる★
    //
    // `hint.suggested_lines` を受け取った AI が
    // 「その明細をそのまま `auto_tax_lines: false` で post する」という
    // 想定どおりの経路が本当に通ることを、実際に両方を実行して突き合わせる。
    #[tokio::test]
    async fn preview_and_execute_agree_on_the_final_lines() {
        let store = InMemoryStore::with_chart(sample_chart_with_tax_account());
        let entry_date = AccountingDate::new(2026, 4, 1).unwrap();

        let previewed = run_preview(
            &store,
            "330",
            PostEntryInput {
                entry_date,
                description: "税抜経理".to_string(),
                lines: tax_exclusive_lines(),
                auto_tax_lines: true,
            },
        )
        .await
        .unwrap();

        // (a) preview が返した明細を auto_tax_lines: false で記帳する。
        let from_hint = {
            let tax = NoTaxPolicy;
            let schema = TagSchema::empty();
            let id_gen = SequentialIdGenerator::starting_at(1);
            let clock = fixed_clock();
            let settings = settings();
            let input = PostEntryInput {
                entry_date,
                description: "税抜経理".to_string(),
                lines: previewed.lines.clone(),
                auto_tax_lines: false,
            };
            let result: Result<PostEntryOutput, PostEntryFailure> = with_tx_err(&store, |tx| {
                Box::pin(async move {
                    execute(tx, &tax, &schema, &id_gen, &clock, &settings, input).await
                })
            })
            .await;
            result.unwrap().entry
        };

        // (b) 同じ入力を auto_tax_lines: true で記帳する。
        let from_auto = {
            let tax = FlatRateTaxPolicy {
                rate: kaikei_core::Ratio::parse_rate("0.10").unwrap(),
                tax_account: "330",
            };
            let schema = TagSchema::empty();
            let id_gen = SequentialIdGenerator::starting_at(100);
            let clock = fixed_clock();
            let settings = settings();
            let input = PostEntryInput {
                entry_date,
                description: "税抜経理".to_string(),
                lines: tax_exclusive_lines(),
                auto_tax_lines: true,
            };
            let result: Result<PostEntryOutput, PostEntryFailure> = with_tx_err(&store, |tx| {
                Box::pin(async move {
                    execute(tx, &tax, &schema, &id_gen, &clock, &settings, input).await
                })
            })
            .await;
            result.unwrap().entry
        };

        let describe = |entry: &JournalEntry| -> Vec<(String, bool, i128)> {
            entry
                .lines()
                .iter()
                .map(|line| {
                    (
                        line.account().as_str().to_string(),
                        line.is_debit(),
                        line.amount().minor(),
                    )
                })
                .collect()
        };
        assert_eq!(describe(&from_hint), describe(&from_auto));
    }

    // PE-19: ★preview と execute は同じ入力に対して同じエラーを返す★
    //
    // 検証の順序が両者で乖離していないことの実行時の裏取り
    // （構造上は `prepare` / `build_entry` を共有しているので乖離しえないが、
    // 将来どちらかにだけ手順を足す改変をこのテストが検出する）。
    #[tokio::test]
    async fn preview_and_execute_agree_on_rejections() {
        let entry_date = AccountingDate::new(2026, 4, 1).unwrap();

        // (1) 貸借不一致 (2) 明細1行 (3) 未知科目 (4) 摘要が空
        let cases: Vec<(&str, PostEntryInput)> = vec![
            (
                "unbalanced",
                PostEntryInput {
                    entry_date,
                    description: "不一致".to_string(),
                    lines: unbalanced_lines_missing_the_tax_line(),
                    auto_tax_lines: false,
                },
            ),
            (
                "too_few_lines",
                PostEntryInput {
                    entry_date,
                    description: "1行".to_string(),
                    lines: vec![balanced_lines().remove(0)],
                    auto_tax_lines: false,
                },
            ),
            (
                "unknown_account",
                PostEntryInput {
                    entry_date,
                    description: "未知科目".to_string(),
                    lines: vec![
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
                    ],
                    auto_tax_lines: false,
                },
            ),
            (
                "empty_description",
                PostEntryInput {
                    entry_date,
                    description: "   ".to_string(),
                    lines: balanced_lines(),
                    auto_tax_lines: false,
                },
            ),
        ];

        for (expected_code, input) in cases {
            let store = InMemoryStore::with_chart(sample_chart_with_tax_account());

            let previewed = {
                let tax = NoTaxPolicy;
                let schema = TagSchema::empty();
                let clock = fixed_clock();
                let settings = settings();
                let input = input.clone();
                let result: Result<PreviewEntryOutput, PostEntryFailure> =
                    with_tx_err(&store, |tx| {
                        Box::pin(async move {
                            preview(tx, &tax, &schema, &clock, &settings, input).await
                        })
                    })
                    .await;
                result.unwrap_err()
            };
            let posted = run_post_entry(&store, 1, input).await.unwrap_err();

            assert_eq!(previewed.code(), expected_code);
            assert_eq!(
                previewed.code(),
                posted.code(),
                "preview と execute でエラーが食い違っている（検証順序の乖離）"
            );
            assert_eq!(previewed.to_string(), posted.to_string());
        }
    }

    // PE-20: preview も失敗経路で注記を運ぶ（`execute` と同じ扱い）。
    #[tokio::test]
    async fn preview_carries_policy_notes_on_the_failure_path() {
        let store = InMemoryStore::with_chart(sample_chart());
        let tax = InclusiveModeTaxPolicy;
        let schema = TagSchema::empty();
        let clock = fixed_clock();
        let settings = settings();

        let input = PostEntryInput {
            entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
            description: "税込経理の設定".to_string(),
            lines: unbalanced_lines_missing_the_tax_line(),
            auto_tax_lines: true,
        };

        let result: Result<PreviewEntryOutput, PostEntryFailure> = with_tx_err(&store, |tx| {
            Box::pin(async move { preview(tx, &tax, &schema, &clock, &settings, input).await })
        })
        .await;

        let failure = result.unwrap_err();
        assert_eq!(failure.code(), crate::error::codes::UNBALANCED);
        assert_eq!(failure.notes.len(), 1);
        assert!(failure.notes[0].message.contains("税込経理"));
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
                let result: Result<PostEntryOutput, PostEntryFailure> = runtime.block_on(with_tx_err(&store, |tx| {
                    Box::pin(async move {
                        execute(tx, &tax, &schema, &id_gen, &clock, &settings, input).await
                    })
                }));

                let entry = result.unwrap().entry;
                prop_assert_eq!(entry.debit_total().minor(), entry.credit_total().minor());
            }
        }
    }
}
