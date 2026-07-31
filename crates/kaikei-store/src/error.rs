//! `sqlx::Error` から [`RepoError`] への変換の入口。
//!
//! SQLSTATE ごとの判別規則そのものは [`crate::sqlstate::map_sqlstate`]
//! （DB接続なしでテスト可能な純関数）に置く。このファイルでは `sqlx::Error`
//! から SQLSTATE とメッセージを取り出す薄いラッパのみを持ち、
//! `crate::sqlstate` と役割が重ならないようにする（`sqlx::Error::Database`
//! の中身 `Box<dyn sqlx::error::DatabaseError>` をテストのためだけに
//! スタブ実装するコストを避けるため、写像ロジックは `map_sqlstate` 側の
//! 純関数テストで検証し、このラッパ自体の実効性検証は PostgreSQL が必要な
//! `pg-tests`（`tests/append_only.rs` 等）に委ねる。`kaikei-store` 独自の
//! 内部エラー型は導入しない。`kaikei-app::error::RepoError` が既に
//! ドメイン語彙の enum として十分な表現力を持つため）。

use crate::sqlstate::map_sqlstate;
use kaikei_app::error::RepoError;

/// `sqlx::Error` を [`RepoError`] へ変換する。
///
/// - `sqlx::Error::RowNotFound` は [`RepoError::NotFound`]
/// - `sqlx::Error::Database` は SQLSTATE を取り出せれば
///   [`crate::sqlstate::map_sqlstate`] に委譲し、SQLSTATE を持たない
///   データベースエラーは [`RepoError::Backend`]
/// - それ以外（接続断・タイムアウト等）は [`RepoError::Backend`]
pub fn from_sqlx_error(err: sqlx::Error) -> RepoError {
    match err {
        sqlx::Error::RowNotFound => RepoError::NotFound {
            reason: "該当するデータが見つかりませんでした".to_string(),
        },
        sqlx::Error::Database(db_err) => match db_err.code() {
            Some(code) => map_sqlstate(&code, db_err.message()),
            None => RepoError::Backend {
                reason: format!(
                    "SQLSTATE を持たないデータベースエラーです: {}",
                    db_err.message()
                ),
            },
        },
        other => RepoError::Backend {
            reason: format!("永続化層でエラーが発生しました: {other}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_not_found_maps_to_not_found() {
        let err = from_sqlx_error(sqlx::Error::RowNotFound);
        assert!(matches!(err, RepoError::NotFound { .. }));
    }

    #[test]
    fn protocol_error_maps_to_backend() {
        let err = from_sqlx_error(sqlx::Error::Protocol("boom".to_string()));
        assert!(matches!(err, RepoError::Backend { .. }));
    }

    #[test]
    fn pool_timed_out_maps_to_backend() {
        let err = from_sqlx_error(sqlx::Error::PoolTimedOut);
        assert!(matches!(err, RepoError::Backend { .. }));
    }
}
