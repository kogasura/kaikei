//! `kaikei-policy` 全体で使うエラー型。
//!
//! `CLAUDE.md` §11 の方針（次の手が分かる文言）に従う。**I/O 系のバリアント
//! （`Io` / `Db` / `Box<dyn Error>` 等）は意図的に定義しない。** policy は
//! I/O を一切行わないため、そのようなエラーが発生する経路自体が存在しない。
//! これが `CLAUDE.md` §3「policy trait は純関数を保つ」の型レベルの担保になる。

use kaikei_core::CoreError;

/// `kaikei-policy` の trait メソッドが失敗したときに返すエラー。
///
/// `#[non_exhaustive]` は付けない。呼び出し側（`kaikei-app`）がバリアントを
/// 網羅的に `match` して対応方針を出し分けられるようにするため。
///
/// **トレードオフ**: Phase 2 以降でこの enum にバリアントを追加すると、
/// 網羅的に `match` している下流コードのコンパイルが壊れる（意図した
/// 破壊的変更になる）。これは許容する代償である。`#[non_exhaustive]` を
/// 付けて「バリアントを追加してもコンパイルが通る」を優先すると、
/// 新しいエラー種別への対応漏れが `_ => {}` 等で静かに握りつぶされる方が
/// 会計データを扱ううえで危険だと判断した。
#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    /// `kaikei-core` の不変条件違反をそのまま伝播する（例: `mul_ratio` の
    /// オーバーフロー）。
    #[error(transparent)]
    Core(#[from] CoreError),

    /// 指定した取引日に適用可能なルールセット（年度別マスタ）が存在しない。
    #[error(
        "{as_of} 時点で適用可能な税制ルールが見つかりません。\
         該当年度のマスタデータが未整備の可能性があります"
    )]
    NoApplicableRuleSet {
        /// 対象の取引日（ISO表記）。
        as_of: String,
    },

    /// 税区分コードが、対象日時点のマスタに存在しない。
    #[error(
        "税区分 \"{code}\" は {as_of} 時点のマスタに存在しません\
         （利用可能な区分: {available}）"
    )]
    UnknownTaxCategory {
        /// 見つからなかった税区分コード。
        code: String,
        /// 対象の取引日（ISO表記）。
        as_of: String,
        /// 利用可能な税区分コードの一覧（表示用に整形済み）。
        available: String,
    },

    /// 税区分がその科目には適用できない（例: 資産科目に売上の税区分を指定した等）。
    #[error("科目 {account} に税区分 \"{code}\" は適用できません: {reason}")]
    TaxCategoryNotApplicable {
        /// 対象の勘定科目コード。
        account: String,
        /// 適用できない税区分コード。
        code: String,
        /// 適用できない理由。
        reason: String,
    },

    /// タグに指定された取引先コードが取引先マスタに存在しない。
    #[error("取引先コード \"{code}\" が見つかりません。取引先マスタに登録してください")]
    UnknownCounterparty {
        /// 見つからなかった取引先コード。
        code: String,
    },

    /// 適格請求書の保存が必要な税区分だが、取引先の適格請求書発行事業者としての
    /// 登録状況が未確認（`Counterparty::is_qualified_invoice_issuer` が `None`）。
    #[error(
        "取引先 {counterparty}（コード: {code}）の適格請求書発行事業者としての登録状況が\
         未確認です。取引先マスタで確認・記録してください"
    )]
    QualifiedInvoiceUnverified {
        /// 対象の取引先コード。
        code: String,
        /// 対象の取引先の表示名。
        counterparty: String,
    },

    /// policy が構築時に受け取ったデータ（年度別マスタ・事業者設定等）が不正。
    #[error("税制ポリシーのデータが不正です: {reason}")]
    InvalidPolicyData {
        /// 不正の理由。
        reason: String,
    },

    /// 現在の実装ではサポートしていない操作。
    #[error("この操作はサポートされていません: {reason}")]
    Unsupported {
        /// サポートされない理由。
        reason: String,
    },
}
