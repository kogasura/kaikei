//! `kaikei-jp` 全体で使うエラー型。
//!
//! `CLAUDE.md` §11 の方針（次の手が分かる文言）に従う。手書き YAML の
//! タイプミスは、どのファイルの何行目で何が起きたかが分かる形で返す。

/// `kaikei-jp` のローダ・解釈処理が失敗したときに返すエラー。
///
/// `#[non_exhaustive]` は付けない（`kaikei-policy::PolicyError` と同じ理由。
/// 呼び出し側がバリアントを網羅的に `match` して対応方針を出し分けられる
/// ようにするため。バリアント追加は意図した破壊的変更として扱う）。
#[derive(Debug, thiserror::Error)]
pub enum JpError {
    /// YAML の構文・スキーマ検証（`#[serde(deny_unknown_fields)]` 含む）に失敗した。
    ///
    /// `source`（[`serde_norway::Error`]）の `Display` は解析位置（行・列）を
    /// 含む原文のメッセージを保持する（例: `"...at line 4 column 5"`）。
    /// `label` で「どのファイルか」を補うことで、「どのファイルの何行目で
    /// 何が起きたか」が分かる文言にする（`CLAUDE.md` §11）。
    #[error("{label} のYAML解析に失敗しました: {source}")]
    YamlParse {
        /// 読み込み元の識別子（埋め込みYAMLの名称、またはファイルパス）。
        label: String,
        /// 元のパースエラー（行・列を含む）。
        #[source]
        source: serde_norway::Error,
    },

    /// ファイルシステムからの読み込みに失敗した（[`crate::data::load_from_path`] のみ）。
    #[error("YAMLファイル \"{path}\" を読み込めません: {source}")]
    Io {
        /// 読み込もうとしたパス。
        path: String,
        /// 元の I/O エラー。
        #[source]
        source: std::io::Error,
    },

    /// 税区分マスタ（[`crate::tax::TaxCategoryTable`]）1ファイル分の内容が不正。
    ///
    /// YAML の構文自体は正しいが、`applies_from`/`applies_to`/`rate` 等の
    /// 値が期待する形式・範囲に収まっていない場合に返す（`YamlParse` は
    /// 構文・スキーマ形状のみを扱い、値の意味的な妥当性はここで検証する）。
    #[error("税区分マスタ \"{label}\" が不正です: {reason}")]
    InvalidTaxCategoryTable {
        /// 読み込み元の識別子（埋め込みYAMLの名称、またはファイルパス）。
        label: String,
        /// 不正の理由（次に何を直せばよいか分かる文言。`CLAUDE.md` §11）。
        reason: String,
    },

    /// 複数の税区分マスタの適用期間が重なっている（[`crate::tax::TaxRuleSets`] 構築時）。
    ///
    /// 重なりを許すと、ある取引日にどちらのマスタを使うか一意に決められない
    /// ため、ロード時点でエラーにする（`DECISIONS.md` D-054）。
    #[error(
        "税区分マスタの適用期間が重なっています: \"{first_label}\"（{first_range}）と \
         \"{second_label}\"（{second_range}）。重ならないように applies_from / applies_to を \
         見直してください（例: 古い方の applies_to を新しい方の applies_from の前日にする）"
    )]
    OverlappingTaxPeriods {
        /// 一方のマスタの識別子。
        first_label: String,
        /// 一方のマスタの適用期間（表示用）。
        first_range: String,
        /// もう一方のマスタの識別子。
        second_label: String,
        /// もう一方のマスタの適用期間（表示用）。
        second_range: String,
    },

    /// 税区分コードが、指定したマスタに存在しない。
    #[error(
        "税区分コード \"{code}\" は \"{table_label}\"（適用開始 {applies_from}）に存在しません\
         （利用可能な区分: {available}）"
    )]
    UnknownTaxCategoryCode {
        /// 見つからなかった税区分コード。
        code: String,
        /// 検索対象にしたマスタの識別子。
        table_label: String,
        /// 検索対象にしたマスタの適用開始日（ISO表記）。
        applies_from: String,
        /// そのマスタに存在する有効な税区分コード一覧（表示用に整形済み）。
        available: String,
    },
}
