//! `kaikei-policy::TaxPolicy` の日本向け実装（[`JpTaxPolicy`]）と、
//! 事業者ごとの実設定（[`JpSettings`]）。
//!
//! `docs/04-jp-tax.md` §2・§7、`DECISIONS.md` D-025/D-026/D-057〜D-060 を参照。
//!
//! # 全メソッドが純関数であること（`CLAUDE.md` §3）
//!
//! [`JpTaxPolicy`] はマスタ（[`super::TaxRuleSets`]）と事業者設定
//! （[`JpSettings`]）を**構築時**に保持するだけで、`TaxPolicy` の各メソッドは
//! 引数（`ctx` / `tags` / `lines`）だけから結果を決定する。I/O・`Utc::now()`・
//! 内部可変状態は一切持たない。
//!
//! # `JpSettings` と `settings_defaults` の合成規則（`DECISIONS.md` D-057）
//!
//! [`JpSettings::compose`] が、特定1件のマスタが持つ `settings_defaults`
//! （[`super::TaxSettingsDefaults`]）を既定値とし、事業者が明示した
//! [`JpSettingsOverrides`] で上書きする。**この合成は `JpTaxPolicy` の構築時に
//! 一度だけ行われ、`TaxPolicy` の各メソッドが呼ばれるたびに
//! `ctx.as_of` に応じて別のマスタの `settings_defaults` を引き直すことはしない**
//! （[`TaxPolicy::round_mode`] が `ctx` を受け取りながら中身を見ないのはこのため）。
//! 消費税率改正で新旧マスタの `settings_defaults` が異なっていても、事業者が
//! 明示的に設定を切り替えない限り自動追従しない。理由・トレードオフは
//! `DECISIONS.md` D-057 を参照。

use crate::error::JpError;
use crate::tax::category::{TaxCategory, TaxDirection};
use crate::tax::rule_sets::TaxRuleSets;
use crate::tax::settings::{RoundingUnit, TaxMode, TaxSettingsDefaults};
use crate::tax::table::TaxCategoryTable;
use kaikei_core::{
    AccountCode, AccountDef, AccountingDate, JournalLine, Money, Ratio, RoundMode, Side, TagKey,
    TagSet, TagValue,
};
use kaikei_policy::{
    Counterparty, NoteSeverity, PolicyError, PolicyNote, TaxContext, TaxDerivation, TaxPolicy,
};
use rust_decimal::Decimal;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

/// 事業者ごとの実設定の上書き。`None` のフィールドは、合成時に渡された
/// [`TaxSettingsDefaults`] の値を使う（[`JpSettings::compose`]）。
///
/// `is_taxable_business` / `simplified_taxation` はマスタ側に対応する
/// 既定値が存在しない事業者固有の設定のため `Option` にしない
/// （「指定し忘れたら免税事業者扱いになる」ような事故を避けるため、
/// 呼び出し側に必ず明示させる。`Default` も意図的に実装しない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JpSettingsOverrides {
    /// 経理方式の上書き。
    pub tax_mode: Option<TaxMode>,
    /// 端数処理方式の上書き。
    pub rounding: Option<RoundMode>,
    /// 端数処理単位の上書き。
    pub rounding_unit: Option<RoundingUnit>,
    /// 課税事業者かどうか。
    pub is_taxable_business: bool,
    /// 簡易課税を選択しているかどうか。
    pub simplified_taxation: bool,
}

/// 事業者ごとの実設定。`JpTaxPolicy` が構築時に保持する（`TaxContext` には
/// 含めない。`docs/04-jp-tax.md` §2 / `DECISIONS.md` D-025）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JpSettings {
    /// 経理方式（税抜 / 税込）。
    pub tax_mode: TaxMode,
    /// 端数処理方式。
    pub rounding: RoundMode,
    /// 端数処理単位（明細ごと / 請求書単位）。
    pub rounding_unit: RoundingUnit,
    /// 課税事業者か免税事業者か。
    pub is_taxable_business: bool,
    /// 簡易課税を選択しているかどうか。
    pub simplified_taxation: bool,
}

impl JpSettings {
    /// `defaults`（特定1件のマスタの `settings_defaults`）を既定値として、
    /// `overrides` で明示された値だけを上書きして [`JpSettings`] を組み立てる。
    ///
    /// **`defaults` に渡すマスタの選び方は呼び出し側の責務。** 通常は
    /// 「合成ルートの起動時点で有効なマスタ」を渡すことを想定する。この関数
    /// 自体は特定の取引日を意識しない（`DECISIONS.md` D-057）。
    pub fn compose(defaults: TaxSettingsDefaults, overrides: JpSettingsOverrides) -> Self {
        JpSettings {
            tax_mode: overrides.tax_mode.unwrap_or(defaults.tax_mode),
            rounding: overrides.rounding.unwrap_or(defaults.rounding),
            rounding_unit: overrides.rounding_unit.unwrap_or(defaults.rounding_unit),
            is_taxable_business: overrides.is_taxable_business,
            simplified_taxation: overrides.simplified_taxation,
        }
    }
}

/// `kaikei-policy::TaxPolicy` の日本向け実装。
///
/// マスタ（[`TaxRuleSets`]）と事業者設定（[`JpSettings`]）を構築時に保持する。
/// `TaxContext::as_of`（取引日）で、その時点に適用されるマスタを選ぶ
/// （`CLAUDE.md` §7）。
#[derive(Debug, Clone)]
pub struct JpTaxPolicy {
    rule_sets: TaxRuleSets,
    settings: JpSettings,
}

impl JpTaxPolicy {
    /// マスタと事業者設定から構築する。
    pub fn new(rule_sets: TaxRuleSets, settings: JpSettings) -> Self {
        JpTaxPolicy {
            rule_sets,
            settings,
        }
    }

    /// 保持している事業者設定を返す。
    pub fn settings(&self) -> JpSettings {
        self.settings
    }

    /// 保持しているマスタ集合を返す。
    pub fn rule_sets(&self) -> &TaxRuleSets {
        &self.rule_sets
    }

    /// 取引日に適用されるマスタを引く。無ければ
    /// `PolicyError::NoApplicableRuleSet`（`DECISIONS.md` D-055 が定める
    /// `TaxRuleSets::for_date` の `None` を、ここで policy 層のエラーに写像する）。
    fn applicable_table(&self, as_of: AccountingDate) -> Result<&TaxCategoryTable, PolicyError> {
        self.rule_sets
            .for_date(as_of)
            .ok_or_else(|| PolicyError::NoApplicableRuleSet {
                as_of: as_of.to_iso_string(),
            })
    }
}

impl TaxPolicy for JpTaxPolicy {
    /// `tax_category` タグの妥当性を検証する。
    ///
    /// **`tax_category` の必須チェックはここでは行わない**
    /// （`DECISIONS.md` D-060）。`kaikei_core::TagSchema::validate` が
    /// `required_for`（Revenue/Expense）で既に強制しているため、ここで
    /// 重複して検証すると「タグが無い」ときのエラーメッセージが2種類存在する
    /// ことになり、どちらが実際に出るかが呼び出し順序に依存してしまう。
    ///
    /// **`direction` と科目種別の整合チェックも行わない**（例: 売上区分が
    /// 費用科目に付いている等）。課税仕入は費用科目だけでなく固定資産の取得
    /// （資産科目）にも付くなど、どの組み合わせが誤りかは税務ドメインの
    /// 判断を含むため実装しない（`kaikei-jp` の README・PR 説明に論点として
    /// 記録する）。
    fn validate_tag(
        &self,
        ctx: &TaxContext<'_>,
        tags: &TagSet,
        account: &AccountDef,
    ) -> Result<(), PolicyError> {
        let Some(TagValue::Code(code)) = tags.get(tax_category_key()) else {
            // タグが無い、またはコード以外の型（型不一致は core の責務）。
            // ここでは検証すべき対象が無いので何もしない。
            return Ok(());
        };

        let table = self.applicable_table(ctx.as_of)?;
        let category = table
            .category(code)
            .map_err(|source| unknown_category_error(source, ctx.as_of))?;

        if category.requires_qualified_invoice {
            if let Some(TagValue::Code(counterparty_code)) = tags.get(counterparty_key()) {
                if let Some(counterparty) = ctx.counterparties.get(counterparty_code) {
                    // `Some(false)`（明示的に非適格と記録されている）のみ拒否する。
                    // `None`（未確認）はユーザーがまだ調べていないだけかもしれず、
                    // こちらが断定してはいけない（`CLAUDE.md` §10）。
                    if counterparty.is_qualified_invoice_issuer == Some(false) {
                        return Err(unqualified_invoice_error(
                            table,
                            category,
                            counterparty,
                            counterparty_code,
                            account,
                        ));
                    }
                }
                // 取引先マスタに存在しないコードは、ここでは「判定不能」として
                // 通す（このタグ単体の検証はこのメソッドの責務であり、取引先
                // コードそのものの存在検証は別の関心事とする）。
            }
        }

        Ok(())
    }

    /// 税抜経理での消費税行を導出する（`docs/04-jp-tax.md` §7）。
    fn derive_tax_lines(
        &self,
        ctx: &TaxContext<'_>,
        lines: &[JournalLine],
    ) -> Result<TaxDerivation, PolicyError> {
        let table = self.applicable_table(ctx.as_of)?;

        let mut notes = Vec::new();
        if self.settings.simplified_taxation {
            notes.push(simplified_taxation_note());
        }

        // 税込経理、または免税事業者は税行を生成しない（`docs/04-jp-tax.md` §7）。
        if self.settings.tax_mode == TaxMode::Inclusive || !self.settings.is_taxable_business {
            return Ok(TaxDerivation {
                lines: lines.to_vec(),
                notes,
            });
        }

        let category_key = tax_category_key();
        let mut noted_deduction_codes: BTreeSet<String> = BTreeSet::new();
        let mut contributions: Vec<Contribution<'_>> = Vec::new();

        for line in lines {
            let Some(TagValue::Code(code)) = line.tags().get(category_key) else {
                continue;
            };
            let category = table
                .category(code)
                .map_err(|source| unknown_category_error(source, ctx.as_of))?;

            // 非適格の経過措置（deduction_ratio < 1）は税額計算に反映しない。
            // 控除できない部分の帳簿上の処理は税務判断を含むため実装せず、
            // 断定しない注記のみを添える（`DECISIONS.md` D-059）。
            if let Some(ratio) = category.deduction_ratio {
                if ratio.as_decimal() < Decimal::ONE
                    && noted_deduction_codes.insert(category.code.clone())
                {
                    notes.push(deduction_ratio_note(category, ratio));
                }
            }

            if !matches!(
                category.direction,
                TaxDirection::Sales | TaxDirection::Purchase
            ) {
                continue;
            }
            let Some(rate) = category.rate else {
                continue;
            };
            if rate.as_decimal() == Decimal::ZERO {
                continue;
            }
            // `TaxCategoryTable::new` の構築時検証により、direction が
            // sales/purchase かつ rate が 0 でない区分は tax_account を必ず持つ。
            let tax_account = category.tax_account.clone().expect(
                "direction が sales/purchase かつ rate != 0 の区分は \
                 TaxCategoryTable::new の構築時検証により tax_account を必ず持つ",
            );

            contributions.push(Contribution {
                category_code: category.code.as_str(),
                tax_account,
                rate,
                side: line.side(),
                amount: *line.amount(),
            });
        }

        let mut output_lines = lines.to_vec();

        match self.settings.rounding_unit {
            // 明細ごとに端数処理する。
            RoundingUnit::Line => {
                for c in &contributions {
                    let tax_amount = self.apply_ratio(ctx, c.amount, c.rate)?;
                    if tax_amount.is_zero() {
                        // 税額 0 の行は生成しない（`JournalLine::new` が 0 円を拒否する）。
                        continue;
                    }
                    output_lines.push(new_tax_line(
                        c.tax_account.clone(),
                        c.side,
                        tax_amount,
                        c.category_code,
                    )?);
                }
            }
            // 同じ（税区分, 側, 税額科目）の本体を先に合算してから1回だけ丸める。
            //
            // 各明細に按分し直して端数を分配する実装にはしない。Phase 1 で
            // 「明細ごとに丸めると合計が1円ずれる」バグを踏んでおり
            // （`PROGRESS.md` Phase 0 の教訓・`DECISIONS.md` D-058）、
            // 合算してから1回丸めた結果をそのまま1行として計上する方が
            // 単純かつ安全。
            RoundingUnit::Document => {
                // グループの値に `rate` も一緒に持たせる。`table.category(&code)` を
                // 引き直して `rate.expect(..)` するより、contribution の時点で
                // `Some` が確定している値をそのまま運ぶ方が panic 経路が1つ減る
                // （レビュー指摘）。同じ税区分コードなら `rate` も必ず同じ。
                let mut groups: BTreeMap<(String, bool, AccountCode), (Money, Ratio)> =
                    BTreeMap::new();
                for c in &contributions {
                    let is_debit = matches!(c.side, Side::Debit);
                    let group_key = (c.category_code.to_string(), is_debit, c.tax_account.clone());
                    let entry = groups
                        .entry(group_key)
                        .or_insert_with(|| (Money::zero(c.amount.currency()), c.rate));
                    entry.0 = entry.0.add(&c.amount)?;
                }
                for ((code, is_debit, tax_account), (base_total, rate)) in groups {
                    let tax_amount = self.apply_ratio(ctx, base_total, rate)?;
                    if tax_amount.is_zero() {
                        continue;
                    }
                    let side = if is_debit { Side::Debit } else { Side::Credit };
                    output_lines.push(new_tax_line(tax_account, side, tax_amount, &code)?);
                }
            }
        }

        Ok(TaxDerivation {
            lines: output_lines,
            notes,
        })
    }

    fn round_mode(&self, _ctx: &TaxContext<'_>) -> RoundMode {
        self.settings.rounding
    }
}

/// [`JpTaxPolicy::derive_tax_lines`] が税額計算の対象と判定した明細1件分の情報。
struct Contribution<'a> {
    category_code: &'a str,
    tax_account: AccountCode,
    rate: Ratio,
    side: Side,
    amount: Money,
}

/// `validate_tag` は `post_entry` から**明細ごとに**呼ばれる
/// （`kaikei-app/src/usecase/post_entry.rs`）。固定文字列の `TagKey` を
/// 毎回パース・アロケーションしないよう一度だけ作って使い回す（レビュー指摘）。
fn tax_category_key() -> &'static TagKey {
    static KEY: OnceLock<TagKey> = OnceLock::new();
    KEY.get_or_init(|| {
        TagKey::parse("tax_category")
            .expect("\"tax_category\" は tags.yaml に登録された既知のタグキー")
    })
}

fn counterparty_key() -> &'static TagKey {
    static KEY: OnceLock<TagKey> = OnceLock::new();
    KEY.get_or_init(|| {
        TagKey::parse("counterparty")
            .expect("\"counterparty\" は tags.yaml に登録された既知のタグキー")
    })
}

/// 生成した税額行を作る。元の `tax_category` タグを付ける（集計のため。
/// `docs/04-jp-tax.md` §7）。
fn new_tax_line(
    account: AccountCode,
    side: Side,
    amount: Money,
    category_code: &str,
) -> Result<JournalLine, PolicyError> {
    let mut tags = TagSet::new();
    tags.insert(
        tax_category_key().clone(),
        TagValue::Code(category_code.to_string()),
    );
    Ok(JournalLine::new(account, side, amount, tags, None)?)
}

/// `TaxCategoryTable::category` の失敗（`JpError::UnknownTaxCategoryCode`）を
/// `PolicyError::UnknownTaxCategory` に写像する。有効なコード一覧をそのまま運ぶ
/// （`CLAUDE.md` §11: 次の手が分かる文言にする）。
fn unknown_category_error(source: JpError, as_of: AccountingDate) -> PolicyError {
    match source {
        JpError::UnknownTaxCategoryCode {
            code, available, ..
        } => PolicyError::UnknownTaxCategory {
            code,
            as_of: as_of.to_iso_string(),
            available,
        },
        // `TaxCategoryTable::category` はこの変種以外を返さない。将来
        // 変更された場合に panic させず、原因を保持したまま呼び出し側へ
        // 伝える（`InvalidPolicyData` は「構築時に受け取ったデータが不正」
        // という趣旨に近い唯一の汎用バリアント）。
        other => PolicyError::InvalidPolicyData {
            reason: format!(
                "税区分の解決中に想定外のエラーが発生しました \
                 （TaxCategoryTable::category は通常 UnknownTaxCategoryCode のみを返す）: {other}"
            ),
        },
    }
}

/// 適格請求書の保存が必要な税区分に、明示的に非適格と記録された取引先が
/// 紐づいているエラーを組み立てる。経過措置用の候補区分（同じ税率で
/// `requires_qualified_invoice: false` の区分）を挙げるが、「これを使うべき」
/// とは断定しない（`CLAUDE.md` §10）。
fn unqualified_invoice_error(
    table: &TaxCategoryTable,
    category: &TaxCategory,
    counterparty: &Counterparty,
    counterparty_code: &str,
    account: &AccountDef,
) -> PolicyError {
    let mut candidates: Vec<&str> = table
        .categories()
        .filter(|c| !c.requires_qualified_invoice && c.rate == category.rate)
        .map(|c| c.code.as_str())
        .collect();
    candidates.sort_unstable();
    let candidates_display = if candidates.is_empty() {
        "見つかりませんでした".to_string()
    } else {
        candidates.join(", ")
    };

    PolicyError::TaxCategoryNotApplicable {
        account: account.code.as_str().to_string(),
        code: category.code.clone(),
        reason: format!(
            "取引先 {}（コード: {counterparty_code}）は適格請求書発行事業者ではないと\
             記録されています。税区分 \"{}\"（{}）は適格請求書の保存が必要な区分です。\
             同じ税率で適格請求書を要求しない区分の候補: {candidates_display}。\
             どの区分を使うべきかは税理士にご確認ください",
            counterparty.name, category.code, category.label,
        ),
    }
}

/// 非適格の経過措置（`deduction_ratio < 1`）が設定された区分が使われたことを
/// 知らせる注記。控除できない部分の帳簿上の処理はこの実装では行っていない
/// ことと、判断は税理士に確認すべきことを明記する（断定はしない。
/// `CLAUDE.md` §10、`DECISIONS.md` D-059）。
fn deduction_ratio_note(category: &TaxCategory, ratio: Ratio) -> PolicyNote {
    PolicyNote {
        severity: NoteSeverity::Warning,
        message: format!(
            "税区分 \"{}\"（{}）には控除割合 {} が設定されています（インボイス制度の\
             経過措置）。控除できない部分を帳簿上どう扱うか（例: 仮払消費税を減らして\
             本体に上乗せする等）はこの実装では行っておらず、税額は rate のみで計算して\
             います。適用可否・処理方法は税理士にご確認ください（docs/08-compliance.md §9-1）",
            category.code,
            category.label,
            ratio.as_decimal(),
        ),
    }
}

/// 簡易課税が設定されていることを知らせる注記。みなし仕入率による計算は
/// 未実装であり、この設定を保持するだけで `derive_tax_lines` の挙動を変えない
/// ことを明記する（断定はしない。`CLAUDE.md` §10）。
fn simplified_taxation_note() -> PolicyNote {
    PolicyNote {
        severity: NoteSeverity::Warning,
        message: "簡易課税が設定されています。みなし仕入率による消費税額の計算は\
                   この実装では行っておらず、区分ごとの rate に基づく通常の計算と\
                   同じ結果を返します。簡易課税を適用する場合の取り扱いは\
                   税理士にご確認ください"
            .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_core::{AccountType, ChartOfAccounts, Currency, TagSchema};
    use kaikei_policy::CounterpartyIndex;
    use proptest::prelude::*;

    // ---- フィクスチャ ----

    fn default_settings() -> JpSettings {
        JpSettings {
            tax_mode: TaxMode::Exclusive,
            rounding: RoundMode::Floor,
            rounding_unit: RoundingUnit::Line,
            is_taxable_business: true,
            simplified_taxation: false,
        }
    }

    /// `kaikei-jp-data/tax/jp/2026.yaml`（実データ）から構築した `JpTaxPolicy`。
    fn policy_from_embedded(settings: JpSettings) -> JpTaxPolicy {
        let rule_sets = TaxRuleSets::from_embedded().unwrap();
        JpTaxPolicy::new(rule_sets, settings)
    }

    fn empty_chart() -> ChartOfAccounts {
        ChartOfAccounts::new(vec![]).unwrap()
    }

    fn empty_schema() -> TagSchema {
        TagSchema::empty()
    }

    fn account_def(code: &str, account_type: AccountType) -> AccountDef {
        AccountDef {
            code: AccountCode::parse(code).unwrap(),
            name: format!("acct-{code}"),
            account_type,
            parent: None,
            postable: true,
        }
    }

    fn tags_with_category(code: &str) -> TagSet {
        let mut tags = TagSet::new();
        tags.insert(tax_category_key().clone(), TagValue::Code(code.to_string()));
        tags
    }

    fn tags_with_category_and_counterparty(code: &str, counterparty_code: &str) -> TagSet {
        let mut tags = tags_with_category(code);
        tags.insert(
            counterparty_key().clone(),
            TagValue::Code(counterparty_code.to_string()),
        );
        tags
    }

    fn line(account: &str, side: Side, amount_minor: i128, tags: TagSet) -> JournalLine {
        JournalLine::new(
            AccountCode::parse(account).unwrap(),
            side,
            Money::from_minor(amount_minor, Currency::JPY),
            tags,
            None,
        )
        .unwrap()
    }

    fn counterparty(code: &str, is_qualified: Option<bool>) -> Counterparty {
        Counterparty {
            code: code.to_string(),
            name: format!("取引先{code}"),
            invoice_registration_no: None,
            is_qualified_invoice_issuer: is_qualified,
        }
    }

    /// 2026年度マスタの適用期間内の取引日（2026-04-01）を使う context を作る。
    macro_rules! embedded_context {
        ($chart:expr, $schema:expr, $counterparties:expr) => {
            TaxContext {
                as_of: AccountingDate::new(2026, 4, 1).unwrap(),
                chart: &$chart,
                tag_schema: &$schema,
                counterparties: &$counterparties,
            }
        };
    }

    // ---- derive_tax_lines: docs/04-jp-tax.md §7 の例 ----

    #[test]
    fn derive_tax_lines_reproduces_the_docs_section_7_example() {
        let policy = policy_from_embedded(default_settings());
        let chart = empty_chart();
        let schema = empty_schema();
        let counterparties = CounterpartyIndex::empty();
        let ctx = embedded_context!(chart, schema, counterparties);

        // 売掛金 110,000（借） / 売上高 100,000（貸, tax_category=SALES_10）
        let input = vec![
            line("130", Side::Debit, 110_000, TagSet::new()),
            line("500", Side::Credit, 100_000, tags_with_category("SALES_10")),
        ];

        let derivation = policy.derive_tax_lines(&ctx, &input).unwrap();
        assert!(derivation.notes.is_empty());
        assert_eq!(derivation.lines.len(), 3);

        // 売掛金 110,000（借） / 売上高 100,000（貸） + 仮受消費税 10,000（貸）
        let generated = derivation
            .lines
            .iter()
            .find(|l| l.account().as_str() == "330")
            .expect("仮受消費税(330)の行が生成されているはず");
        assert_eq!(generated.side(), Side::Credit);
        assert_eq!(generated.amount().minor(), 10_000);
        assert_eq!(
            generated.tags().get(tax_category_key()),
            Some(&TagValue::Code("SALES_10".to_string()))
        );

        let debit_total: i128 = derivation
            .lines
            .iter()
            .filter(|l| l.is_debit())
            .map(|l| l.amount().minor())
            .sum();
        let credit_total: i128 = derivation
            .lines
            .iter()
            .filter(|l| !l.is_debit())
            .map(|l| l.amount().minor())
            .sum();
        assert_eq!(debit_total, credit_total);
    }

    // ---- derive_tax_lines: direction / rate のバリエーション ----

    #[test]
    fn derive_tax_lines_handles_10_percent_8_percent_export_and_out_of_scope_categories() {
        let policy = policy_from_embedded(default_settings());
        let chart = empty_chart();
        let schema = empty_schema();
        let counterparties = CounterpartyIndex::empty();
        let ctx = embedded_context!(chart, schema, counterparties);

        let input = vec![
            line("500", Side::Credit, 100_000, tags_with_category("SALES_10")),
            line(
                "500",
                Side::Credit,
                50_000,
                tags_with_category("SALES_8_REDUCED"),
            ),
            // 免税売上（rate 0）は税額行を生成しない。
            line(
                "500",
                Side::Credit,
                30_000,
                tags_with_category("SALES_EXPORT"),
            ),
            // 非課税・不課税・対象外（direction: none）も税額行を生成しない。
            line("500", Side::Credit, 5_000, tags_with_category("TAX_FREE")),
            line(
                "500",
                Side::Credit,
                5_000,
                tags_with_category("OUT_OF_SCOPE"),
            ),
            line(
                "500",
                Side::Credit,
                5_000,
                tags_with_category("NOT_APPLICABLE"),
            ),
        ];

        let derivation = policy.derive_tax_lines(&ctx, &input).unwrap();
        // 入力6行 + 税額行2行（10% 分・8% 分）。
        assert_eq!(derivation.lines.len(), 8);

        let generated: Vec<_> = derivation
            .lines
            .iter()
            .filter(|l| l.account().as_str() == "330")
            .collect();
        assert_eq!(generated.len(), 2);
        let total: i128 = generated.iter().map(|l| l.amount().minor()).sum();
        assert_eq!(total, 10_000 + 4_000);
    }

    #[test]
    fn derive_tax_lines_purchase_qualified_generates_input_tax_credit() {
        let policy = policy_from_embedded(default_settings());
        let chart = empty_chart();
        let schema = empty_schema();
        let counterparties = CounterpartyIndex::empty();
        let ctx = embedded_context!(chart, schema, counterparties);

        let input = vec![line(
            "600",
            Side::Debit,
            50_000,
            tags_with_category("PURCHASE_10_QUALIFIED"),
        )];

        let derivation = policy.derive_tax_lines(&ctx, &input).unwrap();
        assert!(derivation.notes.is_empty());
        let generated = derivation
            .lines
            .iter()
            .find(|l| l.account().as_str() == "180")
            .expect("仮払消費税(180)の行が生成されているはず");
        assert_eq!(generated.side(), Side::Debit);
        assert_eq!(generated.amount().minor(), 5_000);
    }

    // ---- 税込経理・免税事業者 ----

    #[test]
    fn derive_tax_lines_inclusive_tax_mode_returns_input_unchanged() {
        let settings = JpSettings {
            tax_mode: TaxMode::Inclusive,
            ..default_settings()
        };
        let policy = policy_from_embedded(settings);
        let chart = empty_chart();
        let schema = empty_schema();
        let counterparties = CounterpartyIndex::empty();
        let ctx = embedded_context!(chart, schema, counterparties);

        let input = vec![
            line("130", Side::Debit, 110_000, TagSet::new()),
            line("500", Side::Credit, 100_000, tags_with_category("SALES_10")),
        ];
        let derivation = policy.derive_tax_lines(&ctx, &input).unwrap();
        assert_eq!(derivation.lines.len(), 2);
    }

    #[test]
    fn derive_tax_lines_non_taxable_business_returns_input_unchanged() {
        let settings = JpSettings {
            is_taxable_business: false,
            ..default_settings()
        };
        let policy = policy_from_embedded(settings);
        let chart = empty_chart();
        let schema = empty_schema();
        let counterparties = CounterpartyIndex::empty();
        let ctx = embedded_context!(chart, schema, counterparties);

        let input = vec![
            line("130", Side::Debit, 110_000, TagSet::new()),
            line("500", Side::Credit, 100_000, tags_with_category("SALES_10")),
        ];
        let derivation = policy.derive_tax_lines(&ctx, &input).unwrap();
        assert_eq!(derivation.lines.len(), 2);
    }

    // ---- rounding_unit: Line と Document で結果が変わること ----

    #[test]
    fn derive_tax_lines_rounding_unit_line_vs_document_produce_different_totals() {
        let chart = empty_chart();
        let schema = empty_schema();
        let counterparties = CounterpartyIndex::empty();
        let ctx = embedded_context!(chart, schema, counterparties);

        // 15円 * 10% = 1.5円。floor で明細ごとに丸めると 1円 + 1円 = 2円。
        // 合算してから丸めると (15+15)*10% = 3.0円 → 3円。
        let input = vec![
            line(
                "600",
                Side::Debit,
                15,
                tags_with_category("PURCHASE_10_QUALIFIED"),
            ),
            line(
                "600",
                Side::Debit,
                15,
                tags_with_category("PURCHASE_10_QUALIFIED"),
            ),
        ];

        let line_policy = policy_from_embedded(JpSettings {
            rounding_unit: RoundingUnit::Line,
            ..default_settings()
        });
        let line_derivation = line_policy.derive_tax_lines(&ctx, &input).unwrap();
        let line_tax_lines: Vec<_> = line_derivation
            .lines
            .iter()
            .filter(|l| l.account().as_str() == "180")
            .collect();
        assert_eq!(line_tax_lines.len(), 2);
        assert!(line_tax_lines.iter().all(|l| l.amount().minor() == 1));
        let line_total: i128 = line_tax_lines.iter().map(|l| l.amount().minor()).sum();
        assert_eq!(line_total, 2);

        let document_policy = policy_from_embedded(JpSettings {
            rounding_unit: RoundingUnit::Document,
            ..default_settings()
        });
        let document_derivation = document_policy.derive_tax_lines(&ctx, &input).unwrap();
        let document_tax_lines: Vec<_> = document_derivation
            .lines
            .iter()
            .filter(|l| l.account().as_str() == "180")
            .collect();
        assert_eq!(document_tax_lines.len(), 1);
        assert_eq!(document_tax_lines[0].amount().minor(), 3);

        assert_ne!(line_total, document_tax_lines[0].amount().minor());
    }

    /// 売上区分（貸方）と仕入区分（借方）が同一仕訳に混在しても、
    /// それぞれ独立した税額行になること。
    ///
    /// `Document` 丸めのグルーピングキーは（税区分, 側, 税額科目）なので、
    /// side を含めていないと売上と仕入が同じグループに落ちて相殺されうる。
    /// 2回目レビューが検証のために手で書いたケースを恒久テストにした。
    #[test]
    fn derive_tax_lines_separates_sales_and_purchase_in_the_same_entry() {
        let settings = JpSettings {
            rounding_unit: RoundingUnit::Document,
            ..default_settings()
        };
        let policy = policy_from_embedded(settings);
        let chart = empty_chart();
        let schema = empty_schema();
        let counterparties = CounterpartyIndex::empty();
        let ctx = embedded_context!(chart, schema, counterparties);

        let input = vec![
            // 売上 100,000（貸, 10%） → 仮受消費税 10,000（貸, 330）
            line("500", Side::Credit, 100_000, tags_with_category("SALES_10")),
            // 仕入 50,000（借, 10%適格） → 仮払消費税 5,000（借, 180）
            line(
                "600",
                Side::Debit,
                50_000,
                tags_with_category("PURCHASE_10_QUALIFIED"),
            ),
        ];

        let derivation = policy.derive_tax_lines(&ctx, &input).unwrap();

        let sales_tax = derivation
            .lines
            .iter()
            .find(|l| l.account().as_str() == "330")
            .expect("仮受消費税(330)が生成されるはず");
        assert_eq!(sales_tax.side(), Side::Credit);
        assert_eq!(sales_tax.amount().minor(), 10_000);

        let purchase_tax = derivation
            .lines
            .iter()
            .find(|l| l.account().as_str() == "180")
            .expect("仮払消費税(180)が生成されるはず");
        assert_eq!(purchase_tax.side(), Side::Debit);
        assert_eq!(purchase_tax.amount().minor(), 5_000);
    }

    /// 同じ税区分が借方・貸方の両方に現れたら、側ごとに別の税額行になること
    /// （売上と売上値引・返品が同一仕訳に混在する形）。
    ///
    /// グルーピングキーから side が抜けると 100,000 - 50,000 = 50,000 に
    /// 相殺され、税額が 5,000 の1行になってしまう。
    #[test]
    fn derive_tax_lines_separates_the_same_category_by_side() {
        let settings = JpSettings {
            rounding_unit: RoundingUnit::Document,
            ..default_settings()
        };
        let policy = policy_from_embedded(settings);
        let chart = empty_chart();
        let schema = empty_schema();
        let counterparties = CounterpartyIndex::empty();
        let ctx = embedded_context!(chart, schema, counterparties);

        let input = vec![
            line("500", Side::Credit, 100_000, tags_with_category("SALES_10")),
            line("500", Side::Debit, 50_000, tags_with_category("SALES_10")),
        ];

        let derivation = policy.derive_tax_lines(&ctx, &input).unwrap();

        let generated: Vec<_> = derivation
            .lines
            .iter()
            .filter(|l| l.account().as_str() == "330")
            .collect();
        assert_eq!(
            generated.len(),
            2,
            "側ごとに分かれて2行になるはず（相殺されていないか）: {generated:?}"
        );

        let credit = generated
            .iter()
            .find(|l| !l.is_debit())
            .expect("貸方の税額行");
        let debit = generated
            .iter()
            .find(|l| l.is_debit())
            .expect("借方の税額行");
        assert_eq!(credit.amount().minor(), 10_000);
        assert_eq!(debit.amount().minor(), 5_000);
    }

    /// 生成される税額行の順序が実行のたびに変わらないこと。
    ///
    /// 順序が非決定的だと、`kaikei-store` が `lines().iter().enumerate()` で
    /// 採番する `line_no` が実行ごとに変わり、append-only の帳簿に
    /// 「同じ仕訳なのに明細順が違う」記録が残りうる。`BTreeMap` を使って
    /// いるので決定的なはずだが、`HashMap` に変えられたら落ちるようにしておく。
    #[test]
    fn derive_tax_lines_output_order_is_deterministic() {
        let settings = JpSettings {
            rounding_unit: RoundingUnit::Document,
            ..default_settings()
        };
        let policy = policy_from_embedded(settings);
        let chart = empty_chart();
        let schema = empty_schema();
        let counterparties = CounterpartyIndex::empty();
        let ctx = embedded_context!(chart, schema, counterparties);

        let input = vec![
            line("500", Side::Credit, 100_003, tags_with_category("SALES_10")),
            line(
                "500",
                Side::Credit,
                50_007,
                tags_with_category("SALES_8_REDUCED"),
            ),
            line(
                "600",
                Side::Debit,
                30_011,
                tags_with_category("PURCHASE_10_QUALIFIED"),
            ),
            line(
                "600",
                Side::Debit,
                20_013,
                tags_with_category("PURCHASE_8_REDUCED_QUALIFIED"),
            ),
        ];

        let fingerprint = |d: &TaxDerivation| -> Vec<(String, i128, bool)> {
            d.lines
                .iter()
                .map(|l| {
                    (
                        l.account().as_str().to_string(),
                        l.amount().minor(),
                        l.is_debit(),
                    )
                })
                .collect()
        };

        let first = fingerprint(&policy.derive_tax_lines(&ctx, &input).unwrap());
        for i in 1..20 {
            let again = fingerprint(&policy.derive_tax_lines(&ctx, &input).unwrap());
            assert_eq!(again, first, "{i}回目の実行で明細の順序・内容が変わった");
        }
    }

    #[test]
    fn derive_tax_lines_document_rounding_keeps_different_categories_separate() {
        let settings = JpSettings {
            rounding_unit: RoundingUnit::Document,
            ..default_settings()
        };
        let policy = policy_from_embedded(settings);
        let chart = empty_chart();
        let schema = empty_schema();
        let counterparties = CounterpartyIndex::empty();
        let ctx = embedded_context!(chart, schema, counterparties);

        // 同じ側（貸方）・同じ税額科目（330）だが税区分が異なるため、
        // Document 集計でも1行にまとめられてはならない。
        let input = vec![
            line("500", Side::Credit, 100_000, tags_with_category("SALES_10")),
            line(
                "500",
                Side::Credit,
                50_000,
                tags_with_category("SALES_8_REDUCED"),
            ),
        ];
        let derivation = policy.derive_tax_lines(&ctx, &input).unwrap();
        let generated: Vec<_> = derivation
            .lines
            .iter()
            .filter(|l| l.account().as_str() == "330")
            .collect();
        assert_eq!(generated.len(), 2);
        let total: i128 = generated.iter().map(|l| l.amount().minor()).sum();
        assert_eq!(total, 10_000 + 4_000);
    }

    // ---- 税額0の行は生成しない ----

    #[test]
    fn derive_tax_lines_zero_tax_amount_generates_no_line() {
        let policy = policy_from_embedded(default_settings());
        let chart = empty_chart();
        let schema = empty_schema();
        let counterparties = CounterpartyIndex::empty();
        let ctx = embedded_context!(chart, schema, counterparties);

        // floor(1 * 0.10) = 0
        let input = vec![line("500", Side::Credit, 1, tags_with_category("SALES_10"))];
        let derivation = policy.derive_tax_lines(&ctx, &input).unwrap();
        assert_eq!(derivation.lines.len(), 1);
    }

    // ---- 非適格の経過措置（deduction_ratio < 1）----

    #[test]
    fn derive_tax_lines_non_qualified_purchase_uses_rate_only_and_adds_warning_note() {
        let policy = policy_from_embedded(default_settings());
        let chart = empty_chart();
        let schema = empty_schema();
        let counterparties = CounterpartyIndex::empty();
        let ctx = embedded_context!(chart, schema, counterparties);

        let input = vec![line(
            "600",
            Side::Debit,
            100_000,
            tags_with_category("PURCHASE_10_NON_QUALIFIED"),
        )];
        let derivation = policy.derive_tax_lines(&ctx, &input).unwrap();

        let generated = derivation
            .lines
            .iter()
            .find(|l| l.account().as_str() == "180")
            .unwrap();
        // deduction_ratio (0.80) を反映せず rate (0.10) のみで計算する。
        assert_eq!(generated.amount().minor(), 10_000);

        assert_eq!(derivation.notes.len(), 1);
        assert_eq!(derivation.notes[0].severity, NoteSeverity::Warning);
        assert!(derivation.notes[0]
            .message
            .contains("PURCHASE_10_NON_QUALIFIED"));
    }

    #[test]
    fn derive_tax_lines_deduplicates_deduction_ratio_notes_for_the_same_category() {
        let policy = policy_from_embedded(default_settings());
        let chart = empty_chart();
        let schema = empty_schema();
        let counterparties = CounterpartyIndex::empty();
        let ctx = embedded_context!(chart, schema, counterparties);

        let input = vec![
            line(
                "600",
                Side::Debit,
                10_000,
                tags_with_category("PURCHASE_10_NON_QUALIFIED"),
            ),
            line(
                "600",
                Side::Debit,
                20_000,
                tags_with_category("PURCHASE_10_NON_QUALIFIED"),
            ),
        ];
        let derivation = policy.derive_tax_lines(&ctx, &input).unwrap();
        assert_eq!(derivation.notes.len(), 1);
    }

    // ---- 簡易課税 ----

    #[test]
    fn derive_tax_lines_simplified_taxation_adds_note_without_changing_lines() {
        let chart = empty_chart();
        let schema = empty_schema();
        let counterparties = CounterpartyIndex::empty();
        let ctx = embedded_context!(chart, schema, counterparties);
        let input = vec![
            line("130", Side::Debit, 110_000, TagSet::new()),
            line("500", Side::Credit, 100_000, tags_with_category("SALES_10")),
        ];

        let without = policy_from_embedded(default_settings())
            .derive_tax_lines(&ctx, &input)
            .unwrap();
        let with = policy_from_embedded(JpSettings {
            simplified_taxation: true,
            ..default_settings()
        })
        .derive_tax_lines(&ctx, &input)
        .unwrap();

        assert!(without.notes.is_empty());
        assert_eq!(with.notes.len(), 1);
        assert_eq!(with.notes[0].severity, NoteSeverity::Warning);
        // 簡易課税は「設定を保持するだけ」で明細の生成結果自体は変えない。
        assert_eq!(with.lines.len(), without.lines.len());
        for (a, b) in with.lines.iter().zip(without.lines.iter()) {
            assert_eq!(a.amount().minor(), b.amount().minor());
            assert_eq!(a.account(), b.account());
        }
    }

    #[test]
    fn derive_tax_lines_simplified_taxation_note_present_even_when_no_tax_lines_are_generated() {
        let settings = JpSettings {
            tax_mode: TaxMode::Inclusive,
            simplified_taxation: true,
            ..default_settings()
        };
        let policy = policy_from_embedded(settings);
        let chart = empty_chart();
        let schema = empty_schema();
        let counterparties = CounterpartyIndex::empty();
        let ctx = embedded_context!(chart, schema, counterparties);

        let derivation = policy.derive_tax_lines(&ctx, &[]).unwrap();
        assert_eq!(derivation.notes.len(), 1);
        assert_eq!(derivation.notes[0].severity, NoteSeverity::Warning);
    }

    // ---- 適用マスタが無い取引日 ----

    fn out_of_range_date() -> AccountingDate {
        // 2026.yaml の適用開始日（2026-01-01）より前。
        AccountingDate::new(2000, 1, 1).unwrap()
    }

    #[test]
    fn derive_tax_lines_no_applicable_rule_set_is_error() {
        let policy = policy_from_embedded(default_settings());
        let chart = empty_chart();
        let schema = empty_schema();
        let counterparties = CounterpartyIndex::empty();
        let ctx = TaxContext {
            as_of: out_of_range_date(),
            chart: &chart,
            tag_schema: &schema,
            counterparties: &counterparties,
        };

        let err = policy.derive_tax_lines(&ctx, &[]).unwrap_err();
        assert!(matches!(err, PolicyError::NoApplicableRuleSet { .. }));
    }

    #[test]
    fn validate_tag_no_applicable_rule_set_with_tax_category_tag_is_error() {
        let policy = policy_from_embedded(default_settings());
        let chart = empty_chart();
        let schema = empty_schema();
        let counterparties = CounterpartyIndex::empty();
        let ctx = TaxContext {
            as_of: out_of_range_date(),
            chart: &chart,
            tag_schema: &schema,
            counterparties: &counterparties,
        };
        let tags = tags_with_category("SALES_10");
        let account = account_def("500", AccountType::Revenue);

        let err = policy.validate_tag(&ctx, &tags, &account).unwrap_err();
        assert!(matches!(err, PolicyError::NoApplicableRuleSet { .. }));
    }

    #[test]
    fn validate_tag_no_applicable_rule_set_without_tax_category_tag_passes() {
        let policy = policy_from_embedded(default_settings());
        let chart = empty_chart();
        let schema = empty_schema();
        let counterparties = CounterpartyIndex::empty();
        let ctx = TaxContext {
            as_of: out_of_range_date(),
            chart: &chart,
            tag_schema: &schema,
            counterparties: &counterparties,
        };
        let account = account_def("130", AccountType::Asset);

        // tax_category タグが無い明細には検証対象が無いので、適用マスタの
        // 有無に関わらず通す。
        assert!(policy.validate_tag(&ctx, &TagSet::new(), &account).is_ok());
    }

    // ---- 未知の税区分コード ----

    #[test]
    fn validate_tag_unknown_tax_category_code_is_error_with_available_codes() {
        let policy = policy_from_embedded(default_settings());
        let chart = empty_chart();
        let schema = empty_schema();
        let counterparties = CounterpartyIndex::empty();
        let ctx = embedded_context!(chart, schema, counterparties);
        let tags = tags_with_category("NOPE");
        let account = account_def("500", AccountType::Revenue);

        let err = policy.validate_tag(&ctx, &tags, &account).unwrap_err();
        match err {
            PolicyError::UnknownTaxCategory {
                code, available, ..
            } => {
                assert_eq!(code, "NOPE");
                assert!(available.contains("SALES_10"), "available = {available}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn derive_tax_lines_unknown_tax_category_code_is_error() {
        let policy = policy_from_embedded(default_settings());
        let chart = empty_chart();
        let schema = empty_schema();
        let counterparties = CounterpartyIndex::empty();
        let ctx = embedded_context!(chart, schema, counterparties);
        let input = vec![line(
            "500",
            Side::Credit,
            100_000,
            tags_with_category("NOPE"),
        )];

        let err = policy.derive_tax_lines(&ctx, &input).unwrap_err();
        assert!(matches!(err, PolicyError::UnknownTaxCategory { .. }));
    }

    // ---- requires_qualified_invoice と取引先の整合 ----

    #[test]
    fn validate_tag_requires_qualified_invoice_rejects_explicitly_unqualified_counterparty() {
        let policy = policy_from_embedded(default_settings());
        let chart = empty_chart();
        let schema = empty_schema();
        let counterparties = CounterpartyIndex::new(vec![counterparty("CP1", Some(false))]);
        let ctx = embedded_context!(chart, schema, counterparties);
        let tags = tags_with_category_and_counterparty("PURCHASE_10_QUALIFIED", "CP1");
        let account = account_def("600", AccountType::Expense);

        let err = policy.validate_tag(&ctx, &tags, &account).unwrap_err();
        match err {
            PolicyError::TaxCategoryNotApplicable { code, reason, .. } => {
                assert_eq!(code, "PURCHASE_10_QUALIFIED");
                // 候補（経過措置区分）を挙げるが、断定はしない。
                assert!(
                    reason.contains("PURCHASE_10_NON_QUALIFIED"),
                    "reason = {reason}"
                );
                assert!(reason.contains("税理士"), "reason = {reason}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn validate_tag_requires_qualified_invoice_passes_when_counterparty_status_unknown() {
        let policy = policy_from_embedded(default_settings());
        let chart = empty_chart();
        let schema = empty_schema();
        let counterparties = CounterpartyIndex::new(vec![counterparty("CP1", None)]);
        let ctx = embedded_context!(chart, schema, counterparties);
        let tags = tags_with_category_and_counterparty("PURCHASE_10_QUALIFIED", "CP1");
        let account = account_def("600", AccountType::Expense);

        assert!(policy.validate_tag(&ctx, &tags, &account).is_ok());
    }

    #[test]
    fn validate_tag_requires_qualified_invoice_passes_when_counterparty_is_qualified() {
        let policy = policy_from_embedded(default_settings());
        let chart = empty_chart();
        let schema = empty_schema();
        let counterparties = CounterpartyIndex::new(vec![counterparty("CP1", Some(true))]);
        let ctx = embedded_context!(chart, schema, counterparties);
        let tags = tags_with_category_and_counterparty("PURCHASE_10_QUALIFIED", "CP1");
        let account = account_def("600", AccountType::Expense);

        assert!(policy.validate_tag(&ctx, &tags, &account).is_ok());
    }

    #[test]
    fn validate_tag_requires_qualified_invoice_passes_when_no_counterparty_tag() {
        let policy = policy_from_embedded(default_settings());
        let chart = empty_chart();
        let schema = empty_schema();
        let counterparties = CounterpartyIndex::empty();
        let ctx = embedded_context!(chart, schema, counterparties);
        let tags = tags_with_category("PURCHASE_10_QUALIFIED");
        let account = account_def("600", AccountType::Expense);

        assert!(policy.validate_tag(&ctx, &tags, &account).is_ok());
    }

    #[test]
    fn validate_tag_requires_qualified_invoice_passes_when_counterparty_code_unregistered() {
        let policy = policy_from_embedded(default_settings());
        let chart = empty_chart();
        let schema = empty_schema();
        let counterparties = CounterpartyIndex::empty();
        let ctx = embedded_context!(chart, schema, counterparties);
        let tags = tags_with_category_and_counterparty("PURCHASE_10_QUALIFIED", "CP-UNKNOWN");
        let account = account_def("600", AccountType::Expense);

        assert!(policy.validate_tag(&ctx, &tags, &account).is_ok());
    }

    // ---- round_mode ----

    #[test]
    fn round_mode_returns_settings_rounding() {
        let policy = policy_from_embedded(JpSettings {
            rounding: RoundMode::HalfUp,
            ..default_settings()
        });
        let chart = empty_chart();
        let schema = empty_schema();
        let counterparties = CounterpartyIndex::empty();
        let ctx = embedded_context!(chart, schema, counterparties);

        assert_eq!(policy.round_mode(&ctx), RoundMode::HalfUp);
    }

    // ---- JpSettings::compose ----

    #[test]
    fn jp_settings_compose_overrides_take_precedence_over_defaults() {
        let defaults = TaxSettingsDefaults {
            tax_mode: TaxMode::Exclusive,
            rounding: RoundMode::Floor,
            rounding_unit: RoundingUnit::Line,
        };
        let overrides = JpSettingsOverrides {
            tax_mode: Some(TaxMode::Inclusive),
            rounding: None,
            rounding_unit: Some(RoundingUnit::Document),
            is_taxable_business: true,
            simplified_taxation: false,
        };
        let settings = JpSettings::compose(defaults, overrides);

        assert_eq!(settings.tax_mode, TaxMode::Inclusive);
        assert_eq!(settings.rounding, RoundMode::Floor);
        assert_eq!(settings.rounding_unit, RoundingUnit::Document);
        assert!(settings.is_taxable_business);
        assert!(!settings.simplified_taxation);
    }

    #[test]
    fn jp_settings_compose_no_overrides_uses_defaults_only() {
        let defaults = TaxSettingsDefaults {
            tax_mode: TaxMode::Inclusive,
            rounding: RoundMode::HalfUp,
            rounding_unit: RoundingUnit::Document,
        };
        let overrides = JpSettingsOverrides {
            tax_mode: None,
            rounding: None,
            rounding_unit: None,
            is_taxable_business: false,
            simplified_taxation: true,
        };
        let settings = JpSettings::compose(defaults, overrides);

        assert_eq!(settings.tax_mode, TaxMode::Inclusive);
        assert_eq!(settings.rounding, RoundMode::HalfUp);
        assert_eq!(settings.rounding_unit, RoundingUnit::Document);
        assert!(!settings.is_taxable_business);
        assert!(settings.simplified_taxation);
    }

    // ---- プロパティテスト ----
    //
    // `PROGRESS.md` Phase 0 の教訓（生成器は「型が表現できる範囲」ではなく
    // 「仕様が許容する範囲」に合わせる）に従い、端数が出やすい金額
    // （1, 3, 7, 999, 10_001）を `prop_oneof!` で明示的に含める。

    /// 本体金額の生成器。端数が出やすい値を明示的に含める。
    fn any_body_minor() -> impl Strategy<Value = i128> {
        prop_oneof![
            6 => 1i128..=1_000_000i128,
            1 => Just(1i128),
            1 => Just(3i128),
            1 => Just(7i128),
            1 => Just(999i128),
            1 => Just(10_001i128),
        ]
    }

    /// 税率の生成器。割り切れる率・割り切れない率・0（免税・非課税相当）に加え、
    /// `Ratio::parse_rate` が「0以上」しか要求しない（1を超える値も拒否しない）
    /// ことに合わせて 1 を超える率も明示的に含める（`PROGRESS.md` Phase 0 の
    /// 教訓: 生成器は「型/現実的な範囲」ではなく「仕様が許容する範囲」に
    /// 合わせる。実際に Phase 0 で `Ratio::parse_fraction`（0〜1限定）だけを
    /// 生成する形にしていたために `parse_rate` 経由で1を超える比率が
    /// 到達する経路をプロパティテストが踏めていなかったバグがあった）。
    fn any_rate_str() -> impl Strategy<Value = &'static str> {
        prop_oneof![
            Just("0.10"),
            Just("0.08"),
            Just("0.03"),
            Just("0.333"),
            Just("0"),
            Just("1"),
            Just("2.5"),
        ]
    }

    fn any_round_mode() -> impl Strategy<Value = RoundMode> {
        prop_oneof![
            Just(RoundMode::Floor),
            Just(RoundMode::Ceil),
            Just(RoundMode::HalfUp),
        ]
    }

    fn any_rounding_unit() -> impl Strategy<Value = RoundingUnit> {
        prop_oneof![Just(RoundingUnit::Line), Just(RoundingUnit::Document)]
    }

    /// 単一の売上区分（`TEST_SALES`）のみを持つ最小マスタから構築した
    /// `JpTaxPolicy`。`rate` と丸め方式・単位をテストごとに変える。
    fn fixture_policy(
        rate_str: &str,
        rounding: RoundMode,
        rounding_unit: RoundingUnit,
    ) -> JpTaxPolicy {
        let category = TaxCategory {
            code: "TEST_SALES".to_string(),
            label: "test".to_string(),
            direction: TaxDirection::Sales,
            rate: Some(Ratio::parse_rate(rate_str).unwrap()),
            deductible: None,
            deduction_ratio: None,
            requires_qualified_invoice: false,
            tax_account: Some(AccountCode::parse("330").unwrap()),
            note: None,
        };
        let defaults = TaxSettingsDefaults {
            tax_mode: TaxMode::Exclusive,
            rounding,
            rounding_unit,
        };
        let table = TaxCategoryTable::new(
            "test".to_string(),
            AccountingDate::new(2026, 1, 1).unwrap(),
            None,
            defaults,
            vec![category],
        )
        .unwrap();
        let rule_sets = TaxRuleSets::new(vec![table]).unwrap();
        let settings = JpSettings {
            tax_mode: TaxMode::Exclusive,
            rounding,
            rounding_unit,
            is_taxable_business: true,
            simplified_taxation: false,
        };
        JpTaxPolicy::new(rule_sets, settings)
    }

    fn fixture_context() -> (ChartOfAccounts, TagSchema, CounterpartyIndex) {
        (empty_chart(), empty_schema(), CounterpartyIndex::empty())
    }

    proptest! {
        /// **最重要の性質**: ユーザーが税込金額（本体 + 税額）を反対側に書いた
        /// 仕訳を `derive_tax_lines` に通すと、導出後も貸借が一致する。
        /// 「反対側」に書く金額は、生成される税額と同じ計算（`apply_ratio`。
        /// `Line` は明細ごとの合計、`Document` は本体合計を1回丸めた値）を
        /// 使って事前に組み立てる。Phase 1 で「明細ごとに丸めると合計が
        /// 1円ずれる」バグを踏んだ領域そのものの回帰検知になる。
        #[test]
        fn derive_tax_lines_preserves_balance_when_reflection_side_matches_expected_tax(
            bodies in prop::collection::vec(any_body_minor(), 1..=4),
            rate_str in any_rate_str(),
            rounding in any_round_mode(),
            rounding_unit in any_rounding_unit(),
        ) {
            let policy = fixture_policy(rate_str, rounding, rounding_unit);
            let (chart, schema, counterparties) = fixture_context();
            let ctx = TaxContext {
                as_of: AccountingDate::new(2026, 4, 1).unwrap(),
                chart: &chart,
                tag_schema: &schema,
                counterparties: &counterparties,
            };
            let rate = Ratio::parse_rate(rate_str).unwrap();

            let expected_tax_total = match rounding_unit {
                RoundingUnit::Line => {
                    let mut total = Money::zero(Currency::JPY);
                    for &b in &bodies {
                        let tax = policy
                            .apply_ratio(&ctx, Money::from_minor(b, Currency::JPY), rate)
                            .unwrap();
                        total = total.add(&tax).unwrap();
                    }
                    total
                }
                RoundingUnit::Document => {
                    let body_sum: i128 = bodies.iter().sum();
                    policy
                        .apply_ratio(&ctx, Money::from_minor(body_sum, Currency::JPY), rate)
                        .unwrap()
                }
            };

            let body_sum: i128 = bodies.iter().sum();
            let reflection_amount = Money::from_minor(body_sum, Currency::JPY)
                .add(&expected_tax_total)
                .unwrap();

            let mut input = vec![line("130", Side::Debit, reflection_amount.minor(), TagSet::new())];
            for &b in &bodies {
                input.push(line("500", Side::Credit, b, tags_with_category("TEST_SALES")));
            }

            let derivation = policy.derive_tax_lines(&ctx, &input).unwrap();

            let debit_total = derivation
                .lines
                .iter()
                .filter(|l| l.is_debit())
                .try_fold(Money::zero(Currency::JPY), |acc, l| acc.add(l.amount()))
                .unwrap();
            let credit_total = derivation
                .lines
                .iter()
                .filter(|l| !l.is_debit())
                .try_fold(Money::zero(Currency::JPY), |acc, l| acc.add(l.amount()))
                .unwrap();

            prop_assert_eq!(debit_total.minor(), credit_total.minor());
        }

        /// `Line` / `Document` いずれでも、生成される税額行の合計が
        /// 「その側・その区分の本体合計に rate を適用して丸めた値」と整合する。
        #[test]
        fn derive_tax_lines_generated_total_matches_direct_computation(
            bodies in prop::collection::vec(any_body_minor(), 1..=4),
            rate_str in any_rate_str(),
            rounding in any_round_mode(),
            rounding_unit in any_rounding_unit(),
        ) {
            let policy = fixture_policy(rate_str, rounding, rounding_unit);
            let (chart, schema, counterparties) = fixture_context();
            let ctx = TaxContext {
                as_of: AccountingDate::new(2026, 4, 1).unwrap(),
                chart: &chart,
                tag_schema: &schema,
                counterparties: &counterparties,
            };
            let rate = Ratio::parse_rate(rate_str).unwrap();

            let input: Vec<JournalLine> = bodies
                .iter()
                .map(|&b| line("500", Side::Credit, b, tags_with_category("TEST_SALES")))
                .collect();

            let derivation = policy.derive_tax_lines(&ctx, &input).unwrap();
            let generated_total: i128 = derivation
                .lines
                .iter()
                .filter(|l| l.account().as_str() == "330")
                .map(|l| l.amount().minor())
                .sum();

            let expected_total = match rounding_unit {
                RoundingUnit::Line => bodies
                    .iter()
                    .map(|&b| {
                        policy
                            .apply_ratio(&ctx, Money::from_minor(b, Currency::JPY), rate)
                            .unwrap()
                            .minor()
                    })
                    .sum::<i128>(),
                RoundingUnit::Document => {
                    let body_sum: i128 = bodies.iter().sum();
                    policy
                        .apply_ratio(&ctx, Money::from_minor(body_sum, Currency::JPY), rate)
                        .unwrap()
                        .minor()
                }
            };

            prop_assert_eq!(generated_total, expected_total);
        }
    }
}
