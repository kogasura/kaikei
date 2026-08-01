//! `kaikei-e2e`: 合成ルートを模した層。**E2Eテスト専用crate。**
//!
//! # このcrateが存在する理由
//!
//! `kaikei-store` は `kaikei-jp` / `kaikei-policy` を知らない
//! （`CLAUDE.md` §1、`.github/workflows/architecture.yml` の
//! 「kaikei-store は kaikei-jp / kaikei-policy に依存しない」ステップが
//! 機械的に検査する）。逆に `kaikei-jp` も `kaikei-store`（DB・sqlx・tokio）を
//! 知らない（同ワークフローの「kaikei-jp は infra を知らない」ステップ）。
//!
//! つまり「税抜経理の消費税行が**実際にPostgreSQLへ記帳できる**」
//! 「`household_split` の3行仕訳が記帳できる」「決算振替仕訳が実際に記帳
//! できる」ことを検証するテストは、`kaikei-store` にも `kaikei-jp` にも
//! 置けない。置ける先は「両方を知ってよい最上位の層」＝合成ルートだけである
//! （`docs/04-jp-tax.md` §2、`DECISIONS.md` D-064 の訂正注記を参照）。
//!
//! 本番の合成ルートは Phase 3 の `kaikei-mcp`（または Phase 4 の
//! `kaikei-api`）になる予定だが、それを先取りして作るのは時期尚早
//! （YAGNI）である。そこで E2E テストの置き場として、この専用crateを
//! 新設した（`DECISIONS.md` D-068）。
//!
//! # 他のどのcrateからも依存されない
//!
//! `kaikei-e2e` は**テスト専用**であり、`kaikei-app` / `kaikei-store` /
//! `kaikei-jp` を含む他のどのcrateの `Cargo.toml` にも（`dev-dependencies`
//! も含めて）現れてはならない。`.github/workflows/architecture.yml` の
//! 「kaikei-e2e は誰からも依存されない」ステップが `cargo tree` でこれを
//! 検査する。依存される側に回った瞬間、「両方を知ってよい最上位の層」と
//! いうこのcrateの位置づけが崩れる。
//!
//! # ここに置いてよいもの・置いてはいけないもの
//!
//! - 置いてよい: 合成ルートが起動時に一度だけ行う**組み立て**
//!   （YAMLロード → policy 構築）を1箇所にまとめたヘルパ（[`compose`]）
//! - 置いてはいけない: 税額計算・按分・決算処理そのもの（それは
//!   `kaikei-jp` の責務）。この crate に業務ロジックを書き始めたら、それは
//!   本来 Phase 3 の `kaikei-mcp`（または Phase 4 の `kaikei-api`）に
//!   属するべきコードが紛れ込んでいるサインである
//!
//! # `JpStatementPolicy` の `chart` について（`DECISIONS.md` D-069）
//!
//! [`compose`] が返す [`Composition`] は `JpStatementPolicy` を**含まない**。
//!
//! `JpTaxPolicy`（年度別マスタ）や `JpSoleProprietorClosingPolicy`
//! （決算科目3つの実在検証）が保持するデータは YAML 由来で、変更するには
//! プロセス再起動が要る（`DECISIONS.md` D-025/D-057/D-066）。これらは
//! 起動時に一度組み立てて長期保持するのが自然である。
//!
//! 一方 `JpStatementPolicy` が保持する `chart` は**DBから読み直される
//! 可変データ**であり、`kaikei-app/src/context.rs` の
//! `load_posting_context` が記帳のたびに `tx.load_chart()` で読み直して
//! いるのと同じ性質を持つ（ユーザーが科目名を編集する経路が存在する）。
//! `JpStatementPolicy` を起動時に一度だけ構築して長期保持すると、
//! 「科目名を変更したのに決算書には古い名前が表示される」という
//! バグになりうる。
//!
//! `JpStatementPolicy::new` はYAML解釈や構築時検証を一切行わない単純な
//! ラッパ（`ChartOfAccounts` を保持するだけ）であり、構築コストは
//! 無視できるほど小さい。そのため方針は
//! **「決算書（BS/PL）を組み立てる直前に、その時点で読み込んだ `chart`
//! から都度 `JpStatementPolicy::new(chart)` する」**とし、`compose` の
//! 戻り値には含めない。呼び出し側（合成ルート）は決算書生成のリクエストの
//! たびに `chart` を読み直してから構築すること。

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use kaikei_core::{AccountingDate, ChartOfAccounts, TagSchema};
use kaikei_jp::chart;
use kaikei_jp::closing::{ClosingAccounts, JpSoleProprietorClosingPolicy};
use kaikei_jp::error::JpError;
use kaikei_jp::tags;
use kaikei_jp::tax::{JpSettings, JpSettingsOverrides, JpTaxPolicy, TaxRuleSets};

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
    pub tag_schema: TagSchema,
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
/// 2. `tags::load_embedded` → `TagSchema`
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
    let tag_schema = tags::load_embedded(kaikei_jp_data::TAGS)?;

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
        &tag_schema,
        options.closing_accounts,
        options.closing_tax_category,
    )?;

    Ok(Composition {
        chart,
        tag_schema,
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
            kaikei_jp::tax::TaxMode::Exclusive
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
