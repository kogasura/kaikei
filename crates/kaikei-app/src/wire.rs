//! ★契約凍結点★ **線上語彙**（列挙型 ⇄ 機械可読名）の唯一の置き場。
//!
//! [`crate::error::codes`] がエラーの分類コードを1箇所に集めたのと同じ理由で、
//! **応答の JSON に現れる列挙型の文字列**もここに1箇所だけ持つ
//! （`DECISIONS.md` D-072 の範囲）。下流（`kaikei-mcp` / 将来の `kaikei-api` /
//! `audit_log.output` を組み立てる層）が同じ対応表を手書きすると、3箇所で
//! 綴りがずれる。
//!
//! | 型 | 値 → 文字列 | 文字列 → 値 |
//! |---|---|---|
//! | [`kaikei_core::AccountType`] | [`account_type_code`] | [`account_type_from_code`] |
//! | [`kaikei_core::Side`] | [`side_code`] | [`side_from_code`] |
//! | [`kaikei_policy::NoteSeverity`] | [`note_severity_code`] | （無し。下記） |
//! | [`crate::context::FiscalYearRule`] | [`fiscal_year_rule_code`] | [`fiscal_year_rule_from_code`] |
//!
//! `AccountType` / `Side` / `NoteSeverity` が**メソッドではなく自由関数**なのは、
//! 定義元の crate（`kaikei-core` / `kaikei-policy`）が凍結層であり
//! （`CLAUDE.md` §1）、他 crate から `impl` を生やせないため。
//! [`crate::error::core_error_code`] / [`crate::error::policy_error_code`] と
//! 同じ形である。
//!
//! # `NoteSeverity` に逆変換が無い理由
//!
//! `PolicyNote` は `kaikei-policy` が組み立てて上位へ流れる**出力専用**の値で、
//! 文字列から `NoteSeverity` を復元する入力経路が存在しない
//! （MCP のツール入力にも `severity` は現れない）。使われない逆変換を
//! 先取りで置かない（YAGNI）。必要になったら足すのは非破壊的変更である。
//!
//! # 語彙の出所
//!
//! - `NoteSeverity` の `"info"` / `"warning"` は `docs/07-mcp-server.md` §3 が
//!   値まで定めている。
//! - `Side` の `"debit"` / `"credit"` は同 §3 の `post_journal_entry` の入力例。
//! - `AccountType` は `docs/07-mcp-server.md` §2 の `list_accounts` が返す
//!   `account_type`。**`kaikei-jp` の YAML（`chart/*.yaml` の `type`、
//!   `tags.yaml` の `required_for`）が使う `Asset` / `Expense` とは
//!   別の語彙である。** YAML は人間が編集する設定ファイルの語彙、ここは
//!   線上の語彙で、`codes` と同じ snake_case に揃える。片方を変えても
//!   もう片方は変わらない（意図的に独立させている）。
//!
//! # 通貨と税制の語彙はここに無い
//!
//! - 通貨コード → [`kaikei_core::Currency`] は [`crate::currency::currency_from_code`]
//!   （小数桁数の解決を伴うため独立したモジュールに置いている）。
//! - `tax_mode` / `rounding` / `rounding_unit` は `kaikei-jp` 側にある
//!   （`kaikei_jp::tax::TaxMode::as_code` 等）。`kaikei-app` は `kaikei-jp` に
//!   依存できない（`CLAUDE.md` §1・CI が検査）ため、ここには置けない。

use crate::context::FiscalYearRule;
use kaikei_core::{AccountType, CoreError, Side};
use kaikei_policy::NoteSeverity;

/// 勘定科目の5要素分類の機械可読名。
///
/// `kaikei_core::AccountType::label_ja`（「資産」等）は人間向けの表示であり、
/// 応答の分岐に使う識別子としては使わない（翻訳の対象になるため）。
pub fn account_type_code(account_type: AccountType) -> &'static str {
    match account_type {
        AccountType::Asset => "asset",
        AccountType::Liability => "liability",
        AccountType::Equity => "equity",
        AccountType::Revenue => "revenue",
        AccountType::Expense => "expense",
    }
}

/// 機械可読名から [`AccountType`] を解決する。
///
/// # Errors
///
/// 未知の値は `CoreError::InvalidValue`（有効な値を列挙する。`CLAUDE.md` §11）。
pub fn account_type_from_code(code: &str) -> Result<AccountType, CoreError> {
    match code {
        "asset" => Ok(AccountType::Asset),
        "liability" => Ok(AccountType::Liability),
        "equity" => Ok(AccountType::Equity),
        "revenue" => Ok(AccountType::Revenue),
        "expense" => Ok(AccountType::Expense),
        other => Err(invalid_value("account_type", other, ACCOUNT_TYPE_CODES)),
    }
}

/// [`account_type_code`] が返しうる値の一覧（エラーメッセージ・スキーマ生成用）。
pub const ACCOUNT_TYPE_CODES: &[&str] = &["asset", "liability", "equity", "revenue", "expense"];

/// 借方・貸方の機械可読名。
pub fn side_code(side: Side) -> &'static str {
    match side {
        Side::Debit => "debit",
        Side::Credit => "credit",
    }
}

/// 機械可読名から [`Side`] を解決する。
///
/// # Errors
///
/// 未知の値は `CoreError::InvalidValue`。
pub fn side_from_code(code: &str) -> Result<Side, CoreError> {
    match code {
        "debit" => Ok(Side::Debit),
        "credit" => Ok(Side::Credit),
        other => Err(invalid_value("side", other, SIDE_CODES)),
    }
}

/// [`side_code`] が返しうる値の一覧。
pub const SIDE_CODES: &[&str] = &["debit", "credit"];

/// 注記の重要度の機械可読名（`docs/07-mcp-server.md` §3 が定める値）。
pub fn note_severity_code(severity: NoteSeverity) -> &'static str {
    match severity {
        NoteSeverity::Info => "info",
        NoteSeverity::Warning => "warning",
    }
}

/// [`note_severity_code`] が返しうる値の一覧。
pub const NOTE_SEVERITY_CODES: &[&str] = &["info", "warning"];

/// 会計年度の区切り規則の機械可読名。
pub fn fiscal_year_rule_code(rule: FiscalYearRule) -> &'static str {
    match rule {
        FiscalYearRule::CalendarYear => "calendar_year",
    }
}

/// 機械可読名から [`FiscalYearRule`] を解決する。
///
/// 設定ファイルから `BookSettings::fiscal_year_rule` を組み立てる合成ルート
/// （`kaikei-mcp` の `config.rs`）が使う。同じ構造体の `book_currency` に
/// [`crate::currency::currency_from_code`] があるのに、こちらに解決手段が
/// 無いという非対称を解消するために置いている。
///
/// # Errors
///
/// 未知の値は `CoreError::InvalidValue`。**既定値へフォールバックしない**
/// （`BookSettings` が `Default` を実装していないのと同じ理由。`DECISIONS.md` D-074）。
pub fn fiscal_year_rule_from_code(code: &str) -> Result<FiscalYearRule, CoreError> {
    match code {
        "calendar_year" => Ok(FiscalYearRule::CalendarYear),
        other => Err(invalid_value(
            "fiscal_year_rule",
            other,
            FISCAL_YEAR_RULE_CODES,
        )),
    }
}

/// [`fiscal_year_rule_code`] が返しうる値の一覧。
pub const FISCAL_YEAR_RULE_CODES: &[&str] = &["calendar_year"];

/// 「未知の機械可読名」に対する共通のエラー文言。
///
/// 有効な値を必ず列挙する（`CLAUDE.md` §11「次の手が分かる文言」）。
fn invalid_value(field: &str, given: &str, valid: &[&str]) -> CoreError {
    CoreError::InvalidValue {
        reason: format!(
            "{field} の値が不正です: \"{given}\"（有効な値: {}）",
            valid.join(", ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // WV-1: AccountType の全5値が異なるコードを持ち、往復する。
    #[test]
    fn account_type_codes_are_distinct_and_round_trip() {
        let all = [
            AccountType::Asset,
            AccountType::Liability,
            AccountType::Equity,
            AccountType::Revenue,
            AccountType::Expense,
        ];
        let mut codes: Vec<&str> = all.iter().copied().map(account_type_code).collect();
        assert_eq!(
            codes,
            ACCOUNT_TYPE_CODES.to_vec(),
            "一覧定数と食い違っている"
        );
        codes.sort_unstable();
        let distinct = codes.len();
        codes.dedup();
        assert_eq!(codes.len(), distinct);

        for account_type in all {
            assert_eq!(
                account_type_from_code(account_type_code(account_type)).unwrap(),
                account_type
            );
        }
    }

    // WV-2: Side の往復。
    #[test]
    fn side_codes_round_trip() {
        for side in [Side::Debit, Side::Credit] {
            assert_eq!(side_from_code(side_code(side)).unwrap(), side);
        }
        assert_eq!(side_code(Side::Debit), "debit");
        assert_eq!(side_code(Side::Credit), "credit");
    }

    // WV-3: NoteSeverity は docs/07 §3 が定める値そのもの。
    #[test]
    fn note_severity_codes_match_the_documented_values() {
        assert_eq!(note_severity_code(NoteSeverity::Info), "info");
        assert_eq!(note_severity_code(NoteSeverity::Warning), "warning");
        assert_eq!(
            [NoteSeverity::Info, NoteSeverity::Warning]
                .iter()
                .copied()
                .map(note_severity_code)
                .collect::<Vec<_>>(),
            NOTE_SEVERITY_CODES.to_vec()
        );
    }

    // WV-4: FiscalYearRule は文字列 ⇄ 値の両方向が引ける。
    #[test]
    fn fiscal_year_rule_codes_round_trip() {
        assert_eq!(
            fiscal_year_rule_code(FiscalYearRule::CalendarYear),
            "calendar_year"
        );
        assert_eq!(
            fiscal_year_rule_from_code("calendar_year").unwrap(),
            FiscalYearRule::CalendarYear
        );
    }

    // WV-5: 未知の値は既定値に落ちず、有効な値を列挙したエラーになる。
    #[test]
    fn unknown_codes_are_errors_listing_the_valid_values() {
        let err = fiscal_year_rule_from_code("fiscal_april").unwrap_err();
        let message = err.to_string();
        assert!(message.contains("fiscal_april"), "{message}");
        assert!(message.contains("calendar_year"), "{message}");

        let err = side_from_code("dr").unwrap_err();
        let message = err.to_string();
        assert!(message.contains("debit"), "{message}");
        assert!(message.contains("credit"), "{message}");

        let err = account_type_from_code("Asset").unwrap_err();
        assert!(err.to_string().contains("asset"), "{err}");
    }

    // WV-6: 線上語彙は `codes` と同じく snake_case の ASCII 識別子である。
    #[test]
    fn all_codes_are_snake_case_ascii_identifiers() {
        for code in ACCOUNT_TYPE_CODES
            .iter()
            .chain(SIDE_CODES)
            .chain(NOTE_SEVERITY_CODES)
            .chain(FISCAL_YEAR_RULE_CODES)
        {
            assert!(!code.is_empty());
            assert!(
                code.chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit()),
                "snake_case ではありません: {code}"
            );
        }
    }
}
