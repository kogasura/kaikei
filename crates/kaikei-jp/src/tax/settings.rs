//! `settings_defaults` に対応する既定値（[`TaxSettingsDefaults`]）と、
//! ★税制設定の線上語彙★（[`TaxMode`] / [`RoundingUnit`] /
//! `kaikei_core::RoundMode` ⇄ 文字列）。
//!
//! ここで表す値はマスタ（YAML）が持つ**既定値**であり、事業者ごとの実設定
//! （税抜/税込の選択、課税事業者かどうか等）は `docs/04-jp-tax.md` §2 の
//! `JpSettings`（`JpTaxPolicy` 実装）が持つ。
//!
//! # 文字列 ⇄ 値の両方向を公開する（PR-B 2巡目）
//!
//! `docs/07-mcp-server.md` §7 の `get_settings` は `tax_mode` /
//! `rounding` / `rounding_unit` を**応答に載せ**、起動時の設定ファイルは
//! 同じ語彙を**入力として受ける**。1巡目まで、その唯一の実装は
//! このファイルの private な `parse` だけで、`&str` を返す入口も無かった。
//! 放置すると `kaikei-mcp` と（将来の）`kaikei-api` が各自で
//! `match` を書き、`"half_up"` と `"halfup"` のような綴りのずれが起きる。
//!
//! - 値 → 文字列: [`TaxMode::as_code`] / [`RoundingUnit::as_code`] /
//!   [`round_mode_code`]
//! - 文字列 → 値: [`TaxMode::from_code`] / [`RoundingUnit::from_code`] /
//!   [`round_mode_from_code`]
//!
//! **語彙は `kaikei-jp-data` の YAML が定める値そのもの**
//! （`exclusive` / `inclusive`、`floor` / `ceil` / `half_up`、
//! `line` / `document`）。YAML のロード（[`TaxSettingsDefaults::from_raw`]）も
//! この公開 API を通るので、YAML と線上で語彙が食い違うことは起きない。
//!
//! `kaikei_core::RoundMode` は `kaikei-core`（凍結層）の型なので、
//! メソッドではなく**自由関数**で提供する（`kaikei-app` の
//! `wire::side_code` 等と同じ形）。

use crate::error::JpError;
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
    /// この列挙型が取りうる機械可読名の一覧（YAML / JSON 共通）。
    pub const CODES: &'static [&'static str] = &["exclusive", "inclusive"];

    /// 機械可読名を返す（`settings_defaults.tax_mode` と同じ語彙）。
    pub fn as_code(&self) -> &'static str {
        match self {
            TaxMode::Exclusive => "exclusive",
            TaxMode::Inclusive => "inclusive",
        }
    }

    /// 機械可読名から値を復元する。
    ///
    /// # Errors
    ///
    /// 未知の値は [`JpError::InvalidSettingCode`]（有効な値を列挙する）。
    /// **既定値へフォールバックしない**（`DECISIONS.md` D-057 と同じ姿勢。
    /// 経理方式を取り違えると税額計算が丸ごと変わる）。
    pub fn from_code(code: &str) -> Result<Self, JpError> {
        match code {
            "exclusive" => Ok(TaxMode::Exclusive),
            "inclusive" => Ok(TaxMode::Inclusive),
            other => Err(invalid_setting_code("tax_mode", other, Self::CODES)),
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
    /// この列挙型が取りうる機械可読名の一覧。
    pub const CODES: &'static [&'static str] = &["line", "document"];

    /// 機械可読名を返す（`settings_defaults.rounding_unit` と同じ語彙）。
    pub fn as_code(&self) -> &'static str {
        match self {
            RoundingUnit::Line => "line",
            RoundingUnit::Document => "document",
        }
    }

    /// 機械可読名から値を復元する。
    ///
    /// # Errors
    ///
    /// 未知の値は [`JpError::InvalidSettingCode`]。
    pub fn from_code(code: &str) -> Result<Self, JpError> {
        match code {
            "line" => Ok(RoundingUnit::Line),
            "document" => Ok(RoundingUnit::Document),
            other => Err(invalid_setting_code("rounding_unit", other, Self::CODES)),
        }
    }
}

/// `kaikei_core::RoundMode` が取りうる機械可読名の一覧。
pub const ROUND_MODE_CODES: &[&str] = &["floor", "ceil", "half_up"];

/// `kaikei_core::RoundMode` の機械可読名を返す。
///
/// `kaikei-core` は YAML も JSON も知らないため文字列化を持たない
/// （凍結層。`CLAUDE.md` §1）。この対応を持つのはこの crate の責務である。
pub fn round_mode_code(mode: RoundMode) -> &'static str {
    match mode {
        RoundMode::Floor => "floor",
        RoundMode::Ceil => "ceil",
        RoundMode::HalfUp => "half_up",
    }
}

/// 機械可読名から `kaikei_core::RoundMode` を復元する。
///
/// # Errors
///
/// 未知の値は [`JpError::InvalidSettingCode`]。
pub fn round_mode_from_code(code: &str) -> Result<RoundMode, JpError> {
    match code {
        "floor" => Ok(RoundMode::Floor),
        "ceil" => Ok(RoundMode::Ceil),
        "half_up" => Ok(RoundMode::HalfUp),
        other => Err(invalid_setting_code("rounding", other, ROUND_MODE_CODES)),
    }
}

/// 「未知の機械可読名」に対する共通のエラー。
fn invalid_setting_code(field: &str, input: &str, valid: &[&str]) -> JpError {
    JpError::InvalidSettingCode {
        field: field.to_string(),
        input: input.to_string(),
        valid: valid.join(", "),
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
    /// YAML の生の形から組み立てる。
    ///
    /// **語彙の解釈は公開 API（`from_code`）に委ねる。** ここに `match` を
    /// 書き写すと、YAML の語彙と線上の語彙が別々に育って食い違う
    /// （本モジュール doc「文字列 ⇄ 値の両方向を公開する」）。
    ///
    /// 呼び出し元（`table.rs`）が `String` のエラーを合成するため、
    /// [`JpError::InvalidSettingCode`] の `Display` をそのまま文字列にする
    /// （文言は1巡目までと同一）。
    pub(super) fn from_raw(raw: &SettingsDefaultsRaw) -> Result<Self, String> {
        Ok(TaxSettingsDefaults {
            tax_mode: TaxMode::from_code(&raw.tax_mode).map_err(|e| e.to_string())?,
            rounding: round_mode_from_code(&raw.rounding).map_err(|e| e.to_string())?,
            rounding_unit: RoundingUnit::from_code(&raw.rounding_unit)
                .map_err(|e| e.to_string())?,
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

    // TS-1（PR-B 2巡目）: 3つの語彙すべてが文字列 ⇄ 値でラウンドトリップする。
    #[test]
    fn setting_codes_round_trip_in_both_directions() {
        for mode in [TaxMode::Exclusive, TaxMode::Inclusive] {
            assert_eq!(TaxMode::from_code(mode.as_code()).unwrap(), mode);
        }
        for unit in [RoundingUnit::Line, RoundingUnit::Document] {
            assert_eq!(RoundingUnit::from_code(unit.as_code()).unwrap(), unit);
        }
        for mode in [RoundMode::Floor, RoundMode::Ceil, RoundMode::HalfUp] {
            assert_eq!(round_mode_from_code(round_mode_code(mode)).unwrap(), mode);
        }
    }

    // TS-2: 語彙は `kaikei-jp-data` の YAML が定める値そのもの。
    #[test]
    fn setting_codes_match_the_yaml_vocabulary() {
        assert_eq!(TaxMode::Exclusive.as_code(), "exclusive");
        assert_eq!(TaxMode::Inclusive.as_code(), "inclusive");
        assert_eq!(RoundingUnit::Line.as_code(), "line");
        assert_eq!(RoundingUnit::Document.as_code(), "document");
        assert_eq!(round_mode_code(RoundMode::Floor), "floor");
        assert_eq!(round_mode_code(RoundMode::Ceil), "ceil");
        assert_eq!(round_mode_code(RoundMode::HalfUp), "half_up");

        // 一覧定数も同じ順・同じ綴りであること（スキーマ生成に使う）。
        assert_eq!(
            [TaxMode::Exclusive, TaxMode::Inclusive]
                .iter()
                .map(TaxMode::as_code)
                .collect::<Vec<_>>(),
            TaxMode::CODES.to_vec()
        );
        assert_eq!(
            [RoundingUnit::Line, RoundingUnit::Document]
                .iter()
                .map(RoundingUnit::as_code)
                .collect::<Vec<_>>(),
            RoundingUnit::CODES.to_vec()
        );
        assert_eq!(
            [RoundMode::Floor, RoundMode::Ceil, RoundMode::HalfUp]
                .into_iter()
                .map(round_mode_code)
                .collect::<Vec<_>>(),
            ROUND_MODE_CODES.to_vec()
        );
    }

    // TS-3: 未知の値は既定値に落ちず、有効な値を列挙したエラーになる。
    #[test]
    fn unknown_setting_codes_are_errors_listing_the_valid_values() {
        let err = TaxMode::from_code("both").unwrap_err();
        assert!(matches!(err, JpError::InvalidSettingCode { .. }));
        let message = err.to_string();
        assert!(message.contains("tax_mode"), "{message}");
        assert!(message.contains("exclusive"), "{message}");
        assert!(message.contains("inclusive"), "{message}");

        assert!(round_mode_from_code("round").is_err());
        assert!(RoundingUnit::from_code("invoice").is_err());
    }

    // TS-4: YAML のロードと線上の解釈が**同じ関数**を通る
    // （語彙が2箇所で育たないことの担保）。
    #[test]
    fn yaml_loading_and_the_wire_vocabulary_share_one_implementation() {
        let defaults =
            TaxSettingsDefaults::from_raw(&raw("inclusive", "half_up", "document")).unwrap();
        assert_eq!(defaults.tax_mode.as_code(), "inclusive");
        assert_eq!(round_mode_code(defaults.rounding), "half_up");
        assert_eq!(defaults.rounding_unit.as_code(), "document");
    }
}
