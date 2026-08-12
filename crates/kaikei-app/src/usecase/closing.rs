//! 決算振替仕訳の提案ユースケース（[`execute`]）。
//!
//! 会計年度の帳簿から試算表を組み立て、[`crate::policy::ClosingPolicy`] に
//! 決算振替仕訳（収益・費用のゼロ化と元入金への振替）を提案させる。
//! [`statements`](crate::usecase::statements) と同じ経路
//! （[`crate::ports::JournalRepo::list_entries_in_period`] →
//! [`kaikei_core::TrialBalance::from_entries`]）を通る。
//!
//! # 提案するだけで、記帳はしない
//!
//! 戻り値は [`crate::policy::ProposedEntry`]（仕訳IDも仕訳番号も持たない案）で
//! あり、このユースケースは帳簿に何も書かない。記帳するかどうかは呼び出し側の
//! 判断で、実際に記帳するときは [`post_entry`](crate::usecase::post_entry) を
//! 通る——**決算振替仕訳も通常の仕訳と同じ検証を受ける**（貸借一致・締め状態・
//! タグスキーマ）。Phase 2 PR-7 が「構築は通るが記帳できない」欠陥を3件出した
//! のはこの経路を通していなかったためで、`DECISIONS.md` D-065 / D-066 が
//! その反省から構築時検証を足している。
//!
//! # 期間は会計年度から導出する（呼び出し側が日付を組み立てない）
//!
//! 決算は会計年度に対して行うものなので、入力は年度ラベル1つだけにする。
//! `from`/`to` を受け取る形にすると、**年度と1日でもずれた期間で決算振替を
//! 提案できてしまう**。ずれた提案をそのまま記帳すると、ゼロ化しきれなかった
//! 収益・費用が翌年度に残る——しかもその誤りは決算書を見ても分からない
//! （貸借は一致したままである）。
//!
//! # 二重の決算振替は自然に防がれる
//!
//! 決算振替が既に記帳されている年度に対してもう一度呼ぶと、収益・費用は
//! 既にゼロ化されているので**提案は空になる**。これは実装した性質ではなく、
//! 「残高をゼロにする仕訳を提案する」という定義から出てくる帰結である。
//! 空の提案を「エラー」にはしない——決算が済んでいる年度に対して呼ぶのは
//! 正常な操作であり、結果が空であること自体が答えになる。

use crate::error::AppError;
use crate::policy::{ClosingPolicy, ProposedEntry};
use crate::ports::{ChartRepo, JournalRepo};
use kaikei_core::{AccountingDate, FiscalYear, TagSchema, TrialBalance};

/// [`execute`] への入力。
#[derive(Debug, Clone)]
pub struct ClosingInput {
    /// 決算する会計年度のラベル（暦年なので西暦の年）。
    pub fiscal_year: i32,
}

/// [`execute`] の出力。
#[derive(Debug, Clone)]
pub struct ClosingOutput {
    /// 提案された決算振替仕訳。**記帳はされていない。**
    ///
    /// 空の場合、ゼロ化すべき収益・費用の残高が無い（帳簿がその年度で空か、
    /// 既に決算振替が済んでいる）。モジュール doc「二重の決算振替は自然に
    /// 防がれる」を参照。
    pub proposals: Vec<ProposedEntry>,
    /// 集計に使った仕訳の件数。
    ///
    /// 提案が空だったとき、「帳簿が空」なのか「既に決算済み」なのかを
    /// 呼び出し側が判別するために返す（前者は 0、後者は正の数）。
    pub entry_count: usize,
    /// 集計対象とした会計年度の開始日。
    pub period_start: AccountingDate,
    /// 集計対象とした会計年度の終了日。
    pub period_end: AccountingDate,
}

/// 決算振替仕訳を提案する。
///
/// `policy` は呼び出し側が渡す。`JpSoleProprietorClosingPolicy` は決算3科目
/// （元入金・事業主貸・事業主借）の実在と記帳可否を**構築時に**検証するので
/// （`DECISIONS.md` D-066）、合成ルートが保持しているものをそのまま渡してよい
/// （勘定科目表を毎回読み直す `StatementPolicy` とは事情が違う。D-069）。
///
/// # Errors
///
/// - 仕訳・勘定科目表の読み込みに失敗した場合は [`AppError::Repo`]
/// - 試算表の組み立てに失敗した場合は [`AppError::Core`]
/// - `ClosingPolicy` が提案を組み立てられなかった場合は [`AppError::Policy`]
pub async fn execute<Tx>(
    tx: &mut Tx,
    policy: &dyn ClosingPolicy,
    tag_schema: &TagSchema,
    input: ClosingInput,
) -> Result<ClosingOutput, AppError>
where
    Tx: JournalRepo + ChartRepo + Send,
{
    let fiscal_year = FiscalYear::calendar_year(input.fiscal_year);
    let period_start = fiscal_year.start();
    let period_end = fiscal_year.end();

    let chart = tx.load_chart().await?;
    let entries = tx.list_entries_in_period(period_start, period_end).await?;
    let entry_count = entries.len();

    let trial_balance = TrialBalance::from_entries(entries.iter(), &chart, tag_schema, &[])?;
    let proposals = policy.closing_entries(&trial_balance, &fiscal_year)?;

    Ok(ClosingOutput {
        proposals,
        entry_count,
        period_start,
        period_end,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{fixed_clock, sample_chart_with_tax_account, AllOpen};
    use crate::testing::InMemoryStore;
    use crate::tx::{with_tx, with_tx_err};
    use kaikei_core::{
        AccountCode, Currency, EntryId, EntryNumber, JournalEntry, JournalLine, Money, NewEntry,
        Side, TagSet,
    };
    use kaikei_policy::testing::NoClosing;

    fn date(year: i32, month: u8, day: u8) -> AccountingDate {
        AccountingDate::new(year, month, day).unwrap()
    }

    fn line(account: &str, side: Side, amount_minor: i128) -> JournalLine {
        JournalLine::new(
            AccountCode::parse(account).unwrap(),
            side,
            Money::from_minor(amount_minor, Currency::JPY),
            TagSet::new(),
            None,
        )
        .unwrap()
    }

    fn entry(id: u128, no: u32, on: AccountingDate, lines: Vec<JournalLine>) -> JournalEntry {
        JournalEntry::new(
            NewEntry {
                id: EntryId::new(id),
                entry_no: EntryNumber::new(no),
                entry_date: on,
                description: "テスト".to_string(),
                lines,
                document_refs: Vec::new(),
            },
            &FiscalYear::calendar_year(on.year()),
            &sample_chart_with_tax_account(),
            &TagSchema::empty(),
            &AllOpen,
            &fixed_clock(),
        )
        .unwrap()
    }

    async fn store_with(entries: Vec<JournalEntry>) -> InMemoryStore {
        let store = InMemoryStore::with_chart(sample_chart_with_tax_account());
        with_tx(&store, |tx| {
            Box::pin(async move {
                for entry in &entries {
                    tx.insert_entry(entry).await?;
                }
                Ok::<(), AppError>(())
            })
        })
        .await
        .unwrap();
        store
    }

    async fn run(store: &InMemoryStore, fiscal_year: i32) -> ClosingOutput {
        with_tx_err(store, |tx| {
            Box::pin(async move {
                execute(
                    tx,
                    &NoClosing,
                    &TagSchema::empty(),
                    ClosingInput { fiscal_year },
                )
                .await
            })
        })
        .await
        .unwrap()
    }

    // CL-1: 集計期間は年度ラベルから導出される（呼び出し側が日付を渡さない）。
    #[tokio::test]
    async fn the_period_is_derived_from_the_fiscal_year_label() {
        let store = store_with(vec![entry(
            1,
            1,
            date(2026, 6, 1),
            vec![
                line("100", Side::Debit, 1_000),
                line("500", Side::Credit, 1_000),
            ],
        )])
        .await;

        let output = run(&store, 2026).await;

        assert_eq!(output.period_start, date(2026, 1, 1));
        assert_eq!(output.period_end, date(2026, 12, 31));
        assert_eq!(output.entry_count, 1);
    }

    // CL-2: 別の年度の仕訳は集計に入らない。
    //
    // 年度をまたいで拾うと、前年度の収益まで当年度の決算でゼロ化してしまう。
    #[tokio::test]
    async fn entries_from_other_fiscal_years_are_not_collected() {
        let store = store_with(vec![
            entry(
                1,
                1,
                date(2025, 12, 31),
                vec![
                    line("100", Side::Debit, 5_000),
                    line("500", Side::Credit, 5_000),
                ],
            ),
            entry(
                2,
                1,
                date(2026, 1, 1),
                vec![
                    line("100", Side::Debit, 1_000),
                    line("500", Side::Credit, 1_000),
                ],
            ),
            entry(
                3,
                1,
                date(2027, 1, 1),
                vec![
                    line("100", Side::Debit, 7_000),
                    line("500", Side::Credit, 7_000),
                ],
            ),
        ])
        .await;

        let output = run(&store, 2026).await;

        assert_eq!(
            output.entry_count, 1,
            "2026年度の仕訳だけが集計対象になるはず"
        );
    }

    // CL-3: 帳簿にその年度の仕訳が無くても、エラーにはせず 0 件で返す。
    //
    // 「まだ記帳していない年度の決算を求めた」は正常な入力である。
    // `entry_count` が 0 なので、呼び出し側は「帳簿が空」と「既に決算済み」を
    // 区別できる（後者は正の数になる）。
    #[tokio::test]
    async fn a_year_with_no_entries_succeeds_with_zero_count() {
        let store = store_with(vec![entry(
            1,
            1,
            date(2026, 6, 1),
            vec![
                line("100", Side::Debit, 1_000),
                line("500", Side::Credit, 1_000),
            ],
        )])
        .await;

        let output = run(&store, 2027).await;

        assert_eq!(output.entry_count, 0);
        assert!(output.proposals.is_empty());
        assert_eq!(output.period_start, date(2027, 1, 1));
    }
}
