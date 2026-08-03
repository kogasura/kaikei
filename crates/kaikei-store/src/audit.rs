//! [`kaikei_app::ports::AuditSink`] の PostgreSQL 実装。
//!
//! # ★ 帳簿とは別のコネクションで書く★（`DECISIONS.md` D-070 / D-075）
//!
//! [`PgAuditSink`] は `PgPool` を保持し、書き込みのたびに**プールから別の
//! 接続を取る**（`execute(&self.pool)`）。`PgStore::begin` が開いた
//! `sqlx::Transaction`（`PgTx`）とは無関係なので、`kaikei_app::tx::with_tx`
//! が rollback しても監査ログの行は消えない。
//!
//! これは実装の都合ではなく設計の本体である。同一トランザクションで書くと
//! **失敗した操作の記録だけが構造的に消える**（`with_tx` は `Err` で必ず
//! rollback する）。PostgreSQL に autonomous transaction は無いので、
//! 経路を分ける以外に手段が無い。
//!
//! この性質は `crates/kaikei-store/tests/audit_log.rs` の
//! `audit_row_written_inside_a_rolled_back_transaction_still_survives` /
//! `failed_posting_rolls_back_the_ledger_but_keeps_both_audit_rows` が
//! 実 PostgreSQL に対して実証している（同一トランザクションで書く実装に
//! 退行したら、この2本だけが落ちる）。
//!
//! # 接続プール
//!
//! 帳簿と同じ `PgPool`（`connect_app` の `max_connections` は 10）から
//! 別接続を取る形でよい。プールを分けないのは、監査ログ専用のプールを
//! 別に張ると接続数が倍になり、単一ユーザー・自己ホスト前提（D-015）の
//! 規模では利点が無いため。ただし**プールの枯渇には注意**する
//! （1リクエストにつき帳簿1接続 + 監査ログ1接続を短時間だけ使う）。

use crate::convert::timestamp_to_datetime;
use crate::error::from_sqlx_error;
use async_trait::async_trait;
use kaikei_app::audit::{status, AuditOutcome, AuditResult, AuditStart};
use kaikei_app::error::RepoError;
use kaikei_app::id::entry_id_to_uuid;
use kaikei_app::ports::AuditSink;
use sqlx::PgPool;
use uuid::Uuid;

/// `audit_log` テーブルへの追記実装。
///
/// `Arc<PgAuditSink>` として合成ルートに持たせる想定
/// （`kaikei_app::ports::AuditSink` は `&self` のメソッドのみを持つ）。
#[derive(Debug, Clone)]
pub struct PgAuditSink {
    pool: PgPool,
}

impl PgAuditSink {
    /// 接続済みの `PgPool`（`kaikei_app` ロール。
    /// [`crate::pool::connect_app`] で作成したもの）から sink を作る。
    ///
    /// **`kaikei_migrator` の接続を渡さないこと。** 所有者はテーブル権限の
    /// `REVOKE` をバイパスできるため、`audit_log` の append-only 防御が
    /// トリガ1層だけになる（`docs/07-mcp-server.md` §8 と同じ理由）。
    pub fn new(pool: PgPool) -> Self {
        PgAuditSink { pool }
    }
}

/// JSON テキストを JSONB 列に渡す値へ変換する。
///
/// `kaikei-app` は serde を持たないため（`CLAUDE.md` §1）、監査ログの
/// `input` / `output` は **JSON テキスト**としてポートを通る。ここで
/// 一度パースしておくことで、壊れた JSON が
/// 「SQLSTATE 22P02 の未分類バックエンドエラー」ではなく
/// **どこが悪いかを述べたエラー**として返る。
///
/// # Errors
///
/// JSON として解釈できない場合は [`RepoError::Corrupt`]
/// （`sqlstate.rs` が `23502`/`23514` を `Corrupt` に寄せているのと同じ
/// 「保存しようとしたデータの構造そのものが不正」の分類）。
fn parse_json(field: &str, text: Option<&str>) -> Result<Option<serde_json::Value>, RepoError> {
    text.map(|text| {
        serde_json::from_str::<serde_json::Value>(text).map_err(|err| RepoError::Corrupt {
            reason: format!(
                "監査ログの {field} に渡された文字列が JSON として解釈できません: {err}。\
                 呼び出し側（presentation 層）が JSON を組み立てる責務を持ちます"
            ),
        })
    })
    .transpose()
}

/// 失敗の結果レコードの `output` 列に入れる JSON を組み立てる。
///
/// 入れるのは **AI に返した本文**（`public_message()`）だけである。
/// `Display` は下位層の生メッセージ（接続文字列・ロール名・制約定義を
/// 含みうる）を持つため転記しない（`docs/07-mcp-server.md` §9）。
/// `kaikei_app::audit::AuditOutcome::Failed` が `public_message` しか
/// 運べない形になっているので、ここに `Display` が届く経路は無い。
fn error_output(public_message: &str) -> serde_json::Value {
    serde_json::json!({ "message": public_message })
}

#[async_trait]
impl AuditSink for PgAuditSink {
    async fn record_start(&self, record: &AuditStart<'_>) -> Result<(), RepoError> {
        let input = parse_json("input", record.input_json)?;

        sqlx::query(
            "INSERT INTO audit_log \
             (request_id, occurred_at, actor, tool, status, input) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(Uuid::from_u128(record.request_id.as_u128()))
        .bind(timestamp_to_datetime(record.occurred_at)?)
        .bind(record.actor)
        .bind(record.tool)
        .bind(status::STARTED)
        .bind(input)
        // ★ `&self.pool` に対して実行する（`PgTx` を経由しない）。
        //   これが「帳簿とは別のコネクション」の実体である。
        .execute(&self.pool)
        .await
        .map_err(from_sqlx_error)?;

        Ok(())
    }

    async fn record_result(&self, record: &AuditResult<'_>) -> Result<(), RepoError> {
        let output = match record.outcome {
            AuditOutcome::Succeeded { output_json } => parse_json("output", output_json)?,
            AuditOutcome::Failed { public_message, .. } => Some(error_output(public_message)),
        };

        sqlx::query(
            "INSERT INTO audit_log \
             (request_id, occurred_at, actor, tool, status, output, error_code, entry_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(Uuid::from_u128(record.request_id.as_u128()))
        .bind(timestamp_to_datetime(record.occurred_at)?)
        .bind(record.actor)
        .bind(record.tool)
        .bind(record.outcome.status_code())
        .bind(output)
        .bind(record.outcome.error_code())
        .bind(record.entry_id.map(entry_id_to_uuid))
        .execute(&self.pool)
        .await
        .map_err(from_sqlx_error)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_json_accepts_well_formed_json_and_passes_none_through() {
        assert!(parse_json("input", None).unwrap().is_none());
        let value = parse_json("input", Some(r#"{"a":1}"#)).unwrap().unwrap();
        assert_eq!(value["a"], serde_json::json!(1));
    }

    #[test]
    fn parse_json_rejects_broken_json_with_a_diagnosable_error() {
        let err = parse_json("output", Some("{壊れた")).unwrap_err();
        assert!(matches!(err, RepoError::Corrupt { .. }));
        assert!(err.to_string().contains("output"));
    }

    // 失敗の結果レコードに載るのは public_message() だけであることの
    // 単体レベルの確認（実 DB 経路は tests/audit_log.rs）。
    #[test]
    fn error_output_carries_only_the_message() {
        let value = error_output("貸借不一致: 借方 110,000 / 貸方 100,000");
        assert_eq!(
            value["message"],
            serde_json::json!("貸借不一致: 借方 110,000 / 貸方 100,000")
        );
        assert_eq!(value.as_object().unwrap().len(), 1);
    }
}
