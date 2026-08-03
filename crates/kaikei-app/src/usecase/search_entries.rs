//! 仕訳検索ユースケース（[`execute`]）。
//!
//! `Tx` を通さず [`crate::ports::SearchEntriesQuery`] に直行する
//! （read model は物理的に分離する。`CLAUDE.md` §6）。
//! ここでの責務は**入力の妥当性検証**だけで、絞り込みそのものは SQL が行う。
//!
//! | # | 検証 | 落ちたときのコード |
//! |---|---|---|
//! | 1 | `from > to` は入力ミスとして拒否する（0件の空結果にしない） | `rejected` |
//! | 2 | `min_amount > max_amount` も同様に拒否する | `rejected` |
//! | 3 | 摘要の検索語が空白のみなら拒否する（全件一致に化けさせない） | `rejected` |
//! | 4 | `limit` は 1〜[`MAX_LIMIT`] の範囲に収める（黙って丸めない） | `rejected` |
//! | 5 | `tags` のキーは `aggregatable: true` のものだけ受け付ける | `not_aggregatable` |
//!
//! # なぜ黙って丸めないのか（4）
//!
//! `limit` を上限に**丸めて成功させる**と、呼び出し元は「自分が要求した
//! 件数だけ返ってきた」と読む。`total_matches` を併せて返しているので
//! 実害は小さいが、丸めた事実そのものは応答から読み取れない。
//! 上限は拒否のメッセージで名乗る（`CLAUDE.md` §11）。
//!
//! # なぜタグの絞り込みを `aggregatable` に限るのか（5）
//!
//! `CLAUDE.md` §4 は「集計軸に使うキーは `aggregatable: true` を宣言する」と
//! 定めている。**絞り込みは集計軸の切り出しであり、同じ性質を要求する。**
//! 軸として宣言していないキー（`business_ratio` のような明細固有の値）で
//! 絞り込めるようにすると、`get_trial_balance` の `group_by` とこのツールで
//! 「使えるキー」が食い違い、AI から見た帳簿の切り口が2種類になる。
//! 規則を1つに保つ（緩めるのは後からでも非破壊的に行える）。
//!
//! テストID（`SRC-1` 等）はこのファイル内でのみ一意な連番であり、
//! `docs/02-test-cases.md` のID体系とは独立している。

use crate::error::AppError;
use crate::ports::{SearchEntriesParams, SearchEntriesQuery};
use crate::view::EntrySearchPageView;
use kaikei_core::{CoreError, TagSchema};

/// 1ページで返す仕訳の既定件数（呼び出し元が `limit` を省略したとき用）。
///
/// 各仕訳は明細を伴うので、件数がそのまま応答の大きさに効く。
pub const DEFAULT_LIMIT: u32 = 20;

/// 1ページで返す仕訳の上限件数。
///
/// **上限そのものは「大きな帳簿で応答が壊れない」ためにある。**
/// 上限に達したかどうかは [`EntrySearchPageView::total_matches`] と
/// [`EntrySearchPageView::next_cursor`] から必ず読み取れる
/// （黙って切らない。`PROGRESS.md`「無言の truncation は『全部見た』と
/// 読める」）。
pub const MAX_LIMIT: u32 = 100;

/// 仕訳を検索する。
///
/// `query` は `Tx` を経由しない read model 専用のクエリ。
///
/// # Errors
///
/// モジュール doc の表の5つ。`query` が失敗した場合は [`AppError::Repo`]。
pub async fn execute(
    query: &dyn SearchEntriesQuery,
    tag_schema: &TagSchema,
    params: SearchEntriesParams,
) -> Result<EntrySearchPageView, AppError> {
    // 1. 期間の妥当性。入力ミスを「0件」として静かに成功させない
    //    （`usecase::report::execute` と同じ規律）。
    if let (Some(from), Some(to)) = (params.from, params.to) {
        if from > to {
            return Err(AppError::Rejected {
                reason: format!(
                    "検索期間の開始日が終了日より後です: from={} to={}。\
                     from と to を入れ替えるか、正しい期間を指定してください",
                    from.to_iso_string(),
                    to.to_iso_string()
                ),
            });
        }
    }

    // 2. 金額範囲の妥当性。
    if let (Some(min), Some(max)) = (params.min_amount.as_ref(), params.max_amount.as_ref()) {
        if min.currency() != max.currency() {
            return Err(AppError::Core(CoreError::CurrencyMismatch {
                a: min.currency().code().to_string(),
                b: max.currency().code().to_string(),
            }));
        }
        if min.minor() > max.minor() {
            return Err(AppError::Rejected {
                reason: format!(
                    "金額の下限が上限より大きくなっています: min_amount={} max_amount={}。\
                     どちらかを直して指定し直してください",
                    min.to_display_string(),
                    max.to_display_string()
                ),
            });
        }
    }

    // 3. 空の検索語は「全件一致」に化ける。指定したつもりで効いていない、
    //    という状態を作らない。
    if let Some(text) = params.description_contains.as_ref() {
        if text.trim().is_empty() {
            return Err(AppError::Rejected {
                reason: "摘要の検索語が空です。検索したい語を指定するか、\
                         description の指定そのものを外してください\
                         （空文字は全件一致になるため受け付けません）"
                    .to_string(),
            });
        }
    }

    // 4. 件数の上限。丸めずに拒否する（上の doc）。
    if params.limit == 0 || params.limit > MAX_LIMIT {
        return Err(AppError::Rejected {
            reason: format!(
                "limit は 1〜{MAX_LIMIT} で指定してください（指定値: {}）。\
                 上限を超える件数は一度に返しません。続きは応答の next_cursor を\
                 cursor に渡して取得してください",
                params.limit
            ),
        });
    }

    // 5. タグの絞り込みは集計軸として宣言されたキーだけ（上の doc）。
    for (key, _) in &params.tags {
        if !tag_schema.is_aggregatable(key) {
            return Err(AppError::Core(CoreError::NotAggregatable {
                key: key.as_str().to_string(),
            }));
        }
    }

    Ok(query.search_entries(&params).await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::RepoError;
    use crate::view::EntryCursor;
    use async_trait::async_trait;
    use kaikei_core::{
        AccountingDate, Currency, EntryId, EntryNumber, Money, TagDef, TagKey, TagValueType,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    struct FakeSearch {
        received: Mutex<Option<SearchEntriesParams>>,
        call_count: AtomicUsize,
    }

    impl FakeSearch {
        fn new() -> Self {
            FakeSearch {
                received: Mutex::new(None),
                call_count: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl SearchEntriesQuery for FakeSearch {
        async fn search_entries(
            &self,
            params: &SearchEntriesParams,
        ) -> Result<EntrySearchPageView, RepoError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            *self.received.lock().expect("テスト用フェイクの Mutex") = Some(params.clone());
            Ok(EntrySearchPageView {
                entries: Vec::new(),
                total_matches: 0,
                next_cursor: None,
            })
        }
    }

    fn date(y: i32, m: u8, d: u8) -> AccountingDate {
        AccountingDate::new(y, m, d).unwrap()
    }

    fn params() -> SearchEntriesParams {
        SearchEntriesParams {
            from: Some(date(2026, 1, 1)),
            to: Some(date(2026, 12, 31)),
            account: None,
            description_contains: None,
            min_amount: None,
            max_amount: None,
            tags: Vec::new(),
            cursor: None,
            limit: DEFAULT_LIMIT,
        }
    }

    fn schema_with_counterparty() -> TagSchema {
        TagSchema::new(vec![
            (
                TagKey::parse("counterparty").unwrap(),
                TagDef {
                    value_type: TagValueType::Code,
                    aggregatable: true,
                    required_for: Vec::new(),
                },
            ),
            (
                TagKey::parse("business_ratio").unwrap(),
                TagDef {
                    value_type: TagValueType::Decimal,
                    aggregatable: false,
                    required_for: Vec::new(),
                },
            ),
        ])
    }

    // SRC-1: 正常系。条件はそのまま read model に渡る。
    #[tokio::test]
    async fn search_forwards_the_parameters_to_the_read_model() {
        let query = FakeSearch::new();
        let mut input = params();
        input.cursor = Some(EntryCursor {
            entry_date: date(2026, 4, 1),
            entry_no: EntryNumber::new(3),
            entry_id: EntryId::new(7),
        });

        execute(&query, &schema_with_counterparty(), input.clone())
            .await
            .unwrap();

        let received = query.received.lock().unwrap().clone().unwrap();
        assert_eq!(received, input);
    }

    // SRC-2: from > to は 0件の成功にせず拒否する（SQL に到達しない）。
    #[tokio::test]
    async fn search_rejects_from_after_to_without_reaching_the_query() {
        let query = FakeSearch::new();
        let mut input = params();
        input.from = Some(date(2026, 12, 31));
        input.to = Some(date(2026, 1, 1));

        let result = execute(&query, &TagSchema::empty(), input).await;

        match result {
            Err(AppError::Rejected { reason }) => {
                assert!(reason.contains("2026-12-31"), "{reason}");
                assert!(reason.contains("2026-01-01"), "{reason}");
            }
            other => panic!("Rejected を期待したが: {other:?}"),
        }
        assert_eq!(query.call_count.load(Ordering::SeqCst), 0);
    }

    // SRC-3: 金額の下限が上限より大きい場合も拒否する。
    #[tokio::test]
    async fn search_rejects_a_min_amount_greater_than_the_max() {
        let query = FakeSearch::new();
        let mut input = params();
        input.min_amount = Some(Money::from_minor(2_000, Currency::JPY));
        input.max_amount = Some(Money::from_minor(1_000, Currency::JPY));

        assert!(matches!(
            execute(&query, &TagSchema::empty(), input).await,
            Err(AppError::Rejected { .. })
        ));
        assert_eq!(query.call_count.load(Ordering::SeqCst), 0);
    }

    // SRC-4: 空の検索語は全件一致に化けるので拒否する。
    #[tokio::test]
    async fn search_rejects_a_blank_description_term() {
        let query = FakeSearch::new();
        for blank in ["", "   ", "\u{3000}"] {
            let mut input = params();
            input.description_contains = Some(blank.to_string());
            assert!(
                matches!(
                    execute(&query, &TagSchema::empty(), input).await,
                    Err(AppError::Rejected { .. })
                ),
                "{blank:?} が受理されています"
            );
        }
        assert_eq!(query.call_count.load(Ordering::SeqCst), 0);
    }

    // SRC-5: limit は黙って丸めず、範囲外なら上限を名乗って拒否する。
    #[tokio::test]
    async fn search_rejects_a_limit_outside_the_allowed_range() {
        let query = FakeSearch::new();
        for limit in [0, MAX_LIMIT + 1] {
            let mut input = params();
            input.limit = limit;
            match execute(&query, &TagSchema::empty(), input).await {
                Err(AppError::Rejected { reason }) => {
                    assert!(reason.contains(&MAX_LIMIT.to_string()), "{reason}");
                    // 次の手（続きの取り方）が書いてある。
                    assert!(reason.contains("next_cursor"), "{reason}");
                }
                other => panic!("Rejected を期待したが: {other:?}"),
            }
        }
        assert_eq!(query.call_count.load(Ordering::SeqCst), 0);
    }

    // SRC-6: `aggregatable: false` のキー・未登録のキーでは絞り込めない。
    #[tokio::test]
    async fn search_rejects_tag_keys_that_are_not_aggregatable() {
        let query = FakeSearch::new();
        for key in ["business_ratio", "unregistered"] {
            let mut input = params();
            input.tags = vec![(TagKey::parse(key).unwrap(), "x".to_string())];
            assert!(
                matches!(
                    execute(&query, &schema_with_counterparty(), input).await,
                    Err(AppError::Core(CoreError::NotAggregatable { .. }))
                ),
                "{key} が受理されています"
            );
        }
        assert_eq!(query.call_count.load(Ordering::SeqCst), 0);
    }

    // SRC-7: `aggregatable: true` のキーは通る（SRC-6 が常に緑にならないことの対照）。
    #[tokio::test]
    async fn search_accepts_an_aggregatable_tag_key() {
        let query = FakeSearch::new();
        let mut input = params();
        input.tags = vec![(TagKey::parse("counterparty").unwrap(), "CP0001".to_string())];

        execute(&query, &schema_with_counterparty(), input)
            .await
            .unwrap();
        assert_eq!(query.call_count.load(Ordering::SeqCst), 1);
    }

    // SRC-8: 期間の指定は任意（片側だけ・両方なしでも通る）。
    #[tokio::test]
    async fn search_allows_an_open_ended_period() {
        let query = FakeSearch::new();
        for (from, to) in [
            (None, None),
            (Some(date(2026, 1, 1)), None),
            (None, Some(date(2026, 12, 31))),
        ] {
            let mut input = params();
            input.from = from;
            input.to = to;
            execute(&query, &TagSchema::empty(), input).await.unwrap();
        }
        assert_eq!(query.call_count.load(Ordering::SeqCst), 3);
    }

    // SRC-9: read model の失敗はそのまま伝播する。
    #[tokio::test]
    async fn search_propagates_query_failure() {
        struct Failing;

        #[async_trait]
        impl SearchEntriesQuery for Failing {
            async fn search_entries(
                &self,
                _params: &SearchEntriesParams,
            ) -> Result<EntrySearchPageView, RepoError> {
                Err(RepoError::Backend {
                    reason: "テスト用の意図的な接続断".to_string(),
                })
            }
        }

        assert!(matches!(
            execute(&Failing, &TagSchema::empty(), params()).await,
            Err(AppError::Repo(RepoError::Backend { .. }))
        ));
    }
}
