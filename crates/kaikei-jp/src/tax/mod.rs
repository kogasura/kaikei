//! 消費税区分マスタ（[`TaxCategoryTable`]）と、取引日による適用マスタの選択
//! （[`TaxRuleSets`]）。
//!
//! `docs/04-jp-tax.md` §1「税率も控除割合もコードに書かない」に従い、
//! この module は `kaikei-jp-data/tax/jp/{year}.yaml` に**書かれた値をそのまま
//! 解釈するだけ**の機構である。税額計算そのもの（消費税行の自動生成）は
//! `kaikei-policy::TaxPolicy::derive_tax_lines` の `kaikei-jp` 側実装
//! （[`JpTaxPolicy`]。PR-4）が担う。
//!
//! # 適用期間による選択（`CLAUDE.md` §7）
//!
//! 年度別データの選択は**取引日**で行う。記帳日ではない。[`TaxRuleSets::for_date`]
//! が取引日に対応するマスタを返す。どのマスタの適用期間にも入らない取引日は
//! `None`（正常な戻り値）であり、上位（`JpTaxPolicy`）が
//! `kaikei_policy::PolicyError::NoApplicableRuleSet` に写像することを想定する
//! （`DECISIONS.md` D-055）。
//!
//! # 構築時に検証すること
//!
//! - 複数マスタの適用期間が重ならないこと（`DECISIONS.md` D-054）
//! - 各マスタ内で `applies_from <= applies_to`、`categories[].code` が一意であること
//! - YAML の `version` / `country` が既知の値であること（`DECISIONS.md` D-056）
//!
//! # モジュール構成
//!
//! - [`category`][]: 税区分1件（[`TaxCategory`] / [`TaxDirection`]）
//! - [`settings`][]: `settings_defaults` に対応する既定値（[`TaxMode`] / [`RoundingUnit`] /
//!   [`TaxSettingsDefaults`]）。事業者ごとの実設定は [`JpSettings`] が持つ
//! - [`table`][]: 1つの適用期間ぶんのマスタ（[`TaxCategoryTable`]）
//! - [`rule_sets`][]: 適用期間の異なる複数マスタの集合（[`TaxRuleSets`]）
//! - [`policy`][]: `kaikei-policy::TaxPolicy` の実装（[`JpTaxPolicy`] /
//!   [`JpSettings`] / [`JpSettingsOverrides`]）
//!
//! # 免責
//!
//! 本 module が解釈する YAML の内容（税率・控除割合等）は公開情報の整理であり、
//! 税務上の正しさを保証しない（crate ルートの doc、`docs/04-jp-tax.md` §11 を参照）。

mod category;
mod policy;
mod rule_sets;
mod settings;
mod table;

pub use category::{TaxCategory, TaxDirection};
pub use policy::{JpSettings, JpSettingsOverrides, JpTaxPolicy};
pub use rule_sets::TaxRuleSets;
pub use settings::{RoundingUnit, TaxMode, TaxSettingsDefaults};
pub use table::TaxCategoryTable;
