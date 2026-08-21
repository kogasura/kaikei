//! 消費税の申告に向けた集計（原則課税・税込経理）。
//!
//! # これは申告書ではない
//!
//! **出すのは集計値だけである。** 申告書の各欄に何を書くかは申告上の判断で
//! あり、このモジュールは決めない（`kaikei_jp::depreciation` や
//! `household_split` と同じ立場。`CLAUDE.md` §10）。
//!
//! 特に、次のものは**反映していない**。
//!
//! | 反映していないもの | なぜ |
//! |---|---|
//! | 家事按分 | どの科目が按分対象かは帳簿から決まらない |
//! | 適格請求書発行事業者かどうかの確認 | 取引先の登録番号を確かめていない |
//! | 経過措置の控除割合（80%／70%） | 非適格の区分が使われていれば別に集計する |
//! | 端数処理の規定 | 申告書上の規定であり、帳簿の集計とは別 |
//! | 課税売上割合・個別対応方式／一括比例配分方式 | 非課税売上がある場合の按分 |
//!
//! # 税込経理を前提にする
//!
//! 税込経理（`KAIKEI_TAX_MODE=inclusive`）では、仮受消費税・仮払消費税の
//! 明細が立たない。**税額は税込金額から割り戻す。**
//!
//! ```text
//! 消費税相当額 = 税込金額 × 税率 ÷ (1 + 税率)
//! ```
//!
//! 税抜経理の帳簿でこれを使うと、**税額を二重に数える**（税抜金額から
//! さらに割り戻してしまう）。呼び出し側が経理方式を確かめること。
//!
//! # 貸方に立つ課税仕入れは差し引く
//!
//! 返金・値引き（仕入対価の返還）は課税仕入れを減らす。借方だけを足すと
//! 控除額が過大になる。**同じ区分の貸方を差し引く。**
//!
//! 売上側も同じで、売上の返還（返品・値引き）は課税売上を減らす。

use crate::tax::{TaxCategoryTable, TaxDirection};
use kaikei_core::{Money, Side};
use std::collections::BTreeMap;

/// 区分ごとの集計。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryTotal {
    /// 税区分コード。
    pub code: String,
    /// 表示名。
    pub label: String,
    /// 向き。
    pub direction: TaxDirection,
    /// 税込金額（貸方を差し引いた後）。
    pub amount: Money,
    /// 消費税相当額。税率を持たない区分では `None`。
    pub tax: Option<Money>,
}

/// 集計の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    /// 区分ごとの内訳。コード順。
    pub categories: Vec<CategoryTotal>,
    /// 税区分が付いていない明細の数。
    ///
    /// **0 でないなら集計は不完全である。** 課税取引なのにタグが無い明細が
    /// あれば、その分が抜ける。
    pub lines_without_a_category: usize,
    /// 表に無いコードが付いていた明細の数。
    ///
    /// **黙って捨てない。** 年度をまたぐ区分の追加や打ち間違いで起きる。
    pub lines_with_an_unknown_category: usize,
}

impl Summary {
    /// 課税売上の税込合計。
    pub fn taxable_sales(&self) -> Money {
        self.sum_of(TaxDirection::Sales, |c| c.amount)
    }

    /// 課税売上に係る消費税相当額。
    pub fn tax_on_sales(&self) -> Money {
        self.sum_of(TaxDirection::Sales, |c| {
            c.tax.unwrap_or(Money::zero(kaikei_core::Currency::JPY))
        })
    }

    /// 課税仕入の税込合計。
    pub fn taxable_purchases(&self) -> Money {
        self.sum_of(TaxDirection::Purchase, |c| c.amount)
    }

    /// 課税仕入に係る消費税相当額。
    pub fn tax_on_purchases(&self) -> Money {
        self.sum_of(TaxDirection::Purchase, |c| {
            c.tax.unwrap_or(Money::zero(kaikei_core::Currency::JPY))
        })
    }

    fn sum_of(&self, direction: TaxDirection, pick: impl Fn(&CategoryTotal) -> Money) -> Money {
        let total: i128 = self
            .categories
            .iter()
            .filter(|c| c.direction == direction)
            .map(|c| pick(c).minor())
            .sum();
        Money::from_minor(total, kaikei_core::Currency::JPY)
    }
}

/// 納付税額を売上の税額だけから求める特例。
///
/// # 適用できる年（個人事業者・暦年）
///
/// | 年 | 特例 | 根拠 |
/// |---|---|---|
/// | 令和5〜8年分（2023〜2026） | 2割特例 | 28年改正法附則51の2 |
/// | 令和9・10年分（2027・2028） | 3割特例 | 令和8年度改正・28年改正法附則51の3 |
/// | 令和11年分（2029）以降 | なし | |
///
/// 2割特例は「令和8年9月30日までの日の属する課税期間」で終わる。個人事業者の
/// 課税期間は暦年なので、令和8年分（1/1〜12/31）はその日を含む。**つまり
/// 2026年分が最後である。**
///
/// # このソフトは適用できるかを判定しない
///
/// どちらも**免税事業者がインボイス登録で課税事業者になった場合**の特例で、
/// 基準期間（2年前）の課税売上高が1,000万円を超えるなど、除外要件がいくつも
/// ある。帳簿には2年前が入っていないことがあるので、**判定はしない。**
/// 出すのは試算だけである。
///
/// 事前の届出も継続適用の義務もなく、申告書への付記だけで使える
/// （28年改正法附則51の2③）。だから**年ごとに選べる**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecialRule {
    /// 2割特例。納付税額は売上の税額の20%。
    TwentyPercent,
    /// 3割特例。納付税額は売上の税額の30%。
    ThirtyPercent,
}

impl SpecialRule {
    /// 納付税額に掛ける割合（百分率）。
    #[must_use]
    pub fn percent(self) -> i128 {
        match self {
            SpecialRule::TwentyPercent => 20,
            SpecialRule::ThirtyPercent => 30,
        }
    }

    /// 画面に出す名前。
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            SpecialRule::TwentyPercent => "2割特例",
            SpecialRule::ThirtyPercent => "3割特例",
        }
    }
}

/// その年（個人事業者・暦年）に使える特例。
#[must_use]
pub fn special_rule_for(year: i32) -> Option<SpecialRule> {
    match year {
        2023..=2026 => Some(SpecialRule::TwentyPercent),
        2027 | 2028 => Some(SpecialRule::ThirtyPercent),
        _ => None,
    }
}

/// 特例を使ったときの納付税額の試算。
///
/// **売上の税額だけで決まる。** 仕入の税額は関係しない（だから
/// 適格請求書が揃わなくても額が変わらない）。
///
/// 端数は切り捨てる。申告書では課税標準額の1,000円未満切捨てと納付税額の
/// 100円未満切捨てが別にあるので、**この値は申告書の金額ではない。**
#[must_use]
pub fn tax_under(rule: SpecialRule, tax_on_sales: Money) -> Money {
    Money::from_minor(
        tax_on_sales.minor() * rule.percent() / 100,
        kaikei_core::Currency::JPY,
    )
}

/// 集計に渡す明細1行。
///
/// **`kaikei-core` の `JournalLine` を直接受け取らない。** この crate は
/// 帳簿の読み方を知らなくてよく、テストも組み立てやすくなる。
#[derive(Debug, Clone)]
pub struct TaggedLine {
    /// 税区分コード。タグが無ければ `None`。
    pub tax_category: Option<String>,
    /// 借方・貸方。
    pub side: Side,
    /// 税込金額。
    pub amount: Money,
}

/// 税込金額から消費税相当額を割り戻す。
///
/// `税込 × 税率 ÷ (1 + 税率)`。端数は切り捨てる。
///
/// **切り捨てにするのは、控除額を過大にしないためである。** 申告書上の
/// 端数処理の規定とは別で、ここは集計の便宜にすぎない。
pub fn tax_included_in(
    amount: &Money,
    rate: kaikei_core::Ratio,
) -> Result<Money, kaikei_core::CoreError> {
    use rust_decimal::prelude::ToPrimitive;
    use rust_decimal::Decimal;

    let r = rate.as_decimal();
    if r <= Decimal::ZERO {
        return Ok(Money::from_minor(0, kaikei_core::Currency::JPY));
    }

    // **割り算を最後にする。** `rate / (1 + rate)` を先に求めると、
    // 10% で 0.0909090909… という割り切れない小数になり、桁が落ちる。
    // 実際、9,757,440円 の税額が 887,039円 になった（正しくは 887,040円）。
    //
    // 掛けてから割れば、10% の場合は 975,744 ÷ 1.1 = 887,040 で
    // 割り切れる。
    let base = Decimal::try_from_i128_with_scale(amount.minor(), 0).map_err(|_| {
        kaikei_core::CoreError::InvalidAmount {
            reason: format!("金額が範囲外です: {}", amount.minor()),
        }
    })?;
    let tax = (base * r / (Decimal::ONE + r)).floor();
    let minor = tax
        .to_i128()
        .ok_or_else(|| kaikei_core::CoreError::InvalidAmount {
            reason: format!("税額が範囲外です: {tax}"),
        })?;
    Ok(Money::from_minor(minor, kaikei_core::Currency::JPY))
}

/// 税区分ごとに集計する。
///
/// # 貸方を差し引く
///
/// 同じ区分の貸方（返金・値引き）を借方から差し引く。売上側も仕入側も同じ。
///
/// # 知らないコードは数えて知らせる
///
/// 表に無いコードは金額に足さず、`lines_with_an_unknown_category` に数える。
/// **黙って捨てると、集計が合っていないことに気づけない。**
pub fn summarize(
    lines: &[TaggedLine],
    table: &TaxCategoryTable,
) -> Result<Summary, kaikei_core::CoreError> {
    let mut by_code: BTreeMap<String, i128> = BTreeMap::new();
    let mut without = 0usize;
    let mut unknown = 0usize;

    for line in lines {
        let Some(code) = &line.tax_category else {
            without += 1;
            continue;
        };
        if table.category(code).is_err() {
            unknown += 1;
            continue;
        }
        let signed = match line.side {
            Side::Debit => line.amount.minor(),
            Side::Credit => -line.amount.minor(),
        };
        *by_code.entry(code.clone()).or_insert(0) += signed;
    }

    let mut categories = Vec::new();
    for (code, signed) in by_code {
        let category = table.category(&code).expect("上で存在を確かめた区分");
        // **向きで符号を揃える。** 売上は貸方に立つので、そのままだと負に
        // なる。集計値は「いくらの課税売上か」なので正で出す。
        let amount = match category.direction {
            TaxDirection::Sales => -signed,
            _ => signed,
        };
        let amount = Money::from_minor(amount, kaikei_core::Currency::JPY);
        let tax = match category.rate {
            Some(rate) => Some(tax_included_in(&amount, rate)?),
            None => None,
        };
        categories.push(CategoryTotal {
            code: category.code.clone(),
            label: category.label.clone(),
            direction: category.direction,
            amount,
            tax,
        });
    }

    Ok(Summary {
        categories,
        lines_without_a_category: without,
        lines_with_an_unknown_category: unknown,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_core::{Currency, Ratio};

    fn table() -> TaxCategoryTable {
        crate::tax::TaxRuleSets::from_embedded()
            .unwrap()
            .iter()
            .next()
            .expect("同梱の税区分マスタが1つ以上ある")
            .clone()
    }

    fn yen(v: i128) -> Money {
        Money::from_minor(v, Currency::JPY)
    }

    fn line(code: Option<&str>, side: Side, amount: i128) -> TaggedLine {
        TaggedLine {
            tax_category: code.map(str::to_string),
            side,
            amount: yen(amount),
        }
    }

    // ---- 割り戻し ----

    /// **本命。** 税込から税額を割り戻す。
    ///
    /// 検証帳簿の課税売上 9,757,440円（税込10%）なら 887,040円。
    #[test]
    fn the_tax_is_backed_out_of_a_tax_included_amount() {
        let rate = Ratio::parse_rate("0.10").unwrap();
        assert_eq!(
            tax_included_in(&yen(9_757_440), rate).unwrap().minor(),
            887_040
        );
    }

    /// 端数は切り捨てる。**控除額を過大にしない。**
    #[test]
    fn the_remainder_is_dropped() {
        let rate = Ratio::parse_rate("0.10").unwrap();
        // 1,000 × 10/110 = 90.909… → 90
        assert_eq!(tax_included_in(&yen(1_000), rate).unwrap().minor(), 90);
    }

    /// 税率0なら0。
    #[test]
    fn a_zero_rate_yields_no_tax() {
        let rate = Ratio::parse_rate("0").unwrap();
        assert_eq!(tax_included_in(&yen(1_000), rate).unwrap().minor(), 0);
    }

    // ---- 集計 ----

    /// **本命。** 売上は貸方に立つので、正の値で出す。
    #[test]
    fn sales_are_reported_as_a_positive_amount() {
        let lines = vec![line(Some("SALES_10"), Side::Credit, 9_757_440)];

        let summary = summarize(&lines, &table()).unwrap();

        assert_eq!(summary.taxable_sales().minor(), 9_757_440);
        assert_eq!(summary.tax_on_sales().minor(), 887_040);
    }

    /// **本命。** 貸方に立つ課税仕入れ（返金）は差し引く。
    ///
    /// 借方だけを足すと控除額が過大になる。検証帳簿ではドメイン代の返金が
    /// これに当たった。
    #[test]
    fn a_refund_reduces_the_taxable_purchase() {
        let lines = vec![
            line(Some("PURCHASE_10_QUALIFIED"), Side::Debit, 2_407_340),
            line(Some("PURCHASE_10_QUALIFIED"), Side::Credit, 49_380),
        ];

        let summary = summarize(&lines, &table()).unwrap();

        assert_eq!(summary.taxable_purchases().minor(), 2_357_960);
    }

    /// 売上の返還（返品・値引き）も差し引く。
    #[test]
    fn a_sales_return_reduces_the_taxable_sale() {
        let lines = vec![
            line(Some("SALES_10"), Side::Credit, 1_000_000),
            line(Some("SALES_10"), Side::Debit, 100_000),
        ];

        let summary = summarize(&lines, &table()).unwrap();

        assert_eq!(summary.taxable_sales().minor(), 900_000);
    }

    /// **本命。** 税区分が付いていない明細を数えて知らせる。
    ///
    /// **0 でないなら集計は不完全である。** 課税取引なのにタグが無ければ
    /// その分が抜ける。
    #[test]
    fn lines_without_a_category_are_counted() {
        let lines = vec![
            line(Some("SALES_10"), Side::Credit, 1_000),
            line(None, Side::Debit, 500),
            line(None, Side::Credit, 500),
        ];

        let summary = summarize(&lines, &table()).unwrap();

        assert_eq!(summary.lines_without_a_category, 2);
        assert_eq!(summary.taxable_sales().minor(), 1_000, "金額には入れない");
    }

    /// **本命。** 知らないコードは黙って捨てず、数えて知らせる。
    #[test]
    fn lines_with_an_unknown_category_are_counted() {
        let lines = vec![
            line(Some("SALES_10"), Side::Credit, 1_000),
            line(Some("NO_SUCH_CATEGORY"), Side::Debit, 999),
        ];

        let summary = summarize(&lines, &table()).unwrap();

        assert_eq!(summary.lines_with_an_unknown_category, 1);
        assert_eq!(summary.taxable_purchases().minor(), 0, "金額には入れない");
    }

    /// 税率を持たない区分（対象外・非課税）は税額を出さない。
    ///
    /// **0 と `None` は違う。** 0 と書くと「計算した結果0」に読める。
    #[test]
    fn a_category_without_a_rate_has_no_tax() {
        let lines = vec![line(Some("OUT_OF_SCOPE"), Side::Debit, 10_880)];

        let summary = summarize(&lines, &table()).unwrap();

        let out_of_scope = summary
            .categories
            .iter()
            .find(|c| c.code == "OUT_OF_SCOPE")
            .unwrap();
        assert!(out_of_scope.tax.is_none(), "{out_of_scope:?}");
    }

    /// 区分はコード順に並ぶ（実行のたびに変わらない）。
    #[test]
    fn the_categories_are_in_a_stable_order() {
        let lines = vec![
            line(Some("SALES_10"), Side::Credit, 1_000),
            line(Some("PURCHASE_10_QUALIFIED"), Side::Debit, 1_000),
            line(Some("OUT_OF_SCOPE"), Side::Debit, 1_000),
        ];

        let codes: Vec<String> = summarize(&lines, &table())
            .unwrap()
            .categories
            .into_iter()
            .map(|c| c.code)
            .collect();
        let mut sorted = codes.clone();
        sorted.sort();
        assert_eq!(codes, sorted, "{codes:?}");
    }

    /// **本命。** 検証帳簿（2026年）の数字を再現する。
    #[test]
    fn it_reproduces_the_real_book() {
        let lines = vec![
            line(Some("SALES_10"), Side::Credit, 9_757_440),
            line(Some("PURCHASE_10_QUALIFIED"), Side::Debit, 2_407_340),
            line(Some("PURCHASE_10_QUALIFIED"), Side::Credit, 49_380),
            line(Some("OUT_OF_SCOPE"), Side::Debit, 10_880),
            line(Some("TAX_FREE"), Side::Debit, 168_000),
        ];

        let summary = summarize(&lines, &table()).unwrap();

        assert_eq!(summary.taxable_sales().minor(), 9_757_440);
        assert_eq!(summary.tax_on_sales().minor(), 887_040);
        assert_eq!(summary.taxable_purchases().minor(), 2_357_960);
        assert_eq!(summary.tax_on_purchases().minor(), 214_360);
    }
    // ─── 売上の税額だけで決まる特例 ─────────────────

    // **本命。** 2026年分（令和8年分）は2割特例の最後の年。
    //
    // 「令和8年9月30日までの日の属する課税期間」で終わる。個人事業者の
    // 課税期間は暦年なので、令和8年分（1/1〜12/31）はその日を含む。
    // **9月30日で切ってしまうと、最後の1年を丸ごと落とす。**
    #[test]
    fn two_thousand_twenty_six_is_the_last_year_of_the_twenty_percent_rule() {
        assert_eq!(special_rule_for(2026), Some(SpecialRule::TwentyPercent));
    }

    // **本命。** 2027・2028年分は3割特例。
    #[test]
    fn the_two_years_after_that_use_the_thirty_percent_rule() {
        assert_eq!(special_rule_for(2027), Some(SpecialRule::ThirtyPercent));
        assert_eq!(special_rule_for(2028), Some(SpecialRule::ThirtyPercent));
    }

    // **本命。** 2029年分以降はどちらも使えない。
    #[test]
    fn from_two_thousand_twenty_nine_there_is_no_special_rule() {
        assert_eq!(special_rule_for(2029), None);
        assert_eq!(special_rule_for(2030), None);
    }

    // 制度が始まる前の年には無い。
    #[test]
    fn before_the_invoice_system_there_is_no_special_rule() {
        assert_eq!(special_rule_for(2022), None);
        assert_eq!(special_rule_for(2023), Some(SpecialRule::TwentyPercent));
    }

    // **本命。** 納付税額は売上の税額だけで決まる。
    //
    // 検証帳簿（2026年）の売上税額 887,040円 で、2割なら 177,408円。
    // 一般課税は 887,040 − 214,360 = 672,680円 なので、差は 495,272円。
    #[test]
    fn the_twenty_percent_rule_uses_only_the_sales_tax() {
        assert_eq!(
            tax_under(SpecialRule::TwentyPercent, yen(887_040)).minor(),
            177_408
        );
    }

    #[test]
    fn the_thirty_percent_rule_takes_three_tenths() {
        assert_eq!(
            tax_under(SpecialRule::ThirtyPercent, yen(887_040)).minor(),
            266_112
        );
    }

    // **本命。** 端数は切り捨てる。切り上げると納めすぎになる。
    #[test]
    fn the_remainder_of_the_special_rule_is_dropped() {
        // 999 × 20 / 100 = 199.8
        assert_eq!(tax_under(SpecialRule::TwentyPercent, yen(999)).minor(), 199);
        // 1 × 20 / 100 = 0.2
        assert_eq!(tax_under(SpecialRule::TwentyPercent, yen(1)).minor(), 0);
    }

    // 売上が無ければ0。
    #[test]
    fn no_sales_means_no_tax() {
        assert_eq!(tax_under(SpecialRule::TwentyPercent, yen(0)).minor(), 0);
    }
}
