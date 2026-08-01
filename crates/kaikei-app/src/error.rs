//! アプリケーション層全体で使うエラー型（[`AppError`] / [`RepoError`]）。
//!
//! `CLAUDE.md` §11 の方針（次の手が分かる文言にする）に従う。`RepoError` は
//! ドメイン語彙の enum にする（`Box<dyn Error>` 一本にしない）。永続化層が
//! `Box<dyn Error>` 一本を返す設計だと、append-only 違反（DB権限・トリガによる
//! 拒否）が「ただの DB エラー」の1バリアントに潰れてしまい、この方針を
//! 満たせなくなる。SQLSTATE（`42501` = 権限拒否 / `P0010` = append-only 違反の
//! トリガ / `P0011` = 貸借不一致のトリガ / `23505` = 一意制約 等）の判別は
//! 実装側（`kaikei-store` の sqlstate マッピング）が行い、この enum の適切な
//! バリアントへ写像する（`DECISIONS.md` D-032、および `P0010`/`P0011` を
//! 汎用の `P0001` から分離した D-038）。

use kaikei_core::{CoreError, EntryNumber};
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

    /// 一意制約違反（重複データ）。例えば同じ仕訳IDや `(fiscal_year,
    /// entry_no)` の組を持つ仕訳を重ねて挿入しようとした場合に返す
    /// （[`crate::ports::JournalRepo::insert_entry`] の `# Errors` を参照）。
    #[error("既に存在します: {reason}")]
    Conflict {
        /// 重複の詳細。
        reason: String,
    },

    /// 保存されているデータが不正（永続化層からの復元処理の直前に行う
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
/// `#[from]` を用意する。
///
/// `#[non_exhaustive]` を付ける。`kaikei-policy::PolicyError` が意図的に
/// `#[non_exhaustive]` を**付けない**選択をしているのとは逆の判断である
/// （`kaikei-policy/src/error.rs` の doc を参照）。`PolicyError` の消費者は
/// `kaikei-app` の中だけ（同一ワークスペース内で足並みを揃えて更新できる）
/// だが、`AppError` の消費者は `kaikei-api` / `kaikei-mcp` のようなさらに
/// 下流の crate になる。バリアント追加のたびにそれらの網羅的 `match` が
/// 壊れるより、`_ => {}` の一手で追従できる方が実用上安全と判断した
/// （後から `#[non_exhaustive]` を付けるのは破壊的変更になるため、最初から
/// 付けておく）。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
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

    /// 既に赤伝（逆仕訳）済みの仕訳を再度取り消そうとした。
    ///
    /// 二重取消は既定で拒否する（誤操作・AI の暴走が帳簿の残高を静かに
    /// 壊すことを防ぐ多層防御の一つ）。許可する運用（`allow_double_reversal`
    /// をユースケース入力に明示した場合のみ許可）は、その入力型を持つ
    /// ユースケース本体（後続の PR）が実装する。
    #[error(
        "仕訳 {} は既に取消（逆仕訳 {}）済みです。\
         二重取消を許可する場合は allow_double_reversal を指定してください",
        entry_no.as_u32(),
        reversal_no.as_u32()
    )]
    AlreadyReversed {
        /// 取り消そうとした仕訳の番号。
        entry_no: EntryNumber,
        /// 既存の逆仕訳の番号。
        reversal_no: EntryNumber,
    },

    /// 試算表の検算に失敗した（借方合計 ≠ 貸方合計）。正しく記帳された
    /// データからは発生しない。データ破損、または実装のバグを示す。
    #[error(
        "試算表の貸借が一致しません: 借方 {debit} / 貸方 {credit}。\
         データが破損している可能性があります。管理者に連絡してください"
    )]
    Inconsistent {
        /// 借方合計（表示用文字列）。
        debit: String,
        /// 貸方合計（表示用文字列）。
        credit: String,
    },

    /// 上記に分類できない業務ルール違反。次の手が分かる文言にすること
    /// （`CLAUDE.md` §11）。
    #[error("{reason}")]
    Rejected {
        /// 拒否の理由と、可能であれば次に取るべき手。
        reason: String,
    },
}
