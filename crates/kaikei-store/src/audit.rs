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
//! # 入力が原因で「監査ログが使えない」と誤診しない（D-075）
//!
//! `with_audit` は開始レコードを書けなければ操作を実行しない（fail-closed）。
//! したがって**入力に起因する INSERT の失敗をそのまま返すと、正常に動いて
//! いる監査ログを「使えない」と誤診し、しかも同じ入力で再試行する限り
//! 永久に成功しない**。AI には原因（自分が送った1文字）が分からない。
//! 具体例は `{"description":"A\u0000B"}`——JSON としても JSON-RPC としても
//! 正当だが、PostgreSQL の `jsonb` は NUL 文字（U+0000）を格納できず
//! SQLSTATE `22P05` で拒否する。
//!
//! そこでこの実装は**格納できない内容を無害化してでも行を残す**。
//!
//! 1. NUL 文字（U+0000）を U+FFFD に置換する（[`sanitize_in_place`]）。
//!    置換したときは値を封筒（`_audit` + `value`）に包み、
//!    **原文どおりではないことが記録から読み取れる**ようにする。
//! 2. それでも INSERT が失敗したら、`input` / `output` を
//!    「記録できなかった旨のプレースホルダ」に差し替えて**1度だけ**再試行する。
//!    監査の証拠（誰がいつ何のツールを呼んだか）だけは必ず残す。
//! 3. JSON として解釈できない文字列（呼び出し側のバグ）も同じ扱いにする。
//!    行を捨てるより、`tool` と `actor` が残る方が監査として価値がある。
//!
//! この結果、fail-closed に落ちる経路は「**sink が本当に使えない**」場合
//! （権限剥奪・接続断・`actor`/`tool` が空という呼び出し側のバグ）だけになる。
//!
//! # 42501 をそのまま返さない（D-075）
//!
//! [`crate::sqlstate::map_sqlstate`] は関与テーブルを見ないため、
//! `42501`（`REVOKE INSERT ON audit_log FROM kaikei_app`）を帳簿と同じ
//! [`RepoError::AppendOnlyViolation`] に写像する。その `public_message()` は
//! 「訂正は逆仕訳（reverse_journal_entry）で行ってください」であり、
//! 監査ログに対しては的外れである（D-038 が潰した誤診クラス）。
//! `AuditLogUnavailable::cause` は pub フィールドなので、presentation 層が
//! 診断のつもりでそれを出せば誤案内が復活する。
//! [`audit_sink_error`] がこの経路の 42501 を [`RepoError::Backend`] に
//! 包み直し、**到達可能な位置に「逆仕訳で訂正を」を残さない**。
//! 共通写像（`sqlstate.rs`）は帳簿側に波及するので触らない。
//!
//! # 接続プール
//!
//! 帳簿と同じ `PgPool`（`connect_app` の `max_connections` は 10）から
//! 別接続を取る形でよい。プールを分けないのは、監査ログ専用のプールを
//! 別に張ると接続数が倍になり、単一ユーザー・自己ホスト前提（D-015）の
//! 規模では利点が無いため。
//!
//! **1リクエストが帳簿と監査ログの接続を同時に保持することは無い。**
//! `with_audit` は `record_start`（監査ログの接続を取って返す）→ `with_tx`
//! （帳簿の接続を取って返す）→ `record_result`（監査ログの接続を取って返す）
//! の順で、どの時点でも保持している接続は1本である。
//! **同時に2本保持している状態を観測したら、それは「`with_tx` の内側から
//! 監査ログを書いている」証拠**であり（このモジュールが禁じている形。
//! `kaikei_app::audit::with_audit` の doc）、プール枯渇の前に設計の退行を
//! 疑うこと。

use crate::convert::timestamp_to_datetime;
use crate::error::from_sqlx_error;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use kaikei_app::audit::{status, AuditOutcome, AuditResult, AuditStart};
use kaikei_app::error::RepoError;
use kaikei_app::id::entry_id_to_uuid;
use kaikei_app::ports::AuditSink;
use serde_json::{json, Value};
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

/// `audit_log` に INSERT する1行分の値。
///
/// 開始レコードと結果レコードで文を分けず、使わない列に `NULL` を入れる。
/// 差し替えて再試行する経路（[`PgAuditSink::append`]）を1箇所に閉じるため。
struct AuditRow<'a> {
    request_id: Uuid,
    occurred_at: DateTime<Utc>,
    actor: &'a str,
    tool: &'a str,
    status: &'a str,
    input: Option<Value>,
    output: Option<Value>,
    error_code: Option<&'a str>,
    entry_id: Option<Uuid>,
}

/// JSONB 列に載せる値のうち、この実装が付ける注記の置き場。
///
/// 監査ログとして**原文どおりではないことを黙らせない**ための鍵であり、
/// 読む側はこのキーの有無で「加工されたか」を判定できる。
const AUDIT_NOTE_KEY: &str = "_audit";

/// JSONB が格納できない NUL 文字（U+0000）の代替。
const REPLACEMENT: char = '\u{FFFD}';

impl PgAuditSink {
    /// 1行を INSERT する（失敗はそのまま `sqlx::Error` で返す）。
    async fn insert(&self, row: &AuditRow<'_>) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO audit_log \
             (request_id, occurred_at, actor, tool, status, input, output, error_code, entry_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(row.request_id)
        .bind(row.occurred_at)
        .bind(row.actor)
        .bind(row.tool)
        .bind(row.status)
        .bind(row.input.clone())
        .bind(row.output.clone())
        .bind(row.error_code)
        .bind(row.entry_id)
        // ★ `&self.pool` に対して実行する（`PgTx` を経由しない）。
        //   これが「帳簿とは別のコネクション」の実体である。
        .execute(&self.pool)
        .await
        .map(|_| ())
    }

    /// 1行を追記する。**入力が原因で落ちたなら内容を諦めてでも行を残す。**
    ///
    /// `input` / `output` を載せた INSERT が失敗した場合に限り、
    /// その2列を「記録できなかった旨のプレースホルダ」に差し替えて
    /// **1度だけ**再試行する。プレースホルダはこの実装が組み立てた
    /// 固定構造なので、再試行も失敗したなら原因は payload ではない
    /// （＝ sink が本当に使えない）と判断できる。
    async fn append(&self, mut row: AuditRow<'_>) -> Result<(), RepoError> {
        let first = match self.insert(&row).await {
            Ok(()) => return Ok(()),
            Err(err) => err,
        };

        if row.input.is_none() && row.output.is_none() {
            // 載せている JSONB が無いのだから payload は原因ではない。
            return Err(audit_sink_error(first));
        }

        let sqlstate = first
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .map(|code| code.into_owned());
        row.input = row
            .input
            .map(|_| unstorable_placeholder("input", sqlstate.as_deref()));
        row.output = row
            .output
            .map(|_| unstorable_placeholder("output", sqlstate.as_deref()));

        self.insert(&row).await.map_err(audit_sink_error)
    }
}

/// 監査ログの書き込み失敗を [`RepoError`] に写す。
///
/// [`from_sqlx_error`] の結果のうち [`RepoError::AppendOnlyViolation`] だけを
/// [`RepoError::Backend`] に包み直す。到達しうるのは `42501`
/// （`REVOKE INSERT ON audit_log FROM kaikei_app`。`sqlstate.rs` は関与
/// テーブルを見ないので帳簿と同じ写像先になる）で、そのまま返すと
/// `public_message()` が「訂正は逆仕訳（reverse_journal_entry）で行って
/// ください」になる。監査ログが書けないことに対する案内としては
/// 完全に誤りであり、D-038 が潰した誤診クラスの再演になる。
///
/// `P0010`（帳簿の append-only トリガ）はこの経路では発火しえないが、
/// 仮に発火しても「監査ログが逆仕訳で直せる」わけではないので同じ扱いでよい。
fn audit_sink_error(err: sqlx::Error) -> RepoError {
    rewrap_for_audit_log(from_sqlx_error(err))
}

/// [`audit_sink_error`] の写像部分（`sqlx::Error` を作らずに検証できる形）。
fn rewrap_for_audit_log(mapped: RepoError) -> RepoError {
    match mapped {
        RepoError::AppendOnlyViolation { reason } => RepoError::Backend {
            reason: format!(
                "監査ログ（audit_log）への追記が拒否されました: {reason}。\
                 audit_log は追記のみのテーブルで、帳簿の訂正手段とは無関係です。\
                 kaikei_app ロールに INSERT 権限があるか\
                 （GRANT SELECT, INSERT ON audit_log TO kaikei_app）を確認してください"
            ),
        },
        other => other,
    }
}

/// 文字列中の NUL 文字（U+0000）を U+FFFD に置換し、置換した個数を返す。
fn sanitize_string(text: &mut String) -> usize {
    let count = text.matches('\0').count();
    if count > 0 {
        *text = text.replace('\0', &REPLACEMENT.to_string());
    }
    count
}

/// JSON 値の全ての文字列（オブジェクトのキーを含む）から NUL 文字を除く。
///
/// PostgreSQL の `jsonb` が格納できない文字は U+0000 だけである
/// （対になっていないサロゲートは `serde_json` のパース段階で弾かれ、
/// Rust の `String` はそもそも保持できない）。
fn sanitize_in_place(value: &mut Value) -> usize {
    match value {
        Value::String(text) => sanitize_string(text),
        Value::Array(items) => items.iter_mut().map(sanitize_in_place).sum(),
        Value::Object(map) => {
            let mut replaced = 0;
            if map.keys().any(|key| key.contains('\0')) {
                let rebuilt = std::mem::take(map)
                    .into_iter()
                    .map(|(key, value)| {
                        let mut key = key;
                        replaced += sanitize_string(&mut key);
                        (key, value)
                    })
                    .collect();
                *map = rebuilt;
            }
            replaced + map.values_mut().map(sanitize_in_place).sum::<usize>()
        }
        _ => 0,
    }
}

/// 加工した値（または加工していないことを明示したい値）を包む封筒。
///
/// **原文どおりに記録されたと読めてはならない**（監査ログとして不誠実）。
/// 置換が起きたときは `_audit.verbatim = false` と置換数を添え、
/// 元の内容は `value` に入れる。
fn envelope(field: &str, replaced: usize, value: Value) -> Value {
    let note = if replaced == 0 {
        // 呼び出し側の JSON が予約キーを持っていた場合（下記 prepare_value）。
        "呼び出し側の JSON が予約キー _audit を含むため封筒に入れました。value は原文どおりです"
    } else {
        "JSONB に格納できない NUL 文字（U+0000）を U+FFFD に置換して記録しました。\
         value は呼び出し側が渡した原文とは異なります"
    };
    json!({
        AUDIT_NOTE_KEY: {
            "field": field,
            "verbatim": replaced == 0,
            "recorded": true,
            "replaced_nul": replaced,
            "note": note,
        },
        "value": value,
    })
}

/// 記録できなかった内容の代わりに入れるプレースホルダ（JSON として不正）。
fn unparsable_placeholder(field: &str, parse_error: &serde_json::Error) -> Value {
    json!({
        AUDIT_NOTE_KEY: {
            "field": field,
            "verbatim": false,
            "recorded": false,
            "reason": "invalid_json",
            "parse_error": parse_error.to_string(),
            "note": "呼び出し側から渡された文字列が JSON として解釈できなかったため、\
                     内容を記録できませんでした。誰がいつどのツールを呼んだかを残すため、\
                     行自体は記録しています（presentation 層が JSON を組み立てる責務を持ちます）",
        }
    })
}

/// 記録できなかった内容の代わりに入れるプレースホルダ（DB が格納を拒否）。
fn unstorable_placeholder(field: &str, sqlstate: Option<&str>) -> Value {
    json!({
        AUDIT_NOTE_KEY: {
            "field": field,
            "verbatim": false,
            "recorded": false,
            "reason": "not_storable_as_jsonb",
            "sqlstate": sqlstate,
            "note": "内容をデータベースが格納できなかったため、記録できませんでした。\
                     誰がいつどのツールを呼んだかを残すため、行自体は記録しています",
        }
    })
}

/// 既に組み立て済みの JSON 値を JSONB 列に載せる形にする。
///
/// 置換が起きた場合は封筒に包む（起きなければ原文そのまま）。
///
/// **呼び出し側の JSON が最上位に予約キー [`AUDIT_NOTE_KEY`] を持っている
/// 場合も封筒に包む。** そうしないと、呼び出し側が書いた `_audit` を
/// この実装が書いた注記と読み違えうる（`verbatim: false` を騙られると
/// 「加工された記録」に見える）。封筒に包めば**最上位の `_audit` は常に
/// この実装が書いたもの**になり、相手の `_audit` は `value` の中に入る。
fn prepare_value(field: &str, mut value: Value) -> Value {
    let replaced = sanitize_in_place(&mut value);
    let collides = value
        .as_object()
        .is_some_and(|map| map.contains_key(AUDIT_NOTE_KEY));
    if replaced == 0 && !collides {
        value
    } else {
        envelope(field, replaced, value)
    }
}

/// JSON テキストを JSONB 列に渡す値へ変換する。**失敗しない。**
///
/// `kaikei-app` は serde を持たないため（`CLAUDE.md` §1）、監査ログの
/// `input` / `output` は **JSON テキスト**としてポートを通る。
///
/// 解釈できない文字列（呼び出し側のバグ）でも `Err` にはしない。
/// ここで `Err` を返すと `with_audit` が fail-closed に落ち、
/// 「監査ログが使えない」という誤診と、再試行しても直らない詰みが生まれる
/// （モジュール doc）。内容の代わりに
/// [`unparsable_placeholder`] を記録し、行そのものは残す。
fn prepare_payload(field: &str, text: Option<&str>) -> Option<Value> {
    let text = text?;
    match serde_json::from_str::<Value>(text) {
        Ok(value) => Some(prepare_value(field, value)),
        Err(err) => Some(unparsable_placeholder(field, &err)),
    }
}

/// 失敗の結果レコードの `output` 列に入れる JSON を組み立てる。
///
/// 入れるのは **AI に返した本文**（`public_message()`）だけである。
/// `Display` は下位層の生メッセージ（接続文字列・ロール名・制約定義を
/// 含みうる）を持つため転記しない（`docs/07-mcp-server.md` §9）。
/// `kaikei_app::audit::AuditOutcome::Failed` が `public_message` しか
/// 運べない形になっているので、ここに `Display` が届く経路は無い。
fn error_output(public_message: &str) -> Value {
    json!({ "message": public_message })
}

#[async_trait]
impl AuditSink for PgAuditSink {
    async fn record_start(&self, record: &AuditStart<'_>) -> Result<(), RepoError> {
        self.append(AuditRow {
            request_id: Uuid::from_u128(record.request_id.as_u128()),
            occurred_at: timestamp_to_datetime(record.occurred_at)?,
            actor: record.actor,
            tool: record.tool,
            status: status::STARTED,
            input: prepare_payload("input", record.input_json),
            output: None,
            error_code: None,
            entry_id: None,
        })
        .await
    }

    async fn record_result(&self, record: &AuditResult<'_>) -> Result<(), RepoError> {
        let output = match record.outcome {
            AuditOutcome::Succeeded { output_json } => prepare_payload("output", output_json),
            // 本文（public_message）にも NUL が混じりうる（入力に由来する
            // 文字列を含む文言があるため）。同じ無害化を通す。
            AuditOutcome::Failed { public_message, .. } => {
                Some(prepare_value("output", error_output(public_message)))
            }
        };

        self.append(AuditRow {
            request_id: Uuid::from_u128(record.request_id.as_u128()),
            occurred_at: timestamp_to_datetime(record.occurred_at)?,
            actor: record.actor,
            tool: record.tool,
            status: record.outcome.status_code(),
            input: None,
            output,
            error_code: record.outcome.error_code(),
            entry_id: record.entry_id.map(entry_id_to_uuid),
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepare_payload_passes_none_and_well_formed_json_through_unchanged() {
        assert!(prepare_payload("input", None).is_none());
        let value = prepare_payload("input", Some(r#"{"a":1}"#)).unwrap();
        assert_eq!(value["a"], json!(1));
        // 加工していないので封筒は付かない。
        assert!(value.get(AUDIT_NOTE_KEY).is_none());
    }

    // 【B】U+0000 を含む入力でも「記録できない」にはしない。
    #[test]
    fn nul_characters_are_replaced_and_the_record_says_it_is_not_verbatim() {
        let value = prepare_payload("input", Some("{\"description\":\"A\\u0000B\"}")).unwrap();

        // 封筒が付き、原文どおりでないことが記録から読み取れる。
        let note = &value[AUDIT_NOTE_KEY];
        assert_eq!(note["verbatim"], json!(false));
        assert_eq!(note["recorded"], json!(true));
        assert_eq!(note["replaced_nul"], json!(1));
        assert_eq!(note["field"], json!("input"));

        // 中身は置換されたうえで残る。
        assert_eq!(value["value"]["description"], json!("A\u{FFFD}B"));
        assert!(!value.to_string().contains('\0'));
    }

    #[test]
    fn nul_characters_are_replaced_in_object_keys_and_nested_values() {
        let value = prepare_value(
            "input",
            json!({ "a\u{0000}b": ["x\u{0000}", { "c": "y\u{0000}z" }] }),
        );
        assert_eq!(value[AUDIT_NOTE_KEY]["replaced_nul"], json!(3));
        assert!(!value.to_string().contains('\0'));
        assert_eq!(value["value"]["a\u{FFFD}b"][0], json!("x\u{FFFD}"));
        assert_eq!(value["value"]["a\u{FFFD}b"][1]["c"], json!("y\u{FFFD}z"));
    }

    // 壊れた JSON（呼び出し側のバグ）でも行を捨てない。
    #[test]
    fn broken_json_becomes_a_placeholder_instead_of_an_error() {
        let value = prepare_payload("output", Some("{壊れた")).unwrap();
        let note = &value[AUDIT_NOTE_KEY];
        assert_eq!(note["recorded"], json!(false));
        assert_eq!(note["reason"], json!("invalid_json"));
        assert_eq!(note["field"], json!("output"));
        assert!(note["parse_error"].as_str().is_some());
    }

    // 呼び出し側が予約キーを送ってきても、最上位の `_audit` は必ず
    // この実装が書いたものになる（注記を騙られない）。
    #[test]
    fn a_caller_supplied_audit_key_is_pushed_into_the_envelope() {
        let value = prepare_value("input", json!({ "_audit": { "verbatim": false } }));
        assert_eq!(value[AUDIT_NOTE_KEY]["verbatim"], json!(true));
        assert_eq!(value[AUDIT_NOTE_KEY]["replaced_nul"], json!(0));
        assert_eq!(value["value"]["_audit"]["verbatim"], json!(false));
    }

    #[test]
    fn unstorable_placeholder_keeps_the_sqlstate_for_diagnosis() {
        let value = unstorable_placeholder("input", Some("22P05"));
        assert_eq!(value[AUDIT_NOTE_KEY]["sqlstate"], json!("22P05"));
        assert_eq!(value[AUDIT_NOTE_KEY]["recorded"], json!(false));
    }

    // 失敗の結果レコードに載るのは public_message() だけであることの
    // 単体レベルの確認（実 DB 経路は tests/audit_log.rs）。
    #[test]
    fn error_output_carries_only_the_message() {
        let value = error_output("貸借不一致: 借方 110,000 / 貸方 100,000");
        assert_eq!(
            value["message"],
            json!("貸借不一致: 借方 110,000 / 貸方 100,000")
        );
        assert_eq!(value.as_object().unwrap().len(), 1);
    }

    // 【C】42501 を AppendOnlyViolation のまま返さない。
    // cause は pub フィールドなので、presentation 層が cause.public_message()
    // を出しても「逆仕訳で訂正を」が現れてはいけない（D-038 の再演防止）。
    #[test]
    fn permission_denied_on_audit_log_never_advises_a_reversal() {
        let wrapped = rewrap_for_audit_log(RepoError::AppendOnlyViolation {
            reason: "権限エラーです（SQLSTATE 42501: insufficient_privilege）: \
                     permission denied for table audit_log"
                .to_string(),
        });

        assert!(matches!(wrapped, RepoError::Backend { .. }));
        assert!(!wrapped.public_message().contains("逆仕訳"));
        assert!(!wrapped.to_string().contains("逆仕訳"));
        assert_ne!(
            wrapped.code(),
            kaikei_app::error::codes::APPEND_ONLY_VIOLATION
        );
        // 診断情報（SQLSTATE と DB のメッセージ）は失わない。
        assert!(wrapped.to_string().contains("42501"));
        assert!(wrapped.to_string().contains("permission denied"));
    }

    // 包み直すのは AppendOnlyViolation だけ（診断の分類を潰さない）。
    #[test]
    fn other_repo_errors_pass_through_the_audit_wrapper_unchanged() {
        let corrupt = rewrap_for_audit_log(RepoError::Corrupt {
            reason: "check constraint".to_string(),
        });
        assert!(matches!(corrupt, RepoError::Corrupt { .. }));

        let backend = rewrap_for_audit_log(RepoError::Backend {
            reason: "connection closed".to_string(),
        });
        assert!(matches!(backend, RepoError::Backend { .. }));
    }
}
