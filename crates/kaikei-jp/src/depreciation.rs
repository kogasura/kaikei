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
    /// 定額法の償却率（千分率）。定額法以外は `None`。
    ///
    /// 青色申告決算書の「減価償却費の計算」欄に**償却率を書く欄がある**ので、
    /// 計算に使った値をそのまま出せるようにしておく。
    pub rate_per_mille: Option<u32>,
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

/// 定額法の償却率（千分率）。`0.334` なら `334` を返す。
///
/// # 1/耐用年数 ではない
///
/// 減価償却資産の耐用年数等に関する省令 別表第八の定額法償却率は、
/// **1/耐用年数を小数第3位に切り上げた値**である。
///
/// | 耐用年数 | 1/n | 償却率 |
/// |---|---|---|
/// | 2年 | 0.500000 | 0.500 |
/// | **3年** | 0.333333 | **0.334** |
/// | 4年 | 0.250000 | 0.250 |
/// | **6年** | 0.166667 | **0.167** |
/// | **7年** | 0.142857 | **0.143** |
///
/// 切り上げなので、償却率を掛けた額の合計は取得価額を**超える**。
/// 最終年で残額まで頭打ちにすることで辻褄が合う（[`schedule`] の doc）。
///
/// **2〜20年について別表第八と一致することを確かめた**
/// （`the_rate_matches_the_published_table`）。それより長い耐用年数は
/// 表と突き合わせていない——同じ規則で導けるはずだが、確認していない。
pub fn straight_line_rate_per_mille(useful_life_years: u8) -> u32 {
    // 切り上げ除算。1/n を小数第3位で切り上げる（＝1000/n の切り上げ）。
    1000_u32.div_ceil(u32::from(useful_life_years))
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
    let mut rate_per_mille = None;

    // 「毎年いくら」「何年」「最後に残す額」の3つに落としてから組み立てる。
    // 方法ごとの違いはここだけで、以降の処理は共通になる。
    let (per_year, count, residual, prorate_first_year) = match asset.method {
        DepreciationMethod::StraightLine { useful_life_years } => {
            if useful_life_years == 0 {
                return Err(JpError::InvalidFixedAsset {
                    reason: "定額法の耐用年数は1年以上である必要があります".to_string(),
                });
            }
            let rate = straight_line_rate_per_mille(useful_life_years);
            rate_per_mille = Some(rate);
            (
                // 取得価額 × 償却率。円未満は切り捨てる。
                cost.minor() * i128::from(rate) / 1000,
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

    Ok(Schedule {
        years,
        rate_per_mille,
    })
}

// ─── 取得価額と償却方法の食い違い ──────────────────────────

/// 取得価額から見て、その償却方法が選べるかの指摘。
///
/// **エラーではない。** 判定には帳簿から分からない要素（後述）が絡むので、
/// 断定せず「確かめる価値がある」と伝えるにとどめる（`CLAUDE.md` §10）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CostConcern {
    /// 何が引っかかったか。
    pub message: String,
    /// 根拠の条文。**必ず付ける**——確かめようがない指摘は無視されるだけである。
    pub basis: &'static str,
}

/// 一括償却資産の上限（所令139条）。20万円未満。
const LUMP_SUM_LIMIT: i128 = 200_000;
/// 少額減価償却資産の特例の上限（措法28条の2）。30万円未満。
///
/// **令和8年4月1日以後の取得は40万円未満に引き上げられた**（令和8年度税制改正）。
/// ただしこの数字は税制改正大綱に基づく二次情報での確認にとどまり、公布後の
/// 条文を当たれていない。**だから引き上げ後の額でこちらから通すことはしない。**
/// 30万円を超える取得を見たら、引き上げの可能性がある旨を添えて指摘する。
const IMMEDIATE_LIMIT_BEFORE_R8_APRIL: i128 = 300_000;
/// 減価償却をしない額（所令138条）。10万円未満。
const NOT_DEPRECIABLE_LIMIT: i128 = 100_000;

/// 取得価額と償却方法が食い違っていないかを見る。
///
/// # なぜ要るのか
///
/// **選べない方法を選んでも、帳簿は何も言わない。** 50万円の資産に少額特例を
/// 当てれば初年度に50万円が経費になり、決算書の貸借は一致したまま所得だけが
/// 減る。`verify` でも拾えない——台帳に書いてある方法で計算した結果と帳簿が
/// 一致してしまうからである。
///
/// # 断定しない理由
///
/// 判定額は**経理方式**で変わる（タックスアンサー No.2100「取得価額の判定に
/// 際し、消費税の額を含めるかどうかは納税者の経理方式によります」）。
/// また少額特例には青色申告・年間300万円・貸付用でないことなど、取得価額
/// 以外の要件がある。**ここで見るのは金額の帯だけ**である。
///
/// # 税込経理で境目をまたぐ場合
///
/// 実例がある。電動アシスト自転車等 108,000円（税込）は10万円以上だが、
/// 税抜なら 98,181円 で10万円を下回り、**全額を必要経費にできる区分**
/// （所令138条）に落ちる。金額の帯が変わるので、経理方式の選択が償却額を
/// 左右する。境目の近くではそれを伝える。
pub fn cost_concerns(
    cost: &Money,
    method: DepreciationMethod,
    tax_mode: Option<crate::tax::TaxMode>,
) -> Vec<CostConcern> {
    let amount = cost.minor();
    let mut concerns = Vec::new();

    match method {
        DepreciationMethod::LumpSumOverThreeYears if amount >= LUMP_SUM_LIMIT => {
            concerns.push(CostConcern {
                message: format!(
                    "取得価額 {amount} 円は20万円以上です。一括償却資産は20万円未満が対象です。"
                ),
                basis: "所得税法施行令139条",
            });
        }
        DepreciationMethod::ImmediateExpense if amount >= IMMEDIATE_LIMIT_BEFORE_R8_APRIL => {
            concerns.push(CostConcern {
                message: format!(
                    "取得価額 {amount} 円は30万円以上です。少額減価償却資産の特例は30万円未満が対象です
      （令和8年4月1日以後の取得は40万円未満に引き上げられたとされますが、条文で確認できていません）。"
                ),
                basis: "租税特別措置法28条の2",
            });
        }
        _ => {}
    }

    // 10万円未満は、そもそも償却せず全額を必要経費にできる区分がある。
    if amount < NOT_DEPRECIABLE_LIMIT && !matches!(method, DepreciationMethod::ImmediateExpense) {
        concerns.push(CostConcern {
            message: format!(
                "取得価額 {amount} 円は10万円未満です。償却せず全額を必要経費にできる区分があります。"
            ),
            basis: "所得税法施行令138条",
        });
    }

    // 税込経理で、税抜にすると帯が変わる場合。
    //
    // **経理方式が分からないときは「分からない」と言う。** 黙って片方だと
    // 決めると、帯が変わることに気づけないまま選択が固まる。
    if let Some(exclusive) = the_amount_without_tax_if_it_crosses_a_band(amount) {
        match tax_mode {
            Some(crate::tax::TaxMode::Inclusive) => concerns.push(CostConcern {
                message: format!(
                    "この帳簿は税込経理です。取得価額 {amount} 円は{}以上ですが、税抜なら {} 円で{}を下回ります。
      **経理方式によって選べる扱いが変わります。**",
                    exclusive.label, exclusive.amount, exclusive.label
                ),
                basis: "タックスアンサー No.2100（取得価額の判定は経理方式による）",
            }),
            Some(crate::tax::TaxMode::Exclusive) => {}
            None => concerns.push(CostConcern {
                message: format!(
                    "取得価額 {amount} 円は{}の境目に近い額です（税込なら税抜 {} 円で{}を下回ります）。
      **経理方式が分からないので確かめられませんでした**（KAIKEI_TAX_MODE が未設定）。",
                    exclusive.label, exclusive.amount, exclusive.label
                ),
                basis: "タックスアンサー No.2100（取得価額の判定は経理方式による）",
            }),
        }
    }

    concerns
}

/// 帯をまたぐ場合の、税抜額とその境目。
struct AcrossABand {
    amount: i128,
    label: &'static str,
}

/// 税込の額から税抜（10%）に直すと帯が変わるか。
///
/// **10%で見る。** 軽減税率8%の資産（固定資産で該当することはまず無い）を
/// 網羅するより、境目をまたぐことに気づける方が大事である。
fn the_amount_without_tax_if_it_crosses_a_band(inclusive: i128) -> Option<AcrossABand> {
    // 税抜額 = 税込 ÷ 1.1 の切り捨て。整数で計算する。
    let exclusive = inclusive * 10 / 11;
    for (limit, label) in [
        (NOT_DEPRECIABLE_LIMIT, "10万円"),
        (LUMP_SUM_LIMIT, "20万円"),
        (IMMEDIATE_LIMIT_BEFORE_R8_APRIL, "30万円"),
    ] {
        if inclusive >= limit && exclusive < limit {
            return Some(AcrossABand {
                amount: exclusive,
                label,
            });
        }
    }
    None
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

    // **本命。** 定額法の償却率が別表第八と一致する。
    //
    // 1/耐用年数ではない。**小数第3位への切り上げ**である。
    // 3年・6年・7年・9年・11〜15年で 1/n と食い違う。
    #[test]
    fn the_rate_matches_the_published_table() {
        // 減価償却資産の耐用年数等に関する省令 別表第八（定額法・2〜20年）。
        let table = [
            (2u8, 500u32),
            (3, 334),
            (4, 250),
            (5, 200),
            (6, 167),
            (7, 143),
            (8, 125),
            (9, 112),
            (10, 100),
            (11, 91),
            (12, 84),
            (13, 77),
            (14, 72),
            (15, 67),
            (16, 63),
            (17, 59),
            (18, 56),
            (19, 53),
            (20, 50),
        ];
        for (years, expected) in table {
            assert_eq!(
                straight_line_rate_per_mille(years),
                expected,
                "耐用年数 {years} 年の償却率"
            );
        }
    }

    // **本命。** 3年は 1/3 ではなく 0.334 で計算する。
    //
    // 100,000 ÷ 3 = 33,333 ではなく 100,000 × 0.334 = 33,400。
    #[test]
    fn a_three_year_life_uses_the_rounded_up_rate() {
        let s = schedule(&asset(
            100_000,
            on(2025, 1, 1),
            DepreciationMethod::StraightLine {
                useful_life_years: 3,
            },
        ))
        .unwrap();

        assert_eq!(s.rate_per_mille, Some(334));
        assert_eq!(
            amounts(&s),
            vec![(2025, 33_400), (2026, 33_400), (2027, 33_199)],
            "最終年は残額まで（33,400 × 3 = 100,200 は取得価額を超える）"
        );
        assert_eq!(s.total().unwrap().minor(), 99_999, "備忘価額1円を残す");
        assert_eq!(s.years.last().unwrap().book_value.minor(), 1);
    }

    // 償却率は定額法のときだけ出る。
    #[test]
    fn only_straight_line_has_a_rate() {
        let lump = schedule(&asset(
            118_800,
            on(2022, 8, 5),
            DepreciationMethod::LumpSumOverThreeYears,
        ))
        .unwrap();
        assert_eq!(lump.rate_per_mille, None);

        let immediate = schedule(&asset(
            100_000,
            on(2025, 1, 1),
            DepreciationMethod::ImmediateExpense,
        ))
        .unwrap();
        assert_eq!(immediate.rate_per_mille, None);
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
            vec![(2025, 40_080), (2026, 40_080), (2027, 39_839)],
            "償却率 0.334（1/3 ではない）。最終年は残額まで"
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
        assert_eq!(
            s.years[0].amount.minor(),
            3_340,
            "120,000 × 0.334 = 40,080 の 1/12 を切り捨て"
        );
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
            40_080,
            "按分前は変わらない"
        );
        assert_eq!(s.years[0].amount.minor(), 32_064, "40,080 × 0.8");
        assert_eq!(
            s.years[0].book_value.minor(),
            79_920,
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

    // ---- 取得価額と償却方法の食い違い ----

    use crate::tax::TaxMode;

    /// **本命。** 20万円以上に一括償却を当てたら指摘する。
    ///
    /// 実帳簿のパソコン・周辺機器 280,717円 でこれを選ぶと、3年で全額が
    /// 経費になる。所令139条は20万円未満が対象である。
    #[test]
    fn lump_sum_over_two_hundred_thousand_is_flagged() {
        let concerns = cost_concerns(
            &yen(280_717),
            DepreciationMethod::LumpSumOverThreeYears,
            Some(TaxMode::Exclusive),
        );
        assert_eq!(concerns.len(), 1, "{concerns:?}");
        assert!(concerns[0].message.contains("20万円未満"), "{concerns:?}");
        assert_eq!(concerns[0].basis, "所得税法施行令139条");
    }

    /// 20万円ちょうどは対象外（「20万円未満」なので）。
    #[test]
    fn lump_sum_at_exactly_two_hundred_thousand_is_flagged() {
        let concerns = cost_concerns(
            &yen(200_000),
            DepreciationMethod::LumpSumOverThreeYears,
            Some(TaxMode::Exclusive),
        );
        assert_eq!(concerns.len(), 1, "20万円ちょうどは帯の外: {concerns:?}");
    }

    #[test]
    fn lump_sum_just_under_the_limit_is_fine() {
        let concerns = cost_concerns(
            &yen(199_999),
            DepreciationMethod::LumpSumOverThreeYears,
            Some(TaxMode::Exclusive),
        );
        assert!(concerns.is_empty(), "{concerns:?}");
    }

    /// **本命。** 30万円以上に少額特例を当てたら指摘する。
    #[test]
    fn the_immediate_expense_over_three_hundred_thousand_is_flagged() {
        let concerns = cost_concerns(
            &yen(500_000),
            DepreciationMethod::ImmediateExpense,
            Some(TaxMode::Exclusive),
        );
        assert_eq!(concerns.len(), 1, "{concerns:?}");
        assert!(concerns[0].message.contains("30万円未満"), "{concerns:?}");
        assert_eq!(concerns[0].basis, "租税特別措置法28条の2");
    }

    /// 実帳簿の 280,717円 に少額特例は帯としては通る。
    #[test]
    fn the_real_book_pc_fits_the_immediate_expense_band() {
        let concerns = cost_concerns(
            &yen(280_717),
            DepreciationMethod::ImmediateExpense,
            Some(TaxMode::Exclusive),
        );
        assert!(concerns.is_empty(), "{concerns:?}");
    }

    /// **本命。** 10万円未満は、償却せず全額を経費にできる区分がある。
    #[test]
    fn under_one_hundred_thousand_points_at_the_immediate_deduction() {
        let concerns = cost_concerns(
            &yen(98_181),
            DepreciationMethod::LumpSumOverThreeYears,
            Some(TaxMode::Exclusive),
        );
        assert_eq!(concerns.len(), 1, "{concerns:?}");
        assert_eq!(concerns[0].basis, "所得税法施行令138条");
    }

    /// 少額特例を選んでいるなら、10万円未満でも重ねて言わない。
    ///
    /// どちらも初年度に全額が経費になるので、言っても行動が変わらない。
    #[test]
    fn the_immediate_expense_under_one_hundred_thousand_says_nothing() {
        let concerns = cost_concerns(
            &yen(98_181),
            DepreciationMethod::ImmediateExpense,
            Some(TaxMode::Exclusive),
        );
        assert!(concerns.is_empty(), "{concerns:?}");
    }

    /// **本命。** 税込経理で境目をまたぐ額は、それを伝える。
    ///
    /// 実帳簿の電動アシスト自転車等 108,000円（税込）は10万円以上だが、
    /// 税抜なら 98,181円 で10万円を下回る。**経理方式の選択が償却額を
    /// 左右する**ので、気づけないと選択そのものを誤る。
    #[test]
    fn an_amount_that_crosses_a_band_without_tax_is_reported() {
        let concerns = cost_concerns(
            &yen(108_000),
            DepreciationMethod::LumpSumOverThreeYears,
            Some(TaxMode::Inclusive),
        );
        assert_eq!(concerns.len(), 1, "{concerns:?}");
        assert!(
            concerns[0].message.contains("98181"),
            "税抜額を出すこと: {concerns:?}"
        );
        assert!(concerns[0].message.contains("10万円"), "{concerns:?}");
    }

    /// 税抜経理なら、この指摘は出ない（既に税抜で入っている）。
    #[test]
    fn the_band_note_is_only_for_tax_inclusive_books() {
        let concerns = cost_concerns(
            &yen(108_000),
            DepreciationMethod::LumpSumOverThreeYears,
            Some(TaxMode::Exclusive),
        );
        assert!(concerns.is_empty(), "{concerns:?}");
    }

    /// 境目から離れていれば言わない（毎回出る注意書きにしない）。
    #[test]
    fn an_amount_far_from_a_band_says_nothing() {
        let concerns = cost_concerns(
            &yen(150_000),
            DepreciationMethod::LumpSumOverThreeYears,
            Some(TaxMode::Inclusive),
        );
        assert!(concerns.is_empty(), "{concerns:?}");
    }

    /// **本命。** 経理方式が分からないときは「分からない」と言う。
    ///
    /// 黙って片方だと決めると、帯が変わることに気づけないまま選択が固まる。
    /// なお帯の検査（所令138/139・措法28の2）は経理方式なしでも動く——
    /// **設定が無いことを理由に、本体の検査まで止めない。**
    #[test]
    fn an_unknown_tax_mode_is_said_out_loud() {
        let concerns = cost_concerns(
            &yen(108_000),
            DepreciationMethod::LumpSumOverThreeYears,
            None,
        );
        assert_eq!(concerns.len(), 1, "{concerns:?}");
        assert!(
            concerns[0].message.contains("確かめられませんでした"),
            "{concerns:?}"
        );
        assert!(
            concerns[0].message.contains("KAIKEI_TAX_MODE"),
            "何を設定すればよいか言うこと: {concerns:?}"
        );
    }

    /// 経理方式が分からなくても、帯の検査そのものは動く。
    #[test]
    fn the_band_check_works_without_a_tax_mode() {
        let concerns = cost_concerns(&yen(500_000), DepreciationMethod::ImmediateExpense, None);
        assert_eq!(concerns.len(), 1, "{concerns:?}");
        assert_eq!(concerns[0].basis, "租税特別措置法28条の2");
    }

    /// 境目から離れていれば、経理方式が分からなくても黙る。
    #[test]
    fn an_unknown_tax_mode_says_nothing_far_from_a_band() {
        let concerns = cost_concerns(
            &yen(150_000),
            DepreciationMethod::LumpSumOverThreeYears,
            None,
        );
        assert!(concerns.is_empty(), "{concerns:?}");
    }

    /// 定額法には帯の制限が無い（どの額でも選べる）。
    #[test]
    fn the_straight_line_has_no_band() {
        for amount in [100_000_i128, 280_717, 5_000_000] {
            let concerns = cost_concerns(
                &yen(amount),
                DepreciationMethod::StraightLine {
                    useful_life_years: 4,
                },
                Some(TaxMode::Exclusive),
            );
            assert!(concerns.is_empty(), "{amount}: {concerns:?}");
        }
    }

    /// 指摘には必ず条文を付ける。確かめようがない指摘は無視されるだけである。
    #[test]
    fn every_concern_carries_its_basis() {
        let cases = [
            (
                280_717_i128,
                DepreciationMethod::LumpSumOverThreeYears,
                Some(TaxMode::Exclusive),
            ),
            (
                500_000,
                DepreciationMethod::ImmediateExpense,
                Some(TaxMode::Exclusive),
            ),
            (
                98_181,
                DepreciationMethod::LumpSumOverThreeYears,
                Some(TaxMode::Exclusive),
            ),
            (
                108_000,
                DepreciationMethod::LumpSumOverThreeYears,
                Some(TaxMode::Inclusive),
            ),
        ];
        for (amount, method, mode) in cases {
            for concern in cost_concerns(&yen(amount), method, mode) {
                assert!(!concern.basis.is_empty(), "{amount}");
                assert!(!concern.message.is_empty(), "{amount}");
            }
        }
    }
}
