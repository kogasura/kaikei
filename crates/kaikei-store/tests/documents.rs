//! 証憑の登録と検索を実 PostgreSQL に対して確かめる。
//!
//! `docs/06-documents.md` §3・§4。
//!
//! ここで見たいのは**電子取引データの検索要件**（取引年月日・取引金額・取引先の
//! 組み合わせと範囲指定）が実際に効くことと、**帳簿から証憑へ辿れること**
//! （相互関連性）である。

#![cfg(feature = "pg-tests")]

mod common;

use kaikei_app::ports::{DocumentQueryPort, DocumentRepo, NewDocument};
use kaikei_app::tx::with_tx;
use kaikei_app::view::DocumentQuery;
use kaikei_core::{AccountingDate, EntryId, Timestamp};
use kaikei_store::documents::PgDocumentQuery;
use kaikei_store::pool::PgStore;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

fn date(year: i32, month: u8, day: u8) -> AccountingDate {
    AccountingDate::new(year, month, day).unwrap()
}

fn document(id: &str, doc_date: AccountingDate) -> NewDocument {
    NewDocument {
        id: format!("00000000-0000-0000-0000-{id:0>12}"),
        // 内容の SHA-256 の代わりに、テストでは id から作った16進64文字を使う。
        blob_hash: format!("{:0>64}", id.to_lowercase()),
        original_name: "請求書.pdf".to_string(),
        mime_type: "application/pdf".to_string(),
        byte_size: 1024,
        doc_date,
        amount_minor: Some(550_000),
        counterparty: Some("ABC".to_string()),
        doc_type: "invoice".to_string(),
        received_via: "email".to_string(),
        received_at: Timestamp::from_unix_nanos(1_700_000_000_000_000_000),
        note: None,
    }
}

async fn insert(store: &PgStore, documents: Vec<NewDocument>) {
    with_tx(store, |tx| {
        Box::pin(async move {
            for doc in &documents {
                tx.insert_document(doc).await?;
            }
            Ok::<(), kaikei_app::error::AppError>(())
        })
    })
    .await
    .unwrap();
}

// DOC-1: 登録した証憑が検索で返る。
#[sqlx::test]
async fn a_registered_document_can_be_found(pool_opts: PgPoolOptions, conn_opts: PgConnectOptions) {
    let pool = common::app_pool(conn_opts.clone()).await;
    let _ = pool_opts;
    let store = PgStore::new(pool.clone());
    insert(&store, vec![document("1", date(2026, 6, 15))]).await;

    let query = PgDocumentQuery::new(pool);
    let found = query
        .search_documents(&DocumentQuery::default(), 100)
        .await
        .unwrap();

    assert_eq!(found.len(), 1);
    assert_eq!(found[0].doc_date, date(2026, 6, 15));
    assert_eq!(found[0].amount_minor, Some(550_000));
    assert_eq!(found[0].counterparty.as_deref(), Some("ABC"));
    assert_eq!(found[0].doc_type, "invoice");
}

// DOC-2: **本命。** 検索要件の3項目が範囲で効く。
#[sqlx::test]
async fn the_three_search_criteria_work_as_ranges(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let pool = common::app_pool(conn_opts.clone()).await;
    let _ = pool_opts;
    let store = PgStore::new(pool.clone());

    let mut cheap = document("1", date(2026, 3, 1));
    cheap.amount_minor = Some(1_000);
    cheap.counterparty = Some("A社".to_string());
    let mut mid = document("2", date(2026, 6, 15));
    mid.amount_minor = Some(50_000);
    mid.counterparty = Some("B社".to_string());
    let mut expensive = document("3", date(2026, 9, 30));
    expensive.amount_minor = Some(900_000);
    expensive.counterparty = Some("A社".to_string());
    insert(&store, vec![cheap, mid, expensive]).await;

    let query = PgDocumentQuery::new(pool);
    let ids = |found: Vec<kaikei_app::view::DocumentView>| {
        found
            .into_iter()
            .map(|d| d.amount_minor.unwrap())
            .collect::<Vec<_>>()
    };

    // 日付の範囲。
    let by_date = query
        .search_documents(
            &DocumentQuery {
                date_from: Some(date(2026, 4, 1)),
                date_to: Some(date(2026, 8, 31)),
                ..Default::default()
            },
            100,
        )
        .await
        .unwrap();
    assert_eq!(ids(by_date), vec![50_000]);

    // 金額の範囲。
    let by_amount = query
        .search_documents(
            &DocumentQuery {
                amount_min: Some(10_000),
                amount_max: Some(100_000),
                ..Default::default()
            },
            100,
        )
        .await
        .unwrap();
    assert_eq!(ids(by_amount), vec![50_000]);

    // 取引先。
    let by_counterparty = query
        .search_documents(
            &DocumentQuery {
                counterparty: Some("A社".to_string()),
                ..Default::default()
            },
            100,
        )
        .await
        .unwrap();
    // 並びは取引年月日の降順。
    assert_eq!(ids(by_counterparty), vec![900_000, 1_000]);

    // 3項目の組み合わせ。
    let combined = query
        .search_documents(
            &DocumentQuery {
                date_from: Some(date(2026, 1, 1)),
                amount_min: Some(500_000),
                counterparty: Some("A社".to_string()),
                ..Default::default()
            },
            100,
        )
        .await
        .unwrap();
    assert_eq!(ids(combined), vec![900_000]);
}

// DOC-3: 金額の無い証憑（契約書など）も登録でき、金額で絞ると外れる。
//
//        **0 で埋めない。**「金額が無い」と「0円」は違う。
#[sqlx::test]
async fn a_document_without_an_amount_is_stored_as_null(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let pool = common::app_pool(conn_opts.clone()).await;
    let _ = pool_opts;
    let store = PgStore::new(pool.clone());

    let mut contract = document("1", date(2026, 4, 1));
    contract.amount_minor = None;
    contract.counterparty = None;
    contract.doc_type = "contract".to_string();
    insert(&store, vec![contract]).await;

    let query = PgDocumentQuery::new(pool);

    let all = query
        .search_documents(&DocumentQuery::default(), 100)
        .await
        .unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].amount_minor, None, "0 で埋めないこと");
    assert_eq!(all[0].counterparty, None);

    // 金額で絞ると外れる（NULL は比較で真にならない）。
    let by_amount = query
        .search_documents(
            &DocumentQuery {
                amount_min: Some(0),
                ..Default::default()
            },
            100,
        )
        .await
        .unwrap();
    assert!(
        by_amount.is_empty(),
        "金額の無い証憑は金額の条件に当たらない"
    );
}

// DOC-4: **本命。** 帳簿から証憑へ辿れる（相互関連性）。
#[sqlx::test]
async fn documents_can_be_reached_from_the_journal_entry(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let pool = common::app_pool(conn_opts.clone()).await;
    let _ = pool_opts;
    let store = PgStore::new(pool.clone());

    let entry_id = EntryId::new(0x0199_0000_0000_7000_8000_0000_0000_0001);
    common::insert_balanced_entry(&pool, uuid::Uuid::from_u128(entry_id.as_u128()), 2026, 1)
        .await
        .unwrap();

    let doc = document("1", date(2026, 6, 15));
    let doc_id = doc.id.clone();
    insert(&store, vec![doc]).await;
    with_tx(&store, |tx| {
        let doc_id = doc_id.clone();
        Box::pin(async move {
            tx.link_document(entry_id, &doc_id).await?;
            // **2回紐付けても失敗しない。** 取り込みを何度流しても同じ結果に。
            tx.link_document(entry_id, &doc_id).await?;
            Ok::<(), kaikei_app::error::AppError>(())
        })
    })
    .await
    .unwrap();

    let query = PgDocumentQuery::new(pool);
    let linked = query.documents_of_entry(entry_id).await.unwrap();

    assert_eq!(linked.len(), 1, "2回紐付けても1件のまま");
    assert_eq!(linked[0].original_name, "請求書.pdf");
}

// DOC-5: 紐付いていない仕訳からは何も返らない（空を返す。失敗にしない）。
#[sqlx::test]
async fn an_entry_without_documents_returns_nothing(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let pool = common::app_pool(conn_opts.clone()).await;
    let _ = pool_opts;
    let query = PgDocumentQuery::new(pool);

    let linked = query
        .documents_of_entry(EntryId::new(0x0199_0000_0000_7000_8000_0000_0000_0009))
        .await
        .unwrap();

    assert!(linked.is_empty());
}

// DOC-6: 上限を超える limit を渡しても、返る件数は上限で頭打ちになる。
//
//        条件を付け忘れた検索が帳簿全体を返さないようにする。
#[sqlx::test]
async fn the_search_limit_is_capped(pool_opts: PgPoolOptions, conn_opts: PgConnectOptions) {
    let pool = common::app_pool(conn_opts.clone()).await;
    let _ = pool_opts;
    let store = PgStore::new(pool.clone());

    let docs: Vec<NewDocument> = (1..=3)
        .map(|n| document(&n.to_string(), date(2026, 6, n as u8)))
        .collect();
    insert(&store, docs).await;

    let query = PgDocumentQuery::new(pool);
    let found = query
        .search_documents(&DocumentQuery::default(), u32::MAX)
        .await
        .unwrap();

    // 上限（200）より少ないので全件返るが、u32::MAX がそのまま SQL に渡ると
    // BIGINT の範囲で問題になる。落ちずに返ることを見る。
    assert_eq!(found.len(), 3);
}

/// **本命。** 件数は「行の数」であって「内容の種類」ではない。
///
/// `all_blob_hashes` は `DISTINCT blob_hash` なので、**同じ内容の証憑が
/// 別の取引に付いていると少なく出る**（内容は SHA-256 で1つに束ねて保存
/// するので、同じ請求書を2つの仕訳に紐付けると起きる）。
///
/// `search_documents` はこの数を `total_registered` として返し、「1件も
/// 登録されていない」と「条件に合わなかった」を区別させる。**数が違うと
/// 区別そのものが狂う。** 実際に4件登録されている帳簿で3と出た。
#[sqlx::test]
async fn the_document_count_counts_rows_not_distinct_contents(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let pool = common::app_pool(conn_opts.clone()).await;
    let _ = pool_opts;
    let store = PgStore::new(pool.clone());
    // 同じ内容（同じ blob_hash）を2件、別の内容を1件。
    // **ID は別にする。** 同じ請求書を2つの仕訳に紐付けた形を作りたいので、
    // 行としては別だが内容は同じ、という状態を作る。
    let mut same_content = document("2", date(2026, 7, 20));
    same_content.blob_hash = document("1", date(2026, 6, 15)).blob_hash;
    insert(
        &store,
        vec![
            document("1", date(2026, 6, 15)),
            same_content,
            document("3", date(2026, 8, 25)),
        ],
    )
    .await;

    let query = PgDocumentQuery::new(pool);

    assert_eq!(query.count_documents().await.unwrap(), 3, "行の数");
    assert_eq!(
        query.all_blob_hashes().await.unwrap().len(),
        2,
        "内容の種類（こちらを件数に使うと少なく出る）"
    );
}

/// 1件も無ければ 0。**「無い」と「読めなかった」を混ぜない。**
#[sqlx::test]
async fn an_empty_book_has_no_documents(pool_opts: PgPoolOptions, conn_opts: PgConnectOptions) {
    let pool = common::app_pool(conn_opts.clone()).await;
    let _ = pool_opts;

    let query = PgDocumentQuery::new(pool);

    assert_eq!(query.count_documents().await.unwrap(), 0);
}
