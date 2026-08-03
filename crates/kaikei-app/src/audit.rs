//! 監査ログ（`audit_log`）に記録する値と、操作を監査ログで挟む手順。
//!
//! ポート（trait）は [`crate::ports::AuditSink`] に置いてある
//! （`ports.rs` は trait 定義だけを持つ。`docs/07-mcp-server.md` §9 が
//! 置き場に指定しているのもそこ）。このモジュールが持つのは、
//! そのポートに渡す**値**と、fail-closed / fail-open の**手順**である。
//!
//! # 1リクエスト＝2行（`DECISIONS.md` D-070）
//!
//! ```text
//! 開始レコード（status='started'）を書く   ← 帳簿とは別のコネクション
//!   ↓ 失敗したら fail-closed（操作を実行しない）
//! with_tx(...) で操作を実行
//!   ↓
//! 結果レコード（status='ok' | 'error'）を書く  ← 帳簿とは別のコネクション
//!   ↓ 失敗したら fail-open（操作は成功として返し、警告を添える）
//! ```
//!
//! **帳簿と同一トランザクションで書いてはならない。**
//! [`crate::tx::with_tx`] は `Err` で必ず rollback するため、同一
//! トランザクションで書くと**失敗した操作の記録が構造的に消える**。
//! 「AI が何をしようとしたか」を最も知りたいのは失敗したときであり、
//! その記録だけが残らない設計になってしまう。PostgreSQL に autonomous
//! transaction は無いので、経路を分けること自体が唯一の手段である。
//!
//! # なぜ手順を [`with_audit`] に閉じるのか（`DECISIONS.md` D-076）
//!
//! 「開始レコードを書く → 操作 → 結果レコードを書く」を各ツールが手で
//! 書くと、fail-closed / fail-open の規律が Phase 3 の11ツールに複製され、
//! どれか1つで順序を間違えても誰も気づかない（`PROGRESS.md` Phase 1 の
//! 教訓6「手で維持する一覧は必ず腐る。構造で閉じる」）。とくに
//! **開始レコードの失敗時に操作を実行しない**ことは、書き忘れても
//! 正常系のテストが全て緑のまま通ってしまう種類の規律である。

use crate::error::{codes, RepoError};
use crate::ports::{AppClock, AuditSink};
use kaikei_core::{EntryId, Timestamp};
use std::future::Future;
use uuid::Uuid;

/// `audit_log.actor` 列に入れる値。
///
/// 「どの入口から呼ばれたか」であり、認証された利用者ではない
/// （`kaikei-mcp` は認証を持たない。`docs/07-mcp-server.md` §8）。
pub mod actor {
    /// MCP サーバー（`kaikei-mcp`）経由。
    pub const MCP: &str = "mcp";
    /// CLI 経由。
    pub const CLI: &str = "cli";
    /// HTTP API（`kaikei-api`）経由。
    pub const API: &str = "api";
}

/// `audit_log.status` 列に入れる値。
///
/// DB 側の `CHECK (status IN ('started', 'ok', 'error'))` と綴りを一致させる
/// 唯一の置き場（`crates/kaikei-store/migrations/0009_audit_log.sql`）。
pub mod status {
    /// 開始レコード（操作の前）。結果レコードが無い行は「結果不明」として読む。
    pub const STARTED: &str = "started";
    /// 結果レコード（操作が成功）。
    pub const OK: &str = "ok";
    /// 結果レコード（操作が失敗）。`error_code` が必ず入る。
    pub const ERROR: &str = "error";
}

/// ツール呼び出し1回を識別するID（`audit_log.request_id`）。
///
/// **サーバが採番する。** JSON-RPC の `id` は number にもなりうるので流用
/// しない（`docs/07-mcp-server.md` §9）。開始レコードと結果レコードを
/// 突き合わせる唯一の手掛かりであり、2行は必ず同じ値を持つ。
///
/// [`crate::id::EntryId`] と同じく内部表現は `u128` で、線上・DB には
/// **UUID の正準表記**で出す（[`RequestId::to_uuid_string`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RequestId(u128);

impl RequestId {
    /// 新しいリクエストIDを UUID v7 として採番する。
    ///
    /// UUID v7 はビット列に時刻を埋め込むが、これは一意性と生成順序のための
    /// ものであり、`CLAUDE.md` §7 が求める「時刻は `Clock` 経由」の対象では
    /// ない（記録される時刻は `occurred_at` として別途 [`AppClock`] から取る）。
    /// [`crate::id::new_entry_id`] と同じ扱いである。
    #[must_use]
    pub fn new_v7() -> Self {
        RequestId(Uuid::now_v7().as_u128())
    }

    /// 既知の `u128` から作る（永続化層からの復元・テスト用）。
    #[must_use]
    pub fn new(value: u128) -> Self {
        RequestId(value)
    }

    /// 内部表現。永続化層が `uuid::Uuid::from_u128` に渡す。
    #[must_use]
    pub fn as_u128(self) -> u128 {
        self.0
    }

    /// **UUID の正準表記**（小文字ハイフン付き36文字）。
    ///
    /// 人間・AI に見せる場所（fail-open の警告文言、監査ログの照会）は
    /// 必ずこの表記に揃える。10進表記（最大39桁）を使わない
    /// （[`crate::id::entry_id_to_uuid_string`] と同じ規律）。
    #[must_use]
    pub fn to_uuid_string(self) -> String {
        Uuid::from_u128(self.0).to_string()
    }
}

/// 監査ログから見た「1回のツール呼び出し」。開始レコードの内容そのもの。
#[derive(Debug, Clone, Copy)]
pub struct AuditCall<'a> {
    /// このツール呼び出しのID。開始レコードと結果レコードで同じ値を使う。
    pub request_id: RequestId,
    /// 呼び出し経路（[`actor`] の定数）。
    pub actor: &'a str,
    /// ツール名（MCP に登録した名前）。
    pub tool: &'a str,
    /// ツールに渡された入力の JSON テキスト。
    ///
    /// **組み立てるのは presentation 層**（`kaikei-app` は serde を持たない。
    /// `CLAUDE.md` §1）。`audit_log.input` は帳簿本体と同等の機微度として
    /// 扱う（自由記述欄に個人情報が入る前提。`docs/07-mcp-server.md` §9）。
    ///
    /// **接続文字列・認証情報を入れないこと。** Phase 3 の11ツールは
    /// それらを入力として受け取らないので、素直に「受け取った引数」を
    /// 載せる限りこの不変条件は保たれる。
    pub input_json: Option<&'a str>,
}

/// 開始レコード（`status='started'`）。[`AuditSink::record_start`] に渡す。
#[derive(Debug, Clone, Copy)]
pub struct AuditStart<'a> {
    /// [`AuditCall::request_id`]。
    pub request_id: RequestId,
    /// 記録時刻（[`AppClock`] から取得した値。`DEFAULT now()` は使わない）。
    pub occurred_at: Timestamp,
    /// [`AuditCall::actor`]。
    pub actor: &'a str,
    /// [`AuditCall::tool`]。
    pub tool: &'a str,
    /// [`AuditCall::input_json`]。
    pub input_json: Option<&'a str>,
}

/// 結果レコードの結末。`status` と `error_code` の対応を型で固定する。
///
/// - 成功なら `error_code` を持てない
/// - 失敗なら `error_code` を必ず持つ
/// - `'started'` はここから作れない（開始レコード専用）
///
/// DB 側にも同じ対応を `CHECK ((status = 'error') = (error_code IS NOT NULL))`
/// として置いてある（多層防御）。
#[derive(Debug, Clone, Copy)]
pub enum AuditOutcome<'a> {
    /// 操作が成功した（`status='ok'`）。
    Succeeded {
        /// 応答に載せた内容の JSON テキスト（確定後明細と `PolicyNote`）。
        /// 組み立てるのは presentation 層。
        output_json: Option<&'a str>,
    },
    /// 操作が失敗した（`status='error'`）。
    Failed {
        /// 分類コード（[`crate::error::codes`]）。`error_code` 列に入る。
        error_code: &'a str,
        /// **AI に返した本文**。`Display` ではなく `public_message()` を使う
        /// （`docs/07-mcp-server.md` §9。`RepoError::Backend` 等の `reason`
        /// には DB が返した文字列がそのまま入っており、接続文字列・ロール名・
        /// 制約定義が混じりうる）。[`with_audit`] がここを自動で埋めるため、
        /// 呼び出し側が `to_string()` を渡す余地は無い。
        public_message: &'a str,
    },
}

impl AuditOutcome<'_> {
    /// `audit_log.status` 列に入れる値（[`status`] の定数）。
    #[must_use]
    pub fn status_code(&self) -> &'static str {
        match self {
            AuditOutcome::Succeeded { .. } => status::OK,
            AuditOutcome::Failed { .. } => status::ERROR,
        }
    }

    /// `audit_log.error_code` 列に入れる値（成功なら `None`）。
    #[must_use]
    pub fn error_code(&self) -> Option<&str> {
        match self {
            AuditOutcome::Succeeded { .. } => None,
            AuditOutcome::Failed { error_code, .. } => Some(error_code),
        }
    }
}

/// 結果レコード（`status='ok' | 'error'`）。[`AuditSink::record_result`] に渡す。
#[derive(Debug, Clone, Copy)]
pub struct AuditResult<'a> {
    /// 開始レコードと**同じ** [`RequestId`]。
    pub request_id: RequestId,
    /// 記録時刻（[`AppClock`] から取得した値）。
    pub occurred_at: Timestamp,
    /// [`AuditCall::actor`]。
    pub actor: &'a str,
    /// [`AuditCall::tool`]。
    pub tool: &'a str,
    /// 結末（`status` と `error_code`）。
    pub outcome: AuditOutcome<'a>,
    /// 記帳した仕訳のID（書き込み系ツールが成功したときのみ）。
    ///
    /// 永続化層は **UUID** として保存する。外部キーは張らない
    /// （rollback された操作の仕訳IDは `journal_entries` に存在しえないが、
    /// 「存在しないことを記録できること」自体に価値がある）。
    pub entry_id: Option<EntryId>,
}

/// 操作が成功したときに結果レコードへ載せる内容。
///
/// [`with_audit`] の `describe` クロージャが返す。`kaikei-app` は serde を
/// 持たないため、JSON の組み立ては presentation 層の責務である。
#[derive(Debug, Clone, Default)]
pub struct AuditSuccess {
    /// 記帳した仕訳のID（読み取り系ツールでは `None`）。
    pub entry_id: Option<EntryId>,
    /// 応答に載せた内容の JSON テキスト。
    pub output_json: Option<String>,
}

/// 監査ログの結果レコードに載せられるエラー。
///
/// [`crate::error::AppError`] とユースケース固有の失敗値
/// （[`crate::usecase::post_entry::PostEntryFailure`]）の両方を
/// [`with_audit`] が同じ手順で扱えるようにするための最小の抽象。
pub trait AuditableError {
    /// 分類コード（[`crate::error::codes`]）。
    fn audit_error_code(&self) -> &'static str;

    /// **外部に出してよい本文**（`public_message()`）。
    ///
    /// `Display`（`to_string()`）を返してはならない。下位層の生メッセージ
    /// （接続文字列・ロール名を含みうる）が `audit_log.output` に落ちる。
    fn audit_public_message(&self) -> String;
}

impl AuditableError for crate::error::AppError {
    fn audit_error_code(&self) -> &'static str {
        self.code()
    }

    fn audit_public_message(&self) -> String {
        self.public_message()
    }
}

impl AuditableError for crate::usecase::post_entry::PostEntryFailure {
    fn audit_error_code(&self) -> &'static str {
        self.code()
    }

    fn audit_public_message(&self) -> String {
        self.public_message()
    }
}

/// **開始レコードが書けなかったので操作を実行しなかった**（fail-closed）。
///
/// まだ何も起きていない時点なので、拒否して安全側に倒せる。
/// 記録されない操作を実行しない、が `ROADMAP.md` Phase 3 の完了条件
/// 「全操作が audit_log に記録される」を成立させている部分である。
///
/// [`crate::error::AppError`] のバリアントには**しない**。判定は
/// [`crate::tx::with_tx`] の外側で起きており、ユースケースには到達しない
/// （`docs/07-mcp-server.md` §6 の「`AppError` のバリアントを持たないコード」）。
#[derive(Debug, thiserror::Error)]
#[error(
    "監査ログの開始レコードを記録できなかったため、ツール {tool} を実行しませんでした\
     （帳簿は変更されていません）: {cause}"
)]
pub struct AuditLogUnavailable {
    /// 採番済みだが記録できなかったリクエストID。
    pub request_id: RequestId,
    /// 実行しなかったツール名。
    pub tool: String,
    /// 記録に失敗した理由（**診断用**。応答に載せない）。
    #[source]
    pub cause: RepoError,
}

impl AuditLogUnavailable {
    /// 分類コード。常に [`codes::AUDIT_LOG_UNAVAILABLE`]。
    ///
    /// [`codes::REJECTED`] を借りてはならない（`rejected` は入力を直せば
    /// 通る拒否に使っている。同じコードにすると AI が「入力を直せばよいのか」
    /// 「サーバ都合で今は実行できないのか」を区別できない）。
    #[must_use]
    pub fn code(&self) -> &'static str {
        codes::AUDIT_LOG_UNAVAILABLE
    }

    /// **外部（応答）に出してよい本文**。
    ///
    /// **帳簿が無変更であることまで含める**（`CLAUDE.md` §11。
    /// これが無いと AI は「記帳されたかもしれない」と考えて確認や
    /// 二重記帳に走る）。`cause` は含めない（下位層の生メッセージが漏れる）。
    #[must_use]
    pub fn public_message(&self) -> String {
        "監査ログに記録できなかったため、操作を実行していません。帳簿は変更されていません。\
         時間をおいて再試行するか、サーバのログを添えて管理者に連絡してください"
            .to_string()
    }
}

/// **操作は完了したが結果レコードが書けなかった**（fail-open）。
///
/// 操作は既に確定しており、拒否しても取り消せない。開始レコードだけが残る
/// ので、その行は「結果不明」として読む。
#[derive(Debug, thiserror::Error)]
#[error("監査ログの結果レコードを記録できませんでした（request_id={request_id_text}）: {cause}")]
pub struct AuditResultNotRecorded {
    /// 開始レコードを引くためのリクエストID。
    pub request_id: RequestId,
    /// `Display` 用に整形済みの [`RequestId`]（UUID 正準表記）。
    request_id_text: String,
    /// 記録に失敗した理由（**診断用**。応答に載せない）。
    #[source]
    pub cause: RepoError,
}

impl AuditResultNotRecorded {
    /// **外部（応答）に添える警告文**。
    ///
    /// **再実行を促す文言にしない**（二重計上を招く。
    /// `docs/07-mcp-server.md` §9）。操作は完了している。
    #[must_use]
    pub fn public_message(&self) -> String {
        format!(
            "操作は完了しましたが、監査ログの結果記録に失敗しました。\
             request_id={} の行（開始レコードのみが残っています）を確認してください。\
             操作は既に完了しているため、やり直さないでください",
            self.request_id_text
        )
    }
}

/// [`with_audit`] の戻り値。**操作は実行された**（成功・失敗を問わない）。
#[derive(Debug)]
pub struct AuditedCall<T, E> {
    /// このツール呼び出しのID。監査ログの2行はこの値で引ける。
    pub request_id: RequestId,
    /// 操作そのものの結果。監査ログの都合で書き換えない。
    pub result: Result<T, E>,
    /// 結果レコードを記録できなかった場合の警告（fail-open）。
    ///
    /// `Some` でも `result` は加工しない。応答に
    /// [`AuditResultNotRecorded::public_message`] を添えること。
    pub warning: Option<AuditResultNotRecorded>,
}

/// 操作を監査ログで挟んで実行する（開始レコード → 操作 → 結果レコード）。
///
/// # fail-closed / fail-open
///
/// - 開始レコードが書けなければ **`operation` を呼ばない**（`Err`）。
///   `operation` はクロージャで受け取っており、呼ばれなければ future すら
///   構築されない。
/// - 結果レコードが書けなくても操作の結果はそのまま返す
///   （[`AuditedCall::warning`] に理由が入る）。
///
/// # 引数
///
/// - `sink`: 監査ログの記録先。**帳簿とは別のコネクション**で書く実装で
///   あること（[`AuditSink`] の doc）。
/// - `clock`: `occurred_at` の取得元（`CLAUDE.md` §7）。
/// - `call`: 開始レコードに載せる内容。
/// - `operation`: 実行する操作。ふつうは
///   [`crate::tx::with_tx`] / [`crate::tx::with_tx_err`] の呼び出し。
///   **この中で監査ログを書かないこと**（同一トランザクションに巻き込まれる）。
/// - `describe`: 成功時に結果レコードへ載せる内容
///   （仕訳IDと出力 JSON）を組み立てる。失敗時は
///   [`AuditableError`] から自動で組み立てるので呼ばれない。
///
/// # Errors
///
/// 開始レコードを記録できなかった場合のみ [`AuditLogUnavailable`]
/// （このとき **`operation` は実行されていない**）。操作自体の失敗は
/// `Ok(AuditedCall { result: Err(..), .. })` として返る。
pub async fn with_audit<T, E, Op, Fut, D>(
    sink: &dyn AuditSink,
    clock: &dyn AppClock,
    call: &AuditCall<'_>,
    operation: Op,
    describe: D,
) -> Result<AuditedCall<T, E>, AuditLogUnavailable>
where
    E: AuditableError,
    Op: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, E>>,
    D: FnOnce(&T) -> AuditSuccess,
{
    let start = AuditStart {
        request_id: call.request_id,
        occurred_at: clock.now(),
        actor: call.actor,
        tool: call.tool,
        input_json: call.input_json,
    };

    // ★ fail-closed。ここで return すると `operation` は一度も呼ばれない。
    if let Err(cause) = sink.record_start(&start).await {
        return Err(AuditLogUnavailable {
            request_id: call.request_id,
            tool: call.tool.to_string(),
            cause,
        });
    }

    let result = operation().await;

    // 結果レコードに載せる値を、借用が生きる形で先に組み立てる。
    let success = result.as_ref().ok().map(describe);
    let failure = result
        .as_ref()
        .err()
        .map(|err| (err.audit_error_code(), err.audit_public_message()));

    let outcome = match (&success, &failure) {
        (Some(success), _) => AuditOutcome::Succeeded {
            output_json: success.output_json.as_deref(),
        },
        (None, Some((error_code, public_message))) => AuditOutcome::Failed {
            error_code,
            public_message,
        },
        // `Result` は Ok/Err のどちらかなので到達しない。
        (None, None) => unreachable!("Result が Ok でも Err でもない状態は存在しない"),
    };

    let record = AuditResult {
        request_id: call.request_id,
        occurred_at: clock.now(),
        actor: call.actor,
        tool: call.tool,
        outcome,
        entry_id: success.as_ref().and_then(|s| s.entry_id),
    };

    // ★ fail-open。結果レコードが書けなくても操作の結果は握りつぶさない。
    let warning = match sink.record_result(&record).await {
        Ok(()) => None,
        Err(cause) => Some(AuditResultNotRecorded {
            request_id: call.request_id,
            request_id_text: call.request_id.to_uuid_string(),
            cause,
        }),
    };

    Ok(AuditedCall {
        request_id: call.request_id,
        result,
        warning,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::SystemClock;
    use crate::error::AppError;
    use crate::testing::RecordingAuditSink;

    fn call<'a>(request_id: RequestId, input: Option<&'a str>) -> AuditCall<'a> {
        AuditCall {
            request_id,
            actor: actor::MCP,
            tool: "post_journal_entry",
            input_json: input,
        }
    }

    // AUD-01: 成功した操作は開始レコードと結果レコード（ok）の2行になり、
    // 同じ request_id を持つ。
    #[tokio::test]
    async fn successful_operation_records_start_and_ok_with_the_same_request_id() {
        let sink = RecordingAuditSink::new();
        let request_id = RequestId::new_v7();

        let audited = with_audit(
            &sink,
            &SystemClock,
            &call(request_id, Some(r#"{"entry_date":"2026-04-15"}"#)),
            || async { Ok::<_, AppError>(42_u32) },
            |_| AuditSuccess {
                entry_id: Some(EntryId::new(7)),
                output_json: Some(r#"{"entry_no":1}"#.to_string()),
            },
        )
        .await
        .expect("開始レコードは書けるはず");

        assert_eq!(audited.result.unwrap(), 42);
        assert!(audited.warning.is_none());

        let rows = sink.rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].status, status::STARTED);
        assert_eq!(
            rows[0].input_json.as_deref(),
            Some(r#"{"entry_date":"2026-04-15"}"#)
        );
        assert_eq!(rows[1].status, status::OK);
        assert_eq!(rows[1].error_code, None);
        assert_eq!(rows[1].entry_id.map(|id| id.as_u128()), Some(7));
        assert!(rows.iter().all(|row| row.request_id == request_id));
        assert!(rows.iter().all(|row| row.tool == "post_journal_entry"));
    }

    // AUD-02: 失敗した操作も2行残り、結果レコードに分類コードが入る。
    #[tokio::test]
    async fn failed_operation_records_start_and_error_with_the_error_code() {
        let sink = RecordingAuditSink::new();

        let audited = with_audit(
            &sink,
            &SystemClock,
            &call(RequestId::new_v7(), None),
            || async {
                Err::<u32, _>(AppError::Rejected {
                    reason: "テスト用の意図的な失敗".to_string(),
                })
            },
            |_| AuditSuccess::default(),
        )
        .await
        .expect("開始レコードは書けるはず");

        assert!(audited.result.is_err());
        let rows = sink.rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].status, status::STARTED);
        assert_eq!(rows[1].status, status::ERROR);
        assert_eq!(rows[1].error_code.as_deref(), Some(codes::REJECTED));
        assert_eq!(rows[1].entry_id, None);
    }

    // AUD-03（fail-closed）: 開始レコードが書けないとき、操作は実行されない。
    #[tokio::test]
    async fn start_record_failure_prevents_the_operation_from_running() {
        let sink = RecordingAuditSink::failing_on_start();
        let mut executed = false;

        let err = with_audit(
            &sink,
            &SystemClock,
            &call(RequestId::new_v7(), None),
            || {
                executed = true;
                async { Ok::<_, AppError>(1_u32) }
            },
            |_| AuditSuccess::default(),
        )
        .await
        .expect_err("開始レコードが書けなければ fail-closed になるはず");

        assert!(!executed, "fail-closed なのに操作が実行された");
        assert_eq!(err.code(), codes::AUDIT_LOG_UNAVAILABLE);
        assert_ne!(err.code(), codes::REJECTED);
        assert!(sink.rows().is_empty());
    }

    // AUD-04: fail-closed の応答本文は「帳簿は変更されていません」を含み、
    // 逆仕訳を案内しない（下位層が返す AppendOnlyViolation の文言に
    // 引きずられていないこと。D-038 と同じ誤診クラスへの防御）。
    #[tokio::test]
    async fn fail_closed_message_states_the_ledger_is_unchanged_and_never_mentions_reversal() {
        let sink = RecordingAuditSink::failing_on_start();

        let err = with_audit(
            &sink,
            &SystemClock,
            &call(RequestId::new_v7(), None),
            || async { Ok::<_, AppError>(1_u32) },
            |_| AuditSuccess::default(),
        )
        .await
        .expect_err("fail-closed になるはず");

        let message = err.public_message();
        assert!(message.contains("帳簿は変更されていません"), "{message}");
        assert!(!message.contains("逆仕訳"), "{message}");
        // 下位層の生メッセージ（cause）を応答に混ぜない。
        assert!(!message.contains("permission denied"), "{message}");
        // 診断用の Display 側には残っている。
        assert!(err.to_string().contains("permission denied"));
    }

    // AUD-05（fail-open）: 結果レコードが書けなくても操作の結果は返る。
    #[tokio::test]
    async fn result_record_failure_still_returns_the_operation_result_with_a_warning() {
        let sink = RecordingAuditSink::failing_on_result();
        let request_id = RequestId::new_v7();

        let audited = with_audit(
            &sink,
            &SystemClock,
            &call(request_id, None),
            || async { Ok::<_, AppError>(42_u32) },
            |_| AuditSuccess::default(),
        )
        .await
        .expect("開始レコードは書けるはず");

        assert_eq!(audited.result.unwrap(), 42);
        let warning = audited.warning.expect("警告が添えられるはず");
        let message = warning.public_message();
        assert!(message.contains(&request_id.to_uuid_string()), "{message}");
        // 再実行を促さない（二重計上を招く）。
        assert!(!message.contains("再実行"), "{message}");
        assert!(!message.contains("やり直してください"), "{message}");
        // 開始レコードだけが残る（＝「結果不明」として読める）。
        assert_eq!(sink.rows().len(), 1);
        assert_eq!(sink.rows()[0].status, status::STARTED);
    }

    // AUD-06: 結果レコードに載る本文は public_message() であり、
    // 下位層の生メッセージ（接続文字列を含みうる）を転記しない。
    #[tokio::test]
    async fn error_output_uses_public_message_and_never_leaks_the_connection_string() {
        let sink = RecordingAuditSink::new();
        let secret = "postgres://kaikei_app:s3cret@localhost:5432/kaikei";

        let audited = with_audit(
            &sink,
            &SystemClock,
            &call(RequestId::new_v7(), None),
            || async {
                Err::<u32, _>(AppError::Repo(RepoError::Backend {
                    reason: format!("接続に失敗しました: {secret}"),
                }))
            },
            |_| AuditSuccess::default(),
        )
        .await
        .expect("開始レコードは書けるはず");

        assert!(audited.result.is_err());
        let rows = sink.rows();
        let message = rows[1]
            .public_message
            .as_deref()
            .expect("失敗の結果レコードには本文が入る");
        assert!(!message.contains("postgres://"), "{message}");
        assert!(!message.contains("s3cret"), "{message}");
        assert_eq!(rows[1].error_code.as_deref(), Some(codes::BACKEND));
    }

    // AUD-07: RequestId は UUID の正準表記で出す（10進表記にしない）。
    #[test]
    fn request_id_is_rendered_in_the_canonical_uuid_form() {
        let id = RequestId::new(0x0192_a7b3_1234_7abc_8def_0123_4567_89ab);
        let text = id.to_uuid_string();
        assert_eq!(text.len(), 36);
        assert_eq!(text.matches('-').count(), 4);
        assert_ne!(text, id.as_u128().to_string());
    }

    #[test]
    fn request_id_new_v7_generates_distinct_values() {
        assert_ne!(RequestId::new_v7(), RequestId::new_v7());
    }

    // AUD-08: 結末の型が status / error_code の対応を保証する。
    #[test]
    fn outcome_maps_to_status_and_error_code_consistently() {
        let ok = AuditOutcome::Succeeded { output_json: None };
        assert_eq!(ok.status_code(), status::OK);
        assert_eq!(ok.error_code(), None);

        let failed = AuditOutcome::Failed {
            error_code: codes::UNBALANCED,
            public_message: "貸借不一致",
        };
        assert_eq!(failed.status_code(), status::ERROR);
        assert_eq!(failed.error_code(), Some(codes::UNBALANCED));
        assert_ne!(failed.status_code(), status::STARTED);
    }
}
