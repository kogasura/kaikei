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

    /// インボイス登録番号（[`crate::invoice::InvoiceRegistrationNo`]）が `'T'` から
    /// 始まっていない。
    ///
    /// 小文字の `'t'` や `T` 以外の文字から始まる入力、空文字列もこのバリアントになる。
    #[error("インボイス登録番号は先頭が \"T\"（大文字）である必要があります: \"{input}\"")]
    InvoiceRegNoMissingPrefix {
        /// 検証に失敗した入力文字列（そのまま）。
        input: String,
    },

    /// `T` の後ろの文字数が13ではない。
    ///
    /// 半角数字とは限らない文字数を指す（前後の空白や全角文字を含めた文字数）。
    #[error(
        "インボイス登録番号は \"T\" の後に13桁の数字が必要ですが、{actual_len}文字です: \"{input}\""
    )]
    InvoiceRegNoWrongLength {
        /// 検証に失敗した入力文字列（そのまま）。
        input: String,
        /// `T` の後ろの文字数。
        actual_len: usize,
    },

    /// `T` の後ろの13文字に半角数字以外が含まれる（全角数字・ハイフン・空白等）。
    #[error(
        "インボイス登録番号の \"T\" の後は半角数字のみである必要があります（全角数字・ハイフン・空白は不可）: \"{input}\""
    )]
    InvoiceRegNoNonDigit {
        /// 検証に失敗した入力文字列（そのまま）。
        input: String,
    },

    /// チェックデジット（先頭1桁）が基礎番号（残り12桁）から計算した値と一致しない。
    #[error(
        "インボイス登録番号のチェックデジットが一致しません（期待 {expected} / 実際 {actual}）。入力を確認してください: \"{input}\""
    )]
    InvoiceRegNoCheckDigit {
        /// 検証に失敗した入力文字列（そのまま）。
        input: String,
        /// 基礎番号（残り12桁）から計算した期待値。
        expected: u32,
        /// 入力の先頭1桁として書かれていた実際の値。
        actual: u32,
    },
}
