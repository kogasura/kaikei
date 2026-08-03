//! 記帳ユースケースに必要な文脈の組み立て（[`PostingContext`] / [`load_posting_context`]）。
//!
//! 会計年度の決定（[`fiscal_year_for`]）や、勘定科目表・取引先索引・締め状態の
//! 読み込みをユースケース本体から追い出し、ここに集約する。

use crate::error::RepoError;
use crate::period_guard::ClosedPeriodGuard;
use crate::ports::{ChartRepo, PeriodRepo};
use kaikei_core::{AccountingDate, ChartOfAccounts, Currency, FiscalYear};
use kaikei_policy::CounterpartyIndex;

/// 帳簿全体で共通の設定。
///
/// 勘定科目表やタグスキーマのような可変データ（YAML 由来、`kaikei-jp` が
/// 読み込む）とは異なり、会計年度の区切り方のような、より安定した規則を
/// ここに持たせる。
///
/// **フィールドは全て必須にする（`Option` にしない・`Default` を実装しない）。**
/// 指定し忘れが会計上の実害になる設定に既定値を与えると、事故が起きても
/// 構築時にエラーにならず気づけない（`DECISIONS.md` D-057 が
/// `is_taxable_business` について下したのと同じ判断。D-074）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BookSettings {
    /// 会計年度の区切り規則。
    pub fiscal_year_rule: FiscalYearRule,

    /// 帳簿通貨（通貨コードと小数桁数）。
    ///
    /// 金額の入力に通貨が明示されなかったときに使う既定であり、
    /// 「この帳簿は何建てか」という事業者設定そのもの
    /// （`docs/07-mcp-server.md` §5 / §7）。
    ///
    /// - **`Option` にしない。既定で JPY にフォールバックしない。**
    ///   `kaikei_core::Currency` はコードと**小数桁数**の組であり、
    ///   桁数を1つ間違えると金額が100倍ずれて記帳される
    ///   （`CLAUDE.md` §8）。合成ルート（`kaikei-mcp` の `config.rs`）が
    ///   設定から必ず解決して渡す。未設定なら起動を中止する。
    /// - コード文字列から解決する場合は
    ///   [`crate::currency::currency_from_code`] を使う。この関数は
    ///   **未知のコードの桁数を推測せずエラーにする**。その方針を
    ///   迂回して `Currency::new(code, 0)` を書かないこと。
    /// - この設定は「既定の通貨」であって「唯一許される通貨」ではない。
    ///   1つの仕訳の中に異なる通貨が混在すれば
    ///   `kaikei_core::JournalEntry::new` が `CurrencyMismatch` を返す。
    pub book_currency: Currency,
}

/// 会計年度の区切り規則。
///
/// Phase 1 は個人事業主（暦年）のみを対象とするためバリアントは1つだが、
/// 将来任意の決算月を持つ法人に対応する際にバリアントを追加する
/// （`ARCHITECTURE.md` §9 の拡張余地）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FiscalYearRule {
    /// 暦年（1/1〜12/31）。個人事業主向け。
    CalendarYear,
}

/// 指定した取引日が属する会計年度を、指定した規則に従って決定する。
///
/// 純粋な計算のみを行う（DB も現在時刻も参照しない）。「今日」がいつかの
/// 決定はこの関数の責務ではない（`clock.rs` の doc を参照。`CLAUDE.md` §7）。
pub fn fiscal_year_for(date: AccountingDate, rule: FiscalYearRule) -> FiscalYear {
    match rule {
        FiscalYearRule::CalendarYear => FiscalYear::calendar_year(date.year()),
    }
}

/// 記帳ユースケース（`post_entry` 等）に必要な文脈一式。
///
/// [`load_posting_context`] で組み立てる。フィールドはすべて「その時点の
/// スナップショット」であり、ユースケース内でこれを書き換えて永続化する
/// 経路は無い（読み取り専用データ）。
#[derive(Debug, Clone)]
pub struct PostingContext {
    /// 取引日から決定した会計年度。
    pub fiscal_year: FiscalYear,
    /// 勘定科目表のスナップショット。
    pub chart: ChartOfAccounts,
    /// 取引先索引のスナップショット。
    pub counterparties: CounterpartyIndex,
    /// 締め状態の判定に使うガード。
    pub guard: ClosedPeriodGuard,
}

/// `tx` から勘定科目表・取引先索引・締め状態を読み込み、[`PostingContext`] に
/// 組み立てる。
///
/// `tag_schema`（`kaikei-jp-data` 相当のタグ定義）はここでは読み込まない。
/// DB からではなく合成ルートが起動時に注入するデータであり、`TaxContext` に
/// 詰める段階で呼び出し側が別途用意する（`CLAUDE.md` §3）。
pub async fn load_posting_context<Tx>(
    tx: &mut Tx,
    entry_date: AccountingDate,
    settings: &BookSettings,
) -> Result<PostingContext, RepoError>
where
    Tx: ChartRepo + PeriodRepo,
{
    let fiscal_year = fiscal_year_for(entry_date, settings.fiscal_year_rule);
    let chart = tx.load_chart().await?;
    let counterparties = tx.load_counterparties().await?;
    let closed_through = tx.closed_through(fiscal_year.label()).await?;
    let guard = ClosedPeriodGuard::new(closed_through);
    Ok(PostingContext {
        fiscal_year,
        chart,
        counterparties,
        guard,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AppError;
    use crate::testing::InMemoryStore;
    use crate::tx::with_tx;
    use kaikei_core::AccountingDate;

    #[test]
    fn fiscal_year_for_calendar_year_matches_the_date_year() {
        let date = AccountingDate::new(2026, 4, 15).unwrap();
        let fy = fiscal_year_for(date, FiscalYearRule::CalendarYear);
        assert_eq!(fy.label(), 2026);
        assert!(fy.contains(date));
    }

    // BookSettings は帳簿通貨を必須で保持し、その解決は
    // `currency::currency_from_code`（未知のコードを推測しない）を通す。
    #[test]
    fn book_settings_holds_a_book_currency_resolved_without_guessing() {
        let settings = BookSettings {
            fiscal_year_rule: FiscalYearRule::CalendarYear,
            book_currency: crate::currency::currency_from_code("JPY").unwrap(),
        };
        assert_eq!(settings.book_currency.code(), "JPY");
        assert_eq!(settings.book_currency.minor_unit(), 0);

        // 未知の通貨コードは桁数を推測せずエラーになるため、
        // `BookSettings` をそもそも組み立てられない（既定値へは落ちない）。
        assert!(crate::currency::currency_from_code("KWD").is_err());
    }

    #[tokio::test]
    async fn load_posting_context_reads_chart_counterparties_and_closed_through() {
        let store = InMemoryStore::new();
        let closed_through_date = AccountingDate::new(2025, 12, 31).unwrap();
        store.set_closed_through(2026, closed_through_date);
        let settings = BookSettings {
            fiscal_year_rule: FiscalYearRule::CalendarYear,
            book_currency: Currency::JPY,
        };
        let entry_date = AccountingDate::new(2026, 4, 1).unwrap();

        let result: Result<_, AppError> = with_tx(&store, |tx| {
            Box::pin(async move {
                let ctx = load_posting_context(tx, entry_date, &settings).await?;
                Ok(ctx)
            })
        })
        .await;

        let ctx = result.unwrap();
        assert_eq!(ctx.fiscal_year.label(), 2026);
        assert!(ctx.counterparties.is_empty());
        use kaikei_core::PeriodGuard;
        assert_eq!(
            ctx.guard.status(closed_through_date),
            kaikei_core::PeriodStatus::Closed
        );
        assert_eq!(
            ctx.guard.status(entry_date),
            kaikei_core::PeriodStatus::Open
        );
    }
}
