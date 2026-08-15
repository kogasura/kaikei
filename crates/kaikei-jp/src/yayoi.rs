//! 弥生の仕訳データインポート用の税区分の写像（[`YayoiTaxMap`]）。
//!
//! `docs/10-report.md` §6。同梱の写像は
//! `kaikei_jp_data::YAYOI_TAX_CATEGORIES`。
//!
//! # この写像は税理士の確認を受けていない
//!
//! **ある区分を別の区分として出力することは、その取引の税務上の扱いを変える
//! ことになる**（§6-4）。名称は弥生のサポート情報から拾ったもので、実機で
//! 取り込めることを確かめていない。[`YayoiTaxMap::all_verified`] が false を
//! 返す間は、出力側が利用者へその旨を知らせること。
//!
//! # 対応しない区分は変換せずエラーにする
//!
//! 表に無い区分は**黙って近い区分に丸めない**。丸めると、消費税の申告額が
//! 変わったことに誰も気づけない。経過措置の区分（適格請求書発行事業者以外
//! からの仕入）は、弥生側の名称が控除割合を含むのに対し kaikei の区分は
//! 割合を持たないため、**意図的に写像を用意していない**。

use crate::error::JpError;
use kaikei_jp_data::EmbeddedYaml;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

/// この crate が読める唯一のスキーマ版。
const SUPPORTED_VERSION: u32 = 1;

/// 税区分1件の写像。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaxCategoryMapping {
    /// 弥生側の税区分名（売上側、または向きを問わないもの）。
    pub yayoi: String,
    /// 仕入側で使う弥生の税区分名。
    ///
    /// # 片方だけだと向きを取り違える
    ///
    /// 非課税のように**売上にも仕入にも立つ**区分がある。弥生は売上側と
    /// 仕入側で区分が分かれているので、売上側だけを持つと、非課税の仕入が
    /// 「非課売上」として出力される（住宅の家賃・支払利息・保険料など、
    /// 個人事業主に普通にある取引で起きる）。
    ///
    /// `None` なら向きを問わない区分であり、[`Self::yayoi`] を使う。
    pub yayoi_purchase: Option<String>,
    /// 弥生の実機で取り込めることを確認済みか。
    pub verified: bool,
}

/// 弥生向けの税区分の写像。
#[derive(Debug, Clone)]
pub struct YayoiTaxMap {
    tax_mode: String,
    taxation_method: String,
    categories: BTreeMap<String, TaxCategoryMapping>,
}

impl YayoiTaxMap {
    /// この写像が前提にしている経理方式（`inclusive` = 税込）。
    pub fn tax_mode(&self) -> &str {
        &self.tax_mode
    }

    /// この写像が前提にしている課税方式。
    pub fn taxation_method(&self) -> &str {
        &self.taxation_method
    }

    /// kaikei の税区分に対応する弥生の税区分。
    ///
    /// 表に無ければ `None`。**呼び出し側は `None` をエラーにすること**——
    /// 近い区分に丸めてはならない。
    pub fn get(&self, kaikei_category: &str) -> Option<&TaxCategoryMapping> {
        self.categories.get(kaikei_category)
    }

    /// すべての写像が実機で確認済みか。
    ///
    /// false の間は、出力側が「この写像は未確認である」と知らせること。
    pub fn all_verified(&self) -> bool {
        self.categories.values().all(|mapping| mapping.verified)
    }

    /// 未確認の写像の数。
    pub fn unverified_count(&self) -> usize {
        self.categories
            .values()
            .filter(|mapping| !mapping.verified)
            .count()
    }
}

/// 埋め込み YAML から写像を読み込む。
pub fn load_embedded(embedded: EmbeddedYaml) -> Result<YayoiTaxMap, JpError> {
    let raw: MapRaw = crate::yaml::load_embedded(embedded)?;
    from_raw(embedded.label, raw)
}

/// 任意のファイルパスから読み込む（利用者が写像を差し替える経路）。
pub fn load_from_path(path: &Path) -> Result<YayoiTaxMap, JpError> {
    let raw: MapRaw = crate::yaml::load_from_path(path)?;
    from_raw(&path.display().to_string(), raw)
}

/// YAML 文字列から読み込む。
pub fn load_from_str(source: &str, label: &str) -> Result<YayoiTaxMap, JpError> {
    let raw: MapRaw = crate::yaml::load_str(source, label)?;
    from_raw(label, raw)
}

fn from_raw(label: &str, raw: MapRaw) -> Result<YayoiTaxMap, JpError> {
    let invalid = |reason: String| JpError::InvalidChart {
        label: label.to_string(),
        reason,
    };

    if raw.version != SUPPORTED_VERSION {
        return Err(invalid(format!(
            "対応していないスキーマバージョンです: {}（対応: {}）",
            raw.version, SUPPORTED_VERSION
        )));
    }

    let mut categories = BTreeMap::new();
    for entry in raw.categories {
        // **同じ区分を2度書かせない。** どちらが効くかが読む人に決められない。
        if categories
            .insert(
                entry.kaikei.clone(),
                TaxCategoryMapping {
                    yayoi: entry.yayoi,
                    yayoi_purchase: entry.yayoi_purchase,
                    verified: entry.verified,
                },
            )
            .is_some()
        {
            return Err(invalid(format!("税区分 {} が重複しています", entry.kaikei)));
        }
    }

    Ok(YayoiTaxMap {
        tax_mode: raw.tax_mode,
        taxation_method: raw.taxation_method,
        categories,
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MapRaw {
    version: u32,
    tax_mode: String,
    taxation_method: String,
    #[allow(dead_code)]
    source: String,
    categories: Vec<CategoryRaw>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CategoryRaw {
    kaikei: String,
    yayoi: String,
    /// 仕入側の区分。**省略できる**（向きを問わない区分がほとんどのため）。
    #[serde(default)]
    yayoi_purchase: Option<String>,
    verified: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn embedded() -> YayoiTaxMap {
        load_embedded(kaikei_jp_data::YAYOI_TAX_CATEGORIES).unwrap()
    }

    // YT-1: 同梱の写像が読める。
    #[test]
    fn the_embedded_map_parses() {
        let map = embedded();
        assert_eq!(map.tax_mode(), "inclusive");
        assert_eq!(map.taxation_method(), "原則課税");
        assert_eq!(map.get("SALES_10").unwrap().yayoi, "課税売上込10%");
    }

    // YT-2: **本命。** 経過措置の区分は写像を持たない。
    //
    //       弥生側の名称は控除割合を含む（「区分80%」等）が、kaikei の区分は
    //       割合を持たない（施行日で決まる）。決め打ちにすると施行日を
    //       またいだときに誤るので、変換せずエラーにする。
    #[test]
    fn the_transitional_measure_categories_are_deliberately_unmapped() {
        let map = embedded();

        for category in [
            "PURCHASE_10_NON_QUALIFIED",
            "PURCHASE_8_REDUCED_NON_QUALIFIED",
        ] {
            assert!(
                map.get(category).is_none(),
                "{category} に写像を用意すると、控除割合を決め打ちすることになる"
            );
        }
    }

    // YT-3: 未確認であることを持ち帰れる。
    #[test]
    fn the_map_reports_that_it_is_not_verified_yet() {
        let map = embedded();

        assert!(
            !map.all_verified(),
            "実機で確認できていないので、確認済みを名乗らないこと"
        );
        assert!(map.unverified_count() > 0);
    }

    // YT-4: 同じ区分を2度書いたら拒否する。
    #[test]
    fn a_duplicate_category_is_rejected() {
        let source = r#"
version: 1
tax_mode: inclusive
taxation_method: "原則課税"
source: "test"
categories:
  - { kaikei: SALES_10, yayoi: "課税売上込10%", verified: false }
  - { kaikei: SALES_10, yayoi: "別の名前", verified: false }
"#;
        let err = load_from_str(source, "test").expect_err("重複は拒否されるはず");
        assert!(format!("{err}").contains("重複"), "{err}");
    }

    // YT-5: 未知のスキーマ版は拒否する。
    #[test]
    fn an_unsupported_schema_version_is_rejected() {
        let source = r#"
version: 99
tax_mode: inclusive
taxation_method: "原則課税"
source: "test"
categories: []
"#;
        let err = load_from_str(source, "test").expect_err("拒否されるはず");
        assert!(format!("{err}").contains("バージョン"), "{err}");
    }
}
