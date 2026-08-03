//! 合成ルートが起動時に行う組み立て（YAML のロード → policy の構築）。
//!
//! `kaikei-mcp`（Phase 3）や `kaikei-api`（Phase 4）といった presentation 層は、
//! いずれも起動時に同じ組み立てを行う。その手順をここに1つだけ置く
//! （`DECISIONS.md` D-068）。
//!
//! # なぜ `kaikei-jp` に置くのか
//!
//! この組み立ては `kaikei-core` / `kaikei-jp` / `kaikei-jp-data` だけで完結し、
//! `kaikei-app` も `kaikei-store` も使わない。つまり「両方を知ってよい層」で
//! なければ書けないコードではなく、`kaikei-jp` 単体で閉じている。
//!
//! 当初は E2E テスト専用 crate（`kaikei-e2e`）に置いていたが、それだと
//! Phase 3・Phase 4 の合成ルートが再利用できず、**同じ組み立てが3箇所に
//! 複製される**（`DECISIONS.md` D-047「手で維持する複製は腐る」と同型）。
//! レビュー指摘を受けてここへ移した。
//!
//! 実 DB に繋ぐ E2E テストは引き続き `kaikei-e2e` にある（そちらは
//! `kaikei-store` を知る必要があるため、`kaikei-jp` には置けない）。
//!
//! # I/O は構築時のみ
//!
//! YAML のロードはここ（構築時）で済ませる。`TaxPolicy` 等の trait メソッド
//! 自体は純関数を保つ（`CLAUDE.md` §3 / `DECISIONS.md` D-025）。

use crate::chart;
use crate::closing::{ClosingAccounts, JpSoleProprietorClosingPolicy};
use crate::error::JpError;
use crate::tags::TagCatalog;
use crate::tax::{JpSettings, JpSettingsOverrides, JpTaxPolicy, TaxRuleSets};
use kaikei_core::{AccountingDate, ChartOfAccounts};

/// [`compose`] への入力。
#[derive(Debug, Clone)]
pub struct ComposeOptions {
    /// 税額計算に使うマスタ群。通常は `TaxRuleSets::from_embedded()` の
    /// 結果を渡す。年度別マスタの切り替えを検証するテストでは、合成した
    /// マスタ集合を渡せる。
    pub rule_sets: TaxRuleSets,
    /// 事業者ごとの実設定の上書き。
    pub settings_overrides: JpSettingsOverrides,
    /// `JpSettings::compose` の既定値（`settings_defaults`）として使う
    /// マスタを選ぶための日付。「どのマスタを渡すかは呼び出し側の責務」
    /// （`DECISIONS.md` D-057）であり、通常は合成ルート起動時点の日付を渡す。
    pub defaults_as_of: AccountingDate,
    /// 決算処理に使う3科目（元入金・事業主貸・事業主借）。
    pub closing_accounts: ClosingAccounts,
    /// 決算振替の収益・費用ゼロ化明細に付与する消費税区分コード。
    /// どの区分コードを使うかは合成ルート（呼び出し側）の判断であり、
    /// この crate はハードコードしない（`docs/04-jp-tax.md` §1、
    /// `CLAUDE.md` §10、`DECISIONS.md` D-066 の追記）。
    pub closing_tax_category: Option<String>,
}

/// [`compose`] が組み立てる依存一式。
///
/// `JpStatementPolicy` を含まない理由はクレートdocの
/// 「`JpStatementPolicy` の `chart` について」を参照。
#[derive(Debug, Clone)]
pub struct Composition {
    /// 勘定科目表（埋め込みテンプレート由来）。
    pub chart: ChartOfAccounts,
    /// タグスキーマ（埋め込みテンプレート由来）。
    ///
    /// `kaikei_core::TagSchema` そのものではなく [`TagCatalog`] を持つ。
    /// 検証に渡す `&TagSchema` は `tag_catalog.schema()` で取れるうえ、
    /// 線上（JSON）の `tags` を `TagSet` にするのに必要なキーごとの
    /// `TagValueType` も同じ値から引ける（`crate::tags` のモジュールdoc）。
    /// `TagSchema` と定義一覧を別々のフィールドで持つと、片方だけ差し替えた
    /// 組み合わせが作れてしまう。
    pub tag_catalog: TagCatalog,
    /// `kaikei-policy::TaxPolicy` の実装。
    pub tax_policy: JpTaxPolicy,
    /// `kaikei-policy::ClosingPolicy` の実装。
    pub closing_policy: JpSoleProprietorClosingPolicy,
}

/// 合成に失敗した理由。
#[derive(Debug, thiserror::Error)]
pub enum ComposeError {
    /// `kaikei-jp` 側のロード・構築が失敗した
    /// （YAMLの構文・スキーマ不正、決算科目の不在等）。
    #[error(transparent)]
    Jp(#[from] JpError),

    /// `defaults_as_of` の時点で有効な税区分マスタが `rule_sets` に無く、
    /// `JpSettings::compose` の既定値（`settings_defaults`）を決定できない。
    #[error(
        "{as_of} 時点で有効な消費税区分マスタが見つかりません。\
         事業者設定の既定値（settings_defaults）を決定できないため合成を中止します。\
         ComposeOptions::defaults_as_of を見直すか、対応するマスタを rule_sets に \
         追加してください"
    )]
    NoApplicableRuleSetForDefaults {
        /// 対象の日付（ISO表記）。
        as_of: String,
    },
}

/// 起動時に一度だけ行う組み立て（YAMLロード → policy 構築）を1箇所にまとめる。
///
/// 手順:
/// 1. `chart::load_embedded` → `ChartOfAccounts`
/// 2. `TagCatalog::from_embedded` → タグスキーマ（+ 線上変換に使う定義一覧）
/// 3. `options.rule_sets`（通常は `TaxRuleSets::from_embedded()` の結果）から
///    `options.defaults_as_of` 時点のマスタを選び、その `settings_defaults` を
///    既定値として `JpSettings::compose`
/// 4. `JpTaxPolicy::new(rule_sets, settings)`
/// 5. `JpSoleProprietorClosingPolicy::new(chart, schema, closing_accounts, tax_category)`
///
/// `JpStatementPolicy` はここでは組み立てない
/// （クレートdoc「`JpStatementPolicy` の `chart` について」を参照。
/// `DECISIONS.md` D-069）。
pub fn compose(options: ComposeOptions) -> Result<Composition, ComposeError> {
    let chart = chart::load_embedded(kaikei_jp_data::CHART_SOLE_PROPRIETOR)?;
    let tag_catalog = TagCatalog::from_embedded(kaikei_jp_data::TAGS)?;

    let defaults = options
        .rule_sets
        .for_date(options.defaults_as_of)
        .ok_or_else(|| ComposeError::NoApplicableRuleSetForDefaults {
            as_of: options.defaults_as_of.to_iso_string(),
        })?
        .settings_defaults();
    let settings = JpSettings::compose(defaults, options.settings_overrides);
    let tax_policy = JpTaxPolicy::new(options.rule_sets, settings);

    let closing_policy = JpSoleProprietorClosingPolicy::new(
        &chart,
        tag_catalog.schema(),
        options.closing_accounts,
        options.closing_tax_category,
    )?;

    Ok(Composition {
        chart,
        tag_catalog,
        tax_policy,
        closing_policy,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_core::AccountCode;

    fn valid_options() -> ComposeOptions {
        ComposeOptions {
            rule_sets: TaxRuleSets::from_embedded().unwrap(),
            settings_overrides: JpSettingsOverrides {
                tax_mode: None,
                rounding: None,
                rounding_unit: None,
                is_taxable_business: true,
                simplified_taxation: false,
            },
            defaults_as_of: AccountingDate::new(2026, 4, 1).unwrap(),
            closing_accounts: ClosingAccounts {
                capital: AccountCode::parse("400").unwrap(),
                owner_drawings: AccountCode::parse("410").unwrap(),
                owner_contributions: AccountCode::parse("420").unwrap(),
            },
            closing_tax_category: Some("NOT_APPLICABLE".to_string()),
        }
    }

    #[test]
    fn compose_succeeds_with_the_bundled_jp_data() {
        let composition = compose(valid_options()).unwrap();
        assert!(composition.chart.iter().count() > 0);
        assert_eq!(
            composition.tax_policy.settings().tax_mode,
            crate::tax::TaxMode::Exclusive
        );
    }

    #[test]
    fn compose_fails_when_defaults_as_of_has_no_applicable_master() {
        let mut options = valid_options();
        options.defaults_as_of = AccountingDate::new(2000, 1, 1).unwrap();
        let err = compose(options).unwrap_err();
        assert!(matches!(
            err,
            ComposeError::NoApplicableRuleSetForDefaults { .. }
        ));
    }

    #[test]
    fn compose_fails_when_closing_account_is_missing_from_chart() {
        let mut options = valid_options();
        options.closing_accounts.capital = AccountCode::parse("999").unwrap();
        let err = compose(options).unwrap_err();
        assert!(matches!(
            err,
            ComposeError::Jp(JpError::MissingClosingAccount { .. })
        ));
    }
}
