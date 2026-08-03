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
/// | マスタ・設定のロード失敗（10バリアント） | [`codes::INVALID_POLICY_DATA`] | 下表 |
///
/// # マスタ・設定のロード失敗をまとめる理由
///
/// `YamlParse` / `Io` / `InvalidTaxCategoryTable` / `OverlappingTaxPeriods` /
/// `InvalidChart` / `InvalidTagSchema` / `MissingClosingAccount` /
/// `NotPostableClosingAccount` / `DuplicateClosingAccount` /
/// `ClosingTagSchemaMismatch` は、いずれも**サーバ側の同梱マスタ・起動設定が
/// 不正**であることを示す。呼び出し元（AI）の入力を直しても解消しない点で
/// 同じ分類であり、`PolicyError::InvalidPolicyData`（「policy が構築時に
/// 受け取ったデータが不正」）と意味が一致する。**同じ意味には同じコードを
/// 使う**（`docs/07-mcp-server.md` §6）。
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

        // サーバ側のマスタ・設定が不正（入力を直しても解消しない）。
        JpError::YamlParse { .. }
        | JpError::Io { .. }
        | JpError::InvalidTaxCategoryTable { .. }
        | JpError::OverlappingTaxPeriods { .. }
        | JpError::InvalidChart { .. }
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
        // `YamlParse` は `serde_norway::Error` を外から構築できないため
        // ここには含めない（`Io` と同じ腕に落ちることは match が保証する）。
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
}
