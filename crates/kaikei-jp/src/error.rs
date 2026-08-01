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

    /// 勘定科目テンプレート（[`crate::chart`]）1ファイル分の内容が不正。
    ///
    /// YAML の構文自体は正しいが、`version`/`type`/科目コード等の値が期待する
    /// 形式・範囲に収まっていない場合（`InvalidTaxCategoryTable` と同じ扱い）、
    /// または `kaikei_core::ChartOfAccounts::new` が検証する不変条件（親科目の
    /// 不在・循環参照・コード重複）に違反する場合に返す。後者は
    /// `kaikei_core::CoreError` の `Display` をそのまま `reason` に含めるため、
    /// 元の理由は失われない。
    #[error("勘定科目テンプレート \"{label}\" が不正です: {reason}")]
    InvalidChart {
        /// 読み込み元の識別子（埋め込みYAMLの名称、またはファイルパス）。
        label: String,
        /// 不正の理由（次に何を直せばよいか分かる文言。`CLAUDE.md` §11）。
        reason: String,
    },

    /// タグスキーマ（[`crate::tags`]）1ファイル分の内容が不正。
    ///
    /// YAML の構文自体は正しいが、`version`/`value_type`/`required_for`
    /// 等の値が期待する形式・範囲に収まっていない場合、またはタグキーが
    /// 重複している場合に返す（`InvalidTaxCategoryTable` と同じ扱い）。
    #[error("タグスキーマ \"{label}\" が不正です: {reason}")]
    InvalidTagSchema {
        /// 読み込み元の識別子（埋め込みYAMLの名称、またはファイルパス）。
        label: String,
        /// 不正の理由（次に何を直せばよいか分かる文言。`CLAUDE.md` §11）。
        reason: String,
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

    /// 家事按分（[`crate::household_split::household_split`]）の事業割合が
    /// 0 以上 1 以下の範囲外。
    ///
    /// `kaikei_core::Ratio` 型自体は 0〜1 に制約されていない
    /// （`Ratio::parse_rate` 経由なら 1 を超える値も、`Ratio::parse_fraction`
    /// 経由なら 0〜1 の範囲に収まる値のみが構築できる。呼び出し側がどちらで
    /// 構築したかを型からは区別できないため、ここで実行時に検証する）。
    #[error(
        "家事按分の事業割合は0以上1以下である必要があります: {ratio}。\
         0%〜100%の範囲で指定してください"
    )]
    InvalidBusinessRatio {
        /// 範囲外だった比率（表示用の10進文字列）。
        ratio: String,
    },

    /// 家事按分（[`crate::household_split::household_split`]）の対象金額が0以下。
    #[error(
        "家事按分の対象金額は正の値である必要があります: {total}。\
         0円または負の金額は按分できません"
    )]
    InvalidHouseholdSplitTotal {
        /// 不正だった金額（表示用）。
        total: String,
    },

    /// `kaikei-core` の演算（`Money::mul_ratio` / `Money::sub` /
    /// `JournalLine::new` 等）が失敗した。
    #[error(transparent)]
    Core(#[from] kaikei_core::CoreError),

    /// 決算処理（[`crate::closing::JpSoleProprietorClosingPolicy`]）に必要な科目
    /// （元入金・事業主貸・事業主借のいずれか）が、構築時に渡された
    /// `ChartOfAccounts` に存在しない。
    ///
    /// 決算処理の実行時ではなく**構築時**に検出することで、記帳作業の途中で
    /// 決算処理だけが失敗する事態を避ける（`docs/04-jp-tax.md` §9）。
    #[error(
        "決算処理に必要な科目（{role}）が勘定科目表に見つかりません: \"{code}\"。\
         勘定科目表にこの科目を追加するか、正しい科目コードを\
         JpSoleProprietorClosingPolicy::new に指定してください"
    )]
    MissingClosingAccount {
        /// 見つからなかった科目の役割（例: "元入金"）。
        role: String,
        /// 見つからなかった科目コード。
        code: String,
    },
}
