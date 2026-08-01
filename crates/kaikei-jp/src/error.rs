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
}
