//! 1つの適用期間ぶんの消費税区分マスタ（[`TaxCategoryTable`]）。
//!
//! `kaikei-jp-data/tax/jp/{year}.yaml` 1ファイルに対応する集約。
//! `CLAUDE.md` §6「集約は1モジュールに収める」に従い、`categories` の
//! 重複検証など不変条件を守るコードはすべてこのファイルに閉じる。

use crate::error::JpError;
use crate::tax::category::{TaxCategory, TaxCategoryRaw};
use crate::tax::settings::{SettingsDefaultsRaw, TaxSettingsDefaults};
use crate::yaml::{load_embedded, load_from_path, load_str};
use kaikei_core::AccountingDate;
use kaikei_jp_data::EmbeddedYaml;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::Path;

/// 1つの適用期間ぶんの消費税区分マスタ。
///
/// `applies_from` 〜 `applies_to`（両端含む閉区間。`applies_to == None` なら
/// 無期限）の間の取引日に適用される。この構造体自体は自分がどの取引日に
/// 適用されるかを知らず、複数マスタをまたいだ選択は [`super::TaxRuleSets`]
/// が行う。
#[derive(Debug, Clone)]
pub struct TaxCategoryTable {
    label: String,
    applies_from: AccountingDate,
    applies_to: Option<AccountingDate>,
    settings_defaults: TaxSettingsDefaults,
    categories: Vec<TaxCategory>,
}

impl TaxCategoryTable {
    /// この PR 時点でこの crate が読める唯一のスキーマ版。
    ///
    /// 未知のバージョンは構築時に拒否する（`DECISIONS.md` D-056）。
    const SUPPORTED_VERSION: u32 = 1;

    /// この crate（`kaikei-jp`）が対象とする国コード。
    ///
    /// `country` が一致しない YAML は構築時に拒否する（`DECISIONS.md` D-056）。
    const EXPECTED_COUNTRY: &'static str = "JP";

    /// 既に検証済みの値からマスタを構築する。
    ///
    /// 検証内容:
    /// - `applies_from <= applies_to`（`applies_to` が `Some` の場合）
    /// - `categories[].code` が重複していないこと
    pub fn new(
        label: String,
        applies_from: AccountingDate,
        applies_to: Option<AccountingDate>,
        settings_defaults: TaxSettingsDefaults,
        categories: Vec<TaxCategory>,
    ) -> Result<Self, JpError> {
        if let Some(to) = applies_to {
            if applies_from > to {
                return Err(JpError::InvalidTaxCategoryTable {
                    label,
                    reason: format!(
                        "適用開始日が終了日より後です: applies_from={} applies_to={}。\
                         開始日を終了日以前に修正するか、無期限にする場合は applies_to を \
                         null にしてください",
                        applies_from.to_iso_string(),
                        to.to_iso_string()
                    ),
                });
            }
        }

        let mut seen_codes = BTreeSet::new();
        for category in &categories {
            if !seen_codes.insert(category.code.as_str()) {
                return Err(JpError::InvalidTaxCategoryTable {
                    label,
                    reason: format!(
                        "税区分コードが重複しています: \"{}\"。categories[].code は \
                         マスタ内で一意である必要があります",
                        category.code
                    ),
                });
            }
        }

        Ok(TaxCategoryTable {
            label,
            applies_from,
            applies_to,
            settings_defaults,
            categories,
        })
    }

    /// `kaikei-jp-data` の埋め込み YAML から読み込む。
    pub fn from_embedded(embedded: EmbeddedYaml) -> Result<Self, JpError> {
        let raw: TaxCategoryTableRaw = load_embedded(embedded)?;
        Self::from_raw(embedded.label, raw)
    }

    /// 任意のファイルパスから読み込む（ユーザーが自分の税区分マスタに
    /// 差し替える経路）。
    pub fn from_path(path: &Path) -> Result<Self, JpError> {
        let raw: TaxCategoryTableRaw = load_from_path(path)?;
        Self::from_raw(&path.display().to_string(), raw)
    }

    /// YAML 文字列から読み込む（テスト、および `from_embedded` / `from_path`
    /// の共通経路）。
    pub fn from_yaml_str(source: &str, label: &str) -> Result<Self, JpError> {
        let raw: TaxCategoryTableRaw = load_str(source, label)?;
        Self::from_raw(label, raw)
    }

    fn from_raw(label: &str, raw: TaxCategoryTableRaw) -> Result<Self, JpError> {
        let invalid = |reason: String| JpError::InvalidTaxCategoryTable {
            label: label.to_string(),
            reason,
        };

        if raw.version != Self::SUPPORTED_VERSION {
            return Err(invalid(format!(
                "対応していないスキーマバージョンです: {}（対応: {}）。新しいバージョンの \
                 スキーマを読むには kaikei-jp 側の対応が必要です",
                raw.version,
                Self::SUPPORTED_VERSION
            )));
        }
        if raw.country != Self::EXPECTED_COUNTRY {
            return Err(invalid(format!(
                "country が \"{}\" ではありません: \"{}\"。この crate（kaikei-jp）は \
                 日本の税制のみを対象とします",
                Self::EXPECTED_COUNTRY,
                raw.country
            )));
        }

        let applies_from = AccountingDate::parse(&raw.applies_from)
            .map_err(|source| invalid(format!("applies_from が不正です: {source}")))?;
        let applies_to = raw
            .applies_to
            .as_deref()
            .map(AccountingDate::parse)
            .transpose()
            .map_err(|source| invalid(format!("applies_to が不正です: {source}")))?;

        let settings_defaults = TaxSettingsDefaults::from_raw(&raw.settings_defaults)
            .map_err(|reason| invalid(format!("settings_defaults: {reason}")))?;

        let categories = raw
            .categories
            .into_iter()
            .map(|c| TaxCategory::from_raw(c).map_err(invalid))
            .collect::<Result<Vec<_>, _>>()?;

        Self::new(
            label.to_string(),
            applies_from,
            applies_to,
            settings_defaults,
            categories,
        )
    }

    /// 読み込み元の識別子（埋め込みYAMLの名称、またはファイルパス）。
    pub fn label(&self) -> &str {
        &self.label
    }

    /// 適用開始日。
    pub fn applies_from(&self) -> AccountingDate {
        self.applies_from
    }

    /// 適用終了日。`None` なら無期限。
    pub fn applies_to(&self) -> Option<AccountingDate> {
        self.applies_to
    }

    /// `settings_defaults` の内容。
    pub fn settings_defaults(&self) -> TaxSettingsDefaults {
        self.settings_defaults
    }

    /// このマスタが持つ税区分を巡回する。順序は YAML に書かれた順。
    pub fn categories(&self) -> impl Iterator<Item = &TaxCategory> {
        self.categories.iter()
    }

    /// 取引日がこのマスタの適用期間に含まれるか（両端を含む閉区間）。
    pub fn contains(&self, date: AccountingDate) -> bool {
        self.applies_from <= date && self.applies_to.map_or(true, |to| date <= to)
    }

    /// もう一方のマスタと適用期間が重なっているか。
    ///
    /// [`super::TaxRuleSets::new`] がマスタ集合を構築する際に使う
    /// （`DECISIONS.md` D-054）。
    pub(super) fn overlaps(&self, other: &Self) -> bool {
        let self_starts_before_other_ends =
            other.applies_to.map_or(true, |to| self.applies_from <= to);
        let other_starts_before_self_ends =
            self.applies_to.map_or(true, |to| other.applies_from <= to);
        self_starts_before_other_ends && other_starts_before_self_ends
    }

    /// 適用期間の表示用文字列（エラーメッセージ用）。
    pub(super) fn range_display(&self) -> String {
        match self.applies_to {
            Some(to) => format!(
                "{} 〜 {}",
                self.applies_from.to_iso_string(),
                to.to_iso_string()
            ),
            None => format!("{} 〜 無期限", self.applies_from.to_iso_string()),
        }
    }

    /// 税区分コードから [`TaxCategory`] を引く。
    ///
    /// 見つからない場合、エラーメッセージにこのマスタに存在する有効な
    /// コード一覧を含める（`CLAUDE.md` §11。MCP 経由で AI が自己修正できる形にする）。
    pub fn category(&self, code: &str) -> Result<&TaxCategory, JpError> {
        self.categories
            .iter()
            .find(|c| c.code == code)
            .ok_or_else(|| {
                let mut available: Vec<&str> =
                    self.categories.iter().map(|c| c.code.as_str()).collect();
                available.sort_unstable();
                JpError::UnknownTaxCategoryCode {
                    code: code.to_string(),
                    table_label: self.label.clone(),
                    applies_from: self.applies_from.to_iso_string(),
                    available: available.join(", "),
                }
            })
    }
}

/// [`TaxCategoryTable`] の YAML 上の生の形。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaxCategoryTableRaw {
    version: u32,
    country: String,
    applies_from: String,
    applies_to: Option<String>,
    settings_defaults: SettingsDefaultsRaw,
    categories: Vec<TaxCategoryRaw>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_YAML: &str = r#"
version: 1
country: JP
applies_from: 2026-01-01
applies_to: null
settings_defaults:
  tax_mode: exclusive
  rounding: floor
  rounding_unit: line
categories:
  - code: SALES_10
    label: "課税売上 10%"
    direction: sales
    rate: "0.10"
    tax_account: "330"
  - code: TAX_FREE
    label: "非課税"
    direction: none
    rate: null
"#;

    /// 実データ（`kaikei-jp-data::TAX_CATEGORY_SOURCES` 全件）がパースできること。
    #[test]
    fn from_embedded_parses_every_bundled_source() {
        for source in kaikei_jp_data::TAX_CATEGORY_SOURCES {
            let table = TaxCategoryTable::from_embedded(*source)
                .unwrap_or_else(|e| panic!("{} のパースに失敗: {e}", source.label));
            assert_eq!(table.label(), source.label);
            assert!(
                table.categories().count() > 0,
                "{} に categories が1件もありません",
                source.label
            );
        }
    }

    #[test]
    fn from_yaml_str_parses_valid_table() {
        let table = TaxCategoryTable::from_yaml_str(VALID_YAML, "test").unwrap();
        assert_eq!(table.label(), "test");
        assert_eq!(
            table.applies_from(),
            AccountingDate::new(2026, 1, 1).unwrap()
        );
        assert_eq!(table.applies_to(), None);
        assert_eq!(table.categories().count(), 2);
    }

    #[test]
    fn from_yaml_str_rejects_unknown_top_level_field() {
        let yaml = format!("{VALID_YAML}\nextra_field: true\n");
        let err = TaxCategoryTable::from_yaml_str(&yaml, "test").unwrap_err();
        assert!(matches!(err, JpError::YamlParse { .. }));
    }

    #[test]
    fn from_yaml_str_rejects_unknown_category_field() {
        let yaml = VALID_YAML.replace(
            "    tax_account: \"330\"\n",
            "    tax_account: \"330\"\n    unexpected: true\n",
        );
        let err = TaxCategoryTable::from_yaml_str(&yaml, "test").unwrap_err();
        assert!(matches!(err, JpError::YamlParse { .. }));
    }

    #[test]
    fn from_yaml_str_unsupported_version_is_error() {
        let yaml = VALID_YAML.replace("version: 1", "version: 2");
        let err = TaxCategoryTable::from_yaml_str(&yaml, "test").unwrap_err();
        match err {
            JpError::InvalidTaxCategoryTable { reason, .. } => {
                assert!(reason.contains('2'), "reason = {reason}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn from_yaml_str_unexpected_country_is_error() {
        let yaml = VALID_YAML.replace("country: JP", "country: US");
        let err = TaxCategoryTable::from_yaml_str(&yaml, "test").unwrap_err();
        match err {
            JpError::InvalidTaxCategoryTable { reason, .. } => {
                assert!(reason.contains("US"), "reason = {reason}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn from_yaml_str_applies_from_after_applies_to_is_error() {
        let yaml = VALID_YAML
            .replace("applies_from: 2026-01-01", "applies_from: 2026-12-31")
            .replace("applies_to: null", "applies_to: 2026-01-01");
        let err = TaxCategoryTable::from_yaml_str(&yaml, "test").unwrap_err();
        assert!(matches!(err, JpError::InvalidTaxCategoryTable { .. }));
    }

    #[test]
    fn from_yaml_str_duplicate_category_code_is_error() {
        let yaml = VALID_YAML.replace("TAX_FREE", "SALES_10");
        let err = TaxCategoryTable::from_yaml_str(&yaml, "test").unwrap_err();
        match err {
            JpError::InvalidTaxCategoryTable { reason, .. } => {
                assert!(reason.contains("SALES_10"), "reason = {reason}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn contains_includes_both_boundaries() {
        let table = TaxCategoryTable::new(
            "test".to_string(),
            AccountingDate::new(2026, 1, 1).unwrap(),
            Some(AccountingDate::new(2026, 3, 31).unwrap()),
            valid_settings_defaults(),
            vec![],
        )
        .unwrap();

        assert!(table.contains(AccountingDate::new(2026, 1, 1).unwrap()));
        assert!(table.contains(AccountingDate::new(2026, 3, 31).unwrap()));
        assert!(table.contains(AccountingDate::new(2026, 2, 1).unwrap()));
        assert!(!table.contains(AccountingDate::new(2025, 12, 31).unwrap()));
        assert!(!table.contains(AccountingDate::new(2026, 4, 1).unwrap()));
    }

    #[test]
    fn contains_with_unbounded_applies_to_is_open_ended() {
        let table = TaxCategoryTable::new(
            "test".to_string(),
            AccountingDate::new(2026, 1, 1).unwrap(),
            None,
            valid_settings_defaults(),
            vec![],
        )
        .unwrap();

        assert!(table.contains(AccountingDate::new(2026, 1, 1).unwrap()));
        assert!(table.contains(AccountingDate::new(2999, 12, 31).unwrap()));
        assert!(!table.contains(AccountingDate::new(2025, 12, 31).unwrap()));
    }

    #[test]
    fn category_unknown_code_lists_available_codes() {
        let table = TaxCategoryTable::from_yaml_str(VALID_YAML, "test").unwrap();
        let err = table.category("NOPE").unwrap_err();
        let message = err.to_string();
        assert!(message.contains("NOPE"));
        assert!(message.contains("SALES_10"));
        assert!(message.contains("TAX_FREE"));
    }

    #[test]
    fn category_known_code_returns_the_category() {
        let table = TaxCategoryTable::from_yaml_str(VALID_YAML, "test").unwrap();
        let category = table.category("SALES_10").unwrap();
        assert_eq!(category.label, "課税売上 10%");
    }

    fn valid_settings_defaults() -> TaxSettingsDefaults {
        TaxCategoryTable::from_yaml_str(VALID_YAML, "test")
            .unwrap()
            .settings_defaults()
    }
}
