//! [`kaikei_app::ports::JournalRepo`] の PostgreSQL 実装。
//!
//! - [`row`]: `journal_entries`/`journal_lines` の生 DB 行表現と、両者を束ねる
//!   ローカル型 `EntryRows`（孤児則の回避。`row.rs` のモジュール doc を参照）
//! - [`mapper`]: `EntryRows` → `JournalEntry` への変換。無検証の復元専用
//!   コンストラクタを呼んでよい唯一の場所（`mapper.rs` のモジュール doc を参照）

mod mapper;
mod row;

use crate::convert::{
    accounting_date_to_naive_date, entry_no_from_i32, entry_no_to_i32, money_to_columns,
    side_to_i16, timestamp_to_datetime,
};
use crate::error::from_sqlx_error;
use crate::store::PgTx;
use crate::tags::tag_set_to_json;
use async_trait::async_trait;
use kaikei_app::error::RepoError;
use kaikei_app::id::{entry_id_from_uuid, entry_id_to_uuid};
use kaikei_app::ports::JournalRepo;
use kaikei_core::{AccountingDate, EntryId, EntryNumber, JournalEntry, TagValue};
use row::{EntryRows, JournalEntryRow, JournalLineRow};
use std::collections::HashMap;

/// PostgreSQL の `text`/`jsonb` は U+0000（NUL）を格納できない。
///
/// `JournalEntry::new` の摘要検証は `trim().is_empty()` のみで NUL を拒否せず、
/// `JournalLine::new` も明細メモ・タグの `Text`/`Code` 値に対して同様の検証を
/// 行わない（phase1計画 R12）。ドメイン検証を通過したデータが保存段階で
/// 分かりにくい DB エラーとして落ちるのを避けるため、`insert_entry` が
/// 摘要・明細メモ・逆仕訳理由・タグの `Text`/`Code` 値のすべてに対してこの
/// 関数で明示的に検出する（`DECISIONS.md` D-041 の適用範囲を拡張）。
fn reject_nul(s: &str) -> Result<(), RepoError> {
    if s.contains('\0') {
        return Err(RepoError::Corrupt {
            reason: "保存しようとした文字列に制御文字（NUL）が含まれているため保存できません"
                .to_string(),
        });
    }
    Ok(())
}

/// `journal_entries` から SELECT する列。
///
/// 1件取得（`find_entry`）と期間取得（`list_entries_in_period`）で共有する。
/// 列を片方にだけ足すと [`JournalEntryRow`] の `FromRow` が**実行時にしか**
/// 落ちない（コンパイルは通る）。1箇所に置いて、その事故が起きない形にする。
const ENTRY_COLUMNS: &str = "id, fiscal_year, entry_no, entry_date, description, \
                             reverses, reverse_reason, recorded_at";

/// `journal_lines` から SELECT する列。同上。
const LINE_COLUMNS: &str = "entry_id, line_no, account_code, side, amount_minor, \
                            currency, currency_minor_unit, tags, memo";

#[async_trait]
impl JournalRepo for PgTx<'_> {
    async fn find_entry(&mut self, id: EntryId) -> Result<Option<JournalEntry>, RepoError> {
        let uuid = entry_id_to_uuid(id);

        let entry_row: Option<JournalEntryRow> = sqlx::query_as(&format!(
            "SELECT {ENTRY_COLUMNS} FROM journal_entries WHERE id = $1"
        ))
        .bind(uuid)
        .fetch_optional(self.conn())
        .await
        .map_err(from_sqlx_error)?;

        let Some(entry_row) = entry_row else {
            return Ok(None);
        };

        let line_rows: Vec<JournalLineRow> = sqlx::query_as(&format!(
            "SELECT {LINE_COLUMNS} FROM journal_lines WHERE entry_id = $1 ORDER BY line_no"
        ))
        .bind(uuid)
        .fetch_all(self.conn())
        .await
        .map_err(from_sqlx_error)?;

        let entry = JournalEntry::try_from(EntryRows {
            entry: entry_row,
            lines: line_rows,
        })?;
        Ok(Some(entry))
    }

    async fn list_entries_in_period(
        &mut self,
        from: AccountingDate,
        to: AccountingDate,
    ) -> Result<Vec<JournalEntry>, RepoError> {
        let from_date = accounting_date_to_naive_date(from)?;
        let to_date = accounting_date_to_naive_date(to)?;

        // 並びは (entry_date, entry_no)。仕訳日記帳がこの順で出せる形にしておく。
        // `entry_no` は会計年度ごとの連番なので、年度をまたぐ期間では
        // `entry_date` が先に効く必要がある（順序を入れ替えると年度の
        // 切り替わりで並びが崩れる）。
        let entry_rows: Vec<JournalEntryRow> = sqlx::query_as(&format!(
            "SELECT {ENTRY_COLUMNS} FROM journal_entries \
             WHERE entry_date >= $1 AND entry_date <= $2 \
             ORDER BY entry_date, entry_no"
        ))
        .bind(from_date)
        .bind(to_date)
        .fetch_all(self.conn())
        .await
        .map_err(from_sqlx_error)?;

        if entry_rows.is_empty() {
            return Ok(Vec::new());
        }

        // 明細は1回のクエリでまとめて取る。仕訳ごとに引き直すと件数ぶんの
        // 往復になる（決算では1年分＝数千件を読む）。
        let ids: Vec<uuid::Uuid> = entry_rows.iter().map(|row| row.id).collect();
        let line_rows: Vec<JournalLineRow> = sqlx::query_as(&format!(
            "SELECT {LINE_COLUMNS} FROM journal_lines \
             WHERE entry_id = ANY($1) ORDER BY entry_id, line_no"
        ))
        .bind(&ids)
        .fetch_all(self.conn())
        .await
        .map_err(from_sqlx_error)?;

        let mut lines_by_entry: HashMap<uuid::Uuid, Vec<JournalLineRow>> = HashMap::new();
        for line in line_rows {
            lines_by_entry.entry(line.entry_id).or_default().push(line);
        }

        let mut entries = Vec::with_capacity(entry_rows.len());
        for entry_row in entry_rows {
            // 明細が1行も無い仕訳は `JournalEntry::try_from` が弾く
            // （貸借不一致として `RepoError::Corrupt`）。ここで空を
            // 黙って通さないこと——読めた仕訳が壊れていたら、決算書の
            // 金額が静かに狂う。
            let lines = lines_by_entry.remove(&entry_row.id).unwrap_or_default();
            entries.push(JournalEntry::try_from(EntryRows {
                entry: entry_row,
                lines,
            })?);
        }
        Ok(entries)
    }

    async fn find_reversal_of(
        &mut self,
        id: EntryId,
    ) -> Result<Option<(EntryId, EntryNumber)>, RepoError> {
        let uuid = entry_id_to_uuid(id);

        let row: Option<(uuid::Uuid, i32)> =
            sqlx::query_as("SELECT id, entry_no FROM journal_entries WHERE reverses = $1")
                .bind(uuid)
                .fetch_optional(self.conn())
                .await
                .map_err(from_sqlx_error)?;

        match row {
            Some((reversal_id, entry_no)) => {
                let entry_no = entry_no_from_i32(entry_no)?;
                Ok(Some((entry_id_from_uuid(reversal_id), entry_no)))
            }
            None => Ok(None),
        }
    }

    async fn insert_entry(&mut self, entry: &JournalEntry) -> Result<(), RepoError> {
        // F-1（人間承認済み）: 証憑紐付け（document_refs）は Phase 1 ではサポートしない
        // （Phase 4 の attach_document ユースケースに送る）。「保存できないものを
        // 静かに落とさない」ため、非空なら明示的に拒否する。
        if !entry.document_refs().is_empty() {
            return Err(RepoError::Unsupported {
                reason: "証憑（document_refs）の保存は Phase 1 ではサポートされていません\
                         （Phase 4 の attach_document ユースケースで対応予定です）"
                    .to_string(),
            });
        }

        // phase1計画 R12（人間確認事項）: 摘要・明細メモ・逆仕訳理由・タグの
        // Text/Code 値のすべてに NUL バイト拒否を適用する（`reject_nul` の doc
        // を参照。修正前は摘要のみに適用されており、memo/reverse_reason/タグ
        // 値が素通りしていた）。
        reject_nul(entry.description())?;
        if let Some(reason) = entry.reverse_reason() {
            reject_nul(reason)?;
        }
        for line in entry.lines() {
            if let Some(memo) = line.memo() {
                reject_nul(memo)?;
            }
            for (_, value) in line.tags().iter() {
                match value {
                    TagValue::Text(s) | TagValue::Code(s) => reject_nul(s)?,
                    // Decimal/Date は構造化された値であり、そのシリアライズ表現
                    // （数字・ISO日付文字列）に NUL が混入する経路が無い。
                    TagValue::Decimal(_) | TagValue::Date(_) => {}
                }
            }
        }

        let id = entry_id_to_uuid(entry.id());
        let entry_no = entry_no_to_i32(entry.entry_no())?;
        let entry_date = accounting_date_to_naive_date(entry.entry_date())?;
        let reverses = entry.reverses().map(entry_id_to_uuid);
        let recorded_at = timestamp_to_datetime(entry.recorded_at())?;

        sqlx::query(
            "INSERT INTO journal_entries \
             (id, fiscal_year, entry_no, entry_date, description, reverses, reverse_reason, \
              recorded_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(id)
        .bind(entry.fiscal_year())
        .bind(entry_no)
        .bind(entry_date)
        .bind(entry.description())
        .bind(reverses)
        .bind(entry.reverse_reason())
        .bind(recorded_at)
        .execute(self.conn())
        .await
        .map_err(from_sqlx_error)?;

        let n = entry.lines().len();
        let mut line_nos: Vec<i16> = Vec::with_capacity(n);
        let mut account_codes: Vec<String> = Vec::with_capacity(n);
        let mut sides: Vec<i16> = Vec::with_capacity(n);
        let mut amounts: Vec<i64> = Vec::with_capacity(n);
        let mut currencies: Vec<String> = Vec::with_capacity(n);
        let mut minor_units: Vec<i16> = Vec::with_capacity(n);
        let mut tags: Vec<serde_json::Value> = Vec::with_capacity(n);
        let mut memos: Vec<Option<String>> = Vec::with_capacity(n);

        for (index, line) in entry.lines().iter().enumerate() {
            let line_no = i16::try_from(index + 1).map_err(|_| RepoError::OutOfRange {
                reason: format!(
                    "仕訳の明細数が保存できる範囲を超えています（{n}行、上限 {}）",
                    i16::MAX
                ),
            })?;
            let (amount_minor, currency, currency_minor_unit) = money_to_columns(line.amount())?;

            line_nos.push(line_no);
            account_codes.push(line.account().as_str().to_string());
            sides.push(side_to_i16(line.side()));
            amounts.push(amount_minor);
            currencies.push(currency);
            minor_units.push(currency_minor_unit);
            tags.push(tag_set_to_json(line.tags()));
            memos.push(line.memo().map(str::to_string));
        }

        // G7: 明細の一括 INSERT は UNNEST で1文にまとめ、往復を減らす。
        sqlx::query(
            "INSERT INTO journal_lines \
             (entry_id, line_no, account_code, side, amount_minor, currency, \
              currency_minor_unit, tags, memo) \
             SELECT $1, u.line_no, u.account_code, u.side, u.amount_minor, u.currency, \
                    u.currency_minor_unit, u.tags, u.memo \
             FROM UNNEST($2::smallint[], $3::text[], $4::smallint[], $5::bigint[], \
                         $6::text[], $7::smallint[], $8::jsonb[], $9::text[]) \
                  AS u(line_no, account_code, side, amount_minor, currency, \
                       currency_minor_unit, tags, memo)",
        )
        .bind(id)
        .bind(line_nos)
        .bind(account_codes)
        .bind(sides)
        .bind(amounts)
        .bind(currencies)
        .bind(minor_units)
        .bind(tags)
        .bind(memos)
        .execute(self.conn())
        .await
        .map_err(from_sqlx_error)?;

        Ok(())
    }
}
