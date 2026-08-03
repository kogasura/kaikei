//! PostgreSQL の SQLSTATE から [`RepoError`] への写像。
//!
//! PR-5 本体（書き込み側の `insert_entry` 等）と PR-6（read model の SQL
//! 集計）の両方が参照する共有基盤（`DECISIONS.md` D-034）。
//!
//! [`map_sqlstate`] は DB 接続を必要としない純関数にしてある。`sqlx::Error`
//! から実際の SQLSTATE 文字列とメッセージを取り出す薄いラッパは
//! [`crate::error::from_sqlx_error`] に置き、役割が重ならないようにする
//! （`sqlx::Error::Database` の中身は `Box<dyn sqlx::error::DatabaseError>`
//! で、テストのためだけにスタブ実装を用意するコストが見合わない。写像
//! ロジックだけをこの純関数として切り出して検証し、`sqlx::Error` を実際に
//! 構築して検証するテストは PostgreSQL が必要な `pg-tests` 側
//! （`tests/append_only.rs` / `tests/schema_constraints.rs`）に委ねる）。
//!
//! # 対応表（`DECISIONS.md` D-032 / D-038）
//!
//! | SQLSTATE | 意味 | 写像先 |
//! |---|---|---|
//! | `42501` | insufficient_privilege（`kaikei_app` が帳簿を UPDATE 等しようとした） | [`RepoError::AppendOnlyViolation`] |
//! | `P0010` | `reject_mutation` トリガの発火（append-only 違反。`migrations/0008_distinct_error_codes.sql`） | [`RepoError::AppendOnlyViolation`] |
//! | `P0011` | `assert_entry_is_balanced` トリガの発火（貸借不一致。store層のバグ検出。同上） | [`RepoError::Corrupt`]（理由は下記） |
//! | `P0012` | `reject_audit_log_mutation` トリガの発火（監査ログの変更。`migrations/0009_audit_log.sql`） | [`RepoError::Backend`]（理由は下記） |
//! | `P0001` | raise_exception（ERRCODE を指定しない汎用の `RAISE EXCEPTION`。どのトリガかを断定できない） | [`RepoError::Backend`]（理由は下記） |
//! | `23505` | unique_violation | [`RepoError::Conflict`] |
//! | `22003` | numeric_value_out_of_range（`SUM(...)::BIGINT` の桁あふれ等。`DECISIONS.md` D-033） | [`RepoError::OutOfRange`] |
//! | `23502` / `23514` | not_null_violation / check_violation | [`RepoError::Corrupt`]（理由は下記） |
//! | その他 | 未分類 | [`RepoError::Backend`] |
//!
//! ## `P0010`/`P0011` を分けた経緯（`DECISIONS.md` D-038。D-037 を上書きする決定）
//!
//! 当初 `migrations/0004_append_only_triggers.sql` の `reject_mutation`
//! （append-only 違反）と `assert_entry_is_balanced`（貸借不一致検出）は
//! どちらも ERRCODE を指定しない `RAISE EXCEPTION` を使っており、結果として
//! 両方とも PostgreSQL の既定コード `P0001` を返していた。この状態で
//! `P0001` を `AppendOnlyViolation` に写像すると、貸借不一致（store層の
//! バグでしか起こりえない）が「append-only 違反」として報告され、
//! 「訂正は逆仕訳で行ってください」という**完全に誤った対処法**を
//! 提示してしまう（`CLAUDE.md` §11 違反。Phase 0 の循環参照バグ「無関係の
//! 科目を犯人として名指しし、破壊的で無駄な修正に誘導する」と同じ欠陥
//! クラス）。`migrations/0008_distinct_error_codes.sql` で両トリガに
//! 別々の ERRCODE（`P0010`/`P0011`）を与えることでこれを解消した
//! （D-037 の「既知の限界として受容する」という判断を D-038 で覆した）。
//!
//! ## なぜ `P0001` を `Backend` にしたか
//!
//! `P0010`/`P0011` を専用コードとして切り出した後も、`P0001` 自体は
//! 汎用の `raise_exception`（将来 ERRCODE を指定しない `RAISE EXCEPTION` が
//! 別の用途で追加される可能性がある）として残る。この汎用コードだけからは
//! 「append-only 違反」なのか「貸借不一致」なのか、あるいは全く別の理由
//! なのかを一切断定できないため、`AppendOnlyViolation`（「逆仕訳で」という
//! 特定の対処法を案内する）に寄せることはできない。診断情報を失わせない
//! `Backend` に写像し、`reason` にメッセージ本文をそのまま含める。
//!
//! ## なぜ `P0012`（監査ログ）を `AppendOnlyViolation` にしなかったか（`DECISIONS.md` D-075）
//!
//! [`RepoError::AppendOnlyViolation`] の `Display` と `public_message` は
//! 「訂正は逆仕訳（`reverse_journal_entry`）で行ってください」と案内する。
//! これは**帳簿本体に対してのみ正しい**案内であり、`audit_log` に対しては
//! 的外れである（監査ログは逆仕訳で直すものではない。記録の訂正は新しい行の
//! 追加で行う）。`P0010` と同じバリアントに寄せると、D-038 が潰したのと同じ
//! 「完全に誤った対処法を案内する」欠陥をそのまま再生産することになる。
//!
//! `RepoError` に監査ログ専用のバリアントを足すことは**しなかった**。
//! この enum は `#[non_exhaustive]` ではなく（`kaikei-app` の `error.rs`
//! の doc）、バリアント追加は下流の網羅 `match` を壊す破壊的変更である。
//! 一方 `P0012` が実際に発生するのは「アプリが `audit_log` を
//! UPDATE/DELETE しようとした」場合だけで、そのようなコードはこの
//! リポジトリに存在しない（`AuditSink` は INSERT しか持たない）。
//! つまりこれは**実装のバグでのみ到達する**経路であり、
//! [`RepoError::Backend`]（「入力の問題ではない。ログを添えて管理者へ」）が
//! 意味的に最も近い。診断に要る情報は `reason` に残る。
//!
//! ## なぜ `23502`/`23514` を `Corrupt` にしたか（`DECISIONS.md` D-037）
//!
//! `insert_entry` 等に渡すデータは `kaikei_core::JournalEntry::new` /
//! `reverse` が既に検証済みのため、この2つの SQLSTATE が実際に発生するのは
//! 「store 層のマッピングコードがドメインの不変条件と食い違う行を組み立てた」
//! 場合にほぼ限られる。`Conflict`（一意制約=重複データ）とは意味が異なるため
//! 使わず、`Backend`（接続断等の無分類failure）に埋もれさせると「保存しようと
//! したデータの構造そのものが不正」という診断情報が消えてしまう。
//! [`RepoError::Corrupt`] の doc コメントは主に「復元処理の直前に行う
//! 再検証で検出した不整合」を指すが、「永続化しようとしたデータが構造的な
//! 不変条件を満たさない」という点で性質が同じであり、既存の7バリアントの
//! 中ではこれが最も近い。`P0011`（貸借不一致）も同じ理由で `Corrupt` にし、
//! メッセージは「逆仕訳で訂正してください」とは案内しない（対処法が
//! 全く異なるため。逆仕訳で直せるのは正しく記帳された仕訳の取消であり、
//! 貸借不一致はそもそも記帳処理自体が壊れていたことを示す）。

use kaikei_app::error::RepoError;

/// SQLSTATE コードとメッセージから [`RepoError`] を組み立てる。
///
/// `code` は5文字の SQLSTATE（例: `"42501"`, `"P0010"`）。`message` は
/// データベースが返した人間可読のエラーメッセージで、そのまま `reason` に
/// 含める（診断情報として有用なため）。
pub fn map_sqlstate(code: &str, message: &str) -> RepoError {
    match code {
        "42501" => RepoError::AppendOnlyViolation {
            reason: format!("権限エラーです（SQLSTATE 42501: insufficient_privilege）: {message}"),
        },
        "P0010" => RepoError::AppendOnlyViolation {
            reason: format!(
                "append-only トリガによって拒否されました（SQLSTATE P0010）: {message}"
            ),
        },
        "P0011" => RepoError::Corrupt {
            reason: format!(
                "貸借不一致を検出しました（SQLSTATE P0011）。アプリ層の検証\
                 （JournalEntry::new）を経ずに journal_lines へ書き込まれた\
                 可能性があります（store層のバグ）: {message}"
            ),
        },
        "P0012" => RepoError::Backend {
            reason: format!(
                "監査ログ（audit_log）の変更が拒否されました（SQLSTATE P0012）。\
                 監査ログは追記のみで、記録の訂正は新しい行の追加で行います\
                 （帳簿の逆仕訳とは別の話です）。ここに到達するのは実装のバグです: {message}"
            ),
        },
        "23505" => RepoError::Conflict {
            reason: format!("一意制約違反です（SQLSTATE 23505: unique_violation）: {message}"),
        },
        "22003" => RepoError::OutOfRange {
            reason: format!(
                "データベース側で数値が表現可能な範囲を超えました\
                 （SQLSTATE 22003: numeric_value_out_of_range）: {message}"
            ),
        },
        "23502" | "23514" => RepoError::Corrupt {
            reason: format!(
                "保存しようとしたデータがデータベースの制約に違反しています\
                 （SQLSTATE {code}）: {message}"
            ),
        },
        other => RepoError::Backend {
            reason: format!("未分類のデータベースエラーです（SQLSTATE {other}）: {message}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insufficient_privilege_maps_to_append_only_violation() {
        let err = map_sqlstate("42501", "permission denied for table journal_entries");
        assert!(matches!(err, RepoError::AppendOnlyViolation { .. }));
    }

    // CLAUDE.md §11: append-only 違反は「訂正は逆仕訳で」と伝わる文言にする。
    // その文言は RepoError::AppendOnlyViolation の Display 実装（kaikei-app）が
    // 既に持っているため、ここでは写像先バリアントの選択だけを検証すれば
    // メッセージ全体の要件も満たされる。
    #[test]
    fn append_only_violation_message_mentions_reversal() {
        let err = map_sqlstate("42501", "permission denied");
        assert!(err.to_string().contains("逆仕訳"));
    }

    #[test]
    fn reject_mutation_trigger_maps_to_append_only_violation() {
        let err = map_sqlstate(
            "P0010",
            "append-only table: journal_entries は変更できません（訂正は逆仕訳で行ってください）",
        );
        assert!(matches!(err, RepoError::AppendOnlyViolation { .. }));
    }

    // D-038: 貸借不一致トリガ（P0011）は AppendOnlyViolation ではなく Corrupt に
    // 写像され、「逆仕訳で」という誤った対処法を案内しないこと。
    #[test]
    fn unbalanced_entry_trigger_maps_to_corrupt_not_append_only_violation() {
        let err = map_sqlstate(
            "P0011",
            "貸借不一致: entry_id=... の借方合計(1000) と貸方合計(500) が一致しません",
        );
        assert!(matches!(err, RepoError::Corrupt { .. }));
        assert!(!err.to_string().contains("逆仕訳"));
    }

    // D-075: 監査ログのトリガ（P0012）は AppendOnlyViolation に寄せない。
    // 「訂正は逆仕訳で」は監査ログに対しては的外れな案内であり、
    // D-038 が潰したのと同じ誤診クラスになる。
    #[test]
    fn audit_log_trigger_maps_to_backend_and_never_advises_a_reversal() {
        let err = map_sqlstate(
            "P0012",
            "監査ログ（audit_log）は追記のみです。記録の訂正は新しい行の追加で行ってください",
        );
        assert!(matches!(err, RepoError::Backend { .. }));
        assert_ne!(err.code(), kaikei_app::error::codes::APPEND_ONLY_VIOLATION);
        assert!(!err.public_message().contains("逆仕訳"));
    }

    // 汎用の raise_exception（P0001）はどちらのトリガかを断定できないため Backend。
    #[test]
    fn generic_raise_exception_maps_to_backend() {
        let err = map_sqlstate("P0001", "some other raised exception");
        assert!(matches!(err, RepoError::Backend { .. }));
    }

    #[test]
    fn unique_violation_maps_to_conflict() {
        let err = map_sqlstate(
            "23505",
            "duplicate key value violates unique constraint \"journal_entries_pkey\"",
        );
        assert!(matches!(err, RepoError::Conflict { .. }));
    }

    #[test]
    fn numeric_out_of_range_maps_to_out_of_range() {
        let err = map_sqlstate("22003", "bigint out of range");
        assert!(matches!(err, RepoError::OutOfRange { .. }));
    }

    #[test]
    fn not_null_violation_maps_to_corrupt() {
        let err = map_sqlstate(
            "23502",
            "null value in column \"currency_minor_unit\" violates not-null constraint",
        );
        assert!(matches!(err, RepoError::Corrupt { .. }));
    }

    #[test]
    fn check_violation_maps_to_corrupt() {
        let err = map_sqlstate(
            "23514",
            "new row for relation \"journal_lines\" violates check constraint \"journal_lines_amount_minor_check\"",
        );
        assert!(matches!(err, RepoError::Corrupt { .. }));
    }

    #[test]
    fn unknown_sqlstate_maps_to_backend() {
        let err = map_sqlstate("08006", "connection failure");
        assert!(matches!(err, RepoError::Backend { .. }));
    }

    #[test]
    fn message_is_preserved_in_reason_for_diagnostics() {
        match map_sqlstate(
            "23505",
            "duplicate key value violates unique constraint \"journal_entries_pkey\"",
        ) {
            RepoError::Conflict { reason } => {
                assert!(reason.contains("journal_entries_pkey"));
            }
            other => panic!("RepoError::Conflict を期待しましたが {other:?} でした"),
        }
    }
}
