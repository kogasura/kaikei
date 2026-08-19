//! 財務諸表（貸借対照表・損益計算書）ユースケース（[`execute`]）。
//!
//! [`report`](crate::usecase::report) が read model（SQL 集計）から試算表の
//! **表示用 DTO** を返すのに対し、こちらは**帳簿から計算し直す**。
//! [`crate::policy::StatementPolicy`] が `kaikei_core::TrialBalance` を要求し、
//! それは `kaikei-core` の外から構築できない（`DECISIONS.md` D-031）ため、
//! [`kaikei_core::TrialBalance::from_entries`] にドメインモデルの仕訳を
//! 流し込む経路——[`crate::ports::JournalRepo::list_entries_in_period`]——を通る。
//!
//! **画面に出す試算表は read model、決算に使うものはこの経路**、という住み分け。
//!
//! # 貸借対照表には期首残高が要る
//!
//! ここは利用者が最も間違えやすい。このユースケースは**渡された期間の仕訳
//! だけ**を集計するので、期間が会計年度の途中から始まっていると、
//! 貸借対照表は「その期間中の増減」しか映さない。前期から繰り越した現預金も
//! 元入金も落ちた、**成立していない貸借対照表**が「成功」で返る。
//!
//! 損益計算書は逆に、期間の損益そのものなので期間指定が素直に効く。
//!
//! この非対称は会計の性質であって実装の都合ではないため、ユースケース側で
//! 「正しい期間」を推測して補正はしない。代わりに、
//! [`StatementsOutput::entry_count`] と [`StatementsOutput::first_entry_date`]
//! を返して**呼び出し側が気づける**ようにする。期首残高を帳簿に入れる方法は
//! 「期首日付の開始仕訳を1本記帳する」であり、この実装が自動生成することは
//! しない（`DECISIONS.md` D-065。期首の振替は税務判断を含む）。

use crate::error::AppError;
use crate::policy::{Statement, StatementPolicy};
use crate::ports::{ChartRepo, JournalRepo};
use kaikei_core::{AccountingDate, TagSchema, TrialBalance};

/// [`execute`] への入力。
#[derive(Debug, Clone)]
pub struct StatementsInput {
    /// 集計対象期間の開始日（取引日、両端を含む）。
    ///
    /// 貸借対照表を出す目的なら、**会計年度の開始日**を渡すこと
    /// （モジュール doc「貸借対照表には期首残高が要る」を参照）。
    pub from: AccountingDate,
    /// 集計対象期間の終了日（取引日、両端を含む）。
    pub to: AccountingDate,

    /// この日付の決算振替（`entry_kind: closing`）を集計から外す。
    ///
    /// # なぜ外せる必要があるのか
    ///
    /// 決算振替は収益・費用をゼロにする。**記帳した瞬間から、その年度の
    /// 決算書は売上0・所得0になる。** 帳簿は追記型なので取り消せず、逆仕訳
    /// を切るしか戻す手がない。決算書が最も要る時期（申告直前）にこれが
    /// 起きる。
    ///
    /// 青色申告決算書の貸借対照表は「青色申告特別控除前の所得金額」を独立の
    /// 行として持つ——**所得を元入金へ振り替える前の姿**である。決算書を
    /// 出す経路は常に外すのが正しい。
    ///
    /// 仕訳日記帳・総勘定元帳は外さない（帳簿に実在する仕訳である）。
    ///
    /// # なぜ日付で絞るのか（全部外さないのか）
    ///
    /// 外すべきなのは**その年度の**決算振替だけである。全部外すと:
    ///
    /// - **前年度の決算振替**（前年12/31の所得→元入金）まで外れ、元入金が
    ///   過少になり、前年度の収益・費用が当年度の貸借対照表に漏れる
    /// - 期首振替（`entry_kind: opening`）は別種別なので元から外れない
    ///
    /// 実際、最初は真偽値で全部外していた。実帳簿の複製で2027年の決算書を
    /// 出したところ、期首振替を記帳済みなのに**事業主貸 8,052,438 が
    /// 貸借対照表に残った**。
    ///
    /// `None` なら何も外さない。
    pub exclude_closing_on: Option<AccountingDate>,

    /// この日付の仕訳は、期首振替（`entry_kind: opening`）だけを入れる。
    ///
    /// # 何のためか
    ///
    /// 決算書の**期首の列**を作るための指定である。期首の姿とは
    /// 「期首振替を済ませた後、その年の商売が始まる前」であって、
    /// 期首振替（1月1日）はそこに**含まれる**。
    ///
    /// 前日（前年12/31）までで切ると期首振替が入らず、事業主貸・事業主借が
    /// 期首に残ったままになり、元入金も前年のままになる。実際に実帳簿の
    /// 複製で2027年の決算書を出したところ、**期首の貸借が合わなかった**
    /// （資産 8,141,943 に対し負債・資本 905,600）。
    ///
    /// 同じ日の普通の取引（1月1日の売上など）は入れない。それは期首の姿では
    /// なく、その年の商売である。
    ///
    /// `None` なら何もしない。
    pub only_opening_on: Option<AccountingDate>,
}

/// [`execute`] の出力。
#[derive(Debug, Clone)]
pub struct StatementsOutput {
    /// 貸借対照表。
    pub balance_sheet: Statement,
    /// 損益計算書。
    pub income_statement: Statement,
    /// 集計に使った仕訳の件数。
    ///
    /// **0 件でも空の財務諸表が「成功」で返る**ので、呼び出し側がそれを
    /// 「帳簿が空」なのか「期間の指定を間違えた」のか判別できるように返す。
    /// 期間を逆に指定したときに「貸借一致した空の試算表」が成功で返るのは
    /// `PROGRESS.md` Phase 1 の教訓3 が名指しした誤診の形であり、
    /// `from > to` はこのユースケースが拒否するが、**単に仕訳が無い期間**は
    /// 正常な入力なので拒否できない。数で伝える。
    pub entry_count: usize,
    /// 集計に使った仕訳のうち最も古い取引日（0 件なら `None`）。
    ///
    /// 指定した `from` より後ろに離れていれば、**期首残高の仕訳が帳簿に
    /// 無い**可能性が高い（会計年度の開始日を `from` に渡したのに、最初の
    /// 仕訳が 3 月なら、1〜2 月に取引が無かったのか期首残高を入れ忘れたのか
    /// を呼び出し側が問える）。
    pub first_entry_date: Option<AccountingDate>,
}

/// 財務諸表を組み立てる。
///
/// `policy` は呼び出し側が**その都度構築して**渡す。合成ルートが長期保持する
/// 構造体に入れてはならない（`DECISIONS.md` D-069。`JpStatementPolicy` が
/// 保持する勘定科目表は DB から頻繁に読み直される可変データであり、起動時に
/// 固めると「科目名を変更したのに決算書には古い名前が出る」というバグになる）。
///
/// # Errors
///
/// - `input.from > input.to` の場合は [`AppError::Rejected`]
/// - 仕訳・勘定科目表の読み込みに失敗した場合は [`AppError::Repo`]
/// - 試算表の組み立てに失敗した場合（帳簿に存在しない科目コードの明細、
///   通貨の食い違い、合算のオーバーフロー等）は [`AppError::Core`]
pub async fn execute<Tx>(
    tx: &mut Tx,
    policy: &dyn StatementPolicy,
    tag_schema: &TagSchema,
    input: StatementsInput,
) -> Result<StatementsOutput, AppError>
where
    Tx: JournalRepo + ChartRepo + Send,
{
    // `report` / `ledger` と同じ分担で、期間の妥当性はユースケースが見る
    // （ポートの実装は SQL に徹する）。
    if input.from > input.to {
        return Err(AppError::Rejected {
            reason: format!(
                "集計期間の開始日が終了日より後です: from={} to={}。\
                 from と to を入れ替えるか、正しい期間を指定してください",
                input.from.to_iso_string(),
                input.to.to_iso_string()
            ),
        });
    }

    let chart = tx.load_chart().await?;
    let mut entries = tx.list_entries_in_period(input.from, input.to).await?;
    if let Some(on) = input.exclude_closing_on {
        entries.retain(|entry| !(entry.entry_date() == on && is_closing_entry(entry)));
    }
    if let Some(on) = input.only_opening_on {
        entries.retain(|entry| entry.entry_date() != on || is_opening_entry(entry));
    }

    let entry_count = entries.len();
    // 並びは `(entry_date, entry_no)` 昇順なので先頭が最も古い
    // （`JournalRepo::list_entries_in_period` の契約）。
    let first_entry_date = entries.first().map(|entry| entry.entry_date());

    // 集計軸は使わない（財務諸表は科目で集計する）。タグによる内訳が要る
    // 場合は `report` の `group_by` を使う——こちらに軸を足すと、
    // `StatementPolicy` が科目ごとの残高を前提にしているため意味が壊れる。
    let trial_balance = TrialBalance::from_entries(entries.iter(), &chart, tag_schema, &[])?;

    Ok(StatementsOutput {
        balance_sheet: policy.balance_sheet(&trial_balance),
        income_statement: policy.income_statement(&trial_balance),
        entry_count,
        first_entry_date,
    })
}

/// 当年度末の決算振替（`entry_kind: closing`）の仕訳か。
///
/// **1明細でも印が付いていれば決算振替とみなす。** 決算振替は1本の仕訳に
/// 収益・費用のゼロ化と所得の振替をまとめており、一部だけを外すと貸借が
/// 合わなくなる。印の付いていない明細が混ざるくらいなら、仕訳ごと外す方が
/// 安全側に倒れる。
///
/// **期首振替（`entry_kind: opening`）はここに含めない。** 役割が逆で、
/// あちらは外してはいけない（外すと事業主貸・事業主借が翌年度の貸借対照表に
/// 残り続ける）。
fn is_closing_entry(entry: &kaikei_core::JournalEntry) -> bool {
    entry_kind_is(entry, "closing")
}

/// 期首振替（`entry_kind: opening`）の仕訳か。
fn is_opening_entry(entry: &kaikei_core::JournalEntry) -> bool {
    entry_kind_is(entry, "opening")
}

fn entry_kind_is(entry: &kaikei_core::JournalEntry, kind: &str) -> bool {
    let Ok(key) = kaikei_core::TagKey::parse("entry_kind") else {
        return false;
    };
    entry.lines().iter().any(|line| {
        matches!(
            line.tags().get(&key),
            Some(kaikei_core::TagValue::Code(found)) if found == kind
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{fixed_clock, sample_chart_with_tax_account, AllOpen};
    use crate::testing::InMemoryStore;
    use crate::tx::{with_tx, with_tx_err};
    use kaikei_core::{
        AccountCode, Currency, EntryId, EntryNumber, FiscalYear, JournalEntry, JournalLine, Money,
        NewEntry, Side, TagSet,
    };
    use kaikei_policy::testing::ByAccountTypeStatement;

    fn date(year: i32, month: u8, day: u8) -> AccountingDate {
        AccountingDate::new(year, month, day).unwrap()
    }

    fn schema_with_entry_kind() -> TagSchema {
        TagSchema::new(vec![(
            kaikei_core::TagKey::parse("entry_kind").unwrap(),
            kaikei_core::TagDef {
                value_type: kaikei_core::TagValueType::Code,
                aggregatable: false,
                required_for: vec![],
            },
        )])
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

    fn entry(
        id: u128,
        no: u32,
        on: AccountingDate,
        description: &str,
        lines: Vec<JournalLine>,
    ) -> JournalEntry {
        JournalEntry::new(
            NewEntry {
                id: EntryId::new(id),
                entry_no: EntryNumber::new(no),
                entry_date: on,
                description: description.to_string(),
                lines,
                document_refs: Vec::new(),
            },
            &FiscalYear::calendar_year(on.year()),
            &sample_chart_with_tax_account(),
            // `entry_kind` を登録したスキーマ。決算振替の印を付けた明細を
            // 作れないと、除外の検査が書けない。
            &schema_with_entry_kind(),
            &AllOpen,
            &fixed_clock(),
        )
        .unwrap()
    }

    /// 期首残高の仕訳（1/1）と期中の売上（6/1）を入れた帳簿。
    ///
    /// 期首: 現金(100) 10,000 / 借入金(330) 10,000
    /// 期中: 現金(100)  1,000 / 売上高(500) 1,000
    async fn store_with_opening_and_period_entries() -> InMemoryStore {
        let store = InMemoryStore::with_chart(sample_chart_with_tax_account());
        with_tx(&store, |tx| {
            Box::pin(async move {
                tx.insert_entry(&entry(
                    1,
                    1,
                    date(2026, 1, 1),
                    "期首残高",
                    vec![
                        line("100", Side::Debit, 10_000),
                        line("330", Side::Credit, 10_000),
                    ],
                ))
                .await?;
                tx.insert_entry(&entry(
                    2,
                    2,
                    date(2026, 6, 1),
                    "売上",
                    vec![
                        line("100", Side::Debit, 1_000),
                        line("500", Side::Credit, 1_000),
                    ],
                ))
                .await?;
                Ok::<(), AppError>(())
            })
        })
        .await
        .unwrap();
        store
    }

    async fn run(
        store: &InMemoryStore,
        from: AccountingDate,
        to: AccountingDate,
    ) -> StatementsOutput {
        with_tx_err(store, |tx| {
            Box::pin(async move {
                execute(
                    tx,
                    &ByAccountTypeStatement,
                    &TagSchema::empty(),
                    StatementsInput {
                        from,
                        to,
                        exclude_closing_on: None,
                        only_opening_on: None,
                    },
                )
                .await
            })
        })
        .await
        .unwrap()
    }

    fn section_subtotal(statement: &Statement, title: &str) -> i128 {
        statement
            .sections
            .iter()
            .find(|section| section.title == title)
            .unwrap_or_else(|| panic!("区分 \"{title}\" が見つからない: {statement:?}"))
            .subtotal
            .minor()
    }

    /// `entry_kind: closing` を付けた明細を作る。
    fn closing_line(account: &str, side: Side, amount: i128) -> JournalLine {
        let mut tags = TagSet::new();
        tags.insert(
            kaikei_core::TagKey::parse("entry_kind").unwrap(),
            kaikei_core::TagValue::Code("closing".to_string()),
        );
        JournalLine::new(
            AccountCode::parse(account).unwrap(),
            side,
            Money::from_minor(amount, Currency::JPY),
            tags,
            None,
        )
        .unwrap()
    }

    async fn post_closing_entry(store: &InMemoryStore) {
        with_tx(store, |tx| {
            Box::pin(async move {
                // 売上高 1,000 をゼロにする振替。相手科目はこの検査では
                // 何でもよい（見ているのは収益が消えるかどうか）。実運用では
                // 元入金だが、テスト用の勘定科目表に純資産の科目が無い。
                tx.insert_entry(&entry(
                    3,
                    3,
                    date(2026, 12, 31),
                    "決算振替: 2026年度の収益・費用を元入金へ振替",
                    vec![
                        closing_line("500", Side::Debit, 1_000),
                        closing_line("330", Side::Credit, 1_000),
                    ],
                ))
                .await?;
                Ok::<(), AppError>(())
            })
        })
        .await
        .unwrap();
    }

    async fn run_excluding_closing(
        store: &InMemoryStore,
        from: AccountingDate,
        to: AccountingDate,
    ) -> StatementsOutput {
        with_tx_err(store, |tx| {
            Box::pin(async move {
                execute(
                    tx,
                    &ByAccountTypeStatement,
                    &TagSchema::empty(),
                    StatementsInput {
                        from,
                        to,
                        exclude_closing_on: Some(date(2026, 12, 31)),
                        only_opening_on: None,
                    },
                )
                .await
            })
        })
        .await
        .unwrap()
    }

    // **本命。** 決算振替を記帳しても決算書が変わらない。
    //
    // 外さないと、決算振替を記帳した瞬間に売上0・所得0の決算書になる。
    // 帳簿は追記型なので取り消せず、決算書が最も要る時期（申告直前）に
    // 起きる。
    #[tokio::test]
    async fn posting_the_closing_entry_does_not_change_the_statements() {
        let store = store_with_opening_and_period_entries().await;
        let before = run_excluding_closing(&store, date(2026, 1, 1), date(2026, 12, 31)).await;

        post_closing_entry(&store).await;

        let after = run_excluding_closing(&store, date(2026, 1, 1), date(2026, 12, 31)).await;

        assert_eq!(
            section_subtotal(&after.income_statement, "収益"),
            section_subtotal(&before.income_statement, "収益"),
            "決算振替を記帳しても収益は変わらないこと"
        );
        assert_eq!(
            section_subtotal(&after.income_statement, "収益"),
            1_000,
            "売上 1,000 が残ること"
        );
    }

    // 外さなければ売上が消える（＝この検査が意味を持つことの確認）。
    #[tokio::test]
    async fn without_excluding_closing_the_revenue_disappears() {
        let store = store_with_opening_and_period_entries().await;
        post_closing_entry(&store).await;

        let output = run(&store, date(2026, 1, 1), date(2026, 12, 31)).await;

        assert_eq!(
            section_subtotal(&output.income_statement, "収益"),
            0,
            "決算振替を含めると収益がゼロになる（これが避けたい状態）"
        );
    }

    // 決算振替でない仕訳は外さない。
    #[tokio::test]
    async fn an_ordinary_entry_is_not_excluded() {
        let store = store_with_opening_and_period_entries().await;

        let output = run_excluding_closing(&store, date(2026, 1, 1), date(2026, 12, 31)).await;

        assert_eq!(section_subtotal(&output.income_statement, "収益"), 1_000);
        assert_eq!(output.entry_count, 2, "通常の仕訳2本は数に入ること");
    }

    /// `entry_kind: opening` を付けた明細。
    fn opening_line(account: &str, side: Side, amount: i128) -> JournalLine {
        let mut tags = TagSet::new();
        tags.insert(
            kaikei_core::TagKey::parse("entry_kind").unwrap(),
            kaikei_core::TagValue::Code("opening".to_string()),
        );
        JournalLine::new(
            AccountCode::parse(account).unwrap(),
            side,
            Money::from_minor(amount, Currency::JPY),
            tags,
            None,
        )
        .unwrap()
    }

    /// 期首残高を**前年末日付**で入れた帳簿。
    ///
    /// 検証帳簿もこの形である（「前期末日付で計上」）。
    /// 期首残高を 1/1 に置くと、期首の列から外れる——1月1日に立っている
    /// のが期首の姿なのか初日の商売なのかを、日付だけでは区別できないため。
    /// 区別できるのは期首振替（`entry_kind: opening`）だけである。
    async fn store_with_prior_year_opening_balance() -> InMemoryStore {
        let store = InMemoryStore::with_chart(sample_chart_with_tax_account());
        with_tx(&store, |tx| {
            Box::pin(async move {
                tx.insert_entry(&entry(
                    1,
                    1,
                    date(2025, 12, 31),
                    "期首残高",
                    vec![
                        line("100", Side::Debit, 10_000),
                        line("330", Side::Credit, 10_000),
                    ],
                ))
                .await?;
                tx.insert_entry(&entry(
                    2,
                    2,
                    date(2026, 6, 1),
                    "売上",
                    vec![
                        line("100", Side::Debit, 1_000),
                        line("500", Side::Credit, 1_000),
                    ],
                ))
                .await?;
                Ok::<(), AppError>(())
            })
        })
        .await
        .unwrap();
        store
    }

    async fn run_opening_column(store: &InMemoryStore, from: AccountingDate) -> StatementsOutput {
        with_tx_err(store, |tx| {
            Box::pin(async move {
                execute(
                    tx,
                    &ByAccountTypeStatement,
                    &TagSchema::empty(),
                    StatementsInput {
                        from: date(2020, 1, 1),
                        to: from,
                        exclude_closing_on: None,
                        only_opening_on: Some(from),
                    },
                )
                .await
            })
        })
        .await
        .unwrap()
    }

    // **本命。** 期首の列には期首振替（1/1）が入る。
    //
    // 入らないと事業主貸・事業主借が期首に残り、期首の貸借が合わなくなる。
    // 実帳簿の複製で、資産 8,141,943 に対し負債・資本 905,600 になった。
    #[tokio::test]
    async fn the_opening_column_includes_the_opening_transfer() {
        let store = store_with_prior_year_opening_balance().await;
        with_tx(&store, |tx| {
            Box::pin(async move {
                // 1/1 の期首振替: 現金 500 / 借入金 500
                tx.insert_entry(&entry(
                    3,
                    3,
                    date(2026, 1, 1),
                    "期首振替",
                    vec![
                        opening_line("100", Side::Debit, 500),
                        opening_line("330", Side::Credit, 500),
                    ],
                ))
                .await?;
                Ok::<(), AppError>(())
            })
        })
        .await
        .unwrap();

        let output = run_opening_column(&store, date(2026, 1, 1)).await;

        // 期首残高 10,000 + 期首振替 500 = 10,500。6/1 の売上は入らない。
        assert_eq!(section_subtotal(&output.balance_sheet, "資産"), 10_500);
    }

    // **本命。** 同じ 1/1 でも、普通の取引は期首の列に入らない。
    //
    // 入れてしまうと、その年の商売が期首残高に混ざる。
    #[tokio::test]
    async fn an_ordinary_entry_on_the_same_day_is_not_in_the_opening_column() {
        let store = store_with_prior_year_opening_balance().await;
        with_tx(&store, |tx| {
            Box::pin(async move {
                tx.insert_entry(&entry(
                    4,
                    4,
                    date(2026, 1, 1),
                    "元日の売上",
                    vec![
                        line("100", Side::Debit, 777),
                        line("500", Side::Credit, 777),
                    ],
                ))
                .await?;
                Ok::<(), AppError>(())
            })
        })
        .await
        .unwrap();

        let output = run_opening_column(&store, date(2026, 1, 1)).await;

        assert_eq!(
            section_subtotal(&output.balance_sheet, "資産"),
            10_000,
            "1/1 の普通の取引は期首に入らないこと"
        );
    }

    // **本命。** 前年度の決算振替は当年度の集計から外さない。
    //
    // 外すと元入金が過少になり、前年度の収益・費用が当年度の貸借対照表に漏れる。
    #[tokio::test]
    async fn a_prior_year_closing_entry_is_not_excluded() {
        let store = store_with_opening_and_period_entries().await;
        post_closing_entry(&store).await; // 2026-12-31 の決算振替

        // 2027年度として集計する（外すのは 2027-12-31 の決算振替だけ）。
        let output = with_tx_err(&store, |tx| {
            Box::pin(async move {
                execute(
                    tx,
                    &ByAccountTypeStatement,
                    &TagSchema::empty(),
                    StatementsInput {
                        from: date(2020, 1, 1),
                        to: date(2027, 12, 31),
                        exclude_closing_on: Some(date(2027, 12, 31)),
                        only_opening_on: None,
                    },
                )
                .await
            })
        })
        .await
        .unwrap();

        assert_eq!(
            output.entry_count, 3,
            "2026-12-31 の決算振替も数に入ること（外すのは 2027-12-31 のものだけ）"
        );
    }

    // ST-1: 期間の逆指定は「0件の空の決算書」にせず拒否する。
    #[tokio::test]
    async fn a_reversed_period_is_rejected_not_silently_empty() {
        let store = store_with_opening_and_period_entries().await;

        let result = with_tx_err(&store, |tx| {
            Box::pin(async move {
                execute(
                    tx,
                    &ByAccountTypeStatement,
                    &TagSchema::empty(),
                    StatementsInput {
                        from: date(2026, 12, 31),
                        to: date(2026, 1, 1),
                        exclude_closing_on: None,
                        only_opening_on: None,
                    },
                )
                .await
            })
        })
        .await;

        let err = result.expect_err("期間が逆なら拒否されるはず");
        assert!(matches!(err, AppError::Rejected { .. }), "{err:?}");
    }

    // ST-2: 仕訳が1件も無い期間は「成功」で返る。件数と最古日付で
    //       呼び出し側が「帳簿が空」と「期間の指定ミス」を判別できる。
    #[tokio::test]
    async fn an_empty_period_succeeds_and_reports_zero_entries() {
        let store = store_with_opening_and_period_entries().await;

        let output = run(&store, date(2026, 9, 1), date(2026, 9, 30)).await;

        assert_eq!(output.entry_count, 0);
        assert_eq!(output.first_entry_date, None);
        assert!(output.balance_sheet.total.is_zero());
        assert!(output.income_statement.total.is_zero());
    }

    // ST-3: 会計年度の全期間なら、期首残高を含んだ貸借対照表になる。
    #[tokio::test]
    async fn a_full_fiscal_year_includes_the_opening_balance() {
        let store = store_with_opening_and_period_entries().await;

        let output = run(&store, date(2026, 1, 1), date(2026, 12, 31)).await;

        assert_eq!(output.entry_count, 2);
        assert_eq!(output.first_entry_date, Some(date(2026, 1, 1)));
        // 現金 10,000（期首）+ 1,000（売上）= 11,000
        assert_eq!(section_subtotal(&output.balance_sheet, "資産"), 11_000);
        assert_eq!(section_subtotal(&output.balance_sheet, "負債"), 10_000);
        assert_eq!(section_subtotal(&output.income_statement, "収益"), 1_000);
    }

    // ST-4: **本命。** 期間が年度の途中から始まると、貸借対照表から期首残高が
    //       落ちる。これは実装の誤りではなく会計の性質であり、モジュール doc
    //       が警告している内容そのもの。**この振る舞いが変わったら doc も
    //       直す必要がある**ので、振る舞いとして固定しておく。
    //
    //       損益計算書は逆に期間指定が素直に効く（期間の損益そのもの）。
    #[tokio::test]
    async fn a_period_starting_mid_year_drops_the_opening_balance_from_the_balance_sheet() {
        let store = store_with_opening_and_period_entries().await;

        let full_year = run(&store, date(2026, 1, 1), date(2026, 12, 31)).await;
        let mid_year = run(&store, date(2026, 6, 1), date(2026, 12, 31)).await;

        // 貸借対照表は期首残高を失う（11,000 → 1,000）。
        assert_eq!(section_subtotal(&full_year.balance_sheet, "資産"), 11_000);
        assert_eq!(
            section_subtotal(&mid_year.balance_sheet, "資産"),
            1_000,
            "期首残高の仕訳（1/1）が期間外なので落ちる"
        );
        assert_eq!(
            section_subtotal(&mid_year.balance_sheet, "負債"),
            0,
            "借入金も同様に落ちる"
        );

        // 損益計算書は両方とも同じ（売上は 6/1 なのでどちらの期間にも入る）。
        assert_eq!(
            section_subtotal(&full_year.income_statement, "収益"),
            section_subtotal(&mid_year.income_statement, "収益"),
        );

        // 呼び出し側が気づくための手がかりが出ていること。
        assert_eq!(
            mid_year.first_entry_date,
            Some(date(2026, 6, 1)),
            "最古の仕訳が from と離れていれば、期首残高の入れ忘れを疑える"
        );
    }
}
