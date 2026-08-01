//! `settings_defaults` に対応する既定値（[`TaxSettingsDefaults`]）。
//!
//! ここで表す値はマスタ（YAML）が持つ**既定値**であり、事業者ごとの実設定
//! （税抜/税込の選択、課税事業者かどうか等）は `docs/04-jp-tax.md` §2 の
//! `JpSettings`（`JpTaxPolicy` 実装。別 PR のスコープ）が持つ。この PR では
//! YAML に書かれた既定値をそのまま読むところまでを扱う。

use kaikei_core::RoundMode;
use serde::Deserialize;

/// 経理方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaxMode {
    /// 税抜経理。
    Exclusive,
    /// 税込経理。
    Inclusive,
}

impl TaxMode {
    fn parse(s: &str) -> Result<Self, String> {
        match s {
            "exclusive" => Ok(TaxMode::Exclusive),
            "inclusive" => Ok(TaxMode::Inclusive),
            other => Err(format!(
                "tax_mode の値が不正です: \"{other}\"（有効な値: exclusive, inclusive）"
            )),
        }
    }
}

/// 端数処理の単位。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundingUnit {
    /// 明細ごとに端数処理する。
    Line,
    /// 請求書（帳票）単位で端数処理する。
    Document,
}

impl RoundingUnit {
    fn parse(s: &str) -> Result<Self, String> {
        match s {
            "line" => Ok(RoundingUnit::Line),
            "document" => Ok(RoundingUnit::Document),
            other => Err(format!(
                "rounding_unit の値が不正です: \"{other}\"（有効な値: line, document）"
            )),
        }
    }
}

/// `kaikei_core::RoundMode` は文字列パースを持たない（core は YAML を知らない
/// ため）。この crate 側で `floor`/`ceil`/`half_up` の対応を持つ。
fn parse_round_mode(s: &str) -> Result<RoundMode, String> {
    match s {
        "floor" => Ok(RoundMode::Floor),
        "ceil" => Ok(RoundMode::Ceil),
        "half_up" => Ok(RoundMode::HalfUp),
        other => Err(format!(
            "rounding の値が不正です: \"{other}\"（有効な値: floor, ceil, half_up）"
        )),
    }
}

/// `settings_defaults` の内容。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaxSettingsDefaults {
    /// 経理方式の既定値。
    pub tax_mode: TaxMode,
    /// 端数処理方式の既定値。
    pub rounding: RoundMode,
    /// 端数処理単位の既定値。
    pub rounding_unit: RoundingUnit,
}

impl TaxSettingsDefaults {
    pub(super) fn from_raw(raw: &SettingsDefaultsRaw) -> Result<Self, String> {
        Ok(TaxSettingsDefaults {
            tax_mode: TaxMode::parse(&raw.tax_mode)?,
            rounding: parse_round_mode(&raw.rounding)?,
            rounding_unit: RoundingUnit::parse(&raw.rounding_unit)?,
        })
    }
}

/// [`TaxSettingsDefaults`] の YAML 上の生の形。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SettingsDefaultsRaw {
    pub(super) tax_mode: String,
    pub(super) rounding: String,
    pub(super) rounding_unit: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(tax_mode: &str, rounding: &str, rounding_unit: &str) -> SettingsDefaultsRaw {
        SettingsDefaultsRaw {
            tax_mode: tax_mode.to_string(),
            rounding: rounding.to_string(),
            rounding_unit: rounding_unit.to_string(),
        }
    }

    #[test]
    fn from_raw_valid_values_succeeds() {
        let defaults = TaxSettingsDefaults::from_raw(&raw("exclusive", "floor", "line")).unwrap();
        assert_eq!(defaults.tax_mode, TaxMode::Exclusive);
        assert_eq!(defaults.rounding, RoundMode::Floor);
        assert_eq!(defaults.rounding_unit, RoundingUnit::Line);
    }

    #[test]
    fn from_raw_unknown_tax_mode_is_error() {
        let err = TaxSettingsDefaults::from_raw(&raw("both", "floor", "line")).unwrap_err();
        assert!(err.contains("tax_mode"), "message = {err}");
    }

    #[test]
    fn from_raw_unknown_rounding_is_error() {
        let err = TaxSettingsDefaults::from_raw(&raw("exclusive", "round", "line")).unwrap_err();
        assert!(err.contains("rounding"), "message = {err}");
    }

    #[test]
    fn from_raw_unknown_rounding_unit_is_error() {
        let err = TaxSettingsDefaults::from_raw(&raw("exclusive", "floor", "invoice")).unwrap_err();
        assert!(err.contains("rounding_unit"), "message = {err}");
    }
}
