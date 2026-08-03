//! エラーの返し方（**ツール結果エラー**）と、`kaikei_jp::JpError` の分類コード。
//!
//! # 返し方
//!
//! ドメインのエラーは全て **ツール結果エラー**（`isError: true`）で返す。
//! JSON-RPC のプロトコルエラー（rmcp では `Err(ErrorData)`）は使わない。
//! クライアントがプロトコルエラーを不透明に描画すると、呼び出し元のモデルに
//! メッセージが届かず、`CLAUDE.md` §11「AI が自己修正できる文言」が空文に
//! なるため（`DECISIONS.md` D-071、`docs/07-mcp-server.md` §6）。
//!
//! プロトコルエラーを使ってよいのは、ツール呼び出しに到達できない異常
//! （未知のツール名など）に限る。
//!
//! # コードの写像表は再実装しない
//!
//! `AppError` / `RepoError` / `CoreError` / `PolicyError` の対応表は
//! `crates/kaikei-app/src/error.rs` に1箇所だけある（`DECISIONS.md` D-072）。
//! この層は [`kaikei_app::error::AppError::code`] と
//! [`kaikei_app::error::AppError::public_message`] を**呼ぶだけ**にする。
//!
//! ここに置くのは `kaikei_jp::JpError` の対応表だけである。
//! `kaikei-app` は `kaikei-jp` に依存できない（`CLAUDE.md` §1・CI が検査）ため、
//! `JpError` の写像表は `kaikei-app` には置けない（`docs/07-mcp-server.md` §6）。

use kaikei_app::error::{codes, core_error_code, AppError};
use kaikei_jp::error::JpError;
use rmcp::model::CallToolResult;
use serde_json::{json, Map, Value};

/// `kaikei-mcp` が新しく起こした分類コード。
///
/// `kaikei_app::error::codes` に**既に語彙がある概念には別名を作らない**
/// （`docs/07-mcp-server.md` §6）。ここに定義するのは、`kaikei-app` から
/// 見えるエラーには対応する概念が存在しないものだけである。
pub mod jp_codes {
    /// `JpError::InvoiceRegNoMissingPrefix`（先頭が `T` でない）。
    ///
    /// 登録番号の検証は「先頭文字 → 桁数 → 文字種 → チェックデジット」の順に
    /// 固定されており、最初に失敗した観点だけが返る（`DECISIONS.md` D-053）。
    /// 4つを1つのコードに潰すと、AI は何桁目を直せばよいかを本文の日本語から
    /// 読み取るしかなくなる。
    pub const INVOICE_REG_NO_MISSING_PREFIX: &str = "invoice_reg_no_missing_prefix";
    /// `JpError::InvoiceRegNoWrongLength`（`T` の後が13文字でない）。
    pub const INVOICE_REG_NO_WRONG_LENGTH: &str = "invoice_reg_no_wrong_length";
    /// `JpError::InvoiceRegNoNonDigit`（`T` の後に半角数字以外が混じる）。
    pub const INVOICE_REG_NO_NON_DIGIT: &str = "invoice_reg_no_non_digit";
    /// `JpError::InvoiceRegNoCheckDigit`（チェックデジット不一致）。
    pub const INVOICE_REG_NO_CHECK_DIGIT: &str = "invoice_reg_no_check_digit";

    /// `JpError::DuplicateTagKeyInInput`（入力の `tags` に同じキーが2回以上）。
    ///
    /// `unknown_tag_key`（未登録のキー）とは次の手が違う
    /// （綴りを直すのではなく、重複した指定を1つにまとめる）。
    pub const DUPLICATE_TAG_KEY: &str = "duplicate_tag_key";

    /// `JpError::InvalidSettingCode`（`tax_mode` 等の機械可読名が不正）。
    pub const INVALID_SETTING_CODE: &str = "invalid_setting_code";
}

/// `kaikei_jp::JpError` に対応する分類コードを返す。
///
/// # 既存の語彙に寄せているもの
///
/// | `JpError` | コード | 理由 |
/// |---|---|---|
/// | `Core(_)` | 中身の `CoreError` へ委譲 | `AppError::Core` と同じ扱い |
/// | `UnregisteredTagKey` | [`codes::UNKNOWN_TAG_KEY`] | `CoreError::UnknownTagKey` と同義（未登録のタグキー） |
/// | `InvalidTagValue` | [`codes::TAG_TYPE_MISMATCH`] | `CoreError::TagTypeMismatch` と同義（値が登録された型に合わない） |
/// | `NoApplicableTaxRuleSet` | [`codes::NO_APPLICABLE_RULE_SET`] | `PolicyError::NoApplicableRuleSet` と同義 |
/// | `UnknownTaxCategoryCode` | [`codes::UNKNOWN_TAX_CATEGORY`] | `PolicyError::UnknownTaxCategory` と同義 |
/// | `InvalidBusinessRatio` | [`codes::INVALID_VALUE`] | 値そのものが範囲外 |
/// | `InvalidHouseholdSplitTotal` | [`codes::INVALID_AMOUNT`] | 金額が不正 |
/// | `InvalidChart` | [`codes::INVALID_CHART`] | `CoreError::InvalidChart` と同義（勘定科目表そのものが不正） |
/// | マスタ・設定のロード失敗（9バリアント） | [`codes::INVALID_POLICY_DATA`] | 下表 |
///
/// # `InvalidChart` を `invalid_policy_data` に入れない理由
///
/// [`codes::INVALID_CHART`]（「勘定科目表そのものが不正」）が既にあり、
/// `CoreError::InvalidChart` はそちらに写像されている
/// （`kaikei_app::error::core_error_code`）。そして `JpError::InvalidChart` は
/// `kaikei_core::ChartOfAccounts::new` が返した `CoreError::InvalidChart` を
/// 包み直したものを**含む**（`crates/kaikei-jp/src/chart.rs` の `from_raw` が
/// `CoreError` の `Display` を `reason` に詰める）。
///
/// つまり「勘定科目表の親科目が存在しない」という**同一の条件**が、
/// `kaikei-app` 経由なら `invalid_chart`、`kaikei-jp` 直呼び（§4 の経路 (c)）なら
/// `invalid_policy_data` という**2つのコード**になる。同じ意味に2つの綴りを
/// 作らないという本モジュールの方針（`docs/07-mcp-server.md` §6 /
/// `DECISIONS.md` D-080）にそのまま反するため、`INVALID_CHART` に寄せる。
///
/// # マスタ・設定のロード失敗をまとめる理由
///
/// `YamlParse` / `Io` / `InvalidTaxCategoryTable` / `OverlappingTaxPeriods` /
/// `InvalidTagSchema` / `MissingClosingAccount` /
/// `NotPostableClosingAccount` / `DuplicateClosingAccount` /
/// `ClosingTagSchemaMismatch` は、いずれも**サーバ側の同梱マスタ・起動設定が
/// 不正**であることを示す。呼び出し元（AI）の入力を直しても解消しない点で
/// 同じ分類であり、`PolicyError::InvalidPolicyData`（「policy が構築時に
/// 受け取ったデータが不正」）と意味が一致する。**同じ意味には同じコードを
/// 使う**（`docs/07-mcp-server.md` §6）。
///
/// `InvalidTagSchema` をここに残すのは、`codes` に「タグスキーマそのものが
/// 不正」に相当する語彙が**無い**ため（`unknown_tag_key` /
/// `tag_type_mismatch` はどちらも**入力側**の誤りを指す）。
///
/// なお通常これらはツール応答に現れない。設定・マスタの不備は起動時に
/// 検出して**起動を中止する**（同 §7）ため、ツール呼び出しには到達しない。
///
/// # 網羅 `match`
///
/// `JpError` は `#[non_exhaustive]` では**ない**（意図的。
/// `crates/kaikei-jp/src/error.rs` の doc）。したがってワイルドカードの腕を
/// 置かない。バリアントが増えたらこの関数のコンパイルが壊れ、割り当て漏れが
/// ビルド時に露見する（`kaikei_app::error::AppError::code` と同じ規律）。
pub fn jp_error_code(err: &JpError) -> &'static str {
    match err {
        JpError::Core(inner) => core_error_code(inner),

        // 入力を直せば通る拒否。
        JpError::UnregisteredTagKey { .. } => codes::UNKNOWN_TAG_KEY,
        JpError::InvalidTagValue { .. } => codes::TAG_TYPE_MISMATCH,
        JpError::DuplicateTagKeyInInput { .. } => jp_codes::DUPLICATE_TAG_KEY,
        JpError::NoApplicableTaxRuleSet { .. } => codes::NO_APPLICABLE_RULE_SET,
        JpError::UnknownTaxCategoryCode { .. } => codes::UNKNOWN_TAX_CATEGORY,
        JpError::InvoiceRegNoMissingPrefix { .. } => jp_codes::INVOICE_REG_NO_MISSING_PREFIX,
        JpError::InvoiceRegNoWrongLength { .. } => jp_codes::INVOICE_REG_NO_WRONG_LENGTH,
        JpError::InvoiceRegNoNonDigit { .. } => jp_codes::INVOICE_REG_NO_NON_DIGIT,
        JpError::InvoiceRegNoCheckDigit { .. } => jp_codes::INVOICE_REG_NO_CHECK_DIGIT,
        JpError::InvalidBusinessRatio { .. } => codes::INVALID_VALUE,
        JpError::InvalidHouseholdSplitTotal { .. } => codes::INVALID_AMOUNT,
        JpError::InvalidSettingCode { .. } => jp_codes::INVALID_SETTING_CODE,

        // 勘定科目表そのものが不正。`CoreError::InvalidChart` と同じ条件を
        // 含むため、同じコードにする（別名を作らない。上の doc を参照）。
        JpError::InvalidChart { .. } => codes::INVALID_CHART,

        // サーバ側のマスタ・設定が不正（入力を直しても解消しない）。
        JpError::YamlParse { .. }
        | JpError::Io { .. }
        | JpError::InvalidTaxCategoryTable { .. }
        | JpError::OverlappingTaxPeriods { .. }
        | JpError::InvalidTagSchema { .. }
        | JpError::MissingClosingAccount { .. }
        | JpError::NotPostableClosingAccount { .. }
        | JpError::DuplicateClosingAccount { .. }
        | JpError::ClosingTagSchemaMismatch { .. } => codes::INVALID_POLICY_DATA,
    }
}

/// ツール結果エラー（`isError: true`）の本文。
///
/// `docs/07-mcp-server.md` §3 の失敗時応答の形:
///
/// ```json
/// {
///   "error": "unbalanced",
///   "message": "貸借不一致: 借方 110,000 / 貸方 100,000（差額 10,000）",
///   "debit_total": "110000",
///   "credit_total": "100000",
///   "difference": "10000"
/// }
/// ```
///
/// `error` / `message` 以外の欄（`debit_total` 等）はツールごとに違うので
/// [`ToolError::with_detail`] で足す。
#[derive(Debug, Clone)]
pub struct ToolError {
    code: &'static str,
    message: String,
    details: Map<String, Value>,
}

impl ToolError {
    /// 分類コードと本文から作る。
    ///
    /// 本文は**次の手が分かる文言**にすること（`CLAUDE.md` §11）。
    /// 税務判断を断定しないこと（同 §10）。
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: Map::new(),
        }
    }

    /// [`AppError`] から作る。
    ///
    /// **写像表を書かない。** コードは [`AppError::code`]、本文は
    /// [`AppError::public_message`]（`Display` ではない）を使う。
    /// `Display` は下位層が返した生の文字列（接続文字列・ロール名が
    /// 混じりうる）を含むため、**サーバのログ（stderr）にだけ**出す
    /// （`docs/07-mcp-server.md` §3 / §9）。
    pub fn from_app_error(err: &AppError) -> Self {
        Self::new(err.code(), err.public_message())
    }

    /// [`JpError`] から作る（経路 (c) と、線上の `tags` を `TagSet` にする段）。
    ///
    /// `JpError` の文言は `kaikei-jp` が組み立てたもの（有効なキー一覧・
    /// 有効期間・期待する書式を含む）であり、**言い換えない**
    /// （`CLAUDE.md` §10 / §11）。
    ///
    /// # [`ToolError::from_app_error`] との非対称は意図したもの
    ///
    /// `from_app_error` は `Display` を**使わず** `public_message()` を使う。
    /// `AppError` の `Display` は下位層（sqlx 等）が返した生の文字列を
    /// `RepoError::Backend { reason }` にそのまま抱えており、接続文字列や
    /// ロール名が混じりうるからである
    /// （`tool_error_from_app_error_does_not_leak_the_backend_reason`）。
    ///
    /// 一方こちらは `Display` をそのまま線上に載せる。`JpError` の `Display` は
    /// **`kaikei-jp` が AI 向けに自分で書いた文言**であり、外部クレートの
    /// メッセージを抱え込む口を持たないためである。この性質は口を持つ
    /// バリアント（`#[error(...)]` に `{source}` を書けるもの）を明示リストに
    /// 固定することで保つ（`jp_error_display_is_self_authored`）。
    ///
    /// **サーバ側の情報が線上に出る唯一の例外が `JpError::Io`** で、これは
    /// 読めなかったファイルのパス（`{path}`）を含む。運用者が直すための情報で
    /// あり（`CLAUDE.md` §11）、かつマスタ・設定の不備は起動時に検出して
    /// **起動を中止する**（`docs/07-mcp-server.md` §7）ため、通常ツール応答には
    /// 現れない。この判断は `DECISIONS.md` D-080 に記録してある。
    pub fn from_jp_error(err: &JpError) -> Self {
        Self::new(jp_error_code(err), err.to_string())
    }

    /// ツール固有の欄を足す（`debit_total` / `reversal_id` など）。
    ///
    /// 金額は**文字列**で入れること（`docs/07-mcp-server.md` §5。
    /// 入力だけでなく出力側も number にしない）。
    #[must_use]
    pub fn with_detail(mut self, key: impl Into<String>, value: Value) -> Self {
        self.details.insert(key.into(), value);
        self
    }

    /// 分類コード。`audit_log.error_code` にはこの値だけを入れる（同 §9）。
    pub fn code(&self) -> &'static str {
        self.code
    }

    /// 応答の `message` に載せる本文。
    pub fn message(&self) -> &str {
        &self.message
    }

    /// 応答本文の JSON。
    ///
    /// `error` / `message` は [`ToolError::with_detail`] で上書きできない
    /// （details を先に置いてから固定の2欄を書き込む）。
    pub fn to_json(&self) -> Value {
        let mut object = self.details.clone();
        object.insert("error".to_string(), json!(self.code));
        object.insert("message".to_string(), json!(self.message));
        Value::Object(object)
    }

    /// **ツール結果エラー**（`isError: true`）に変換する。
    ///
    /// `rmcp` の `Err(ErrorData)`（＝JSON-RPC のプロトコルエラー）を返さない
    /// こと（`DECISIONS.md` D-071）。ツールのハンドラは
    /// `Ok(tool_error.into_call_tool_result())` を返す。
    ///
    /// `structured_error` は `structuredContent` と**同じ JSON をテキストとしても**
    /// 載せる。構造化コンテンツをモデルに見せないクライアントでも、本文
    /// （`CLAUDE.md` §11 の「次の手が分かる文言」）が AI に届く。
    pub fn into_call_tool_result(self) -> CallToolResult {
        CallToolResult::structured_error(self.to_json())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_app::error::RepoError;
    use std::collections::BTreeSet;

    /// `crates/kaikei-jp/src/error.rs` のソース。
    ///
    /// [`all_jp_errors`] は**手で維持する一覧**であり、そのままでは
    /// 「新しいバリアントを足したのに一覧に載せ忘れる」を検出できない
    /// （`jp_error_code` の網羅 `match` はコンパイルを壊すが、新バリアントに
    /// `codes::INTERNAL` を割り当ててしまえばコンパイルは通り、
    /// [`every_jp_error_variant_has_a_code`] は一覧に無いので発火しない）。
    /// 「手で維持する一覧は必ず腐る」（`PROGRESS.md` Phase 1 の教訓6 /
    /// `DECISIONS.md` D-047）。
    ///
    /// そこで `crates/kaikei-jp/tests/chart_drift.rs`（D-051）と同じ手法を使い、
    /// **定義元のソースを読んで**バリアント名を抽出し、一覧と突き合わせる。
    /// `include_str!` はコンパイル時に埋め込むので、定義元を変更すれば
    /// このテストは必ず再ビルドされる。
    const JP_ERROR_SOURCE: &str = include_str!("../../kaikei-jp/src/error.rs");

    /// [`all_jp_errors`] に**意図的に含めない**バリアントと、その理由。
    ///
    /// 無言の除外にしない（除外は「検出できない領域」を作る操作なので、
    /// 理由が読めない形で増やせてはいけない）。
    const NOT_CONSTRUCTIBLE_FROM_TESTS: &[(&str, &str)] = &[(
        "YamlParse",
        "source が serde_norway::Error で、外部クレートがコンストラクタを\
         公開していないためテストから値を作れない。jp_error_code では Io と\
         同じ腕に並んでおり、腕から漏れないことは網羅 match が保証する",
    )];

    /// 抽出できるバリアント数の下限（番人）。
    ///
    /// 抽出パターンが `JpError` の書き方の変更に当たらなくなると、
    /// 「0件を突き合わせて全件一致」という形で**検査が黙って機能停止する**。
    /// 実数（現在23件）ではなく下限にするのは、バリアントが1つ増えただけで
    /// このテストが落ちるのを避けるため。
    const MIN_JP_ERROR_VARIANTS: usize = 20;

    /// 自分の `Display` を**他の型に委ねてよい**バリアントと、その理由。
    ///
    /// `ToolError::from_jp_error` は `JpError` の `Display` をそのまま線上に
    /// 載せる（`from_app_error` が `public_message()` を使うのと非対称。
    /// `DECISIONS.md` D-080）。その前提は「`JpError` の文言は `kaikei-jp` が
    /// AI 向けに自分で書いたものであり、下位層の生メッセージを抱えない」で
    /// あり、`{source}` や `#[error(transparent)]` を持つバリアントが黙って
    /// 増えるとこの前提が崩れる。
    const MAY_DELEGATE_DISPLAY: &[(&str, &str)] = &[
        (
            "Io",
            "{source} は std::io::Error。読めなかったパス（{path}）とあわせて、\
             サーバ側の情報が線上に出る唯一の例外（D-080）",
        ),
        (
            "YamlParse",
            "{source} は serde_norway::Error。YAML の解析位置（行・列）が無いと\
             どこを直せばよいか分からない（CLAUDE.md §11）",
        ),
        (
            "Core",
            "#[error(transparent)] で kaikei_core::CoreError に委ねる。\
             CoreError は kaikei-core が AI 向けに書いた文言であり、\
             外部クレートの生メッセージではない",
        ),
    ];

    fn all_jp_errors() -> Vec<JpError> {
        vec![
            JpError::Io {
                path: "tags.yaml".to_string(),
                source: std::io::Error::other("no such file"),
            },
            JpError::InvoiceRegNoMissingPrefix {
                input: "1234567890123".to_string(),
            },
            JpError::InvoiceRegNoWrongLength {
                input: "T123".to_string(),
                actual_len: 3,
            },
            JpError::InvoiceRegNoNonDigit {
                input: "T123456789012A".to_string(),
            },
            JpError::InvoiceRegNoCheckDigit {
                input: "T1234567890123".to_string(),
                expected: 5,
                actual: 1,
            },
            JpError::InvalidTaxCategoryTable {
                label: "jp/2026.yaml".to_string(),
                reason: "rate が不正です".to_string(),
            },
            JpError::OverlappingTaxPeriods {
                first_label: "a".to_string(),
                first_range: "2026-01-01〜".to_string(),
                second_label: "b".to_string(),
                second_range: "2026-06-01〜".to_string(),
            },
            JpError::InvalidChart {
                label: "chart.yaml".to_string(),
                reason: "親科目が存在しません".to_string(),
            },
            JpError::InvalidTagSchema {
                label: "tags.yaml".to_string(),
                reason: "value_type が不正です".to_string(),
            },
            JpError::UnregisteredTagKey {
                key: "tax_cat".to_string(),
                valid: "tax_category, counterparty".to_string(),
            },
            JpError::InvalidTagValue {
                key: "business_ratio".to_string(),
                value_type_label: "小数".to_string(),
                input: "3割".to_string(),
                reason: "0.30 のような小数で指定してください".to_string(),
            },
            JpError::DuplicateTagKeyInInput {
                key: "tax_category".to_string(),
            },
            JpError::NoApplicableTaxRuleSet {
                date: "2030-01-01".to_string(),
                available: "2026-01-01〜2026-12-31".to_string(),
            },
            JpError::UnknownTaxCategoryCode {
                code: "SALES_99".to_string(),
                table_label: "jp/2026.yaml".to_string(),
                applies_from: "2026-01-01".to_string(),
                available: "SALES_10".to_string(),
            },
            JpError::InvalidBusinessRatio {
                ratio: "1.5".to_string(),
            },
            JpError::InvalidHouseholdSplitTotal {
                total: "0".to_string(),
            },
            JpError::Core(kaikei_core::CoreError::EmptyDescription),
            JpError::InvalidSettingCode {
                field: "tax_mode".to_string(),
                input: "zeikomi".to_string(),
                valid: "inclusive, exclusive".to_string(),
            },
            JpError::MissingClosingAccount {
                role: "元入金".to_string(),
                code: "300".to_string(),
            },
            JpError::NotPostableClosingAccount {
                role: "元入金".to_string(),
                code: "300".to_string(),
            },
            JpError::DuplicateClosingAccount {
                role_a: "事業主貸".to_string(),
                role_b: "事業主借".to_string(),
                code: "310".to_string(),
            },
            JpError::ClosingTagSchemaMismatch {
                account_type_label: "収益".to_string(),
                reason: "tax_category が必須です".to_string(),
            },
        ]
        // 除外は [`NOT_CONSTRUCTIBLE_FROM_TESTS`] に理由付きで書く。
        // 載せ忘れは [`all_jp_errors_covers_every_variant_declared_in_kaikei_jp`]
        // がソースと突き合わせて検出する。
    }

    /// `enum <name> {` の本体から、バリアント名と `#[error(...)]` 属性を取り出す。
    ///
    /// 正規表現クレートは使わない（`kaikei-mcp` の依存は CI が許可リストで
    /// 閉じており、テストのために増やす価値が無い。`DECISIONS.md` D-078）。
    /// 対象は自リポジトリ内の既知の書式なので手書きで足りる
    /// （`crates/kaikei-jp/tests/chart_drift.rs` と同じ方針）。
    ///
    /// バリアント名は「行頭（インデント除去後）が大文字で始まり、
    /// `{` / `(` / `,` が続く」もの。構造体フィールドは必ず snake_case なので
    /// 大文字始まりの条件だけで除外できる。
    #[derive(Debug, PartialEq)]
    struct ParsedVariant {
        name: String,
        /// 直前の `#[error(...)]` 属性の中身（属性が無ければ空文字列）。
        error_attribute: String,
    }

    fn parse_enum_variants(source: &str, enum_header: &str) -> Vec<ParsedVariant> {
        let Some(header_at) = source.find(enum_header) else {
            return Vec::new();
        };
        let body = &source[header_at..];

        let mut out: Vec<ParsedVariant> = Vec::new();
        let mut pending_attribute = String::new();
        let mut in_attribute = false;
        let mut depth: i32 = 0;

        for raw_line in body.lines().skip(1) {
            let line = raw_line.trim();

            if in_attribute || line.starts_with("#[error(") {
                pending_attribute.push_str(line);
                // 属性は `)]` で終わる。rustfmt は長い属性を複数行に折り返す。
                in_attribute = !line.ends_with(")]");
                continue;
            }
            // 空行・コメント・その他の属性は、直前の `#[error(...)]` を
            // 破棄せずに読み飛ばす。
            if line.is_empty() || line.starts_with("//") || line.starts_with("#[") {
                continue;
            }

            match variant_name(line).filter(|_| depth == 0) {
                Some(name) => out.push(ParsedVariant {
                    name,
                    error_attribute: std::mem::take(&mut pending_attribute),
                }),
                None => pending_attribute.clear(),
            }

            depth += line.matches('{').count() as i32;
            depth -= line.matches('}').count() as i32;
            if depth < 0 {
                break; // enum の閉じ括弧
            }
        }

        out
    }

    /// バリアント宣言の行なら、その名前を返す。
    ///
    /// 構造体フィールド（`label: String,`）は必ず snake_case なので、
    /// 「大文字始まり」の条件だけで除外できる。
    fn variant_name(line: &str) -> Option<String> {
        if !line.starts_with(|c: char| c.is_ascii_uppercase()) {
            return None;
        }
        let ident_len = line
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(line.len());
        // `YamlParse {` のように名前と `{` の間に空白が入る書き方に備える。
        let rest = line[ident_len..].trim_start();
        if rest.is_empty() || rest.starts_with(['{', '(', ',']) {
            Some(line[..ident_len].to_string())
        } else {
            None
        }
    }

    fn jp_error_variants() -> Vec<ParsedVariant> {
        parse_enum_variants(JP_ERROR_SOURCE, "pub enum JpError {")
    }

    /// `Debug` の先頭トークンからバリアント名を取る。
    ///
    /// バリアント名を**値から導出する**ことで、[`all_jp_errors`] の要素と
    /// 名前の対応を手で書かずに済ませる。
    fn debug_variant_name(err: &JpError) -> String {
        let rendered = format!("{err:?}");
        let end = rendered
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(rendered.len());
        rendered[..end].to_string()
    }

    // `all_jp_errors()` が `JpError` の全バリアントを覆っていること。
    //
    // 手で維持する一覧の陳腐化を、定義元のソースと突き合わせて構造的に閉じる
    // （`PROGRESS.md` Phase 1 教訓6 / D-051 と同じ手法）。
    #[test]
    fn all_jp_errors_covers_every_variant_declared_in_kaikei_jp() {
        let declared: BTreeSet<String> = jp_error_variants().into_iter().map(|v| v.name).collect();

        assert!(
            declared.len() >= MIN_JP_ERROR_VARIANTS,
            "crates/kaikei-jp/src/error.rs から抽出できたバリアントが {} 件しかありません\
             （下限 {MIN_JP_ERROR_VARIANTS}）。JpError の書き方が変わって抽出ロジック\
             （parse_enum_variants）が当たらなくなった可能性があります。\
             このまま通すと「0件を突き合わせて全件一致」という形で検査が無言で\
             機能停止します。抽出できたもの: {declared:?}",
            declared.len()
        );

        let covered: BTreeSet<String> = all_jp_errors().iter().map(debug_variant_name).collect();
        let excluded: BTreeSet<String> = NOT_CONSTRUCTIBLE_FROM_TESTS
            .iter()
            .map(|(name, _)| (*name).to_string())
            .collect();

        let missing: Vec<&String> = declared
            .iter()
            .filter(|name| !covered.contains(*name) && !excluded.contains(*name))
            .collect();
        assert!(
            missing.is_empty(),
            "JpError に増えたバリアントが all_jp_errors() に載っていません: {missing:?}\n\
             every_jp_error_variant_has_a_code は一覧に載っているものしか見ないため、\
             このままだと新しいバリアントに codes::INTERNAL を割り当てても検査が発火しません。\n\
             値を作れない場合は NOT_CONSTRUCTIBLE_FROM_TESTS に理由付きで追加してください。"
        );

        let stale: Vec<&String> = covered
            .union(&excluded)
            .filter(|name| !declared.contains(*name))
            .collect();
        assert!(
            stale.is_empty(),
            "all_jp_errors() / NOT_CONSTRUCTIBLE_FROM_TESTS にあるのに \
             crates/kaikei-jp/src/error.rs に見当たらないバリアントがあります: {stale:?}\n\
             改名された（追随してください）か、抽出ロジックが一部を取りこぼしています。"
        );
    }

    // `JpError` の `Display` は `kaikei-jp` が自分で書いた文言である。
    //
    // `ToolError::from_jp_error` はこれをそのまま線上に載せる（`from_app_error`
    // が `public_message()` を使うのと非対称。`DECISIONS.md` D-080）。
    // 前提が崩れる形——外部クレートの `Display` を `{source}` で埋め込む、
    // あるいは `#[error(transparent)]` で丸ごと委ねる——が黙って増えないように、
    // 委譲してよいバリアントを理由付きの明示リストに固定する。
    #[test]
    fn jp_error_display_is_self_authored() {
        let delegating: BTreeSet<String> = jp_error_variants()
            .into_iter()
            .filter(|v| {
                v.error_attribute.contains("{source}") || v.error_attribute.contains("transparent")
            })
            .map(|v| v.name)
            .collect();

        assert!(
            !delegating.is_empty(),
            "Display を委譲しているバリアントを1つも検出できませんでした。\
             #[error(...)] 属性の抽出が働いていません（少なくとも Io は該当するはずです）"
        );

        let allowed: BTreeSet<String> = MAY_DELEGATE_DISPLAY
            .iter()
            .map(|(name, _)| (*name).to_string())
            .collect();

        assert_eq!(
            delegating, allowed,
            "JpError の Display を他の型に委ねているバリアントの集合が\
             MAY_DELEGATE_DISPLAY と一致しません。\n\
             ToolError::from_jp_error は JpError の Display をそのまま線上に載せます。\
             外部クレートのメッセージを線上に出してよいかを判断し、\
             よいなら MAY_DELEGATE_DISPLAY に理由付きで追加してください（D-080）。"
        );
    }

    // 上2つが依存する抽出ロジックが、既知の入力で正しく動くこと。
    //
    // 「一部だけ黙って脱落する」が最も発見しにくい失敗モードなので、
    // 折返し・行末コメント・構造体フィールドを含む形で確認する。
    #[test]
    fn enum_variant_extractor_reads_names_and_error_attributes() {
        let source = r#"
/// doc
#[derive(Debug, thiserror::Error)]
pub enum JpError {
    /// doc comment
    #[error("{label} のYAML解析に失敗しました: {source}")]
    YamlParse {
        label: String,
        #[source]
        source: serde_norway::Error,
    },

    #[error(
        "折返した属性です: \"{input}\""
    )]
    WrappedAttribute {
        input: String,
    },

    #[error(transparent)]
    Core(#[from] kaikei_core::CoreError),
}
"#;
        let parsed = parse_enum_variants(source, "pub enum JpError {");
        let names: Vec<&str> = parsed.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, vec!["YamlParse", "WrappedAttribute", "Core"]);
        assert!(parsed[0].error_attribute.contains("{source}"));
        assert!(!parsed[1].error_attribute.contains("{source}"));
        assert!(parsed[2].error_attribute.contains("transparent"));
    }

    // 定義元が見つからないとき、無言で「0件一致」にせず落ちること。
    #[test]
    #[should_panic(expected = "下限")]
    fn all_jp_errors_check_fails_loudly_when_nothing_can_be_extracted() {
        let declared: BTreeSet<String> = parse_enum_variants(JP_ERROR_SOURCE, "pub enum Renamed {")
            .into_iter()
            .map(|v| v.name)
            .collect();
        assert!(
            declared.len() >= MIN_JP_ERROR_VARIANTS,
            "抽出できたバリアントが {} 件しかありません（下限 {MIN_JP_ERROR_VARIANTS}）",
            declared.len()
        );
    }

    // 全バリアントからコードが引け、受け皿（`internal`）に落ちない。
    #[test]
    fn every_jp_error_variant_has_a_code() {
        for err in all_jp_errors() {
            let code = jp_error_code(&err);
            assert_ne!(code, codes::INTERNAL, "受け皿に落ちています: {err:?}");
            assert!(!code.is_empty());
            assert!(
                code.chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit()),
                "snake_case ではありません: {code}"
            );
        }
    }

    // `Core` は中身の `CoreError` へ委譲する（`AppError::Core` と同じ）。
    #[test]
    fn jp_error_core_delegates_to_the_core_error_code() {
        let err = JpError::Core(kaikei_core::CoreError::EmptyDescription);
        assert_eq!(jp_error_code(&err), codes::EMPTY_DESCRIPTION);
    }

    // 「入力を直せば通る」拒否を `internal` や `invalid_policy_data` に潰さない
    // （`DECISIONS.md` D-074 訂正注記4 / `docs/07-mcp-server.md` §6）。
    #[test]
    fn tag_input_errors_are_not_collapsed_into_server_side_codes() {
        let input_errors = [
            JpError::UnregisteredTagKey {
                key: "tax_cat".to_string(),
                valid: "tax_category".to_string(),
            },
            JpError::InvalidTagValue {
                key: "business_ratio".to_string(),
                value_type_label: "小数".to_string(),
                input: "3割".to_string(),
                reason: "0.30 のような小数で指定してください".to_string(),
            },
            JpError::DuplicateTagKeyInInput {
                key: "tax_category".to_string(),
            },
            JpError::NoApplicableTaxRuleSet {
                date: "2030-01-01".to_string(),
                available: "2026-01-01〜2026-12-31".to_string(),
            },
        ];
        for err in input_errors {
            let code = jp_error_code(&err);
            assert_ne!(code, codes::INVALID_POLICY_DATA, "{err:?}");
            assert_ne!(code, codes::INTERNAL, "{err:?}");
        }
    }

    // 既に語彙がある概念に別名を作っていない（`docs/07-mcp-server.md` §6）。
    #[test]
    fn existing_vocabulary_is_reused_instead_of_new_aliases() {
        assert_eq!(
            jp_error_code(&JpError::UnregisteredTagKey {
                key: "tax_cat".to_string(),
                valid: String::new(),
            }),
            codes::UNKNOWN_TAG_KEY
        );
        assert_eq!(
            jp_error_code(&JpError::UnknownTaxCategoryCode {
                code: "SALES_99".to_string(),
                table_label: String::new(),
                applies_from: String::new(),
                available: String::new(),
            }),
            codes::UNKNOWN_TAX_CATEGORY
        );
        assert_eq!(
            jp_error_code(&JpError::NoApplicableTaxRuleSet {
                date: String::new(),
                available: String::new(),
            }),
            codes::NO_APPLICABLE_RULE_SET
        );
    }

    // 同一条件が2つのコードにならない（`docs/07-mcp-server.md` §6 / D-080）。
    //
    // `JpError::InvalidChart` は `kaikei_core::ChartOfAccounts::new` が返した
    // `CoreError::InvalidChart` を包んだものを含む。同じ「勘定科目表が不正」を
    // 経路によって別コードで返すと、AI は分岐を2つ覚える必要が出る。
    //
    // 定数どうし（`codes::INVALID_CHART` と `codes::INVALID_CHART`）を比べても
    // それは自明に等しいだけで何も固定できない。**両方の写像関数を実際に通し、
    // その結果が一致すること**を見る。将来 `kaikei-app` 側が
    // `CoreError::InvalidChart` の写像を変えたら、ここが落ちる。
    #[test]
    fn jp_and_core_invalid_chart_resolve_to_the_same_code() {
        let via_jp = jp_error_code(&JpError::InvalidChart {
            label: "chart.yaml".to_string(),
            reason: "親科目が存在しません".to_string(),
        });
        let via_core = core_error_code(&kaikei_core::CoreError::InvalidChart {
            reason: "親科目が存在しません".to_string(),
        });
        assert_eq!(
            via_jp, via_core,
            "同じ「勘定科目表が不正」が経路によって別コードになっています\
             （JpError 経由 = {via_jp} / CoreError 経由 = {via_core}）。\n\
             既に語彙がある概念に別名を作らない（docs/07-mcp-server.md §6 / D-080）。\
             どちらかの写像を変えたなら、もう一方も合わせてください。"
        );
    }

    // 登録番号の4つの観点は別コードになる（D-053 の検証順が応答から読める）。
    #[test]
    fn the_four_invoice_checks_have_distinct_codes() {
        let mut invoice_codes = vec![
            jp_codes::INVOICE_REG_NO_MISSING_PREFIX,
            jp_codes::INVOICE_REG_NO_WRONG_LENGTH,
            jp_codes::INVOICE_REG_NO_NON_DIGIT,
            jp_codes::INVOICE_REG_NO_CHECK_DIGIT,
        ];
        invoice_codes.sort_unstable();
        let total = invoice_codes.len();
        invoice_codes.dedup();
        assert_eq!(invoice_codes.len(), total);
    }

    // `AppError` の本文は `public_message()` を使う（`Display` を転記しない）。
    #[test]
    fn tool_error_from_app_error_does_not_leak_the_backend_reason() {
        const SECRET: &str = "postgres://kaikei_app:s3cret@db.internal:5432/kaikei";
        let err = AppError::Repo(RepoError::Backend {
            reason: format!("未分類のデータベースエラーです（SQLSTATE 08006）: {SECRET}"),
        });
        let tool_error = ToolError::from_app_error(&err);
        assert_eq!(tool_error.code(), codes::BACKEND);
        assert!(!tool_error.message().contains(SECRET));
        assert!(!tool_error.to_json().to_string().contains("s3cret"));
        // 診断用の `Display` には残っている（stderr 向け）。
        assert!(err.to_string().contains(SECRET));
    }

    // 応答本文は `error` / `message` を必ず持ち、追加欄と混ざらない。
    #[test]
    fn tool_error_json_always_carries_error_and_message() {
        let body = ToolError::new(codes::UNBALANCED, "貸借不一致: 借方 110,000 / 貸方 100,000")
            .with_detail("debit_total", json!("110000"))
            .with_detail("credit_total", json!("100000"))
            .to_json();
        assert_eq!(body["error"], json!(codes::UNBALANCED));
        assert!(body["message"].as_str().unwrap().contains("貸借不一致"));
        assert_eq!(body["debit_total"], json!("110000"));
        // 金額は文字列（number にしない。§5）。
        assert!(body["debit_total"].is_string());
    }

    // 追加欄で `error` / `message` を上書きできない。
    #[test]
    fn details_cannot_overwrite_the_error_code() {
        let body = ToolError::new(codes::REJECTED, "拒否しました")
            .with_detail("error", json!("something_else"))
            .to_json();
        assert_eq!(body["error"], json!(codes::REJECTED));
    }

    /// テスト用の代表的なツール結果エラー（`docs/07-mcp-server.md` §3 の形）。
    fn sample_tool_result() -> CallToolResult {
        ToolError::new(
            codes::UNBALANCED,
            "貸借不一致: 借方 110,000 / 貸方 100,000（差額 10,000）。\
             仮受消費税の計上漏れの可能性があります。",
        )
        .with_detail("debit_total", json!("110000"))
        .with_detail("credit_total", json!("100000"))
        .with_detail("difference", json!("10000"))
        .into_call_tool_result()
    }

    // ドメインのエラーは **`isError: true`** のツール結果で返る（D-071 / D-080）。
    //
    // `rmcp` の同じ impl には `structured`（`is_error: Some(false)`）が隣接して
    // おり、6文字違いで取り違えられる。取り違えると**全てのドメインエラーが
    // 「成功」として AI に届く**——AI は次の手（`CLAUDE.md` §11）を読まずに
    // 処理を続け、記帳できていないのに完了したと報告する。
    // 本 PR の中心的な主張なので、値を直接検査する。
    #[test]
    fn tool_error_becomes_a_tool_result_error() {
        let result = sample_tool_result();

        assert_eq!(
            result.is_error,
            Some(true),
            "isError が true ではありません。CallToolResult::structured（成功用）と\
             structured_error（エラー用）を取り違えていないか確認してください（D-080）"
        );
    }

    // 構造化コンテンツに応答本文がそのまま載る。
    #[test]
    fn tool_result_error_carries_the_body_as_structured_content() {
        let result = sample_tool_result();

        let structured = result.structured_content.as_ref().expect(
            "structuredContent が空です。構造化コンテンツを読むクライアントが、\
                 error / message を機械的に取り出せなくなります（D-080）",
        );

        assert_eq!(structured["error"], json!(codes::UNBALANCED));
        assert!(structured["message"]
            .as_str()
            .expect("message が文字列ではありません")
            .contains("貸借不一致"));
        // ツール固有の欄も落ちない（金額は文字列。`docs/07-mcp-server.md` §5）。
        assert_eq!(structured["difference"], json!("10000"));
    }

    // **`content` のテキストにも本文が載る。**
    //
    // 「`structured` ではなく `structured_error` を使う」の根拠の半分は
    // `isError` だが、もう半分は「`structuredContent` をモデルに見せない
    // クライアントでも本文が AI に届く」ことである（D-080 の却下表）。
    // ここが崩れると `CallToolResult::error`（テキストのみ）や
    // 構造化のみの応答との差が消え、主張が空文になる。
    #[test]
    fn tool_result_error_repeats_the_body_in_the_text_content() {
        let wire = serde_json::to_value(sample_tool_result())
            .expect("CallToolResult は JSON にできるはず");

        // 線上の綴りも固定する（`isError` / `structuredContent` は camelCase）。
        assert_eq!(wire["isError"], json!(true));
        assert!(wire["structuredContent"].is_object());

        let text = wire["content"][0]["text"]
            .as_str()
            .expect("content にテキストブロックがありません");

        assert!(
            text.contains("貸借不一致") && text.contains("仮受消費税"),
            "content のテキストに本文が載っていません: {text}"
        );
        assert!(
            text.contains(codes::UNBALANCED),
            "content のテキストに分類コードが載っていません: {text}"
        );
    }
}
