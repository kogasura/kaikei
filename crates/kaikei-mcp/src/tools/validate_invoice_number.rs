//! `validate_invoice_number` — 適格請求書発行事業者登録番号の**形式**検証
//! （`docs/07-mcp-server.md` §2 / §10 MC-28）。
//!
//! # 写像するだけのツールである
//!
//! 検証の実体は [`kaikei_jp::invoice::InvoiceRegistrationNo::parse`] にあり、
//! この層は入力を渡して結果を線上の JSON に詰め替えるだけである。
//! 桁数・文字種・チェックデジットの判定をここに書き写さない（D-072）。
//!
//! | 規律 | どこにあるか |
//! |---|---|
//! | 前後の空白をトリムしない（貼り付け由来の空白混入を検出する） | `parse`（`DECISIONS.md` D-052） |
//! | 検証順は 先頭文字 → 桁数 → 文字種 → チェックデジット で固定。**最初に失敗した観点だけ**を返す | `parse`（D-053） |
//! | 4つの観点を別々の分類コードにする | `crate::error::jp_error_code`（D-053 / D-080） |
//!
//! # ★実在確認はしない★（`CLAUDE.md` §10）
//!
//! この番号が国税庁に実在登録されているか、その事業者が適格請求書発行事業者
//! として有効かは**判定しない**（`kaikei-jp` の型 doc も同じことを書いている）。
//! 応答では「何を確認したか」と「何を確認していないか」を必ず並べて返す。
//! 「形式が正しい」を「有効な登録番号である」と読み替えられる文言にしない。

use kaikei_jp::invoice::InvoiceRegistrationNo;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::dispatch::{McpTool, ToolContext, ToolFailure, ToolSuccess};
use crate::error::ToolError;

/// `validate_invoice_number`。
pub struct ValidateInvoiceNumber;

// ★この構造体の doc コメントは `tools/list` の応答に出る★
// 内部設計書への参照・crate 名・Markdown の強調記法を書かないこと。
/// 検証する登録番号。指定していないキーは受け付けません。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValidateInvoiceNumberInput {
    /// 適格請求書発行事業者の登録番号（先頭が T の14文字。例: "T7123456789012"）。
    /// 前後の空白は取り除かずにそのまま検証します（貼り付け時に混ざった
    /// 空白をここで見つけられるようにするためです）。
    pub registration_number: String,
}

impl McpTool for ValidateInvoiceNumber {
    type Input = ValidateInvoiceNumberInput;

    const NAME: &'static str = "validate_invoice_number";

    const DESCRIPTION: &'static str = "\
適格請求書発行事業者の登録番号の形式だけを検証します。\
確認するのは、先頭が T であること・続く13桁が半角数字であること・\
チェックデジットが一致することの3点だけです。\
その番号が実際に登録されているか、その事業者が適格請求書発行事業者として\
有効かどうかは確認しません（このサーバーは外部に問い合わせません）。\
前後の空白は取り除かずに検証するため、空白が混ざっている場合はエラーになります。\
形式が正しくない場合は、最初に不一致となった観点だけをエラーとして返します。";

    async fn run(_ctx: &ToolContext<'_>, input: Self::Input) -> Result<ToolSuccess, ToolFailure> {
        // 帳簿にも DB にも触らない（`docs/07-mcp-server.md` §4 の経路 (c)）。
        let parsed = InvoiceRegistrationNo::parse(&input.registration_number)
            .map_err(|error| ToolError::from_jp_error(&error))?;

        Ok(ToolSuccess::new(success_body(&parsed)))
    }
}

/// このツールが**確認したこと**。
const CHECKED: &[&str] = &[
    "先頭が T であること",
    "T に続く部分が13文字であること",
    "13文字がすべて半角数字であること",
    "先頭1桁の検査用数字が残り12桁から計算した値と一致すること",
];

/// このツールが**確認していないこと**。
///
/// 省略すると「形式が正しい」が「有効な登録番号である」と読み替えられる。
/// `CLAUDE.md` §10 の「税務判断を断定しない」はこの並記で担保する。
const NOT_CHECKED: &[&str] = &[
    "その番号が国税庁に実在登録されているかどうか",
    "登録されている事業者が現時点で適格請求書発行事業者として有効かどうか",
    "その事業者名・所在地がこの取引の相手方と一致するかどうか",
];

/// 成功応答の本文。
fn success_body(parsed: &InvoiceRegistrationNo) -> Map<String, Value> {
    let mut body = Map::new();
    // ★`valid` という名前にしない★ 「登録番号として有効」と読める。
    // 確認したのは形式だけである。
    body.insert("format_valid".to_string(), json!(true));
    body.insert("registration_number".to_string(), json!(parsed.as_str()));
    body.insert(
        "corporate_number".to_string(),
        json!(parsed.corporate_number()),
    );
    body.insert("checked".to_string(), json!(CHECKED));
    body.insert("not_checked".to_string(), json!(NOT_CHECKED));
    body.insert(
        "message".to_string(),
        json!(
            "形式（先頭の T・桁数・文字種・チェックデジット）は確認しました。\
             この番号が実在するか、適格請求書発行事業者として有効かどうかは\
             確認していません。取引先が適格請求書発行事業者かどうかの判断は\
             このサーバーでは行いません"
        ),
    );
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::jp_codes;

    // 基礎番号 123456789012 の検査用数字は 7（`kaikei-jp` の invoice.rs の
    // 手計算例と同じ値）。
    const VALID: &str = "T7123456789012";

    fn validate(input: &str) -> Result<Value, ToolError> {
        InvoiceRegistrationNo::parse(input)
            .map(|parsed| Value::Object(success_body(&parsed)))
            .map_err(|error| ToolError::from_jp_error(&error))
    }

    // MC-28: 形式検証の結果だけを述べ、**実在すると断定しない**。
    #[test]
    fn a_well_formed_number_reports_the_format_only_without_claiming_it_exists() {
        let body = validate(VALID).expect("形式は正しい");

        assert_eq!(body["format_valid"], json!(true));
        assert_eq!(body["registration_number"], json!(VALID));
        assert_eq!(body["corporate_number"], json!("7123456789012"));

        // 何を確認していないかが必ず並ぶ。
        let not_checked = body["not_checked"].as_array().expect("配列");
        assert!(!not_checked.is_empty());
        let joined = format!("{body}");
        assert!(joined.contains("実在"), "{joined}");

        // 断定に読める語を含まない。
        for forbidden in [
            "実在します",
            "有効な登録番号です",
            "適格請求書発行事業者です",
        ] {
            assert!(!joined.contains(forbidden), "{forbidden}: {joined}");
        }
        // 「有効」と読める短いキー名を使っていない（format_valid にしてある）。
        assert!(body.get("valid").is_none(), "{body}");
    }

    // D-053: 最初に失敗した観点だけが返り、観点ごとに分類コードが違う。
    #[test]
    fn each_failed_check_has_its_own_error_code() {
        let cases = [
            ("1234567890123", jp_codes::INVOICE_REG_NO_MISSING_PREFIX),
            ("T712345678901", jp_codes::INVOICE_REG_NO_WRONG_LENGTH),
            ("T71234567890-2", jp_codes::INVOICE_REG_NO_NON_DIGIT),
            ("T8123456789012", jp_codes::INVOICE_REG_NO_CHECK_DIGIT),
        ];
        for (input, expected) in cases {
            let error = validate(input).map(|_| ()).expect_err(input);
            assert_eq!(error.code(), expected, "{input}");
            // 文言は `kaikei-jp` が書いたものを言い換えない（入力を含む）。
            assert!(error.message().contains(input), "{}", error.message());
        }
    }

    // D-052: 前後の空白はトリムしない（混入をここで見つけられるようにする）。
    #[test]
    fn surrounding_whitespace_is_not_trimmed_away() {
        for input in [" T7123456789012", "T7123456789012 "] {
            assert!(validate(input).is_err(), "{input} が受理されました");
        }
    }

    // 説明文が `CLAUDE.md` §10 の禁止表現を含まず、実在確認をしないと明示する。
    #[test]
    fn the_description_avoids_forbidden_claims_and_says_what_is_not_checked() {
        let description = ValidateInvoiceNumber::DESCRIPTION;
        for forbidden in ["準拠", "法令対応", "JIIMA"] {
            assert!(!description.contains(forbidden), "{forbidden}");
        }
        assert!(description.contains("確認しません"));
    }
}
