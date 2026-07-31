//! `kaikei-core` 全体で使うエラー型。
//!
//! MCP 経由で AI が自己修正できる文言にする（`CLAUDE.md` §11）。
//! 差額や期待値を必ず含め、「次に何をすべきか」が読み取れるメッセージにすること。

/// `kaikei-core` の操作が失敗したときに返すエラー。
///
/// 一部のバリアント（`TagTypeMismatch` の `expected: TagValueType`、
/// `MissingRequiredTag` の `account_type: AccountType` を持つもの）は、
/// 依存する型が `tag.rs` / `account.rs`（いずれも未実装）にあるため、
/// Phase 0 後続PR（tag.rs / account.rs 実装時）に追加する。
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// 貸借が一致しない。
    #[error("貸借不一致: 借方 {debit} / 貸方 {credit}（差額 {diff}）")]
    Unbalanced {
        /// 借方合計（表示用文字列）。
        debit: String,
        /// 貸方合計（表示用文字列）。
        credit: String,
        /// 借方と貸方の差額（表示用文字列）。
        diff: String,
    },

    /// 仕訳の明細行が最低数（2行）に満たない。
    #[error("明細が不足しています（{found} 行）。仕訳は 2 行以上必要です")]
    TooFewLines {
        /// 実際に渡された明細行数。
        found: usize,
    },

    /// 勘定科目表に存在しない科目コードが指定された。
    #[error("勘定科目が見つかりません: {code}")]
    UnknownAccount {
        /// 見つからなかった科目コード。
        code: String,
    },

    /// 記帳できない科目（見出し科目、`postable == false`）に記帳しようとした。
    #[error("記帳できない科目です（見出し科目）: {code}")]
    NotPostable {
        /// 記帳しようとした科目コード。
        code: String,
    },

    /// 異なる通貨同士を演算・混在させようとした。
    #[error("通貨が混在しています: {a} と {b}")]
    CurrencyMismatch {
        /// 一方の通貨コード。
        a: String,
        /// もう一方の通貨コード。
        b: String,
    },

    /// 金額の値が不正（桁数超過、パース失敗、オーバーフロー等）。
    #[error("金額が不正です: {reason}")]
    InvalidAmount {
        /// 不正の理由。
        reason: String,
    },

    /// `TagSchema` に登録されていないタグキーが使われた。
    #[error("未登録のタグキーです: {key}。kaikei-jp-data/tags.yaml に登録してください")]
    UnknownTagKey {
        /// 未登録のタグキー。
        key: String,
    },

    // TagTypeMismatch { key: String, expected: TagValueType } は
    // TagValueType（tag.rs、未実装）に依存するため Phase 0 後続PRで追加する。
    //
    // MissingRequiredTag { key: String, account_type: AccountType } は
    // AccountType（account.rs、未実装）に依存するため Phase 0 後続PRで追加する。
    /// 取引日が会計年度の範囲外。
    #[error("取引日 {date} は会計年度 {fy}（{start}〜{end}）の範囲外です")]
    DateOutOfFiscalYear {
        /// 対象の取引日（ISO表記）。
        date: String,
        /// 会計年度ラベル。
        fy: i32,
        /// 会計年度の開始日（ISO表記）。
        start: String,
        /// 会計年度の終了日（ISO表記）。
        end: String,
    },

    /// 対象の会計期間が締められている。
    #[error("会計期間が締められています: {date}")]
    PeriodClosed {
        /// 締められている対象の日付（ISO表記）。
        date: String,
    },

    /// 摘要が空。
    #[error("摘要が空です")]
    EmptyDescription,

    /// 勘定科目表そのものが不正（親不在、循環参照、重複コード等）。
    #[error("勘定科目表が不正です: {reason}")]
    InvalidChart {
        /// 不正の理由。
        reason: String,
    },

    /// `aggregatable: false` のタグキーを集計軸に指定した。
    #[error("集計軸に使えないタグキーです: {key}（aggregatable = false）")]
    NotAggregatable {
        /// 指定されたタグキー。
        key: String,
    },

    /// 上記以外の値の不正（フォーマット不一致等の汎用エラー）。
    #[error("値が不正です: {reason}")]
    InvalidValue {
        /// 不正の理由。
        reason: String,
    },
}
