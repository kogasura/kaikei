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
//! （`tests/append_only.rs` 等）に委ねる）。
//!
//! # 対応表（`DECISIONS.md` D-032）
//!
//! | SQLSTATE | 意味 | 写像先 |
//! |---|---|---|
//! | `42501` | insufficient_privilege（`kaikei_app` が帳簿を UPDATE 等しようとした） | [`RepoError::AppendOnlyViolation`] |
//! | `P0001` | raise_exception（append-only トリガ `reject_mutation` が発火） | [`RepoError::AppendOnlyViolation`] |
//! | `23505` | unique_violation | [`RepoError::Conflict`] |
//! | `22003` | numeric_value_out_of_range（`SUM(...)::BIGINT` の桁あふれ等。`DECISIONS.md` D-033） | [`RepoError::OutOfRange`] |
//! | `23502` / `23514` | not_null_violation / check_violation | [`RepoError::Corrupt`]（理由は下記） |
//! | その他 | 未分類 | [`RepoError::Backend`] |
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
//! 中ではこれが最も近い。
//!
//! ## 既知の限界: `P0001` は `reject_mutation` 以外のトリガとも共有される
//!
//! `migrations/0004_append_only_triggers.sql` の `assert_entry_is_balanced`
//! （貸借不一致を検出する遅延制約トリガ）も無条件の `RAISE EXCEPTION`
//! （既定 SQLSTATE `P0001`）を使う。したがってこの写像は、本来 append-only
//! 違反ではない「貸借不一致トリガの発火」（`JournalEntry::new` の検証を
//! 経ずに `journal_lines` へ書き込まれた場合のみ起こりうる store 層のバグ）も
//! `AppendOnlyViolation` として報告してしまう。両者を判別するにはメッセージ
//! 文字列の内容を見るほかなく、`reject_mutation`（意図したユーザー操作の
//! 拒否）が P0001 の主要な発生源であるため、現時点ではこの限界を意図的に
//! 許容している（このコミットのスコープではメッセージによる判別ロジックを
//! 追加しない）。

use kaikei_app::error::RepoError;

/// SQLSTATE コードとメッセージから [`RepoError`] を組み立てる。
///
/// `code` は5文字の SQLSTATE（例: `"42501"`, `"P0001"`）。`message` は
/// データベースが返した人間可読のエラーメッセージで、そのまま `reason` に
/// 含める（診断情報として有用なため）。
pub fn map_sqlstate(code: &str, message: &str) -> RepoError {
    match code {
        "42501" => RepoError::AppendOnlyViolation {
            reason: format!("権限エラーです（SQLSTATE 42501: insufficient_privilege）: {message}"),
        },
        "P0001" => RepoError::AppendOnlyViolation {
            reason: format!(
                "データベースのトリガによって拒否されました（SQLSTATE P0001）: {message}"
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
    fn raise_exception_maps_to_append_only_violation() {
        let err = map_sqlstate(
            "P0001",
            "append-only table: journal_entries は変更できません（訂正は逆仕訳で行ってください）",
        );
        assert!(matches!(err, RepoError::AppendOnlyViolation { .. }));
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
