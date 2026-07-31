//! アプリケーション層全体で使うエラー型（[`AppError`] / [`RepoError`]）。
//!
//! `CLAUDE.md` §11 の方針（次の手が分かる文言にする）に従う。`RepoError` は
//! ドメイン語彙の enum にする（`Box<dyn Error>` 一本にしない）。永続化層が
//! `Box<dyn Error>` 一本を返す設計だと、append-only 違反（DB権限・トリガによる
//! 拒否）が「ただの DB エラー」の1バリアントに潰れてしまい、この方針を
//! 満たせなくなる。SQLSTATE（`42501` = 権限拒否 / `P0001` = トリガ /
//! `23505` = 一意制約 等）の判別は実装側（`kaikei-store` の sqlstate
//! マッピング）が行い、この enum の適切なバリアントへ写像する
//! （`DECISIONS.md` D-032）。

use kaikei_core::CoreError;
use kaikei_policy::PolicyError;

/// 永続化層（[`crate::ports::Store`] の実装。`kaikei-store` 等）が返すエラー。
///
/// バリアントを分けるのは、呼び出し側（ユースケース）が「次に何をすべきか」を
/// 判断できるようにするため。例えば [`RepoError::AppendOnlyViolation`] を
/// 受け取ったユースケースは「訂正は逆仕訳（`reverse`）で行ってください」と
/// 案内できるが、単一の `Backend` バリアントに潰れているとその案内を
/// 組み立てられない。
#[derive(Debug, thiserror::Error)]
pub enum RepoError {
    /// 指定した対象が永続化層に存在しない。
    #[error("見つかりません: {reason}")]
    NotFound {
        /// 見つからなかった対象の説明。
        reason: String,
    },

    /// append-only の制約（DB 権限の REVOKE、または所有者もバイパスできない
    /// 最後の砦のトリガ）に反する操作をしようとした。帳簿の訂正は逆仕訳
    /// （`JournalEntry::reverse`）のみが許される（`CLAUDE.md` §2）。
    #[error("この操作は許可されていません（帳簿への訂正は逆仕訳のみです）: {reason}")]
    AppendOnlyViolation {
        /// 拒否の詳細（どの操作がどう拒否されたか）。
        reason: String,
    },

    /// 一意制約違反（重複データ）。
    #[error("既に存在します: {reason}")]
    Conflict {
        /// 重複の詳細。
        reason: String,
    },

    /// 保存されているデータが不正（`JournalEntry::rehydrate` の直前に行う
    /// 再検証で検出）。panic させず、この形で呼び出し側に返す。
    #[error("保存データが不正です: {reason}")]
    Corrupt {
        /// 不正の詳細。
        reason: String,
    },

    /// 金額・仕訳番号等が変換先の型で表現できる範囲を超えている
    /// （例: `i128` → `i64`、`u32` → `i32` の変換失敗）。
    #[error("値が範囲外です: {reason}")]
    OutOfRange {
        /// 範囲外の詳細。
        reason: String,
    },

    /// 現在の実装ではサポートしていない操作（例: 逆仕訳への証憑紐付け）。
    #[error("この操作はサポートされていません: {reason}")]
    Unsupported {
        /// サポートされない理由。
        reason: String,
    },

    /// 上記のいずれにも分類できない永続化層の失敗（接続断等）。
    #[error("永続化層でエラーが発生しました: {reason}")]
    Backend {
        /// 失敗の詳細。
        reason: String,
    },
}

/// ユースケースが失敗したときに返すエラー。
///
/// [`RepoError`] / [`PolicyError`] / [`CoreError`] をそのまま伝播できるように
/// `#[from]` を用意する。ユースケース固有の業務ルール違反（例: 既に取消済みの
/// 仕訳を再度取り消そうとした、集計軸に許可されていないタグキーを指定した等）は
/// [`AppError::Rejected`] を使うか、必要になった時点でバリアントを追加する
/// （ユースケース本体は本 PR の対象外であり、後続の PR が担う）。
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// 永続化層のエラー。
    #[error(transparent)]
    Repo(#[from] RepoError),

    /// `kaikei-policy` の trait 実装（`TaxPolicy` 等）が返したエラー。
    #[error(transparent)]
    Policy(#[from] PolicyError),

    /// `kaikei-core` の不変条件違反。
    #[error(transparent)]
    Core(#[from] CoreError),

    /// 上記に分類できない業務ルール違反。次の手が分かる文言にすること
    /// （`CLAUDE.md` §11）。
    #[error("{reason}")]
    Rejected {
        /// 拒否の理由と、可能であれば次に取るべき手。
        reason: String,
    },
}
