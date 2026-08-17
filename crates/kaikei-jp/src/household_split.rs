//! 家事按分（[`household_split`]）。
//!
//! 個人事業主が事業と家事の両方に使う支出（家賃・水道光熱費・通信費等）を、
//! **記帳時**に事業割合に応じて分割するヘルパー。`docs/04-jp-tax.md` §8、
//! `DECISIONS.md` D-063/D-064 を参照。
//!
//! # 出力例（家賃 100,000円 / 事業割合 30%）
//!
//! ```text
//! 地代家賃(615)  30,000（借, tags: business_ratio=0.30, tax_category=...）
//! 事業主貸(410)  70,000（借）
//!                                           現金(100) 100,000（貸）
//! ```
//!
//! `kaikei-core` はこの戻り値を「ただの3行仕訳」として受け取るだけであり、
//! 家事按分という概念そのものを知らない（`CLAUDE.md` §1）。
//!
//! # 事業分 + 家事分 が必ず総額に一致する理由（`DECISIONS.md` D-063）
//!
//! 事業分は `Money::mul_ratio(business_ratio, settings.rounding)` で計算するが、
//! **家事分は `total - 事業分` の引き算で求め、`apply_ratio(total, 1 -
//! business_ratio)` のように別途丸め計算しない。** 端数処理を2回行うと、
//! 丸めの向き（切上/切捨/四捨五入）次第で「事業分 + 家事分」が総額から
//! 1円ずれうる。これは貸借不一致に直結する事故であり、`PROGRESS.md` の
//! Phase 1 で実際に踏んだ「明細ごとに丸めると合計がずれる」バグ
//! （`DECISIONS.md` D-058 が参照する教訓）と同型のクラス。引き算で求めれば、
//! 丸め方式・按分率・金額によらず常に「事業分 + 家事分 = 総額」が構造的に
//! 保証される。
//!
//! # `TaxContext` は不要（`docs/04-jp-tax.md` §2 の設計方針との関係）
//!
//! `kaikei-policy::TaxPolicy::apply_ratio` は `ctx: &TaxContext<'_>` を要求するが、
//! これは trait メソッドとして「税額計算・按分」全般を一つの窓口に揃えるための
//! ものであり、`apply_ratio` の既定実装自体は `ctx` から `round_mode(ctx)`
//! （丸め方式）を読むだけで、`ctx` の他のフィールド（`chart` / `tag_schema` /
//! `counterparties` / `as_of`）は使わない。`household_split` は
//! `TaxCategoryTable`（年度別税区分マスタ）を参照しない（`tax_category` は
//! 検証せずそのままタグに載せるだけ。妥当性検証は記帳時に
//! `TaxPolicy::validate_tag` が別途行う）ため、`as_of`（取引日によるマスタ選択）
//! も `chart`/`tag_schema`/`counterparties` も必要ない。必要なのは丸め方式
//! （`JpSettings.rounding`）だけなので、`JpTaxPolicy`（`TaxPolicy` の実装）を
//! 経由せず `settings: &JpSettings` を直接受け取り、`Money::mul_ratio` を
//! 直接呼ぶ形にした（`DECISIONS.md` D-064）。
//!
//! # `tax_category` を独自の型にしない（`TaxCategoryCode` を新設しない）
//!
//! `docs/04-jp-tax.md` §8 の擬似コードは `tax_category: Option<TaxCategoryCode>`
//! だが、`TaxCategoryCode` という型はこの crate に存在しない。既存の
//! `crate::tax::TaxCategory::code` フィールドも `TagValue::Code` に格納する値
//! （`crate::tax::policy` の税額行生成コード）も、どちらも素の `String`
//! として扱っている。`household_split` はマスタ（`TaxCategoryTable`）を
//! 保持しないため、渡された文字列がマスタに実在する区分コードかどうかを
//! ここで検証できない（検証には `TaxRuleSets` と取引日が要る）。新しい
//! newtype を作ってもこの制約は変わらず、既存の型（`String`）と一貫させる
//! 方が単純なので `Option<String>` のままにする。実在確認は、この関数が
//! 返す明細を仕訳として記帳する際に `TaxPolicy::validate_tag` が行う。
//!
//! # 記帳時按分と決算時一括按分の両方を提供する
//!
//! 家事按分を「記帳時に都度行う」か「決算時に一括で行う」かで帳簿上の表現が
//! 変わる。どちらが妥当かは `docs/08-compliance.md` §9-3 で
//! **税理士確認事項として未解決**だが、**どちらを選ぶかは利用者の判断**であり、
//! 選べないこと自体が実務の妨げになる。
//!
//! | 関数 | いつ | 形 |
//! |---|---|---|
//! | [`household_split`] | 払った時点 | 3行（事業分・家事分・支払元） |
//! | [`year_end_household_split`] | 決算 | 2行（事業主貸・対象科目の取消） |
//!
//! 決算時一括を後から足したのは、実際の帳簿がそれを必要としたためである。
//! WeBanana.SP の2026年は8か月分の家賃・電気代を全額計上したまま
//! 「按分は確定申告時」と摘要に書いて先送りしており、**既に計上した分を
//! 後から按分する形でなければ処理できなかった**。
//!
//! **どちらの関数も、事業割合が妥当かは判断しない。**

use crate::error::JpError;
use crate::tax::JpSettings;
use kaikei_core::{AccountCode, JournalLine, Money, Ratio, Side, TagKey, TagSet, TagValue};
use rust_decimal::Decimal;
use std::sync::OnceLock;

/// [`household_split`] への入力。
#[derive(Debug, Clone)]
pub struct HouseholdSplitInput {
    /// 按分対象の総額。正の値でなければならない（0円・負の金額はエラー）。
    pub total: Money,
    /// 事業割合。0以上1以下でなければならない
    /// （`Ratio` 型自体はこの範囲を強制しないため、実行時に検証する）。
    pub business_ratio: Ratio,
    /// 事業分の計上先科目（地代家賃など）。
    pub expense_account: AccountCode,
    /// 家事分の付け替え先科目（事業主貸）。
    pub owner_account: AccountCode,
    /// 支払い元科目（現金・預金）。
    pub payment_account: AccountCode,
    /// 消費税区分。指定した場合、事業分の明細（`expense_account` の行）にのみ
    /// `tax_category` タグとして付ける。ここでは実在確認をしない
    /// （モジュール doc の「`tax_category` を独自の型にしない」を参照）。
    pub tax_category: Option<String>,
}

/// 家事按分の明細（2〜3行）を組み立てる。
///
/// # 按分率をタグに残す（`docs/04-jp-tax.md` §8）
///
/// 事業分の明細（`expense_account` の行）にのみ `business_ratio` タグ
/// （小数値。`tags.yaml` で `value_type: Decimal` として登録済み）を付ける。
/// 家事分（`owner_account`）・支払い元（`payment_account`）の行にはタグを
/// 付けない。事業割合と家事割合は「1 − 事業割合」の関係で決まる（合計 1）ため、
/// 事業分の行に記録しておけば根拠として十分であり、複数行に同じ情報を
/// 重複させない。
///
/// # 0円になる明細は生成しない
///
/// 事業割合が0%または100%のとき、事業分または家事分のどちらかが0円になる。
/// `JournalLine::new` は0円の明細を拒否するため、その行は生成せず
/// 2行（家事分/事業分のどちらか一方 + 支払い元）の仕訳になる。
///
/// # 断定しないこと（`CLAUDE.md` §10）
///
/// この関数は「指定された事業割合で3行（または2行）の仕訳明細を組み立てる」
/// だけである。指定された事業割合（例: 30%）が税務上妥当かどうかは判断しない。
/// 割合の妥当性は税理士に確認すべき事項であり、この関数の呼び出し元が
/// ユーザーに確認を求める責務を負う。
pub fn household_split(
    input: HouseholdSplitInput,
    settings: &JpSettings,
) -> Result<Vec<JournalLine>, JpError> {
    let HouseholdSplitInput {
        total,
        business_ratio,
        expense_account,
        owner_account,
        payment_account,
        tax_category,
    } = input;

    if total.is_zero() || total.is_negative() {
        return Err(JpError::InvalidHouseholdSplitTotal {
            total: total.to_display_string(),
        });
    }

    let ratio_value = business_ratio.as_decimal();
    if ratio_value < Decimal::ZERO || ratio_value > Decimal::ONE {
        return Err(JpError::InvalidBusinessRatio {
            ratio: ratio_value.to_string(),
        });
    }

    // 事業分を先に計算し、家事分は total - 事業分 で求める（モジュール doc
    // 「事業分 + 家事分 が必ず総額に一致する理由」/ `DECISIONS.md` D-063）。
    let business_amount = total.mul_ratio(business_ratio, settings.rounding)?;
    let household_amount = total.sub(&business_amount)?;

    let mut lines = Vec::with_capacity(3);

    if !business_amount.is_zero() {
        lines.push(JournalLine::new(
            expense_account,
            Side::Debit,
            business_amount,
            business_line_tags(business_ratio, tax_category.as_deref()),
            None,
        )?);
    }

    if !household_amount.is_zero() {
        lines.push(JournalLine::new(
            owner_account,
            Side::Debit,
            household_amount,
            TagSet::new(),
            None,
        )?);
    }

    lines.push(JournalLine::new(
        payment_account,
        Side::Credit,
        total,
        TagSet::new(),
        None,
    )?);

    Ok(lines)
}

/// タグキーは固定文字列であり、`household_split` の呼び出しのたびに
/// パース・アロケーションし直す必要はない（`crate::tax::policy` の
/// `tax_category_key` と同じ意図。モジュールが異なるため個別に保持する）。
fn business_ratio_key() -> &'static TagKey {
    static KEY: OnceLock<TagKey> = OnceLock::new();
    KEY.get_or_init(|| {
        TagKey::parse("business_ratio")
            .expect("\"business_ratio\" は tags.yaml に登録された既知のタグキー")
    })
}

fn tax_category_key() -> &'static TagKey {
    static KEY: OnceLock<TagKey> = OnceLock::new();
    KEY.get_or_init(|| {
        TagKey::parse("tax_category")
            .expect("\"tax_category\" は tags.yaml に登録された既知のタグキー")
    })
}

/// [`year_end_household_split`] への入力。
#[derive(Debug, Clone)]
pub struct YearEndHouseholdSplitInput {
    /// その年に**全額を経費として計上した額**。正の値でなければならない。
    pub total: Money,
    /// 事業割合。0以上1以下。
    pub business_ratio: Ratio,
    /// 按分対象の科目（地代家賃など）。ここから家事分を差し引く。
    pub expense_account: AccountCode,
    /// 家事分の付け替え先科目（事業主貸）。
    pub owner_account: AccountCode,
    /// 消費税区分。指定した場合、**対象科目の行**（貸方）に付ける。
    /// 記帳時按分（[`household_split`]）が事業分の行に付けるのと役割が違う
    /// （下の doc「税区分をどちらの行に付けるか」を参照）。
    pub tax_category: Option<String>,
}

/// 決算時に一括で家事按分する明細（2行）を組み立てる。
///
/// # 記帳時按分（[`household_split`]）との違い
///
/// あちらは**払った時点**で事業分と家事分に分ける（3行。支払元が貸方）。
/// こちらは**既に全額を経費に計上した後**で、家事分を経費から抜く。
///
/// ```text
/// 借 事業主貸    1,292,004
/// 貸 地代家賃            1,292,004  <business_ratio:0.3>
/// ```
///
/// # なぜ両方が要るのか
///
/// 記帳時按分は月次の損益が正しくなるが、**払うたびに事業割合が確定して
/// いる必要がある**。事業割合は決算で決めることが多く、実際 WeBanana.SP の
/// 2026年の帳簿は、8か月分の家賃・電気代を全額計上したまま
/// 「按分は確定申告時」と摘要に書いて先送りしていた。既に計上した分を
/// 後から按分するには、この形しかない。
///
/// # 端数の扱いを記帳時按分と揃える
///
/// **事業分を先に計算し、家事分は `total - 事業分` で求める**（`DECISIONS.md`
/// D-063）。同じ総額・同じ割合なら、どちらの方法でも家事分が1円まで一致する。
/// 揃えておかないと、途中で方法を変えた年に差が出る。
///
/// # 税区分をどちらの行に付けるか
///
/// 記帳時按分は**事業分の行**に付ける（家事分は仕入税額控除の対象外なので
/// 税区分を持たせない）。決算時按分は逆で、**経費を減らす行**に付ける。
/// 元の仕訳が全額に税区分を付けて計上しているため、控除しすぎた分を
/// 同じ税区分で戻さないと、消費税の計算が合わないからである。
///
/// # 振り替えるものが無ければ空を返す
///
/// 事業割合が100%なら家事分は0円で、振り替える必要が無い。`JournalLine` は
/// 0円の明細を拒むので、**空の `Vec` を返す**。呼び出し側は「按分の振替は
/// 要らない」と伝えること。黙って何もしないと、按分したつもりで
/// されていない状態になる。
///
/// # 断定しないこと（`CLAUDE.md` §10）
///
/// この関数は指定された事業割合で2行を組み立てるだけである。**その割合が
/// 税務上妥当かは判断しない。** どの科目が按分対象かも判断しない
/// （自宅兼事務所の家賃なのか、事業専用の事務所なのかを知らない）。
pub fn year_end_household_split(
    input: YearEndHouseholdSplitInput,
    settings: &JpSettings,
) -> Result<Vec<JournalLine>, JpError> {
    let YearEndHouseholdSplitInput {
        total,
        business_ratio,
        expense_account,
        owner_account,
        tax_category,
    } = input;

    if total.is_zero() || total.is_negative() {
        return Err(JpError::InvalidHouseholdSplitTotal {
            total: total.to_display_string(),
        });
    }

    let ratio_value = business_ratio.as_decimal();
    if ratio_value < Decimal::ZERO || ratio_value > Decimal::ONE {
        return Err(JpError::InvalidBusinessRatio {
            ratio: ratio_value.to_string(),
        });
    }

    // 記帳時按分と同じ順序で計算する（事業分が先、家事分は残り）。
    let business_amount = total.mul_ratio(business_ratio, settings.rounding)?;
    let household_amount = total.sub(&business_amount)?;

    if household_amount.is_zero() {
        return Ok(Vec::new());
    }

    Ok(vec![
        JournalLine::new(
            owner_account,
            Side::Debit,
            household_amount,
            TagSet::new(),
            None,
        )?,
        JournalLine::new(
            expense_account,
            Side::Credit,
            household_amount,
            business_line_tags(business_ratio, tax_category.as_deref()),
            None,
        )?,
    ])
}

fn business_line_tags(business_ratio: Ratio, tax_category: Option<&str>) -> TagSet {
    let mut tags = TagSet::new();
    tags.insert(
        business_ratio_key().clone(),
        TagValue::Decimal(business_ratio.as_decimal()),
    );
    if let Some(code) = tax_category {
        tags.insert(tax_category_key().clone(), TagValue::Code(code.to_string()));
    }
    tags
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_core::{Currency, RoundMode};
    use proptest::prelude::*;

    fn settings_with(rounding: RoundMode) -> JpSettings {
        JpSettings {
            tax_mode: crate::tax::TaxMode::Exclusive,
            rounding,
            rounding_unit: crate::tax::RoundingUnit::Line,
            is_taxable_business: true,
            simplified_taxation: false,
        }
    }

    fn account(code: &str) -> AccountCode {
        AccountCode::parse(code).unwrap()
    }

    fn input(
        total_minor: i128,
        ratio_str: &str,
        tax_category: Option<&str>,
    ) -> HouseholdSplitInput {
        HouseholdSplitInput {
            total: Money::from_minor(total_minor, Currency::JPY),
            business_ratio: Ratio::parse_fraction(ratio_str).unwrap(),
            expense_account: account("615"),
            owner_account: account("410"),
            payment_account: account("100"),
            tax_category: tax_category.map(str::to_string),
        }
    }

    // ---- docs/04-jp-tax.md §8 の例 ----

    #[test]
    fn household_split_reproduces_the_docs_section_8_example() {
        let settings = settings_with(RoundMode::Floor);
        let result = household_split(input(100_000, "0.30", None), &settings).unwrap();

        assert_eq!(result.len(), 3);

        let expense = result
            .iter()
            .find(|l| l.account().as_str() == "615")
            .unwrap();
        assert_eq!(expense.side(), Side::Debit);
        assert_eq!(expense.amount().minor(), 30_000);
        assert_eq!(
            expense.tags().get(business_ratio_key()),
            Some(&TagValue::Decimal(Decimal::new(30, 2)))
        );

        let owner = result
            .iter()
            .find(|l| l.account().as_str() == "410")
            .unwrap();
        assert_eq!(owner.side(), Side::Debit);
        assert_eq!(owner.amount().minor(), 70_000);
        assert!(owner.tags().is_empty());

        let payment = result
            .iter()
            .find(|l| l.account().as_str() == "100")
            .unwrap();
        assert_eq!(payment.side(), Side::Credit);
        assert_eq!(payment.amount().minor(), 100_000);
        assert!(payment.tags().is_empty());

        // 事業分 + 家事分 = 総額。
        assert_eq!(
            expense.amount().minor() + owner.amount().minor(),
            payment.amount().minor()
        );
    }

    #[test]
    fn household_split_attaches_tax_category_only_to_expense_line() {
        let settings = settings_with(RoundMode::Floor);
        let result = household_split(
            input(100_000, "0.30", Some("PURCHASE_10_QUALIFIED")),
            &settings,
        )
        .unwrap();

        let expense = result
            .iter()
            .find(|l| l.account().as_str() == "615")
            .unwrap();
        assert_eq!(
            expense.tags().get(tax_category_key()),
            Some(&TagValue::Code("PURCHASE_10_QUALIFIED".to_string()))
        );

        let owner = result
            .iter()
            .find(|l| l.account().as_str() == "410")
            .unwrap();
        assert!(owner.tags().get(tax_category_key()).is_none());

        let payment = result
            .iter()
            .find(|l| l.account().as_str() == "100")
            .unwrap();
        assert!(payment.tags().get(tax_category_key()).is_none());
    }

    #[test]
    fn household_split_without_tax_category_leaves_expense_line_without_that_tag() {
        let settings = settings_with(RoundMode::Floor);
        let result = household_split(input(100_000, "0.30", None), &settings).unwrap();

        let expense = result
            .iter()
            .find(|l| l.account().as_str() == "615")
            .unwrap();
        assert!(expense.tags().get(tax_category_key()).is_none());
    }

    // ---- 端数が出る例（100,001円の30%、RoundMode 3種） ----

    #[test]
    fn household_split_rounding_floor_keeps_total_exact() {
        // 100,001 * 0.30 = 30,000.3 -> floor -> 30,000。家事分は引き算で 70,001。
        let settings = settings_with(RoundMode::Floor);
        let result = household_split(input(100_001, "0.30", None), &settings).unwrap();
        let expense = result
            .iter()
            .find(|l| l.account().as_str() == "615")
            .unwrap();
        let owner = result
            .iter()
            .find(|l| l.account().as_str() == "410")
            .unwrap();
        assert_eq!(expense.amount().minor(), 30_000);
        assert_eq!(owner.amount().minor(), 70_001);
        assert_eq!(expense.amount().minor() + owner.amount().minor(), 100_001);
    }

    #[test]
    fn household_split_rounding_ceil_keeps_total_exact() {
        // 100,001 * 0.30 = 30,000.3 -> ceil -> 30,001。家事分は引き算で 70,000。
        let settings = settings_with(RoundMode::Ceil);
        let result = household_split(input(100_001, "0.30", None), &settings).unwrap();
        let expense = result
            .iter()
            .find(|l| l.account().as_str() == "615")
            .unwrap();
        let owner = result
            .iter()
            .find(|l| l.account().as_str() == "410")
            .unwrap();
        assert_eq!(expense.amount().minor(), 30_001);
        assert_eq!(owner.amount().minor(), 70_000);
        assert_eq!(expense.amount().minor() + owner.amount().minor(), 100_001);
    }

    #[test]
    fn household_split_rounding_half_up_keeps_total_exact() {
        // 100,001 * 0.30 = 30,000.3 -> half_up（.3 は .5 未満なので切捨て）-> 30,000。
        let settings = settings_with(RoundMode::HalfUp);
        let result = household_split(input(100_001, "0.30", None), &settings).unwrap();
        let expense = result
            .iter()
            .find(|l| l.account().as_str() == "615")
            .unwrap();
        let owner = result
            .iter()
            .find(|l| l.account().as_str() == "410")
            .unwrap();
        assert_eq!(expense.amount().minor(), 30_000);
        assert_eq!(owner.amount().minor(), 70_001);
        assert_eq!(expense.amount().minor() + owner.amount().minor(), 100_001);
    }

    // ---- 事業割合 0% / 100%（0円の明細が生成されないこと） ----

    #[test]
    fn household_split_zero_percent_business_ratio_omits_expense_line() {
        let settings = settings_with(RoundMode::Floor);
        let result = household_split(input(100_000, "0", None), &settings).unwrap();

        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|l| l.account().as_str() != "615"));
        let owner = result
            .iter()
            .find(|l| l.account().as_str() == "410")
            .unwrap();
        assert_eq!(owner.amount().minor(), 100_000);
        let payment = result
            .iter()
            .find(|l| l.account().as_str() == "100")
            .unwrap();
        assert_eq!(payment.amount().minor(), 100_000);
    }

    #[test]
    fn household_split_hundred_percent_business_ratio_omits_owner_line() {
        let settings = settings_with(RoundMode::Floor);
        let result = household_split(input(100_000, "1", None), &settings).unwrap();

        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|l| l.account().as_str() != "410"));
        let expense = result
            .iter()
            .find(|l| l.account().as_str() == "615")
            .unwrap();
        assert_eq!(expense.amount().minor(), 100_000);
        let payment = result
            .iter()
            .find(|l| l.account().as_str() == "100")
            .unwrap();
        assert_eq!(payment.amount().minor(), 100_000);
    }

    // ---- business_ratio が範囲外でエラー ----

    #[test]
    fn household_split_business_ratio_above_one_is_error() {
        let settings = settings_with(RoundMode::Floor);
        let over_one = HouseholdSplitInput {
            business_ratio: Ratio::parse_rate("1.5").unwrap(),
            ..input(100_000, "0.30", None)
        };
        let err = household_split(over_one, &settings).unwrap_err();
        assert!(matches!(err, JpError::InvalidBusinessRatio { .. }));
    }

    // ---- total が0または負のときエラー ----

    #[test]
    fn household_split_zero_total_is_error() {
        let settings = settings_with(RoundMode::Floor);
        let err = household_split(input(0, "0.30", None), &settings).unwrap_err();
        assert!(matches!(err, JpError::InvalidHouseholdSplitTotal { .. }));
    }

    #[test]
    fn household_split_negative_total_is_error() {
        let settings = settings_with(RoundMode::Floor);
        let err = household_split(input(-1, "0.30", None), &settings).unwrap_err();
        assert!(matches!(err, JpError::InvalidHouseholdSplitTotal { .. }));
    }

    // ---- プロパティテスト ----
    //
    // `PROGRESS.md` Phase 0 の教訓（生成器は「型が表現できる範囲」ではなく
    // 「仕様が許容する範囲」に合わせる）に従い、端数が出やすい金額
    // （1, 3, 7, 999, 100_001）と、事業割合の境界値（0, 1）を明示的に含める。

    fn any_total_minor() -> impl Strategy<Value = i128> {
        prop_oneof![
            6 => 1i128..=1_000_000_000i128,
            1 => Just(1i128),
            1 => Just(3i128),
            1 => Just(7i128),
            1 => Just(999i128),
            1 => Just(100_001i128),
        ]
    }

    /// 事業割合の生成器。0 と 1 の境界を明示的に含める。
    fn any_business_ratio() -> impl Strategy<Value = Ratio> {
        prop_oneof![
            1 => Just(Ratio::parse_fraction("0").unwrap()),
            1 => Just(Ratio::parse_fraction("1").unwrap()),
            1 => Just(Ratio::parse_fraction("0.3").unwrap()),
            1 => Just(Ratio::parse_fraction("0.333").unwrap()),
            4 => (0u32..1000u32)
                .prop_map(|n| Ratio::parse_fraction(&format!("0.{n:03}")).unwrap()),
        ]
    }

    fn any_round_mode() -> impl Strategy<Value = RoundMode> {
        prop_oneof![
            Just(RoundMode::Floor),
            Just(RoundMode::Ceil),
            Just(RoundMode::HalfUp),
        ]
    }

    proptest! {
        /// **最重要の性質**: 任意の割合・任意の金額・任意の `RoundMode` で、
        /// 事業分 + 家事分 == 元金額（`DECISIONS.md` D-063 の回帰検知）。
        #[test]
        fn household_split_business_plus_household_equals_total(
            total_minor in any_total_minor(),
            business_ratio in any_business_ratio(),
            rounding in any_round_mode(),
        ) {
            let settings = settings_with(rounding);
            let total = Money::from_minor(total_minor, Currency::JPY);
            let result = household_split(
                HouseholdSplitInput {
                    total,
                    business_ratio,
                    expense_account: account("615"),
                    owner_account: account("410"),
                    payment_account: account("100"),
                    tax_category: None,
                },
                &settings,
            )
            .unwrap();

            let business_and_household_total: i128 = result
                .iter()
                .filter(|l| l.account().as_str() != "100")
                .map(|l| l.amount().minor())
                .sum();
            prop_assert_eq!(business_and_household_total, total_minor);
        }

        /// 生成された明細の借方合計 == 貸方合計（貸借一致）。
        #[test]
        fn household_split_debit_total_equals_credit_total(
            total_minor in any_total_minor(),
            business_ratio in any_business_ratio(),
            rounding in any_round_mode(),
        ) {
            let settings = settings_with(rounding);
            let total = Money::from_minor(total_minor, Currency::JPY);
            let result = household_split(
                HouseholdSplitInput {
                    total,
                    business_ratio,
                    expense_account: account("615"),
                    owner_account: account("410"),
                    payment_account: account("100"),
                    tax_category: None,
                },
                &settings,
            )
            .unwrap();

            let debit_total: i128 = result
                .iter()
                .filter(|l| l.is_debit())
                .map(|l| l.amount().minor())
                .sum();
            let credit_total: i128 = result
                .iter()
                .filter(|l| !l.is_debit())
                .map(|l| l.amount().minor())
                .sum();
            prop_assert_eq!(debit_total, credit_total);
        }

        /// 生成される明細は常に2〜3行（事業割合0%/100%で0円の明細を
        /// 除いた結果）で、`JournalEntry::new` が要求する「2行以上」を満たす
        /// （合計が正である限り、支払い元行は必ず残るため最低2行になる）。
        #[test]
        fn household_split_always_produces_between_two_and_three_lines(
            total_minor in any_total_minor(),
            business_ratio in any_business_ratio(),
            rounding in any_round_mode(),
        ) {
            let settings = settings_with(rounding);
            let total = Money::from_minor(total_minor, Currency::JPY);
            let result = household_split(
                HouseholdSplitInput {
                    total,
                    business_ratio,
                    expense_account: account("615"),
                    owner_account: account("410"),
                    payment_account: account("100"),
                    tax_category: None,
                },
                &settings,
            )
            .unwrap();

            prop_assert!(result.len() == 2 || result.len() == 3);
        }
    }

    // ---- 決算時一括按分（year_end_household_split） ----

    fn year_end_input(
        total_minor: i128,
        ratio_str: &str,
        tax_category: Option<&str>,
    ) -> YearEndHouseholdSplitInput {
        YearEndHouseholdSplitInput {
            total: Money::from_minor(total_minor, Currency::JPY),
            business_ratio: Ratio::parse_fraction(ratio_str).unwrap(),
            expense_account: account("615"),
            owner_account: account("410"),
            tax_category: tax_category.map(str::to_string),
        }
    }

    /// **本命。** 既に全額を経費に計上した後、家事分を経費から抜く。
    ///
    /// WeBanana.SP の2026年の地代家賃 1,845,720円 を事業割合30%で按分する形。
    #[test]
    fn year_end_split_moves_the_household_portion_out_of_the_expense() {
        let settings = settings_with(RoundMode::Floor);
        let result =
            year_end_household_split(year_end_input(1_845_720, "0.30", None), &settings).unwrap();

        assert_eq!(result.len(), 2, "決算時按分は2行");

        let owner = result
            .iter()
            .find(|l| l.account().as_str() == "410")
            .unwrap();
        assert_eq!(owner.side(), Side::Debit, "事業主貸は借方");
        assert_eq!(owner.amount().minor(), 1_292_004);

        let expense = result
            .iter()
            .find(|l| l.account().as_str() == "615")
            .unwrap();
        assert_eq!(expense.side(), Side::Credit, "経費を減らすので貸方");
        assert_eq!(expense.amount().minor(), 1_292_004);
    }

    /// **本命。** 記帳時按分と家事分が1円まで一致する。
    ///
    /// 端数の丸め方を揃えていないと、途中で方法を変えた年に差が出る。
    /// 3で割り切れない額と、割り切れる額の両方で見る。
    #[test]
    fn year_end_split_agrees_with_the_at_entry_split_to_the_yen() {
        for rounding in [RoundMode::Floor, RoundMode::Ceil, RoundMode::HalfUp] {
            let settings = settings_with(rounding);
            for total in [1_845_720_i128, 77_261, 100_000, 1, 3, 7, 999_999] {
                for ratio in ["0.30", "0.333", "0.5", "0.67"] {
                    let at_entry = household_split(input(total, ratio, None), &settings).unwrap();
                    let at_year_end =
                        year_end_household_split(year_end_input(total, ratio, None), &settings)
                            .unwrap();

                    let household_at_entry = at_entry
                        .iter()
                        .find(|l| l.account().as_str() == "410")
                        .map(|l| l.amount().minor())
                        .unwrap_or(0);
                    let household_at_year_end = at_year_end
                        .iter()
                        .find(|l| l.account().as_str() == "410")
                        .map(|l| l.amount().minor())
                        .unwrap_or(0);

                    assert_eq!(
                        household_at_entry, household_at_year_end,
                        "総額 {total} / 割合 {ratio} / 丸め {rounding:?} で家事分が食い違う"
                    );
                }
            }
        }
    }

    /// **本命。** 税区分は経費を減らす行に付ける。
    ///
    /// 元の仕訳は全額に税区分を付けて計上している。控除しすぎた分を同じ
    /// 税区分で戻さないと、消費税の計算が合わない。記帳時按分とは逆の行。
    #[test]
    fn year_end_split_puts_the_tax_category_on_the_line_that_reduces_the_expense() {
        let settings = settings_with(RoundMode::Floor);
        let result = year_end_household_split(
            year_end_input(100_000, "0.30", Some("TAXABLE_10")),
            &settings,
        )
        .unwrap();

        let expense = result
            .iter()
            .find(|l| l.account().as_str() == "615")
            .unwrap();
        assert_eq!(
            expense.tags().get(tax_category_key()),
            Some(&TagValue::Code("TAXABLE_10".to_string())),
            "経費を減らす行に税区分が要る"
        );
        assert_eq!(
            expense.tags().get(business_ratio_key()),
            Some(&TagValue::Decimal(Decimal::new(30, 2))),
            "按分率も同じ行に残す"
        );

        let owner = result
            .iter()
            .find(|l| l.account().as_str() == "410")
            .unwrap();
        assert!(owner.tags().is_empty(), "事業主貸にはタグを付けない");
    }

    /// **本命。** 事業割合100%なら振り替えるものが無い。
    ///
    /// 空を返す。呼び出し側が「按分の振替は要らない」と伝えるための形。
    /// 0円の明細を作ろうとして失敗するのではない。
    #[test]
    fn year_end_split_returns_nothing_when_the_business_ratio_is_one() {
        let settings = settings_with(RoundMode::Floor);
        let result =
            year_end_household_split(year_end_input(1_845_720, "1", None), &settings).unwrap();

        assert!(result.is_empty(), "振り替えるものが無ければ空");
    }

    /// 事業割合0%なら全額が家事分。
    #[test]
    fn year_end_split_moves_everything_when_the_business_ratio_is_zero() {
        let settings = settings_with(RoundMode::Floor);
        let result =
            year_end_household_split(year_end_input(100_000, "0", None), &settings).unwrap();

        assert_eq!(result.len(), 2);
        let owner = result
            .iter()
            .find(|l| l.account().as_str() == "410")
            .unwrap();
        assert_eq!(owner.amount().minor(), 100_000);
    }

    #[test]
    fn year_end_split_rejects_a_zero_or_negative_total() {
        let settings = settings_with(RoundMode::Floor);
        for total in [0_i128, -100] {
            let mut input = year_end_input(1000, "0.30", None);
            input.total = Money::from_minor(total, Currency::JPY);
            assert!(matches!(
                year_end_household_split(input, &settings),
                Err(JpError::InvalidHouseholdSplitTotal { .. })
            ));
        }
    }

    /// 1を超える割合を弾く。
    ///
    /// `Ratio::parse_fraction` は0〜1を自分で検査するので、**1超を作るには
    /// `parse_rate` を使う**（記帳時按分の同じ検査と同じ形）。
    ///
    /// **負の割合は検査していない。** `Ratio` を作る手段は `parse_fraction` と
    /// `parse_rate` の2つだけで、どちらも負を拒む。関数側の下限の検査は
    /// 現状**到達しない**。`Ratio` に負を作れる構築子が増えたときのために
    /// 残してある（そのときこの検査を足すこと）。
    #[test]
    fn year_end_split_rejects_a_ratio_above_one() {
        let settings = settings_with(RoundMode::Floor);
        let over_one = YearEndHouseholdSplitInput {
            business_ratio: Ratio::parse_rate("1.5").unwrap(),
            ..year_end_input(1000, "0.30", None)
        };
        assert!(matches!(
            year_end_household_split(over_one, &settings),
            Err(JpError::InvalidBusinessRatio { .. })
        ));
    }
}
