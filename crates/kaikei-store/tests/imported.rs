//! 取り込んだ明細の登録と状態遷移を実 PostgreSQL に対して確かめる。
//!
//! `docs/05-csv-import.md` §3・§4・§6。
//!
//! ここで見たいのは**二重計上を作らないこと**である。取込は「同じ CSV を
//! 何度流しても結果が同じ」でなければ使えず、状態遷移を緩めると帳簿に仕訳
//! だけが残った孤児ができる（帳簿は追記のみなので消せない）。

#![cfg(feature = "pg-tests")]

mod common;

use kaikei_app::error::{AppError, RepoError};
use kaikei_app::ports::ImportedTxQuery;
use kaikei_app::ports::{ImportDirection, ImportOutcome, ImportedTxRepo, NewImportedTransaction};
use kaikei_app::tx::with_tx;
use kaikei_app::view::{ImportStatusCounts, ImportedTxQuerySpec};
use kaikei_core::{AccountingDate, EntryId, Timestamp};
use kaikei_store::imported::PgImportedTxQuery;
use kaikei_store::pool::PgStore;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{PgPool, Row};

fn imported(id: &str, external_key: &str) -> NewImportedTransaction {
    NewImportedTransaction {
        id: format!("00000000-0000-0000-0000-{id:0>12}"),
        source: "mizuho_business".to_string(),
        external_key: external_key.to_string(),
        occurred_on: AccountingDate::new(2026, 6, 15).unwrap(),
        amount_minor: 1_980,
        direction: ImportDirection::Out,
        raw_description: "カ）アマゾンジャパン".to_string(),
        balance_after: Some(500_000),
        raw_row: r#"{"date":"2026/06/15","amount":"1,980"}"#.to_string(),
        imported_at: Timestamp::from_unix_nanos(1_700_000_000_000_000_000),
    }
}

/// 取込明細を1件入れて、結果を返す。
async fn insert(store: &PgStore, tx: NewImportedTransaction) -> ImportOutcome {
    with_tx(store, |t| {
        Box::pin(async move { Ok::<_, AppError>(t.insert_imported(&tx).await?) })
    })
    .await
    .unwrap()
}

/// 明細の状態と仕訳IDを読む。
async fn status_of(pool: &PgPool, id: &str) -> (String, Option<String>) {
    let row = sqlx::query("SELECT status, entry_id::text FROM imported_transactions WHERE id = $1")
        .bind(id.parse::<uuid::Uuid>().unwrap())
        .fetch_one(pool)
        .await
        .unwrap();
    (row.get(0), row.get(1))
}

/// 仕訳を1件作り、そのIDを返す。
async fn an_entry(pool: &PgPool) -> EntryId {
    let uuid = uuid::Uuid::from_u128(0xbeef);
    common::insert_balanced_entry(pool, uuid, 2026, 1)
        .await
        .unwrap();
    EntryId::new(uuid.as_u128())
}

// IMP-1: 取り込んだ明細は未処理として入る。
#[sqlx::test]
async fn an_imported_line_starts_out_pending(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let pool = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    let store = PgStore::new(pool.clone());

    let outcome = insert(&store, imported("1", "key-1")).await;

    assert_eq!(outcome, ImportOutcome::Inserted);
    let (status, entry_id) = status_of(&pool, &imported("1", "key-1").id).await;
    assert_eq!(status, "pending");
    assert_eq!(entry_id, None, "未処理の明細は仕訳を指さない");
}

// IMP-2: **本命。** 同じ CSV を2回流しても重複しない。
//
// 冪等性は取込の必須要件（§4）。エラーにせず「スキップした」と返す——
// 1行の重複で取込全体が止まると、追記された数行を取り込む普通の使い方が
// できなくなる。
#[sqlx::test]
async fn importing_the_same_line_twice_skips_instead_of_failing(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let pool = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    let store = PgStore::new(pool.clone());

    let first = insert(&store, imported("1", "key-1")).await;
    // ID は別でも、同じ (source, external_key) なら同じ明細。
    let second = insert(&store, imported("2", "key-1")).await;

    assert_eq!(first, ImportOutcome::Inserted);
    assert_eq!(second, ImportOutcome::SkippedDuplicate);

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM imported_transactions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "重複は増えないこと");
}

// IMP-3: **本命。** 再取込は、既に仕訳済みの明細を未処理へ巻き戻さない。
//
// 巻き戻ると、もう一度仕訳化されて**二重計上**になる。帳簿は追記のみなので
// 先に作った仕訳は消せず、手で逆仕訳を起こすまで金額がずれ続ける。
#[sqlx::test]
async fn re_importing_does_not_reset_an_already_journalized_line(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let pool = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    let store = PgStore::new(pool.clone());
    let entry = an_entry(&pool).await;
    let id = imported("1", "key-1").id;

    insert(&store, imported("1", "key-1")).await;
    with_tx(&store, |t| {
        let id = id.clone();
        Box::pin(async move { Ok::<_, AppError>(t.mark_journalized(&id, entry).await?) })
    })
    .await
    .unwrap();

    // 同じ CSV をもう一度流す。
    let again = insert(&store, imported("1", "key-1")).await;

    assert_eq!(again, ImportOutcome::SkippedDuplicate);
    let (status, entry_id) = status_of(&pool, &id).await;
    assert_eq!(status, "journalized", "仕訳済みのままであること");
    assert!(entry_id.is_some(), "仕訳への紐付けが残ること");
}

// IMP-4: **本命。** 仕訳済みの明細を、別の仕訳で塗り替えられない。
//
// 塗り替えると、先に作った仕訳が帳簿に残ったまま誰からも指されなくなる
// （帳簿は追記のみなので消せない）。取消は逆仕訳 → revert_to_pending の順。
#[sqlx::test]
async fn a_journalized_line_cannot_be_journalized_again(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let pool = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    let store = PgStore::new(pool.clone());
    let entry = an_entry(&pool).await;
    let id = imported("1", "key-1").id;
    insert(&store, imported("1", "key-1")).await;

    let mark = |id: String| {
        let store = store.clone();
        async move {
            with_tx(&store, |t| {
                let id = id.clone();
                Box::pin(async move { Ok::<_, AppError>(t.mark_journalized(&id, entry).await?) })
            })
            .await
        }
    };

    mark(id.clone()).await.expect("1回目は通る");
    let err = mark(id.clone()).await.expect_err("2回目は拒否されること");

    assert!(
        matches!(err, AppError::Repo(RepoError::NotFound { .. })),
        "{err:?}"
    );
    // 期待した状態が理由に入る（IDの打ち間違いと区別できるように）。
    assert!(err.to_string().contains("未処理"), "{err}");
}

// IMP-5: 無視した明細は理由とともに残り、消えない。
#[sqlx::test]
async fn an_ignored_line_keeps_its_reason(pool_opts: PgPoolOptions, conn_opts: PgConnectOptions) {
    let pool = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    let store = PgStore::new(pool.clone());
    let id = imported("1", "key-1").id;
    insert(&store, imported("1", "key-1")).await;

    with_tx(&store, |t| {
        let id = id.clone();
        Box::pin(async move { Ok::<_, AppError>(t.mark_ignored(&id, "個人の買い物").await?) })
    })
    .await
    .unwrap();

    let row = sqlx::query("SELECT status, ignore_reason FROM imported_transactions WHERE id = $1")
        .bind(id.parse::<uuid::Uuid>().unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>(0), "ignored");
    assert_eq!(
        row.get::<Option<String>, _>(1).as_deref(),
        Some("個人の買い物")
    );
}

// IMP-6: **本命。** 仕訳済みを未処理へ戻すと、仕訳への紐付けが消える。
//
// 紐付けが残ったまま未処理に戻ると、0011 の `imported_pending_is_clean` に
// 弾かれる。制約が正しく効いていることを、実装側の SQL と併せて確かめる。
#[sqlx::test]
async fn reverting_to_pending_clears_the_entry_link(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let pool = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    let store = PgStore::new(pool.clone());
    let entry = an_entry(&pool).await;
    let id = imported("1", "key-1").id;
    insert(&store, imported("1", "key-1")).await;

    with_tx(&store, |t| {
        let id = id.clone();
        Box::pin(async move {
            t.mark_journalized(&id, entry).await?;
            Ok::<_, AppError>(t.revert_to_pending(&id).await?)
        })
    })
    .await
    .unwrap();

    let (status, entry_id) = status_of(&pool, &id).await;
    assert_eq!(status, "pending");
    assert_eq!(entry_id, None, "仕訳への紐付けが消えること");
}

// IMP-7: 未処理の明細は未処理へ戻せない（戻す先が無い）。
//
// 通ってしまうと、呼び出し側は「逆仕訳を起こしたのに実は仕訳が無かった」
// ことに気付けない。
#[sqlx::test]
async fn a_pending_line_cannot_be_reverted(pool_opts: PgPoolOptions, conn_opts: PgConnectOptions) {
    let pool = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    let store = PgStore::new(pool.clone());
    let id = imported("1", "key-1").id;
    insert(&store, imported("1", "key-1")).await;

    let err = with_tx(&store, |t| {
        let id = id.clone();
        Box::pin(async move { Ok::<_, AppError>(t.revert_to_pending(&id).await?) })
    })
    .await
    .expect_err("未処理は戻せないこと");

    assert!(err.to_string().contains("仕訳済み"), "{err}");
}

// IMP-8: 元の CSV 行が失われない。
//
// 解釈を間違えたと後で分かったとき、元が無ければ直せない。
#[sqlx::test]
async fn the_original_csv_row_is_kept(pool_opts: PgPoolOptions, conn_opts: PgConnectOptions) {
    let pool = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    let store = PgStore::new(pool.clone());
    insert(&store, imported("1", "key-1")).await;

    let raw: serde_json::Value =
        sqlx::query_scalar("SELECT raw_row FROM imported_transactions LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(raw["date"], "2026/06/15");
    assert_eq!(raw["amount"], "1,980");
}

// ─── 一覧（read model）────────────────────────────────

/// 日付だけ変えた明細を作る。
fn on(id: &str, month: u8, day: u8) -> NewImportedTransaction {
    let mut tx = imported(id, &format!("key-{id}"));
    tx.occurred_on = AccountingDate::new(2026, month, day).unwrap();
    tx
}

// IMP-9: 一覧は古い順に返る。
//
// 未処理の明細は古いものから順に片付けるものであり、新しい方から見せても
// 手が付かない（証憑の検索が新しい順なのと逆）。
#[sqlx::test]
async fn the_list_comes_back_oldest_first(pool_opts: PgPoolOptions, conn_opts: PgConnectOptions) {
    let pool = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    let store = PgStore::new(pool.clone());
    for tx in [on("3", 9, 1), on("1", 3, 15), on("2", 6, 30)] {
        insert(&store, tx).await;
    }

    let query = PgImportedTxQuery::new(pool);
    let found = query
        .list_imported(&ImportedTxQuerySpec::default(), 100)
        .await
        .unwrap();

    let dates: Vec<_> = found.iter().map(|t| t.occurred_on).collect();
    assert_eq!(
        dates,
        vec![
            AccountingDate::new(2026, 3, 15).unwrap(),
            AccountingDate::new(2026, 6, 30).unwrap(),
            AccountingDate::new(2026, 9, 1).unwrap(),
        ]
    );
}

// IMP-10: 状態・期間・取り込み元で絞れる。
#[sqlx::test]
async fn the_list_can_be_narrowed(pool_opts: PgPoolOptions, conn_opts: PgConnectOptions) {
    let pool = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    let store = PgStore::new(pool.clone());
    let entry = an_entry(&pool).await;
    for tx in [on("1", 3, 15), on("2", 6, 30), on("3", 9, 1)] {
        insert(&store, tx).await;
    }
    // 1件だけ仕訳済みにする。
    let done = on("1", 3, 15).id;
    with_tx(&store, |t| {
        let done = done.clone();
        Box::pin(async move { Ok::<_, AppError>(t.mark_journalized(&done, entry).await?) })
    })
    .await
    .unwrap();

    let query = PgImportedTxQuery::new(pool);

    let pending = query
        .list_imported(
            &ImportedTxQuerySpec {
                status: Some("pending".to_string()),
                ..Default::default()
            },
            100,
        )
        .await
        .unwrap();
    assert_eq!(pending.len(), 2, "仕訳済みの1件が外れること");

    let first_half = query
        .list_imported(
            &ImportedTxQuerySpec {
                date_to: Some(AccountingDate::new(2026, 6, 30).unwrap()),
                ..Default::default()
            },
            100,
        )
        .await
        .unwrap();
    assert_eq!(first_half.len(), 2, "期間の端を含むこと");

    let other_bank = query
        .list_imported(
            &ImportedTxQuerySpec {
                source: Some("rakuten".to_string()),
                ..Default::default()
            },
            100,
        )
        .await
        .unwrap();
    assert!(other_bank.is_empty(), "別の口座は返らないこと");
}

// IMP-11: **本命。** 一覧が空でも、取り込み済みかどうかが分かる。
//
// 「未処理が0件」には2つの意味がある——全部片付いたのか、そもそも1件も
// 取り込んでいないのか。確定申告の直前にこれを取り違えると、帳簿に丸ごと
// 抜けができる。
#[sqlx::test]
async fn an_empty_list_can_still_tell_imported_from_never_imported(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let pool = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    let store = PgStore::new(pool.clone());
    let query = PgImportedTxQuery::new(pool.clone());

    // まだ1件も取り込んでいない。
    assert_eq!(
        query.import_status_counts(None).await.unwrap(),
        ImportStatusCounts::default(),
        "取り込んでいなければ全て0"
    );

    // 取り込んで、全部片付ける。
    let entry = an_entry(&pool).await;
    insert(&store, on("1", 3, 15)).await;
    insert(&store, on("2", 6, 30)).await;
    let (a, b) = (on("1", 3, 15).id, on("2", 6, 30).id);
    with_tx(&store, |t| {
        let (a, b) = (a.clone(), b.clone());
        Box::pin(async move {
            t.mark_journalized(&a, entry).await?;
            Ok::<_, AppError>(t.mark_ignored(&b, "個人の買い物").await?)
        })
    })
    .await
    .unwrap();

    let pending = query
        .list_imported(
            &ImportedTxQuerySpec {
                status: Some("pending".to_string()),
                ..Default::default()
            },
            100,
        )
        .await
        .unwrap();
    let counts = query.import_status_counts(None).await.unwrap();

    // 一覧はどちらの場合も空。合計だけが両者を分ける。
    assert!(pending.is_empty());
    assert_eq!(counts.pending, 0);
    assert_eq!(counts.journalized, 1);
    assert_eq!(counts.ignored, 1);
    assert_eq!(counts.total(), 2, "取り込み済みだと分かること");
}

// IMP-12: 上限を超える件数を頼まれても落ちない。
//
// 上限を素通しすると、LIMIT へ渡す型変換で溢れる。
#[sqlx::test]
async fn asking_for_more_than_the_cap_does_not_break(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let pool = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    let store = PgStore::new(pool.clone());
    for i in 1..=5 {
        insert(&store, imported(&i.to_string(), &format!("key-{i}"))).await;
    }

    let query = PgImportedTxQuery::new(pool);
    let found = query
        .list_imported(&ImportedTxQuerySpec::default(), u32::MAX)
        .await
        .unwrap();

    assert_eq!(found.len(), 5);
}

// IMP-13: 向きが読み戻せる。
//
// 入金と出金が入れ替わると、収入と経費が丸ごと逆になる。
#[sqlx::test]
async fn the_direction_survives_a_round_trip(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let pool = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    let store = PgStore::new(pool.clone());
    let mut money_in = on("1", 3, 15);
    money_in.direction = ImportDirection::In;
    insert(&store, money_in).await;
    insert(&store, on("2", 6, 30)).await; // Out

    let query = PgImportedTxQuery::new(pool);
    let found = query
        .list_imported(&ImportedTxQuerySpec::default(), 100)
        .await
        .unwrap();

    // **IDで引き当てる。** 並び順に頼ると、並びを変える誤りでもこの検査が
    // 落ちてしまい、「向きを見ている」と言えなくなる。
    let flag_of = |id: &str| {
        found
            .iter()
            .find(|t| t.id == id)
            .unwrap_or_else(|| panic!("明細が見つかりません: {id}"))
            .is_money_in
    };
    assert!(flag_of(&on("1", 3, 15).id), "入金であること");
    assert!(!flag_of(&on("2", 6, 30).id), "出金であること");
}

// ─── IDで1件引く ──────────────────────────────────────

// IMP-14: **本命。** 一覧の上限を超えていても、IDで引ける。
//
// 一覧から絞る形だと、上限を超えた分の明細が「見つかりません」になる。
// IDを持っているのに引けないのは、帳簿が育つほど起きやすくなる失敗である。
#[sqlx::test]
async fn a_line_past_the_list_limit_can_still_be_found_by_id(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let pool = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    let store = PgStore::new(pool.clone());

    // 上限（200件）を超える数を入れる。狙いの1件は最後（＝一覧では
    // 古い順に切られて外れる位置）に置く。
    for i in 1..=201 {
        let mut tx = imported(&i.to_string(), &format!("key-{i}"));
        tx.occurred_on = AccountingDate::new(2026, 12, 31).unwrap();
        insert(&store, tx).await;
    }
    let target = imported("201", "key-201").id;

    let query = PgImportedTxQuery::new(pool);

    // 一覧では届かない。
    let listed = query
        .list_imported(&ImportedTxQuerySpec::default(), 200)
        .await
        .unwrap();
    assert_eq!(listed.len(), 200, "上限で切られること");
    assert!(
        !listed.iter().any(|tx| tx.id == target),
        "この明細は一覧に含まれないこと（前提の確認）"
    );

    // IDでは引ける。
    let found = query
        .find_imported(&target)
        .await
        .unwrap()
        .expect("IDで引けること");
    assert_eq!(found.id, target);
    assert_eq!(found.amount_minor, 1_980);
}

// IMP-15: 知らないIDは見つからない（エラーではない）。
#[sqlx::test]
async fn an_unknown_id_is_not_found_rather_than_an_error(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let pool = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    let query = PgImportedTxQuery::new(pool);

    let found = query
        .find_imported("00000000-0000-0000-0000-000000000000")
        .await
        .expect("エラーにしないこと");

    assert!(found.is_none());
}

// IMP-16: **本命。** UUID でない文字列は「見つからない」。
//
// ここを Corrupt にすると、打ち間違いが「保存データが壊れている」という
// 誤診になる。壊れているのは入力であって帳簿ではない。
#[sqlx::test]
async fn a_malformed_id_is_not_found_rather_than_corrupt(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let pool = common::app_pool(conn_opts).await;
    let _ = pool_opts;
    let query = PgImportedTxQuery::new(pool);

    let found = query
        .find_imported("これはUUIDではない")
        .await
        .expect("保存データの異常として扱わないこと");

    assert!(found.is_none());
}
