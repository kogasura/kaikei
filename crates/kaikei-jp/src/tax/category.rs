//! 税区分1件（[`TaxCategory`]）。`kaikei-jp-data/tax/jp/{year}.yaml` の
//! `categories[]` 1要素に対応する。
//!
//! YAML の生の形（[`TaxCategoryRaw`]）とドメイン型（[`TaxCategory`]）を
//! 同じファイルに置く。前者は後者に変換されたら破棄されるだけの
//! デシリアライズ専用の形であり、独立した「ドメイン概念」ではないため
//! 別ファイルに分けない。

use kaikei_core::{AccountCode, Ratio};
use serde::Deserialize;

/// 税区分の向き。
///
/// `direction: none` は非課税・不課税・対象外を表し、税額計算をしない
/// （`docs/04-jp-tax.md` §3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaxDirection {
    /// 売上側の区分（課税売上・免税売上等）。
    Sales,
    /// 仕入側の区分（課税仕入等）。
    Purchase,
    /// 税額計算をしない区分（非課税・不課税・対象外）。
    None,
}

impl TaxDirection {
    /// YAML の `direction` フィールドの値を解釈する。
    fn parse(s: &str) -> Result<Self, String> {
        match s {
            "sales" => Ok(TaxDirection::Sales),
            "purchase" => Ok(TaxDirection::Purchase),
            "none" => Ok(TaxDirection::None),
            other => Err(format!(
                "direction の値が不正です: \"{other}\"（有効な値: sales, purchase, none）"
            )),
        }
    }
}

/// 税区分1件。
///
/// フィールドはすべて `pub`。構築後に変更されることを想定しない値の集まりで、
/// `kaikei_core::AccountDef` と同様に守るべき不変条件を持たないため
/// （守るべき不変条件は「マスタ内でコードが一意であること」であり、それは
/// 集合を扱う [`super::table::TaxCategoryTable`] 側の責務）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaxCategory {
    /// 一意な区分コード（`tags.tax_category` に入る値）。
    pub code: String,
    /// 表示名。
    pub label: String,
    /// 向き（売上・仕入・対象外）。
    pub direction: TaxDirection,
    /// 税率。`direction: none` の区分では `None`（税額計算をしない）。
    pub rate: Option<Ratio>,
    /// 仕入税額控除の対象かどうか（`purchase` のみ意味を持つ）。
    pub deductible: Option<bool>,
    /// 控除割合。`None` なら 1.0（全額控除）として扱う想定（YAML のコメントに
    /// 明記されている解釈。この crate は値をそのまま運ぶだけで、控除割合の
    /// 適用自体は行わない）。
    pub deduction_ratio: Option<Ratio>,
    /// 適格請求書の保存が必要かどうか。
    pub requires_qualified_invoice: bool,
    /// 税額の計上先科目コード（仮受/仮払消費税等）。
    pub tax_account: Option<AccountCode>,
    /// 注記（経過措置の扱い等、断定を避けた補足情報）。
    pub note: Option<String>,
}

impl TaxCategory {
    /// [`TaxCategoryRaw`] から構築する。
    ///
    /// 戻り値の `Err` はフィールド名を含む理由文字列のみ（呼び出し側の
    /// [`super::table::TaxCategoryTable`] がマスタのラベルを付与して
    /// `crate::error::JpError::InvalidTaxCategoryTable` に包む）。
    pub(super) fn from_raw(raw: TaxCategoryRaw) -> Result<Self, String> {
        if raw.code.trim().is_empty() {
            return Err("code が空です".to_string());
        }
        // 前後の空白はトリムせず拒否する（`InvoiceRegistrationNo::parse` の
        // D-052、および `kaikei_core::AccountCode::parse` と同じ方針）。
        //
        // トリムして受理すると YAML 側の値とコード上の値が食い違う。黙って
        // 受理した場合はさらに悪く、`code: " SALES_10"` が
        // 「税区分コード "SALES_10" は存在しません（利用可能な区分: SALES_10）」
        // という**見た目が同一の**エラーになり、原因の特定が極めて難しくなる
        // （半角スペースは端末やログでほぼ判別できない）。
        if raw.code != raw.code.trim() {
            return Err(format!(
                "code の前後に空白を含めることはできません: \"{}\"。\
                 YAML の値から空白を取り除いてください",
                raw.code
            ));
        }
        let code = raw.code;

        let direction = TaxDirection::parse(&raw.direction)
            .map_err(|reason| format!("code={code}: {reason}"))?;

        let rate = raw
            .rate
            .as_deref()
            .map(Ratio::parse_rate)
            .transpose()
            .map_err(|source| format!("code={code}: rate が不正です: {source}"))?;

        let deduction_ratio = raw
            .deduction_ratio
            .as_deref()
            .map(Ratio::parse_fraction)
            .transpose()
            .map_err(|source| format!("code={code}: deduction_ratio が不正です: {source}"))?;

        let tax_account = raw
            .tax_account
            .as_deref()
            .map(AccountCode::parse)
            .transpose()
            .map_err(|source| format!("code={code}: tax_account が不正です: {source}"))?;

        Ok(TaxCategory {
            code,
            label: raw.label,
            direction,
            rate,
            deductible: raw.deductible,
            deduction_ratio,
            requires_qualified_invoice: raw.requires_qualified_invoice,
            tax_account,
            note: raw.note,
        })
    }
}

/// [`TaxCategory`] の YAML 上の生の形。
///
/// `rate` 等は文字列のまま受け取り（`CLAUDE.md` §8: `f64` は使わない。YAML の
/// float を経由させない）、[`TaxCategory::from_raw`] で `rust_decimal` 経由の
/// 型に変換する。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TaxCategoryRaw {
    pub(super) code: String,
    pub(super) label: String,
    pub(super) direction: String,
    pub(super) rate: Option<String>,
    #[serde(default)]
    pub(super) deductible: Option<bool>,
    #[serde(default)]
    pub(super) deduction_ratio: Option<String>,
    #[serde(default)]
    pub(super) requires_qualified_invoice: bool,
    pub(super) tax_account: Option<String>,
    #[serde(default)]
    pub(super) note: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(code: &str, direction: &str, rate: Option<&str>) -> TaxCategoryRaw {
        TaxCategoryRaw {
            code: code.to_string(),
            label: format!("label-{code}"),
            direction: direction.to_string(),
            rate: rate.map(str::to_string),
            deductible: None,
            deduction_ratio: None,
            requires_qualified_invoice: false,
            tax_account: None,
            note: None,
        }
    }

    #[test]
    fn tax_direction_parse_accepts_the_three_known_values() {
        assert_eq!(TaxDirection::parse("sales").unwrap(), TaxDirection::Sales);
        assert_eq!(
            TaxDirection::parse("purchase").unwrap(),
            TaxDirection::Purchase
        );
        assert_eq!(TaxDirection::parse("none").unwrap(), TaxDirection::None);
    }

    #[test]
    fn tax_direction_parse_rejects_unknown_value() {
        let err = TaxDirection::parse("refund").unwrap_err();
        assert!(err.contains("refund"));
        assert!(err.contains("sales"));
        assert!(err.contains("purchase"));
        assert!(err.contains("none"));
    }

    #[test]
    fn from_raw_sales_with_rate_succeeds() {
        let category = TaxCategory::from_raw(raw("SALES_10", "sales", Some("0.10"))).unwrap();
        assert_eq!(category.direction, TaxDirection::Sales);
        assert_eq!(
            category.rate.unwrap().as_decimal(),
            rust_decimal::Decimal::new(10, 2)
        );
    }

    #[test]
    fn from_raw_none_direction_with_null_rate_succeeds() {
        let category = TaxCategory::from_raw(raw("TAX_FREE", "none", None)).unwrap();
        assert_eq!(category.direction, TaxDirection::None);
        assert_eq!(category.rate, None);
    }

    #[test]
    fn from_raw_non_numeric_rate_is_error() {
        let err = TaxCategory::from_raw(raw("BAD", "sales", Some("abc"))).unwrap_err();
        assert!(err.contains("rate"), "message = {err}");
    }

    #[test]
    fn from_raw_malformed_decimal_rate_is_error() {
        let err = TaxCategory::from_raw(raw("BAD", "sales", Some("0.1.2"))).unwrap_err();
        assert!(err.contains("rate"), "message = {err}");
    }

    #[test]
    fn from_raw_unknown_direction_is_error() {
        let err = TaxCategory::from_raw(raw("BAD", "refund", None)).unwrap_err();
        assert!(err.contains("BAD"));
        assert!(err.contains("direction"));
    }

    #[test]
    fn from_raw_empty_code_is_error() {
        let err = TaxCategory::from_raw(raw("", "sales", Some("0.10"))).unwrap_err();
        assert!(err.contains("code"));
    }

    /// 前後に空白を含む `code` は、トリムして受理せず拒否する。
    ///
    /// 受理すると `code: " SALES_10"` が
    /// 「税区分コード "SALES_10" は存在しません（利用可能な区分: SALES_10）」という
    /// **見た目が同一の**エラーになり、原因を特定できなくなる（レビュー指摘）。
    #[test]
    fn from_raw_code_with_surrounding_whitespace_is_rejected_not_trimmed() {
        for code in [" SALES_10", "SALES_10 ", " SALES_10 ", "\tSALES_10"] {
            let result = TaxCategory::from_raw(raw(code, "sales", Some("0.10")));
            let err = result.expect_err(&format!(
                "code=\"{code}\" は空白を含むので拒否されるべきですが受理されました"
            ));
            assert!(
                err.contains("空白"),
                "code=\"{code}\" は空白を理由に拒否されるべき: {err}"
            );
        }
    }

    #[test]
    fn from_raw_invalid_tax_account_is_error() {
        let mut r = raw("SALES_10", "sales", Some("0.10"));
        r.tax_account = Some("勘定科目".to_string());
        let err = TaxCategory::from_raw(r).unwrap_err();
        assert!(err.contains("tax_account"), "message = {err}");
    }

    #[test]
    fn from_raw_invalid_deduction_ratio_is_error() {
        let mut r = raw("PURCHASE_10", "purchase", Some("0.10"));
        r.deduction_ratio = Some("1.5".to_string());
        let err = TaxCategory::from_raw(r).unwrap_err();
        assert!(err.contains("deduction_ratio"), "message = {err}");
    }
}
