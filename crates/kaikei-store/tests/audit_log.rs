//! 監査ログ（`audit_log`）の実効性を実 PostgreSQL に対して検証する。
//!
//! # このファイルの本体は「記帳が失敗しても監査ログが残る」こと
//!
//! `DECISIONS.md` D-070 の決定（**帳簿とは別のコネクションで、操作の前後に
//! 2回書く**）は、同一トランザクションで書く実装と比べて
//! 「失敗した操作の記録が残るかどうか」だけが違う。
//! `kaikei_app::tx::with_tx` は `Err` で必ず rollback するため、同一
//! トランザクションで書く実装に退行すると
//!
//! - `failed_posting_rolls_back_the_ledger_but_keeps_both_audit_rows`
//! - `audit_row_written_inside_a_rolled_back_transaction_still_survives`
//!
//! の2本**だけ**が落ちる。正常系のテストは全て緑のまま通る。
//! この2本がこのファイルの存在理由である
//! （`docs/07-mcp-server.md` §10 の MC-22）。
//!
//! # 対応表（`docs/07-mcp-server.md` §10）
//!
//! | # | ケース | テスト |
//! |---|---|---|
//! | MC-11 | 1呼び出し＝同一 `request_id` で2行 | `successful_posting_records_started_then_ok_with_the_entry_id` |
//! | MC-20 | 開始レコードが書けない → fail-closed | `start_record_failure_prevents_the_posting_and_leaves_the_ledger_untouched` |
//! | MC-21 | 結果レコードだけ書けない → fail-open | `result_record_failure_still_returns_success_and_leaves_a_started_row` |
//! | MC-22 | 記帳が失敗して rollback | `failed_posting_rolls_back_the_ledger_but_keeps_both_audit_rows` |
//! | MC-23 | `kaikei_app` の権限 | `crates/kaikei-store/tests/privileges.rs` / `tests/append_only.rs` |
//!
//! # 入力を理由に fail-closed へ落ちないこと（D-075）
//!
//! `with_audit` は開始レコードが書けなければ操作を実行しない。したがって
//! **入力に起因する INSERT の失敗は「監査ログが使えない」という誤診**になり、
//! 同じ入力で再試行する限り永久に成功しない。JSONB に格納できない入力
//! （U+0000）と、JSON として壊れた入力の2つが実際にその経路へ落ちるため、
//! `PgAuditSink` が無害化して記録する。次の3本がそれを実証している。
//!
//! - `a_nul_character_in_the_input_is_sanitized_and_never_blocks_the_operation`
//! - `a_nul_character_in_the_error_message_still_records_the_result_row`
//! - `an_unparsable_input_is_replaced_by_a_placeholder_instead_of_blocking`
//!
//! `kaikei-mcp` はまだ存在しないため、ツール呼び出しの代わりに
//! 「帳簿への記帳（`JournalRepo::insert_entry` を `with_tx` で包んだもの）」を
//! 操作として使う。検証したいのは監査ログと帳簿トランザクションの関係で
//! あって、ツールの中身ではない。

#![cfg(feature = "pg-tests")]

mod common;

use common::AllOpen;
use kaikei_app::audit::{
    actor, status, with_audit, AuditCall, AuditStart, AuditSuccess, RequestId,
};
use kaikei_app::error::{codes, AppError};
use kaikei_app::ports::{AuditSink, JournalRepo};
use kaikei_app::tx::with_tx;
use kaikei_core::{
    AccountCode, AccountDef, AccountType, AccountingDate, ChartOfAccounts, Currency, EntryId,
    EntryNumber, FiscalYear, FixedClock, JournalEntry, JournalLine, Money, NewEntry, Side,
    TagSchema, TagSet, Timestamp,
};
use kaikei_store::audit::PgAuditSink;
use kaikei_store::convert::timestamp_to_datetime;
use kaikei_store::pool::PgStore;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::PgPool;
use uuid::Uuid;

/// 記帳時刻として固定する値（マイクロ秒境界。`TIMESTAMPTZ` はマイクロ秒精度）。
const FIXED_NANOS: i128 = 1_776_211_200_000_000_000;

fn chart() -> ChartOfAccounts {
    ChartOfAccounts::new(vec![
        AccountDef {
            code: AccountCode::parse("100").unwrap(),
            name: "現金".to_string(),
            account_type: AccountType::Asset,
            parent: None,
            postable: true,
        },
        AccountDef {
            code: AccountCode::parse("500").unwrap(),
            name: "売上高".to_string(),
            account_type: AccountType::Revenue,
            parent: None,
            postable: true,
        },
    ])
    .unwrap()
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

fn balanced_entry(id: u128, entry_no: u32) -> JournalEntry {
    JournalEntry::new(
        NewEntry {
            id: EntryId::new(id),
            entry_no: EntryNumber::new(entry_no),
            entry_date: AccountingDate::new(2026, 4, 15).unwrap(),
            description: "テスト仕訳".to_string(),
            lines: vec![
                line("100", Side::Debit, 1_000),
                line("500", Side::Credit, 1_000),
            ],
            document_refs: Vec::new(),
        },
        &FiscalYear::calendar_year(2026),
        &chart(),
        &TagSchema::new(Vec::new()),
        &AllOpen,
        &FixedClock(Timestamp::from_unix_nanos(FIXED_NANOS)),
    )
    .unwrap()
}

/// `audit_log` の1行（テストのアサーション用）。
#[derive(Debug, sqlx::FromRow)]
struct AuditRow {
    request_id: Uuid,
    occurred_at: chrono::DateTime<chrono::Utc>,
    actor: String,
    tool: String,
    status: String,
    input: Option<serde_json::Value>,
    output: Option<serde_json::Value>,
    error_code: Option<String>,
    entry_id: Option<Uuid>,
}

async fn audit_rows(pool: &PgPool, request_id: RequestId) -> Vec<AuditRow> {
    sqlx::query_as::<_, AuditRow>(
        "SELECT request_id, occurred_at, actor, tool, status, input, output, error_code, entry_id \
         FROM audit_log WHERE request_id = $1 ORDER BY id",
    )
    .bind(Uuid::from_u128(request_id.as_u128()))
    .fetch_all(pool)
    .await
    .expect("audit_log の読み出しに失敗しました")
}

async fn journal_entry_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM journal_entries")
        .fetch_one(pool)
        .await
        .expect("journal_entries の件数取得に失敗しました")
}

/// 帳簿用の `PgStore` と監査ログ用の `PgAuditSink` を、**同一の
/// `kaikei_app` プール**から作る。
///
/// プールを分けないのが本番の形（`docs/07-mcp-server.md` §9。同じ
/// `PgPool` から別の接続を acquire する）。分離の実体は「別プール」では
/// なく「トランザクションを経由しないこと」である点をテスト側でも
/// 崩さない。
async fn fixture(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) -> (PgStore, PgAuditSink, PgPool, PgPool) {
    let roles = common::roles(pool_opts, conn_opts).await;
    let store = PgStore::new(roles.app.clone());
    let sink = PgAuditSink::new(roles.app.clone());
    (store, sink, roles.app, roles.migrator)
}

fn call(request_id: RequestId, input_json: Option<&str>) -> AuditCall<'_> {
    AuditCall {
        request_id,
        actor: actor::MCP,
        tool: "post_journal_entry",
        input_json,
    }
}

/// 記帳（成功する操作）を `with_audit` に渡す形に包む。
async fn post(store: &PgStore, entry: JournalEntry) -> Result<EntryId, AppError> {
    with_tx(store, move |tx| {
        Box::pin(async move {
            tx.insert_entry(&entry).await?;
            Ok(entry.id())
        })
    })
    .await
}

// ---------------------------------------------------------------------------
// MC-11: 成功した操作は開始レコードと結果レコードの2行になる
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn successful_posting_records_started_then_ok_with_the_entry_id(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let (store, sink, app, _migrator) = fixture(pool_opts, conn_opts).await;
    let clock = FixedClock(Timestamp::from_unix_nanos(FIXED_NANOS));
    let request_id = RequestId::new_v7();
    let entry = balanced_entry(1, 1);
    let expected_id = entry.id();

    let audited = with_audit(
        &sink,
        &clock,
        &call(request_id, Some(r#"{"entry_date":"2026-04-15"}"#)),
        || post(&store, entry),
        |id| AuditSuccess {
            entry_id: Some(*id),
            output_json: Some(r#"{"entry_no":1,"policy_notes":[]}"#.to_string()),
        },
    )
    .await
    .expect("開始レコードは書けるはず");

    let mut notes = Vec::new();
    assert_eq!(audited.request_id(), request_id);
    assert!(audited.into_result(&mut notes).is_ok());
    assert!(notes.is_empty(), "結果レコードは書けているはず: {notes:?}");
    assert_eq!(journal_entry_count(&app).await, 1);

    let rows = audit_rows(&app, request_id).await;
    assert_eq!(rows.len(), 2, "1リクエスト＝2行");

    // 同一 request_id で突き合わせられること。
    assert!(rows
        .iter()
        .all(|row| row.request_id == Uuid::from_u128(request_id.as_u128())));
    assert!(rows.iter().all(|row| row.tool == "post_journal_entry"));
    assert!(rows.iter().all(|row| row.actor == actor::MCP));

    // 開始レコード: input のみ。
    assert_eq!(rows[0].status, status::STARTED);
    assert_eq!(
        rows[0].input.as_ref().unwrap()["entry_date"],
        serde_json::json!("2026-04-15")
    );
    assert!(rows[0].output.is_none());
    assert!(rows[0].error_code.is_none());

    // 結果レコード: output と entry_id。
    assert_eq!(rows[1].status, status::OK);
    assert!(rows[1].input.is_none());
    assert_eq!(
        rows[1].output.as_ref().unwrap()["entry_no"],
        serde_json::json!(1)
    );
    assert!(rows[1].error_code.is_none());
    assert_eq!(
        rows[1].entry_id,
        Some(Uuid::from_u128(expected_id.as_u128()))
    );

    // `occurred_at` は Clock から渡した値（`DEFAULT now()` を使っていない）。
    let expected_at = timestamp_to_datetime(Timestamp::from_unix_nanos(FIXED_NANOS)).unwrap();
    assert!(rows.iter().all(|row| row.occurred_at == expected_at));
}

// ---------------------------------------------------------------------------
// ★ MC-22: 記帳が失敗して rollback されても監査ログは残る（D-070 の本体）
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn failed_posting_rolls_back_the_ledger_but_keeps_both_audit_rows(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let (store, sink, app, _migrator) = fixture(pool_opts, conn_opts).await;
    let clock = FixedClock(Timestamp::from_unix_nanos(FIXED_NANOS));
    let request_id = RequestId::new_v7();
    let entry = balanced_entry(2, 1);

    let audited = with_audit(
        &sink,
        &clock,
        &call(request_id, Some(r#"{"description":"A社への請求"}"#)),
        || async {
            // 帳簿には INSERT するが、最後に失敗して with_tx が rollback する。
            // （MCP では貸借不一致・締め済み期間などがこの位置に来る）
            let result: Result<(), AppError> = with_tx(&store, move |tx| {
                Box::pin(async move {
                    tx.insert_entry(&entry).await?;
                    Err(AppError::Rejected {
                        reason: "テスト用の意図的な失敗".to_string(),
                    })
                })
            })
            .await;
            result
        },
        |_| AuditSuccess::default(),
    )
    .await
    .expect("開始レコードは書けるはず");

    let (result, warning) = audited.into_parts_unchecked();
    assert!(warning.is_none(), "結果レコードは書けているはず");
    let err = result.expect_err("操作は失敗するはず");
    assert_eq!(err.code(), codes::REJECTED);

    // 帳簿は変わっていない（rollback された）。
    assert_eq!(
        journal_entry_count(&app).await,
        0,
        "失敗した記帳が帳簿に残っている（rollback されていない）"
    );

    // ★ それでも監査ログは2行とも残る。
    let rows = audit_rows(&app, request_id).await;
    assert_eq!(
        rows.len(),
        2,
        "記帳の rollback で監査ログまで巻き戻っている。\
         audit_log を帳簿と同一トランザクションで書く実装に退行していないか確認すること（D-070）"
    );
    assert_eq!(rows[0].status, status::STARTED);
    assert_eq!(
        rows[0].input.as_ref().unwrap()["description"],
        serde_json::json!("A社への請求"),
        "「AI が何をしようとしたか」が失敗時にこそ残る必要がある"
    );
    assert_eq!(rows[1].status, status::ERROR);
    assert_eq!(rows[1].error_code.as_deref(), Some(codes::REJECTED));
    assert_eq!(
        rows[1].output.as_ref().unwrap()["message"],
        serde_json::json!("テスト用の意図的な失敗")
    );
    assert!(
        rows[1].entry_id.is_none(),
        "記帳されていないので仕訳IDは無い"
    );
}

/// 監査ログの書き込みが**帳簿のトランザクションに巻き込まれない**ことの
/// 直接の証明。
///
/// 上のテストは開始レコードを `with_tx` の外で書いているため、
/// 「別コネクションだから残った」のか「そもそもトランザクションの外だから
/// 残った」のかを区別しない。ここでは `with_tx` の**内側**から監査ログを
/// 書き、そのトランザクションを rollback させる。同一コネクション
/// （`TxOps` に audit メソッドを生やす実装）に退行したら、この行は消える。
#[sqlx::test]
async fn audit_row_written_inside_a_rolled_back_transaction_still_survives(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let (store, sink, app, _migrator) = fixture(pool_opts, conn_opts).await;
    let request_id = RequestId::new_v7();
    let entry = balanced_entry(3, 1);

    let result: Result<(), AppError> = with_tx(&store, |tx| {
        let sink = sink.clone();
        Box::pin(async move {
            tx.insert_entry(&entry).await?;

            sink.record_start(&AuditStart {
                request_id,
                occurred_at: Timestamp::from_unix_nanos(FIXED_NANOS),
                actor: actor::MCP,
                tool: "post_journal_entry",
                input_json: None,
            })
            .await?;

            Err(AppError::Rejected {
                reason: "トランザクションを rollback させるための意図的な失敗".to_string(),
            })
        })
    })
    .await;

    assert!(result.is_err());
    assert_eq!(journal_entry_count(&app).await, 0, "帳簿は rollback される");
    assert_eq!(
        audit_rows(&app, request_id).await.len(),
        1,
        "監査ログが帳簿と同じトランザクションで書かれている（別コネクションになっていない）"
    );
}

/// 上のテストの**対照実験**。
///
/// 「トランザクションの中で書いた行は rollback で消える」ことを、
/// 同一コネクション（同一トランザクション）への素の INSERT で確認する。
/// これが無いと、`audit_row_written_inside_a_rolled_back_transaction_still_survives`
/// が「消える条件が成立していないだけ」でも通ってしまう
/// （空虚な真になっていないことの担保）。
#[sqlx::test]
async fn a_row_written_inside_the_same_transaction_does_disappear_on_rollback(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let (_store, _sink, app, _migrator) = fixture(pool_opts, conn_opts).await;
    let request_id = RequestId::new_v7();

    let mut tx = app.begin().await.expect("トランザクションを開始できること");
    sqlx::query(
        "INSERT INTO audit_log (request_id, occurred_at, actor, tool, status) \
         VALUES ($1, now(), 'mcp', 'post_journal_entry', 'started')",
    )
    .bind(Uuid::from_u128(request_id.as_u128()))
    .execute(&mut *tx)
    .await
    .expect("トランザクション内の INSERT 自体は成功する");
    tx.rollback().await.expect("rollback できること");

    assert_eq!(
        audit_rows(&app, request_id).await.len(),
        0,
        "同一トランザクションで書いた監査ログは rollback で消える（対照実験が成立していない）"
    );
}

// ---------------------------------------------------------------------------
// MC-20 / MC-21: fail-closed と fail-open
// ---------------------------------------------------------------------------

/// MC-20: 開始レコードが書けない状況では、**帳簿に1件も入らない**。
///
/// 状況の作り方は `docs/07-mcp-server.md` §10 が挙げる
/// 「`REVOKE INSERT ON audit_log FROM kaikei_app`」。
#[sqlx::test]
async fn start_record_failure_prevents_the_posting_and_leaves_the_ledger_untouched(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let (store, sink, app, migrator) = fixture(pool_opts, conn_opts).await;
    let clock = FixedClock(Timestamp::from_unix_nanos(FIXED_NANOS));
    let request_id = RequestId::new_v7();
    let entry = balanced_entry(4, 1);

    sqlx::query("REVOKE INSERT ON audit_log FROM kaikei_app")
        .execute(&migrator)
        .await
        .expect("監査ログへの INSERT 権限の剥奪に失敗しました");

    let err = with_audit(
        &sink,
        &clock,
        &call(request_id, None),
        || post(&store, entry),
        |id| AuditSuccess {
            entry_id: Some(*id),
            output_json: None,
        },
    )
    .await
    .expect_err("開始レコードが書けないので fail-closed になるはず");

    // ★ 操作は実行されていない。
    assert_eq!(
        journal_entry_count(&app).await,
        0,
        "監査ログに記録できていないのに記帳されている（fail-closed になっていない）"
    );

    assert_eq!(err.code(), codes::AUDIT_LOG_UNAVAILABLE);
    assert_ne!(err.code(), codes::REJECTED, "入力を直せば通る拒否ではない");

    let message = err.public_message();
    assert!(
        message.contains("帳簿は変更されていません"),
        "帳簿が無変更であることまで伝えないと、AI は二重記帳の確認に走る: {message}"
    );
    // 42501 は RepoError::AppendOnlyViolation に写像されるが、その
    // 「訂正は逆仕訳で」という案内が応答に漏れてはいけない（D-038 と同じ誤診クラス）。
    assert!(!message.contains("逆仕訳"), "{message}");

    // ★ `cause` は pub フィールドである。presentation 層が診断のつもりで
    //   `cause.public_message()` を出しても誤案内が復活しないこと
    //   （`sqlstate.rs` の共通写像は 42501 を一律 AppendOnlyViolation にする。
    //   `kaikei_store::audit` 側で包み直している）。
    assert_eq!(
        err.cause.code(),
        codes::BACKEND,
        "audit_log の 42501 が帳簿と同じ分類のまま漏れている: {:?}",
        err.cause
    );
    assert_ne!(err.cause.code(), codes::APPEND_ONLY_VIOLATION);
    assert!(
        !err.cause.public_message().contains("逆仕訳"),
        "{}",
        err.cause.public_message()
    );
    assert!(!err.cause.to_string().contains("逆仕訳"), "{}", err.cause);
    // 診断情報（SQLSTATE と DB のメッセージ）は失っていない。
    assert!(err.to_string().contains("42501"), "{err}");
    assert!(err.to_string().contains("permission denied"), "{err}");
}

// ---------------------------------------------------------------------------
// ★ 入力が原因で fail-closed に落ちない（D-075）
// ---------------------------------------------------------------------------

/// `input` に **U+0000** を含む JSON（JSON としても JSON-RPC としても正当）を
/// 渡しても、操作は**実行され**、監査ログには**記録が残る**。
///
/// PostgreSQL の `jsonb` は U+0000 を格納できず SQLSTATE `22P05`
/// （`unsupported Unicode escape sequence`）で拒否する。無害化しない実装では
/// 開始レコードの INSERT が失敗し、`with_audit` が fail-closed に落ちて
///
/// 1. 正常な監査ログを「使えない」と誤診し
/// 2. 再試行しても永久に成功せず
/// 3. AI には原因（自分が送った1文字）が分からない
///
/// という詰みになる（`CLAUDE.md` §11 / D-038 が潰した誤診クラス）。
#[sqlx::test]
async fn a_nul_character_in_the_input_is_sanitized_and_never_blocks_the_operation(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let (store, sink, app, _migrator) = fixture(pool_opts, conn_opts).await;
    let clock = FixedClock(Timestamp::from_unix_nanos(FIXED_NANOS));
    let request_id = RequestId::new_v7();
    let entry = balanced_entry(6, 1);

    // レビュアーが実 PostgreSQL で再現に使った入力そのもの。
    let input = "{\"description\":\"A\\u0000B\"}";

    let audited = with_audit(
        &sink,
        &clock,
        &call(request_id, Some(input)),
        || post(&store, entry),
        |id| AuditSuccess {
            entry_id: Some(*id),
            output_json: None,
        },
    )
    .await
    .expect("U+0000 入りの入力で fail-closed に落ちてはいけない");

    // (1) 操作が実行されている。
    let mut notes = Vec::new();
    assert!(audited.into_result(&mut notes).is_ok());
    assert_eq!(
        journal_entry_count(&app).await,
        1,
        "入力の1文字を理由に操作が実行されなくなっている"
    );
    assert!(notes.is_empty(), "結果レコードも書けているはず: {notes:?}");

    // (2) 記録が残っている（2行）。
    let rows = audit_rows(&app, request_id).await;
    assert_eq!(rows.len(), 2, "監査の証拠が残っていない");
    assert_eq!(rows[0].status, status::STARTED);
    assert_eq!(rows[1].status, status::OK);

    // (3) 置換されたことが記録から分かる（「原文どおり」と読めない形）。
    let recorded_input = rows[0].input.as_ref().expect("input が記録されていない");
    assert_eq!(
        recorded_input["_audit"]["verbatim"],
        serde_json::json!(false),
        "原文と違うものを黙って記録している: {recorded_input}"
    );
    assert_eq!(
        recorded_input["_audit"]["replaced_nul"],
        serde_json::json!(1)
    );
    assert_eq!(
        recorded_input["value"]["description"],
        serde_json::json!("A\u{FFFD}B"),
        "内容が失われている: {recorded_input}"
    );
}

/// 失敗の結果レコードに載る本文（`public_message()`）に U+0000 が
/// 混じっても、結果レコードは記録される（fail-open の警告にならない）。
///
/// 本文には入力由来の文字列が入りうる（`AppError::Rejected` の `reason` 等）。
#[sqlx::test]
async fn a_nul_character_in_the_error_message_still_records_the_result_row(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let (_store, sink, app, _migrator) = fixture(pool_opts, conn_opts).await;
    let clock = FixedClock(Timestamp::from_unix_nanos(FIXED_NANOS));
    let request_id = RequestId::new_v7();

    let audited = with_audit(
        &sink,
        &clock,
        &call(request_id, None),
        || async {
            Err::<EntryId, _>(AppError::Rejected {
                reason: "摘要に不正な文字が含まれています: A\u{0000}B".to_string(),
            })
        },
        |id| AuditSuccess {
            entry_id: Some(*id),
            output_json: None,
        },
    )
    .await
    .expect("開始レコードは書けるはず");

    let (result, warning) = audited.into_parts_unchecked();
    assert!(result.is_err());
    assert!(
        warning.is_none(),
        "本文の1文字を理由に結果レコードを落としている"
    );

    let rows = audit_rows(&app, request_id).await;
    assert_eq!(rows.len(), 2);
    let output = rows[1].output.as_ref().expect("output が記録されていない");
    assert_eq!(output["_audit"]["verbatim"], serde_json::json!(false));
    assert!(
        output["value"]["message"]
            .as_str()
            .expect("message は文字列")
            .contains('\u{FFFD}'),
        "{output}"
    );
}

/// JSON として解釈できない `input`（presentation 層のバグ）でも、
/// 操作は実行され、**誰がいつ何のツールを呼んだか**は記録される。
#[sqlx::test]
async fn an_unparsable_input_is_replaced_by_a_placeholder_instead_of_blocking(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let (store, sink, app, _migrator) = fixture(pool_opts, conn_opts).await;
    let clock = FixedClock(Timestamp::from_unix_nanos(FIXED_NANOS));
    let request_id = RequestId::new_v7();
    let entry = balanced_entry(7, 1);

    let audited = with_audit(
        &sink,
        &clock,
        &call(request_id, Some("{壊れた")),
        || post(&store, entry),
        |id| AuditSuccess {
            entry_id: Some(*id),
            output_json: None,
        },
    )
    .await
    .expect("壊れた JSON で fail-closed に落ちてはいけない");

    let mut notes = Vec::new();
    assert!(audited.into_result(&mut notes).is_ok());
    assert_eq!(journal_entry_count(&app).await, 1);

    let rows = audit_rows(&app, request_id).await;
    assert_eq!(rows.len(), 2);
    let recorded_input = rows[0].input.as_ref().expect("input が記録されていない");
    assert_eq!(
        recorded_input["_audit"]["recorded"],
        serde_json::json!(false),
        "記録できなかったことが分かる形になっていない: {recorded_input}"
    );
    assert_eq!(
        recorded_input["_audit"]["reason"],
        serde_json::json!("invalid_json")
    );
    assert_eq!(rows[0].tool, "post_journal_entry");
    assert_eq!(rows[0].actor, actor::MCP);
}

/// MC-21: 結果レコードだけが書けない場合、操作は成功として返り、
/// 開始レコードだけが残る（「結果不明」として識別できる）。
#[sqlx::test]
async fn result_record_failure_still_returns_success_and_leaves_a_started_row(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let (store, sink, app, migrator) = fixture(pool_opts, conn_opts).await;
    let clock = FixedClock(Timestamp::from_unix_nanos(FIXED_NANOS));
    let request_id = RequestId::new_v7();
    let entry = balanced_entry(5, 1);

    let audited = with_audit(
        &sink,
        &clock,
        &call(request_id, None),
        || async {
            let posted = post(&store, entry).await;
            // 記帳が確定した「後で」監査ログが書けなくなる状況を作る。
            sqlx::query("REVOKE INSERT ON audit_log FROM kaikei_app")
                .execute(&migrator)
                .await
                .expect("監査ログへの INSERT 権限の剥奪に失敗しました");
            posted
        },
        |id| AuditSuccess {
            entry_id: Some(*id),
            output_json: None,
        },
    )
    .await
    .expect("開始レコードは書けている（剥奪は操作の後）");

    // 操作は成功として返る（fail-open）。警告は注記に積まれる。
    let mut notes = Vec::new();
    assert!(audited.into_result(&mut notes).is_ok());
    assert_eq!(journal_entry_count(&app).await, 1);

    assert_eq!(notes.len(), 1, "fail-open の警告が注記に積まれていない");
    let message = &notes[0];
    assert!(message.contains(&request_id.to_uuid_string()), "{message}");
    assert!(
        !message.contains("再実行"),
        "再実行を促すと二重計上を招く: {message}"
    );

    // 開始レコードだけが残る＝「結果不明」。
    let rows = audit_rows(&app, request_id).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, status::STARTED);
}

// ---------------------------------------------------------------------------
// 記録される内容に接続文字列・認証情報が入らないこと
// ---------------------------------------------------------------------------

/// `RepoError::Backend` の `reason` には DB が返した文字列（接続文字列・
/// ロール名・制約定義を含みうる）がそのまま入る
/// （`kaikei-store::sqlstate::map_sqlstate`）。`audit_log.output` に載るのは
/// `public_message()` だけであり、`Display` は載らない
/// （`docs/07-mcp-server.md` §9）。
///
/// この不変条件は `kaikei_app::audit::AuditOutcome::Failed` が
/// `public_message` しか運べない形になっていることで構造的に保たれている。
#[sqlx::test]
async fn audit_output_never_carries_the_connection_string_from_a_backend_error(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let (_store, sink, app, _migrator) = fixture(pool_opts, conn_opts).await;
    let clock = FixedClock(Timestamp::from_unix_nanos(FIXED_NANOS));
    let request_id = RequestId::new_v7();
    let secret = "postgres://kaikei_app:s3cret@localhost:5432/kaikei";

    let audited = with_audit(
        &sink,
        &clock,
        &call(request_id, Some(r#"{"description":"A社への請求"}"#)),
        || async {
            Err::<EntryId, _>(AppError::Repo(kaikei_app::error::RepoError::Backend {
                reason: format!("接続に失敗しました: {secret}"),
            }))
        },
        |id| AuditSuccess {
            entry_id: Some(*id),
            output_json: None,
        },
    )
    .await
    .expect("開始レコードは書けるはず");

    let (result, _warning) = audited.into_parts_unchecked();
    assert!(result.is_err());

    let rows = audit_rows(&app, request_id).await;
    // 記録された JSONB 列を全て繋いで、生メッセージが混じっていないか見る。
    let recorded = rows
        .iter()
        .map(|row| {
            format!(
                "{}{}",
                row.input
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                row.output
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("");
    assert!(!recorded.contains("postgres://"), "{recorded}");
    assert!(!recorded.contains("s3cret"), "{recorded}");
    assert_eq!(rows[1].error_code.as_deref(), Some(codes::BACKEND));
}

// ---------------------------------------------------------------------------
// スキーマ側の防御（status と error_code の対応）
// ---------------------------------------------------------------------------

/// `status='error'` なのに `error_code` が無い行は DB が拒否する
/// （`kaikei_app::audit::AuditOutcome` が型で保証しているのと同じ対応を、
/// DB 側にも CHECK として置いてある。多層防御）。
#[sqlx::test]
async fn audit_log_rejects_an_error_row_without_an_error_code(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let (_store, _sink, app, _migrator) = fixture(pool_opts, conn_opts).await;

    let result = sqlx::query(
        "INSERT INTO audit_log (request_id, occurred_at, actor, tool, status) \
         VALUES ($1, now(), 'mcp', 'post_journal_entry', 'error')",
    )
    .bind(Uuid::now_v7())
    .execute(&app)
    .await;

    let err = result.expect_err("error_code の無い error 行は拒否されるはず");
    assert_eq!(common::sqlstate(&err).as_deref(), Some("23514"));
}

/// `status` は 'started' / 'ok' / 'error' の3つに限る。
#[sqlx::test]
async fn audit_log_rejects_an_unknown_status(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let (_store, _sink, app, _migrator) = fixture(pool_opts, conn_opts).await;

    let result = sqlx::query(
        "INSERT INTO audit_log (request_id, occurred_at, actor, tool, status) \
         VALUES ($1, now(), 'mcp', 'post_journal_entry', 'pending')",
    )
    .bind(Uuid::now_v7())
    .execute(&app)
    .await;

    let err = result.expect_err("未知の status は拒否されるはず");
    assert_eq!(common::sqlstate(&err).as_deref(), Some("23514"));
}

/// `entry_id` に外部キーを張っていないこと（`docs/07-mcp-server.md` §9）。
///
/// rollback された操作の仕訳IDは `journal_entries` に存在しえない。
/// FK があると「存在しないことを記録する」ことができなくなる。
#[sqlx::test]
async fn audit_log_accepts_an_entry_id_that_does_not_exist_in_the_ledger(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let (_store, _sink, app, _migrator) = fixture(pool_opts, conn_opts).await;
    let request_id = RequestId::new_v7();

    sqlx::query(
        "INSERT INTO audit_log (request_id, occurred_at, actor, tool, status, entry_id) \
         VALUES ($1, now(), 'mcp', 'post_journal_entry', 'ok', $2)",
    )
    .bind(Uuid::from_u128(request_id.as_u128()))
    .bind(Uuid::now_v7())
    .execute(&app)
    .await
    .expect("帳簿に存在しない仕訳IDでも記録できる必要がある");

    assert_eq!(audit_rows(&app, request_id).await.len(), 1);
    assert_eq!(journal_entry_count(&app).await, 0);
}
