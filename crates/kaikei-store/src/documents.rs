//! 証憑の登録（[`kaikei_app::ports::DocumentRepo`]）と検索
//! （[`kaikei_app::ports::DocumentQueryPort`]）の PostgreSQL 実装。
//!
//! `docs/06-documents.md` §3・§4。ファイルの中身は `kaikei-blob` が内容の
//! SHA-256 で持ち、ここは**メタデータと帳簿との紐付け**を扱う。
//!
//! # 登録は帳簿と同じトランザクションで行う
//!
//! [`DocumentRepo`] は `PgTx` に実装する。仕訳と証憑の紐付けが半分だけ残ると、
//! 帳簿から証憑への道筋が壊れるためである。
//!
//! 一方、検索は `PgPool` から直接引く（`CLAUDE.md` §6「read model は物理的に
//! 分離する」）。
//!
//! # 値の妥当性は DB が持つ
//!
//! `doc_type` / `received_via` の値、ハッシュの表記、ファイル名が空でないこと
//! などは 0010 の CHECK 制約が見る。**ここで二重に検査しない**——検査が2箇所に
//! あると、片方だけ直したときに食い違う。制約違反は
//! [`crate::error::from_sqlx_error`] が `RepoError` に写す。

use async_trait::async_trait;
use kaikei_app::error::RepoError;
use kaikei_app::ports::{DocumentQueryPort, DocumentRepo, NewDocument};
use kaikei_app::view::{DocumentQuery, DocumentView};
use kaikei_core::{AccountingDate, EntryId};
use sqlx::PgPool;

use crate::store::PgTx;

/// 検索で一度に返す上限。
///
/// **呼び出し側が上限を渡すが、それでも上限の上限を持つ。** 条件を付け忘れた
/// 検索が帳簿全体を返すと、応答が帳簿の大きさに比例して膨らむ。
pub const MAX_SEARCH_LIMIT: u32 = 200;

#[async_trait]
impl DocumentRepo for PgTx<'_> {
    async fn insert_document(&mut self, document: &NewDocument) -> Result<(), RepoError> {
        let id = parse_uuid(&document.id)?;
        sqlx::query(
            "INSERT INTO documents \
             (id, blob_hash, original_name, mime_type, byte_size, doc_date, \
              amount_minor, counterparty, doc_type, received_via, received_at, note, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, now())",
        )
        .bind(id)
        .bind(&document.blob_hash)
        .bind(&document.original_name)
        .bind(&document.mime_type)
        .bind(document.byte_size)
        .bind(to_naive_date(document.doc_date)?)
        .bind(document.amount_minor)
        .bind(document.counterparty.as_deref())
        .bind(&document.doc_type)
        .bind(&document.received_via)
        .bind(to_offset_datetime(document.received_at)?)
        .bind(document.note.as_deref())
        .execute(self.conn())
        .await
        .map_err(crate::error::from_sqlx_error)?;
        Ok(())
    }

    async fn link_document(
        &mut self,
        entry_id: EntryId,
        document_id: &str,
    ) -> Result<(), RepoError> {
        let document = parse_uuid(document_id)?;
        // **同じ組み合わせを2回登録しても失敗しない。** 取り込みを何度流しても
        // 同じ結果になるようにする（紐付けは追記のみなので、消して入れ直す
        // 経路が無い）。
        sqlx::query(
            "INSERT INTO entry_documents (entry_id, document_id) VALUES ($1, $2) \
             ON CONFLICT (entry_id, document_id) DO NOTHING",
        )
        .bind(uuid::Uuid::from_u128(entry_id.as_u128()))
        .bind(document)
        .execute(self.conn())
        .await
        .map_err(crate::error::from_sqlx_error)?;
        Ok(())
    }
}

/// 証憑の検索（read model）。
#[derive(Debug, Clone)]
pub struct PgDocumentQuery {
    pool: PgPool,
}

impl PgDocumentQuery {
    /// プールから作る。
    pub fn new(pool: PgPool) -> Self {
        PgDocumentQuery { pool }
    }
}

#[async_trait]
impl DocumentQueryPort for PgDocumentQuery {
    async fn search_documents(
        &self,
        query: &DocumentQuery,
        limit: u32,
    ) -> Result<Vec<DocumentView>, RepoError> {
        let limit = limit.min(MAX_SEARCH_LIMIT) as i64;

        // **SQL は固定にする。** 条件の有無で文字列を組み立てず、NULL のときは
        // その条件を素通りさせる（`$n IS NULL OR ...`）。組み立てをやめれば
        // 注入の余地が構造的に無くなる。
        let rows = sqlx::query_as::<_, DocumentRow>(
            "SELECT id::text, blob_hash, original_name, mime_type, byte_size, \
                    doc_date, amount_minor, counterparty, doc_type, received_via, note \
             FROM documents \
             WHERE ($1::date IS NULL OR doc_date >= $1) \
               AND ($2::date IS NULL OR doc_date <= $2) \
               AND ($3::bigint IS NULL OR amount_minor >= $3) \
               AND ($4::bigint IS NULL OR amount_minor <= $4) \
               AND ($5::text IS NULL OR counterparty = $5) \
               AND ($6::text IS NULL OR doc_type = $6) \
             ORDER BY doc_date DESC, id ASC \
             LIMIT $7",
        )
        .bind(query.date_from.map(to_naive_date).transpose()?)
        .bind(query.date_to.map(to_naive_date).transpose()?)
        .bind(query.amount_min)
        .bind(query.amount_max)
        .bind(query.counterparty.as_deref())
        .bind(query.doc_type.as_deref())
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(crate::error::from_sqlx_error)?;

        rows.into_iter().map(DocumentRow::into_view).collect()
    }

    async fn documents_of_entry(&self, entry_id: EntryId) -> Result<Vec<DocumentView>, RepoError> {
        let rows = sqlx::query_as::<_, DocumentRow>(
            "SELECT d.id::text, d.blob_hash, d.original_name, d.mime_type, d.byte_size, \
                    d.doc_date, d.amount_minor, d.counterparty, d.doc_type, d.received_via, d.note \
             FROM documents d \
             JOIN entry_documents ed ON ed.document_id = d.id \
             WHERE ed.entry_id = $1 \
             ORDER BY d.doc_date DESC, d.id ASC",
        )
        .bind(uuid::Uuid::from_u128(entry_id.as_u128()))
        .fetch_all(&self.pool)
        .await
        .map_err(crate::error::from_sqlx_error)?;

        rows.into_iter().map(DocumentRow::into_view).collect()
    }
}

#[derive(sqlx::FromRow)]
struct DocumentRow {
    id: String,
    blob_hash: String,
    original_name: String,
    mime_type: String,
    byte_size: i64,
    doc_date: chrono::NaiveDate,
    amount_minor: Option<i64>,
    counterparty: Option<String>,
    doc_type: String,
    received_via: String,
    note: Option<String>,
}

impl DocumentRow {
    fn into_view(self) -> Result<DocumentView, RepoError> {
        Ok(DocumentView {
            id: self.id,
            blob_hash: self.blob_hash,
            original_name: self.original_name,
            mime_type: self.mime_type,
            byte_size: self.byte_size,
            doc_date: from_naive_date(self.doc_date)?,
            amount_minor: self.amount_minor,
            counterparty: self.counterparty,
            doc_type: self.doc_type,
            received_via: self.received_via,
            note: self.note,
        })
    }
}

fn parse_uuid(text: &str) -> Result<uuid::Uuid, RepoError> {
    uuid::Uuid::parse_str(text).map_err(|source| RepoError::Corrupt {
        reason: format!("証憑IDが UUID ではありません: \"{text}\"（{source}）"),
    })
}

fn to_naive_date(date: AccountingDate) -> Result<chrono::NaiveDate, RepoError> {
    chrono::NaiveDate::from_ymd_opt(date.year(), date.month() as u32, date.day() as u32).ok_or_else(
        || RepoError::Corrupt {
            reason: format!("日付を変換できません: {}", date.to_iso_string()),
        },
    )
}

fn from_naive_date(date: chrono::NaiveDate) -> Result<AccountingDate, RepoError> {
    use chrono::Datelike;
    AccountingDate::new(date.year(), date.month() as u8, date.day() as u8).map_err(|source| {
        RepoError::Corrupt {
            reason: format!("保存されている日付を復元できません: {date}（{source}）"),
        }
    })
}

fn to_offset_datetime(
    timestamp: kaikei_core::Timestamp,
) -> Result<chrono::DateTime<chrono::Utc>, RepoError> {
    let nanos = timestamp.as_unix_nanos();
    let seconds =
        i64::try_from(nanos.div_euclid(1_000_000_000)).map_err(|_| RepoError::Corrupt {
            reason: format!("授受日時が範囲外です: {nanos} ナノ秒"),
        })?;
    let sub_nanos = nanos.rem_euclid(1_000_000_000) as u32;
    chrono::DateTime::from_timestamp(seconds, sub_nanos).ok_or_else(|| RepoError::Corrupt {
        reason: format!("授受日時を変換できません: {nanos} ナノ秒"),
    })
}
