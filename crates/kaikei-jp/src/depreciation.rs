//! 減価償却の計算（個人事業主）。
//!
//! # このモジュールがすること・しないこと
//!
//! **する**: 取得価額・取得日・償却方法・耐用年数・事業専用割合を**受け取って**、
//! 年ごとの償却費を計算する。四則演算である。
//!
//! **しない**: 耐用年数を決めること、どの償却方法を選ぶかを決めること。
//! どちらも申告上の判断であり、帳簿からは決まらない（`CLAUDE.md` §10）。
//! **入力として受け取る。**
//!
//! ```text
//! 耐用年数・償却方法 ─→ [このモジュール] ─→ 年ごとの償却費
//!   （人が決める）              （計算するだけ）
//! ```
//!
//! # 対応する償却方法
//!
//! | 方法 | 計算 | 月割 | 残す額 |
//! |---|---|---|---|
//! | 定額法 | 取得価額 × 1/耐用年数 | **初年度のみ**（取得月を含む） | 備忘価額 1円 |
//! | 一括償却資産 | 取得価額 × 1/3 を3年 | **しない** | 0円（全額償却） |
//! | 少額減価償却資産 | 取得年に全額 | しない | 0円 |
//!
//! 定率法は実装しない。**個人事業主の法定償却方法は定額法**であり、定率法を
//! 使うには届出が要る。必要になってから足す。
//!
//! # 実例（weBanana.SP）
//!
//! 2022-08-05 取得の「pc」118,800円 は、2022〜2024年に 39,600円ずつ償却されて
//! いた。これは**一括償却資産**（20万円未満・3年均等・月割なし）の形と一致する
//! （パソコンの法定耐用年数は4年なので、定額法なら3年では終わらない）。
//! [`Schedule::lump_sum_matches_the_real_book`] のテストで固定している。

use crate::error::JpError;
use kaikei_core::{AccountingDate, Money, Ratio, RoundMode};

/// 償却方法。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepreciationMethod {
    /// 定額法。取得価額を耐用年数で均等に割る。初年度は月割する。
    ///
    /// 耐用年数は**入力**である（このモジュールは決めない）。
    StraightLine {
        /// 耐用年数（年）。**人が決めて渡す。**
        useful_life_years: u8,
    },

    /// 一括償却資産。取得価額の3分の1を3年。**耐用年数を使わない。**
    ///
    /// 月割もしない（取得が12月でも初年度に3分の1を償却する）。
    LumpSumOverThreeYears,

    /// 少額減価償却資産。取得年に全額を償却する。
    ImmediateExpense,
}

/// 償却の対象。
#[derive(Debug, Clone)]
pub struct FixedAsset {
    /// 資産の名前（決算書の「減価償却費の計算」欄に出す）。
    pub name: String,
    /// 取得年月日。
    pub acquired_on: AccountingDate,
    /// 取得価額。
    pub acquisition_cost: Money,
    /// 償却方法。
    pub method: DepreciationMethod,
    /// 事業専用割合。全部事業なら `None`。
    ///
    /// **按分は最後に1回だけ行う。** 各年の償却費に掛ける（累計に掛けると
    /// 端数処理の回数が変わって合わなくなる）。
    pub business_ratio: Option<Ratio>,
}

/// ある年度の償却費。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YearlyDepreciation {
    /// 会計年度（暦年）。
    pub year: i32,
    /// 事業専用割合を掛ける**前**の償却費。
    pub before_ratio: Money,
    /// 実際に費用にする額（事業専用割合を掛けた後）。
    pub amount: Money,
    /// その年度末の帳簿価額（按分前の未償却残高）。
    pub book_value: Money,
    /// 月割した月数。月割しない方法では常に12。
    pub months: u8,
}

/// 償却の予定表。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    /// 年ごとの償却費（取得年から順に、償却し終わるまで）。
    pub years: Vec<YearlyDepreciation>,
}

impl Schedule {
    /// 指定した年度の償却費を引く。無ければ `None`（償却済み・取得前）。
    pub fn year(&self, year: i32) -> Option<&YearlyDepreciation> {
        self.years.iter().find(|y| y.year == year)
    }

    /// 償却費の合計（按分後）。
    pub fn total(&self) -> Result<Money, JpError> {
        let mut total = Money::from_minor(0, self.currency());
        for y in &self.years {
            total = total
                .add(&y.amount)
                .map_err(|source| JpError::DepreciationArithmetic { source })?;
        }
        Ok(total)
    }

    fn currency(&self) -> kaikei_core::Currency {
        self.years
            .first()
            .map(|y| y.amount.currency())
            .unwrap_or(kaikei_core::Currency::JPY)
    }
}

/// 償却の予定表を作る。
///
/// # 端数
///
/// 円未満は切り捨てる。**最終年で辻褄を合わせる**——毎年切り捨てた分が
/// 積もると、単純に「取得価額 ÷ 年数」を並べただけでは合計が取得価額に
/// 届かない。最終年は「取得価額 − それまでの累計 − 残す額」にする。
///
/// # Errors
///
/// - 耐用年数が0の定額法（`JpError::InvalidFixedAsset`）
/// - 取得価額が0以下（同）
pub fn schedule(asset: &FixedAsset) -> Result<Schedule, JpError> {
    if asset.acquisition_cost.minor() <= 0 {
        return Err(JpError::InvalidFixedAsset {
            reason: format!(
                "取得価額は正の値である必要があります: {}",
                asset.acquisition_cost.to_display_string()
            ),
        });
    }

    let cost = asset.acquisition_cost;
    let currency = cost.currency();
    let first_year = asset.acquired_on.year();

    // 「毎年いくら」「何年」「最後に残す額」の3つに落としてから組み立てる。
    // 方法ごとの違いはここだけで、以降の処理は共通になる。
    let (per_year, count, residual, prorate_first_year) = match asset.method {
        DepreciationMethod::StraightLine { useful_life_years } => {
            if useful_life_years == 0 {
                return Err(JpError::InvalidFixedAsset {
                    reason: "定額法の耐用年数は1年以上である必要があります".to_string(),
                });
            }
            (
                cost.minor() / i128::from(useful_life_years),
                usize::from(useful_life_years),
                // 備忘価額1円を残す。**帳簿から資産が消えないようにするため**で、
                // 除却するまで1円が載り続ける。
                1,
                true,
            )
        }
        DepreciationMethod::LumpSumOverThreeYears => (cost.minor() / 3, 3, 0, false),
        DepreciationMethod::ImmediateExpense => (cost.minor(), 1, 0, false),
    };

    // 初年度の月数（取得月を含む。3月取得なら 3〜12 の10か月）。
    let months = if prorate_first_year {
        12 - asset.acquired_on.month() + 1
    } else {
        12
    };

    let mut years = Vec::with_capacity(count + 1);
    let mut accumulated: i128 = 0;
    // 月割した分は後ろへずれるので、定額法は**耐用年数より1年多くかかる**。
    // 上限を count + 1 にしておく（月割しない方法は count で必ず終わる）。
    let max_years = if prorate_first_year { count + 1 } else { count };
    for index in 0..max_years {
        // まだ償却できる残り。備忘価額はここで差し引く。
        let remaining = cost.minor() - accumulated - residual;
        if remaining <= 0 {
            break;
        }

        let mut raw = if index == 0 && prorate_first_year {
            per_year * i128::from(months) / 12
        } else {
            per_year
        };
        // **月割しない方法は最後の年で辻褄を合わせる。** 毎年切り捨てた分が
        // 積もると、単純に並べただけでは合計が取得価額に届かない。
        if !prorate_first_year && index == count - 1 {
            raw = remaining;
        }
        // 残りを超えて償却しない（定額法の最終年はここで頭打ちになる）。
        if raw > remaining {
            raw = remaining;
        }
        if raw <= 0 {
            break;
        }
        accumulated += raw;

        let before_ratio = Money::from_minor(raw, currency);
        let amount = match asset.business_ratio {
            // 按分は各年の償却費に掛ける。切り捨ては帳簿の既定に合わせる。
            Some(ratio) => before_ratio
                .mul_ratio(ratio, RoundMode::Floor)
                .map_err(|source| JpError::DepreciationArithmetic { source })?,
            None => before_ratio,
        };
        years.push(YearlyDepreciation {
            year: first_year + i32::try_from(index).unwrap_or(0),
            before_ratio,
            amount,
            book_value: Money::from_minor(cost.minor() - accumulated, currency),
            months: if index == 0 { months } else { 12 },
        });
    }

    Ok(Schedule { years })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_core::Currency;

    fn yen(v: i128) -> Money {
        Money::from_minor(v, Currency::JPY)
    }

    fn on(year: i32, month: u8, day: u8) -> AccountingDate {
        AccountingDate::new(year, month, day).unwrap()
    }

    fn asset(cost: i128, acquired: AccountingDate, method: DepreciationMethod) -> FixedAsset {
        FixedAsset {
            name: "テスト資産".to_string(),
            acquired_on: acquired,
            acquisition_cost: yen(cost),
            method,
            business_ratio: None,
        }
    }

    fn amounts(s: &Schedule) -> Vec<(i32, i128)> {
        s.years.iter().map(|y| (y.year, y.amount.minor())).collect()
    }

    // **本命。** 実帳簿の「pc」と一致する。
    //
    // 2022-08-05 取得の 118,800円 は 2022〜2024年に 39,600円ずつ償却されていた。
    // 一括償却資産（3年均等・月割なし）の形である。8月取得でも初年度が
    // 満額なのが決め手で、定額法なら月割で 5/12 になる。
    #[test]
    fn lump_sum_matches_the_real_book() {
        let s = schedule(&asset(
            118_800,
            on(2022, 8, 5),
            DepreciationMethod::LumpSumOverThreeYears,
        ))
        .unwrap();

        assert_eq!(
            amounts(&s),
            vec![(2022, 39_600), (2023, 39_600), (2024, 39_600)]
        );
        assert_eq!(s.years[0].months, 12, "一括償却は月割しない");
        assert_eq!(s.years[2].book_value.minor(), 0, "3年で全額償却する");
        assert_eq!(s.total().unwrap().minor(), 118_800);
    }

    // **本命。** 定額法は初年度を月割し、耐用年数より1年多くかかる。
    //
    // 2025-07-24 取得のパソコン 280,717円 / 耐用年数4年。
    // 7月取得なので初年度は 6か月分。
    #[test]
    fn straight_line_prorates_the_first_year_and_needs_one_more_year() {
        let s = schedule(&asset(
            280_717,
            on(2025, 7, 24),
            DepreciationMethod::StraightLine {
                useful_life_years: 4,
            },
        ))
        .unwrap();

        assert_eq!(s.years[0].months, 6, "7月取得は 7〜12 の6か月");
        assert_eq!(
            amounts(&s),
            vec![
                (2025, 35_089),
                (2026, 70_179),
                (2027, 70_179),
                (2028, 70_179),
                (2029, 35_090),
            ],
            "耐用年数4年でも5暦年にわたる"
        );
        assert_eq!(
            s.total().unwrap().minor(),
            280_716,
            "備忘価額1円を残すので取得価額より1円少ない"
        );
        assert_eq!(s.years.last().unwrap().book_value.minor(), 1);
    }

    // 3月取得の自転車（耐用年数2年）。
    #[test]
    fn straight_line_over_two_years_from_march() {
        let s = schedule(&asset(
            108_000,
            on(2025, 3, 10),
            DepreciationMethod::StraightLine {
                useful_life_years: 2,
            },
        ))
        .unwrap();

        assert_eq!(s.years[0].months, 10, "3月取得は 3〜12 の10か月");
        assert_eq!(
            amounts(&s),
            vec![(2025, 45_000), (2026, 54_000), (2027, 8_999)]
        );
        assert_eq!(s.total().unwrap().minor(), 107_999);
    }

    // 1月取得なら月割しても満額なので、耐用年数どおりで終わる。
    #[test]
    fn acquiring_in_january_finishes_within_the_useful_life() {
        let s = schedule(&asset(
            120_000,
            on(2025, 1, 1),
            DepreciationMethod::StraightLine {
                useful_life_years: 3,
            },
        ))
        .unwrap();

        assert_eq!(s.years[0].months, 12);
        assert_eq!(
            amounts(&s),
            vec![(2025, 40_000), (2026, 40_000), (2027, 39_999)]
        );
        assert_eq!(s.years.last().unwrap().book_value.minor(), 1, "備忘価額1円");
    }

    // 12月取得は初年度が1か月分。
    #[test]
    fn acquiring_in_december_depreciates_one_month() {
        let s = schedule(&asset(
            120_000,
            on(2025, 12, 20),
            DepreciationMethod::StraightLine {
                useful_life_years: 3,
            },
        ))
        .unwrap();

        assert_eq!(s.years[0].months, 1);
        assert_eq!(s.years[0].amount.minor(), 3_333, "40,000 × 1/12 を切り捨て");
    }

    // 少額減価償却資産は取得年に全額。
    #[test]
    fn immediate_expense_takes_everything_in_the_first_year() {
        let s = schedule(&asset(
            280_717,
            on(2025, 7, 24),
            DepreciationMethod::ImmediateExpense,
        ))
        .unwrap();

        assert_eq!(amounts(&s), vec![(2025, 280_717)]);
        assert_eq!(s.years[0].book_value.minor(), 0);
    }

    // **本命。** 切り捨てた分を最後の年で吸収し、合計が取得価額に届く。
    //
    // 100,000 ÷ 3 = 33,333.33...。単純に並べると 99,999 にしかならない。
    #[test]
    fn rounding_leftovers_are_absorbed_by_the_last_year() {
        let s = schedule(&asset(
            100_000,
            on(2025, 5, 1),
            DepreciationMethod::LumpSumOverThreeYears,
        ))
        .unwrap();

        assert_eq!(
            amounts(&s),
            vec![(2025, 33_333), (2026, 33_333), (2027, 33_334)]
        );
        assert_eq!(
            s.total().unwrap().minor(),
            100_000,
            "合計が取得価額に一致する"
        );
    }

    // 事業専用割合は各年の償却費に掛ける。
    #[test]
    fn the_business_ratio_is_applied_to_each_year() {
        let mut a = asset(
            120_000,
            on(2025, 1, 1),
            DepreciationMethod::StraightLine {
                useful_life_years: 3,
            },
        );
        a.business_ratio = Some(Ratio::parse_fraction("0.8").unwrap());
        let s = schedule(&a).unwrap();

        assert_eq!(
            s.years[0].before_ratio.minor(),
            40_000,
            "按分前は変わらない"
        );
        assert_eq!(s.years[0].amount.minor(), 32_000, "40,000 × 0.8");
        assert_eq!(
            s.years[0].book_value.minor(),
            80_000,
            "帳簿価額は按分前で追う（資産の未償却残高そのもの）"
        );
    }

    // 入力が不正なら計算しない。
    #[test]
    fn a_zero_useful_life_is_rejected() {
        let error = schedule(&asset(
            100_000,
            on(2025, 1, 1),
            DepreciationMethod::StraightLine {
                useful_life_years: 0,
            },
        ))
        .unwrap_err();
        assert!(format!("{error}").contains("耐用年数"), "{error}");
    }

    #[test]
    fn a_zero_cost_is_rejected() {
        let error = schedule(&asset(
            0,
            on(2025, 1, 1),
            DepreciationMethod::LumpSumOverThreeYears,
        ))
        .unwrap_err();
        assert!(format!("{error}").contains("取得価額"), "{error}");
    }

    // 年で引ける。
    #[test]
    fn a_year_can_be_looked_up() {
        let s = schedule(&asset(
            118_800,
            on(2022, 8, 5),
            DepreciationMethod::LumpSumOverThreeYears,
        ))
        .unwrap();
        assert_eq!(s.year(2023).unwrap().amount.minor(), 39_600);
        assert!(s.year(2025).is_none(), "償却し終わった年は無い");
        assert!(s.year(2021).is_none(), "取得前の年も無い");
    }
}
