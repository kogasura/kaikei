//! 総勘定元帳ユースケース（[`execute`]）。
//!
//! `Tx` を通さず [`crate::ports::LedgerQuery`] に直行する
//! （read model は物理的に分離する。`CLAUDE.md` §6）。責務は3つ:
//!
//! 1. `from`/`to` の妥当性検証（`from > to` は入力ミスとして拒否する）
//! 2. `limit` の範囲検証（黙って丸めない。[`crate::usecase::search_entries`] と同じ）
//! 3. 帳簿通貨との突き合わせ（read model が別通貨の行を返したらエラー。
//!    `DECISIONS.md` D-042）
//!
//! # 期間は必須にする
//!
//! `search_entries` は期間を省略できるが、元帳は**必ず期間を要求する**。
//! 元帳は「ある科目の全明細を時系列で並べたもの」であり、期間を省略できる
//! ようにすると、既定の挙動が「開設以来の全明細」になる。それは1ページに
//! 収まらないので、実質的に**上限で切られた先頭だけ**が返る。
//! 期間を明示させれば、切れたときに「期間を狭める」という次の手が
//! そのまま使える（`CLAUDE.md` §11）。
//!
//! テストID（`LDG-1` 等）はこのファイル内でのみ一意な連番であり、
//! `docs/02-test-cases.md` のID体系とは独立している。

use crate::context::BookSettings;
use crate::error::AppError;
use crate::ports::{LedgerParams, LedgerQuery};
use crate::view::{LedgerCursor, LedgerPageView};
use kaikei_core::{AccountCode, AccountingDate, CoreError, Money};

/// 1ページで返す元帳の既定行数（呼び出し元が `limit` を省略したとき用）。
///
/// 元帳の1行は仕訳1件より小さい（明細1行ぶん）ので、
/// [`crate::usecase::search_entries::DEFAULT_LIMIT`] より大きく取る。
pub const DEFAULT_LIMIT: u32 = 100;

/// 1ページで返す元帳の上限行数。
///
/// 切れたことは [`LedgerPageView::total_lines`] と
/// [`LedgerPageView::next_cursor`] から必ず読み取れる（黙って切らない）。
pub const MAX_LIMIT: u32 = 500;

/// [`execute`] への入力。
///
/// **帳簿通貨を受け取らない**（[`LedgerParams::book_currency`] は
/// この関数が `settings` から詰める）。呼び出し元が別の通貨を渡せる形に
/// すると、0行の元帳だけが帳簿と違う通貨を名乗るという状態を作れてしまう。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerInput {
    /// 対象の勘定科目コード。
    pub account: AccountCode,
    /// 集計期間の開始日（取引日、両端を含む）。
    pub from: AccountingDate,
    /// 集計期間の終了日（取引日、両端を含む）。
    pub to: AccountingDate,
    /// 続きから読む場合の開始位置。
    pub cursor: Option<LedgerCursor>,
    /// 1ページの上限行数。
    pub limit: u32,
}

/// 総勘定元帳を1ページ分取得する。
///
/// # Errors
///
/// - `input.from > input.to` は [`AppError::Rejected`]
/// - `input.limit` が 1〜[`MAX_LIMIT`] の外なら [`AppError::Rejected`]
/// - 指定した科目が勘定科目マスタに無い場合は [`AppError::Repo`]
///   （[`crate::error::RepoError::NotFound`]。**空の元帳にしない**）
/// - 行の通貨が帳簿通貨と食い違う場合は [`AppError::Core`]
///   （`CoreError::CurrencyMismatch`）
pub async fn execute(
    query: &dyn LedgerQuery,
    settings: &BookSettings,
    input: LedgerInput,
) -> Result<LedgerPageView, AppError> {
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

    if input.limit == 0 || input.limit > MAX_LIMIT {
        return Err(AppError::Rejected {
            reason: format!(
                "limit は 1〜{MAX_LIMIT} で指定してください（指定値: {}）。\
                 上限を超える行数は一度に返しません。続きは応答の next_cursor を\
                 cursor に渡すか、期間を狭めて取得してください",
                input.limit
            ),
        });
    }

    let page = query
        .ledger(&LedgerParams {
            account: input.account,
            from: input.from,
            to: input.to,
            book_currency: settings.book_currency,
            cursor: input.cursor,
            limit: input.limit,
        })
        .await?;

    // 帳簿通貨との突き合わせ（`DECISIONS.md` D-042）。read model は
    // 「保存されている通貨」で組み立てるので、帳簿通貨と違う通貨の明細が
    // あれば残高の意味が変わる。空の成功にせず、ここで検出する。
    check_currency(&page.opening_balance, settings)?;
    check_currency(&page.debit_total, settings)?;
    check_currency(&page.credit_total, settings)?;
    check_currency(&page.closing_balance, settings)?;
    for row in &page.rows {
        check_currency(&row.amount, settings)?;
        check_currency(&row.running_balance, settings)?;
    }

    Ok(page)
}

/// 金額が帳簿通貨建てであることを確かめる。
fn check_currency(money: &Money, settings: &BookSettings) -> Result<(), AppError> {
    if money.currency() == settings.book_currency {
        return Ok(());
    }
    Err(AppError::Core(CoreError::CurrencyMismatch {
        a: settings.book_currency.code().to_string(),
        b: money.currency().code().to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::RepoError;
    use crate::test_support::settings;
    use crate::view::LedgerPageView;
    use async_trait::async_trait;
    use kaikei_core::{AccountType, Currency};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    struct FakeLedger {
        currency: Currency,
        received: Mutex<Option<LedgerParams>>,
        call_count: AtomicUsize,
    }

    impl FakeLedger {
        fn new(currency: Currency) -> Self {
            FakeLedger {
                currency,
                received: Mutex::new(None),
                call_count: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl LedgerQuery for FakeLedger {
        async fn ledger(&self, params: &LedgerParams) -> Result<LedgerPageView, RepoError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            *self.received.lock().expect("テスト用フェイクの Mutex") = Some(params.clone());
            let zero = Money::zero(self.currency);
            Ok(LedgerPageView {
                account: params.account.clone(),
                account_name: "現金".to_string(),
                account_type: AccountType::Asset,
                opening_balance: zero,
                debit_total: zero,
                credit_total: zero,
                closing_balance: zero,
                total_lines: 0,
                rows: Vec::new(),
                next_cursor: None,
            })
        }
    }

    fn date(y: i32, m: u8, d: u8) -> AccountingDate {
        AccountingDate::new(y, m, d).unwrap()
    }

    fn input() -> LedgerInput {
        LedgerInput {
            account: AccountCode::parse("100").unwrap(),
            from: date(2026, 1, 1),
            to: date(2026, 12, 31),
            cursor: None,
            limit: DEFAULT_LIMIT,
        }
    }

    // LDG-1: 帳簿通貨は呼び出し元ではなく設定から渡る。
    #[tokio::test]
    async fn ledger_passes_the_book_currency_from_the_settings() {
        let query = FakeLedger::new(Currency::JPY);
        execute(&query, &settings(), input()).await.unwrap();

        let received = query.received.lock().unwrap().clone().unwrap();
        assert_eq!(received.book_currency, Currency::JPY);
        assert_eq!(received.limit, DEFAULT_LIMIT);
    }

    // LDG-2: from > to は 0行の成功にせず拒否する。
    #[tokio::test]
    async fn ledger_rejects_from_after_to_without_reaching_the_query() {
        let query = FakeLedger::new(Currency::JPY);
        let mut input = input();
        input.from = date(2026, 12, 31);
        input.to = date(2026, 1, 1);

        assert!(matches!(
            execute(&query, &settings(), input).await,
            Err(AppError::Rejected { .. })
        ));
        assert_eq!(query.call_count.load(Ordering::SeqCst), 0);
    }

    // LDG-3: limit は黙って丸めず、上限を名乗って拒否する。
    #[tokio::test]
    async fn ledger_rejects_a_limit_outside_the_allowed_range() {
        let query = FakeLedger::new(Currency::JPY);
        for limit in [0, MAX_LIMIT + 1] {
            let mut input = input();
            input.limit = limit;
            match execute(&query, &settings(), input).await {
                Err(AppError::Rejected { reason }) => {
                    assert!(reason.contains(&MAX_LIMIT.to_string()), "{reason}");
                }
                other => panic!("Rejected を期待したが: {other:?}"),
            }
        }
        assert_eq!(query.call_count.load(Ordering::SeqCst), 0);
    }

    // LDG-4（`DECISIONS.md` D-042）: read model が帳簿通貨と違う通貨の
    // 元帳を返したら、空の成功にせずエラーにする。
    #[tokio::test]
    async fn ledger_rejects_a_page_in_another_currency() {
        let query = FakeLedger::new(Currency::USD);
        assert!(matches!(
            execute(&query, &settings(), input()).await,
            Err(AppError::Core(CoreError::CurrencyMismatch { .. }))
        ));
    }

    // LDG-5: 科目が見つからない場合の `NotFound` はそのまま伝播する
    //（**空の元帳に化けさせない**）。
    #[tokio::test]
    async fn ledger_propagates_not_found_for_an_unknown_account() {
        struct Missing;

        #[async_trait]
        impl LedgerQuery for Missing {
            async fn ledger(&self, _params: &LedgerParams) -> Result<LedgerPageView, RepoError> {
                Err(RepoError::NotFound {
                    reason: "勘定科目 999 は勘定科目マスタにありません".to_string(),
                })
            }
        }

        assert!(matches!(
            execute(&Missing, &settings(), input()).await,
            Err(AppError::Repo(RepoError::NotFound { .. }))
        ));
    }
}
