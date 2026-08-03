//! 訂正ユースケース（[`execute`]）。逆仕訳（赤伝）による訂正のみ。
//!
//! `TaxPolicy` を引数に取らない。明細は [`kaikei_core::JournalEntry::reverse`]
//! が貸借を反転して複製するだけであり、税額行を再度導出すると二重計上になる。
//!
//! 二重訂正（既に赤伝済みの仕訳を再度訂正すること）は既定で拒否する
//! （[`AppError::AlreadyReversed`]）。`allow_double_reversal: true` を明示した
//! 場合のみ許可する。なお、この拒否は「同じ元仕訳を2回赤伝すること」を
//! 対象とし、「赤伝そのものをさらに訂正すること」（逆仕訳の逆仕訳）は別扱い
//! （`original_id` に元の仕訳ではなく赤伝のIDを指定すれば通常どおり許可される。
//! `kaikei_core::JournalEntry::reverse` が意図的に許可している設計と対応する）。
//!
//! # 実行順序
//!
//! 0. **純関数**: 訂正理由（`reason`）が空文字・空白のみでないことを確認する。
//!    I/O より前に置く（採番も読み込みも行わずに弾く）
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
//!
//! テストID（`RE-1` 等）はこのファイル内でのみ一意な連番であり、
//! `docs/02-test-cases.md` のID体系とは独立している。

use crate::context::{load_posting_context, BookSettings, PostingContext};
use crate::error::{AppError, RepoError};
use crate::id::entry_id_to_uuid_string;
use crate::ports::{AppClock, IdGenerator, TxOps};
use kaikei_core::{AccountingDate, EntryId, JournalEntry, TagSchema};

/// [`execute`] への入力。
#[derive(Debug, Clone)]
pub struct ReverseEntryInput {
    /// 訂正対象の仕訳ID。
    pub original_id: EntryId,
    /// 逆仕訳の取引日。会計年度はこの日付から決定する。
    pub reverse_date: AccountingDate,
    /// 訂正理由。**空文字・空白のみは受け付けない**
    /// （[`execute`] が [`AppError::EmptyReverseReason`] で拒否する）。
    ///
    /// 型は素の `String` のまま（非空を型で保証する newtype を作ると、
    /// `kaikei-core` の `JournalEntry::reverse` が `String` を取る以上
    /// 境界で必ず剥がすことになり、検証の置き場が2箇所になる）。
    pub reason: String,
    /// `true` の場合、既に赤伝済みの仕訳を再度訂正することを明示的に許可する。
    /// 既定（`false`）では二重訂正は拒否される。
    pub allow_double_reversal: bool,
}

/// [`execute`] の戻り値。
///
/// **`notes`（`PolicyNote`）を持たない。** `reverse_entry` は `TaxPolicy` を
/// 引数に取らず（明細は貸借を反転して複製するだけで、税額行を再導出すると
/// 二重計上になる）、注記の発生経路そのものが存在しないため。
///
/// 常に空の `notes` を置いて [`crate::usecase::post_entry::PostEntryOutput`]
/// と形を揃えることは**しない**。空配列は「policy を通したが注記が無かった」
/// と区別できず、値は正しいのに診断が誤った方向へ導く型の欠陥になる
/// （`PROGRESS.md` Phase 1 の教訓3「MCP 経由で AI が自己修正する前提の
/// システムでは、誤診は誤値と同じ実害を持つ」）。MCP の応答でも
/// `policy_notes` は**キーごと出さない**（`docs/07-mcp-server.md` §3）。
///
/// それでも `JournalEntry` を裸で返さず構造体で包むのは、post 側と
/// 呼び出しの形（`output.entry`）を揃えるため、および将来フィールドを
/// 足すときに読み取り側を壊さないため（`DECISIONS.md` D-073）。
#[derive(Debug, Clone)]
pub struct ReverseEntryOutput {
    /// 作成された逆仕訳（赤伝）。
    pub entry: JournalEntry,
}

/// 仕訳を訂正する（逆仕訳を作る）。
///
/// トランザクションの開始・確定・破棄は行わない（呼び出し側が
/// [`crate::tx::with_tx`] で管理する）。実行順序は本モジュール doc を参照。
///
/// 戻り値は [`ReverseEntryOutput`]。
///
/// # Errors
///
/// - `input.reason` が空文字または空白のみの場合は
///   [`AppError::EmptyReverseReason`]（I/O を1回も行わずに返す）
/// - `input.original_id` の仕訳が存在しない場合は
///   [`AppError::Repo`]（[`RepoError::NotFound`]）
/// - `input.allow_double_reversal` が `false` で、`input.original_id` が
///   既に他の仕訳から訂正されている場合は [`AppError::AlreadyReversed`]
/// - `tx` からの読み込み（勘定科目表・締め状態・採番）・書き込み
///   （`insert_entry`）が失敗した場合は [`AppError::Repo`]
/// - [`kaikei_core::JournalEntry::reverse`] の検証（科目の記帳可否・通貨・
///   タグスキーマ・会計年度・締め状態・摘要）に失敗した場合は
///   [`AppError::Core`]
pub async fn execute<Tx>(
    tx: &mut Tx,
    tag_schema: &TagSchema,
    id_gen: &dyn IdGenerator,
    clock: &dyn AppClock,
    settings: &BookSettings,
    input: ReverseEntryInput,
) -> Result<ReverseEntryOutput, AppError>
where
    Tx: TxOps,
{
    // 0. 純関数: 訂正理由の非空検証。
    //
    //    下位層はいずれもこれを通す（`JournalEntry::reverse` は代入するだけ、
    //    DB の CHECK は NULL の一致しか見ない）。**MCP 層ではなくここで弾く**
    //    のは、将来の CLI / HTTP API など MCP 以外の呼び出し元にも同じ規律を
    //    効かせるため（`DECISIONS.md` D-074）。
    //
    //    `str::trim` は Unicode の空白を落とすため、全角スペース（U+3000）
    //    のみの理由もここで弾かれる。
    //
    //    **理由の文字列自体は加工しない**（トリムした値を保存しない）。
    //    帳簿に残る文言を利用者に断りなく書き換えないため（D-052 と同じ姿勢）。
    if input.reason.trim().is_empty() {
        return Err(AppError::EmptyReverseReason);
    }

    // 1. I/O: 訂正対象の仕訳を読み込む。
    let original = tx.find_entry(input.original_id).await?.ok_or_else(|| {
        AppError::Repo(RepoError::NotFound {
            // 仕訳IDは **UUID の正準表記**で返す（`docs/07-mcp-server.md` §3）。
            // `EntryId::as_u128()` の10進表記（最大39桁）で組み立てると、
            // 呼び出し元が送った UUID 文字列と突き合わせられない。
            reason: format!(
                "仕訳が見つかりません（仕訳ID: {}）。\
                 仕訳IDが正しいか確認してください",
                entry_id_to_uuid_string(input.original_id)
            ),
        })
    })?;

    // 2. I/O: 二重訂正の検出。既定では拒否する。
    //
    //    `find_reversal_of` が返す `EntryId` を**捨てない**（PR-B 2巡目）。
    //    呼び出し元が仕訳を指すのは UUID であって通し番号ではないため、
    //    番号だけ返すと AI は「既にある赤伝を見る」ことができない
    //    （`JournalRepo` には番号から引く経路が無い）。
    if !input.allow_double_reversal {
        if let Some((reversal_id, reversal_no)) = tx.find_reversal_of(input.original_id).await? {
            return Err(AppError::AlreadyReversed {
                entry_no: original.entry_no(),
                reversal_no,
                reversal_id,
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

    Ok(ReverseEntryOutput { entry })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{JournalRepo, NumberingRepo, Store, TxScope};
    use crate::test_support::{fixed_clock, sample_chart, settings, AllOpen};
    use crate::testing::{InMemoryStore, SequentialIdGenerator};
    use crate::tx::with_tx;
    use kaikei_core::{AccountCode, Currency, JournalLine, Money, NewEntry, Side, TagSet};

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
    ) -> Result<ReverseEntryOutput, AppError> {
        let schema = TagSchema::empty();
        let id_gen = SequentialIdGenerator::starting_at(id_gen_start);
        let clock = fixed_clock();
        let settings = settings();

        with_tx(store, |tx| {
            Box::pin(async move { execute(tx, &schema, &id_gen, &clock, &settings, input).await })
        })
        .await
    }

    // RE-1: 正常系。逆仕訳が貸借反転して作られ、insert_entry まで到達する。
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

        let reversal = result.unwrap().entry;
        assert_eq!(reversal.reverses(), Some(EntryId::new(1)));
        assert!(reversal.lines()[0].side() == Side::Credit);
        assert_eq!(store.committed_entries().len(), 2);
    }

    // RE-2: 二重訂正は既定で AlreadyReversed になり、既存逆仕訳の番号が入る。
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
        let first_reversal = run_reverse_entry(&store, 100, first_input)
            .await
            .unwrap()
            .entry;

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
                reversal_id,
            }) => {
                assert_eq!(entry_no.as_u32(), 1);
                assert_eq!(reversal_no, first_reversal.entry_no());
                // PR-B 2巡目: 既存赤伝の **仕訳ID** も返る（番号だけでは
                // AI がその赤伝を引き直せない）。
                assert_eq!(reversal_id, first_reversal.id());
            }
            other => panic!("AlreadyReversed を期待したが: {other:?}"),
        }
    }

    // RE-12（PR-B 2巡目）: 二重訂正のメッセージに、既存赤伝の仕訳IDが
    // **UUID の正準表記**で含まれる（`docs/07-mcp-server.md` §3）。
    #[tokio::test]
    async fn reverse_entry_double_reversal_message_carries_the_existing_reversal_uuid() {
        let store = InMemoryStore::with_chart(sample_chart());
        let original_date = AccountingDate::new(2026, 4, 1).unwrap();
        seed_entry(&store, 1, original_date).await;

        let first_input = ReverseEntryInput {
            original_id: EntryId::new(1),
            reverse_date: AccountingDate::new(2026, 4, 5).unwrap(),
            reason: "1回目".to_string(),
            allow_double_reversal: false,
        };
        let first_reversal = run_reverse_entry(&store, 100, first_input)
            .await
            .unwrap()
            .entry;

        let second_input = ReverseEntryInput {
            original_id: EntryId::new(1),
            reverse_date: AccountingDate::new(2026, 4, 10).unwrap(),
            reason: "2回目".to_string(),
            allow_double_reversal: false,
        };
        let err = run_reverse_entry(&store, 200, second_input)
            .await
            .unwrap_err();

        let expected = entry_id_to_uuid_string(first_reversal.id());
        let message = err.to_string();
        assert!(
            message.contains(&expected),
            "既存赤伝の UUID が含まれていない: {message}"
        );
        // 10進表記は混ざらない。
        assert!(
            !message.contains(&first_reversal.id().as_u128().to_string()),
            "10進表記が混ざっている: {message}"
        );
    }

    // RE-3: allow_double_reversal: true を指定すれば二重訂正が許可される。
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

    // RE-4: 元仕訳が別年度でも、逆仕訳は指定日付（reverse_date）の年度に属する。
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

        let reversal = result.unwrap().entry;
        assert_eq!(reversal.fiscal_year(), 2026);
        assert_eq!(
            reversal.entry_date(),
            AccountingDate::new(2026, 1, 10).unwrap()
        );
    }

    // RE-5: 存在しない仕訳IDを訂正しようとすると RepoError::NotFound になる。
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

    // RE-6: 締められた期間への訂正は PeriodClosed になる
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

    // RE-7（修正5-3）: 元仕訳が既に逆仕訳（訂正仕訳）である場合でも、
    // その赤伝自体をさらに訂正すること（逆仕訳の逆仕訳）は通常どおり許可される
    // （`allow_double_reversal` を明示しなくても良い。二重訂正の拒否対象は
    // 「同じ元仕訳を2回赤伝すること」であり、「赤伝を訂正すること」ではない）。
    #[tokio::test]
    async fn reverse_entry_of_a_reversal_entry_is_allowed_without_the_double_reversal_flag() {
        let store = InMemoryStore::with_chart(sample_chart());
        let original_date = AccountingDate::new(2026, 4, 1).unwrap();
        seed_entry(&store, 1, original_date).await;

        // 1回目: 元仕訳(1)を訂正して赤伝(reversal_of_original)を作る。
        let first_input = ReverseEntryInput {
            original_id: EntryId::new(1),
            reverse_date: AccountingDate::new(2026, 4, 5).unwrap(),
            reason: "誤りに気づいたので取り消す".to_string(),
            allow_double_reversal: false,
        };
        let reversal_of_original = run_reverse_entry(&store, 100, first_input)
            .await
            .unwrap()
            .entry;

        // 2回目: 赤伝そのものを訂正する（赤伝の打ち間違いを取り消す）。
        // original_id には赤伝(reversal_of_original)のIDを指定する。
        let second_input = ReverseEntryInput {
            original_id: reversal_of_original.id(),
            reverse_date: AccountingDate::new(2026, 4, 12).unwrap(),
            reason: "赤伝自体の打ち間違いを取り消す".to_string(),
            allow_double_reversal: false,
        };
        let second_result = run_reverse_entry(&store, 200, second_input).await;

        let reversal_of_reversal = second_result.unwrap().entry;
        assert_eq!(
            reversal_of_reversal.reverses(),
            Some(reversal_of_original.id())
        );
        assert_eq!(store.committed_entries().len(), 3);
    }

    // RE-8（PR-B / `docs/07-mcp-server.md` MC-12）: 訂正理由が空文字・空白のみ
    // （半角スペース・タブ・改行・全角スペース）の場合は拒否する。
    //
    // **MCP 層ではなくユースケース層で弾く**（`DECISIONS.md` D-074）。
    // 下位層はいずれも空文字を通すため、ここが唯一の関門になる。
    #[tokio::test]
    async fn reverse_entry_rejects_blank_reason() {
        // 全角スペース（U+3000）・タブ・改行を含む。`str::trim` は Unicode の
        // 空白を落とすため、いずれも「空白のみ」として扱われる。
        let blank_reasons = ["", " ", "   ", "\t", "\n", "\u{3000}", " \u{3000}\t\n "];

        for reason in blank_reasons {
            let store = InMemoryStore::with_chart(sample_chart());
            let original_date = AccountingDate::new(2026, 4, 1).unwrap();
            seed_entry(&store, 1, original_date).await;
            let seeded = store.committed_entries().len();

            let input = ReverseEntryInput {
                original_id: EntryId::new(1),
                reverse_date: AccountingDate::new(2026, 4, 5).unwrap(),
                reason: reason.to_string(),
                allow_double_reversal: false,
            };

            let result = run_reverse_entry(&store, 100, input).await;

            match result {
                Err(AppError::EmptyReverseReason) => {}
                other => panic!("EmptyReverseReason を期待したが（理由 {reason:?}）: {other:?}"),
            }
            // 逆仕訳は1件も作られていない。
            assert_eq!(store.committed_entries().len(), seeded);
        }
    }

    // RE-9（PR-B）: 空の訂正理由は I/O より前に弾かれる。
    //
    // 元仕訳が存在しない ID を指定しても `NotFound` ではなく
    // `EmptyReverseReason` が返る（＝`find_entry` に到達していない）。
    // 検証を I/O の後ろに置くと採番を消費したり無駄な読み込みが走る。
    #[tokio::test]
    async fn reverse_entry_checks_the_reason_before_touching_the_repository() {
        let store = InMemoryStore::with_chart(sample_chart());

        let input = ReverseEntryInput {
            original_id: EntryId::new(999),
            reverse_date: AccountingDate::new(2026, 4, 5).unwrap(),
            reason: "  ".to_string(),
            allow_double_reversal: false,
        };

        let result = run_reverse_entry(&store, 100, input).await;

        assert!(matches!(result, Err(AppError::EmptyReverseReason)));
    }

    // RE-10（PR-B）: 前後に空白がある理由は**受理し、加工せずに**保存する。
    // 非空判定は `trim` した結果で行うが、保存する文言は入力のまま。
    #[tokio::test]
    async fn reverse_entry_accepts_a_padded_reason_without_rewriting_it() {
        let store = InMemoryStore::with_chart(sample_chart());
        let original_date = AccountingDate::new(2026, 4, 1).unwrap();
        seed_entry(&store, 1, original_date).await;

        let input = ReverseEntryInput {
            original_id: EntryId::new(1),
            reverse_date: AccountingDate::new(2026, 4, 5).unwrap(),
            reason: "  請求金額の誤り  ".to_string(),
            allow_double_reversal: false,
        };

        let reversal = run_reverse_entry(&store, 100, input).await.unwrap().entry;

        assert_eq!(reversal.reverse_reason(), Some("  請求金額の誤り  "));
    }

    // RE-11（PR-B / `docs/07-mcp-server.md` §3）: 存在しない仕訳IDの NotFound は
    // 仕訳IDを **UUID の正準表記**（ハイフン付き36文字）で示す。
    // `EntryId::as_u128()` の10進表記だと、呼び出し元が送った UUID 文字列と
    // 突き合わせられない。
    #[tokio::test]
    async fn reverse_entry_not_found_reports_the_id_in_canonical_uuid_form() {
        let store = InMemoryStore::with_chart(sample_chart());
        let missing = crate::id::entry_id_from_uuid(
            uuid::Uuid::parse_str("0192a7b3-1234-7abc-8def-0123456789ab").unwrap(),
        );

        let input = ReverseEntryInput {
            original_id: missing,
            reverse_date: AccountingDate::new(2026, 4, 5).unwrap(),
            reason: "存在しない仕訳を訂正しようとした".to_string(),
            allow_double_reversal: false,
        };

        let result = run_reverse_entry(&store, 100, input).await;

        let message = match result {
            Err(AppError::Repo(RepoError::NotFound { reason })) => reason,
            other => panic!("NotFound を期待したが: {other:?}"),
        };
        assert!(
            message.contains("0192a7b3-1234-7abc-8def-0123456789ab"),
            "UUID の正準表記が含まれていない: {message}"
        );
        assert!(
            !message.contains(&missing.as_u128().to_string()),
            "10進表記が混ざっている: {message}"
        );
    }
}
