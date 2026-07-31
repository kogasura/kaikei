//! 金額と通貨。
//!
//! 内部表現は最小通貨単位の整数（`i128`）。`f64` は使わない（`CLAUDE.md` §8）。
//! 異通貨の加減算は型では防がず `Result` で弾く（`DECISIONS.md` D-003）。

use crate::error::CoreError;
use rust_decimal::{Decimal, RoundingStrategy};
use std::fmt::Write as _;
use std::str::FromStr;

/// 通貨。ISO 4217 風のコード（英大文字3文字）と小数桁数を持つ。
///
/// 小数桁数は通貨ごとに異なる（JPY=0, USD=2, KWD=3）。
/// 「金額 = セント」という前提を core は置かない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Currency {
    code: [u8; 3],
    minor_unit: u8,
}

impl Currency {
    /// 日本円。小数桁なし。
    pub const JPY: Currency = Currency {
        code: *b"JPY",
        minor_unit: 0,
    };

    /// 米ドル。小数2桁。
    pub const USD: Currency = Currency {
        code: *b"USD",
        minor_unit: 2,
    };

    /// `minor_unit` に許容する最大値（18）。
    ///
    /// ISO 4217 の実在通貨で最大の小数桁数は 4 桁だが、`DOMAIN.md` は
    /// 暗号資産（8桁以上）への対応を示唆しており、Ethereum の最小単位 Wei は
    /// 18 桁である。これを許容範囲の上限とする。上限が無いと
    /// `to_display_string` の `10u128.pow(minor_unit)` が桁あふれし、
    /// 金額を無言に誤表示（release ビルド）または panic（debug ビルド）させる
    /// （`DECISIONS.md` D-020）。
    pub const MAX_MINOR_UNIT: u8 = 18;

    /// 通貨を作る。コードは英大文字3文字、`minor_unit` は 0〜`MAX_MINOR_UNIT`
    /// でなければならない。
    pub fn new(code: &str, minor_unit: u8) -> Result<Self, CoreError> {
        let bytes = code.as_bytes();
        if bytes.len() != 3 || !bytes.iter().all(u8::is_ascii_uppercase) {
            return Err(CoreError::InvalidValue {
                reason: format!("通貨コードは英大文字3文字である必要があります: \"{code}\""),
            });
        }
        if minor_unit > Self::MAX_MINOR_UNIT {
            return Err(CoreError::InvalidValue {
                reason: format!(
                    "小数桁数は0〜{}である必要があります: {minor_unit}",
                    Self::MAX_MINOR_UNIT
                ),
            });
        }
        let mut array = [0u8; 3];
        array.copy_from_slice(bytes);
        Ok(Currency {
            code: array,
            minor_unit,
        })
    }

    /// 通貨コード（英大文字3文字）を返す。
    pub fn code(&self) -> &str {
        // 構築経路（定数 / `new`）で常に有効な ASCII 大文字のみを許容しているため安全。
        std::str::from_utf8(&self.code).expect("Currency::code は常に有効なASCIIである")
    }

    /// 小数桁数を返す。
    pub fn minor_unit(&self) -> u8 {
        self.minor_unit
    }
}

/// 金額。最小通貨単位の整数で保持する。`f64` は使わない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Money {
    minor: i128,
    currency: Currency,
}

impl Money {
    /// 最小通貨単位の整数値から直接構築する。
    pub fn from_minor(minor: i128, currency: Currency) -> Self {
        Money { minor, currency }
    }

    /// `"1234.56"` のような文字列から構築する。
    ///
    /// 小数桁数が `currency.minor_unit()` を超えたらエラー。
    /// 小数を扱えない通貨（`minor_unit() == 0`）に小数を渡したらエラー。
    pub fn parse(s: &str, currency: Currency) -> Result<Self, CoreError> {
        let invalid_amount = |reason: String| CoreError::InvalidAmount { reason };

        let (negative, rest) = match s.strip_prefix('-') {
            Some(r) => (true, r),
            None => (false, s),
        };

        let mut parts = rest.splitn(2, '.');
        let integer_part = parts.next().expect("splitn は常に1要素以上を返す");
        let decimal_part = parts.next();

        let integer_digits = validate_and_strip_thousands_separators(integer_part, s)?;

        let minor_unit = currency.minor_unit() as usize;

        let decimal_digits = match decimal_part {
            Some(digits) => {
                if minor_unit == 0 {
                    return Err(invalid_amount(format!(
                        "{} は小数を扱えない通貨です: \"{s}\"",
                        currency.code()
                    )));
                }
                if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(invalid_amount(format!("数値として解釈できません: \"{s}\"")));
                }
                if digits.len() > minor_unit {
                    return Err(invalid_amount(format!(
                        "{} の小数桁数（{minor_unit}）を超えています: \"{s}\"",
                        currency.code()
                    )));
                }
                let mut padded = String::with_capacity(minor_unit);
                padded.push_str(digits);
                padded.push_str(&"0".repeat(minor_unit - digits.len()));
                padded
            }
            None => "0".repeat(minor_unit),
        };

        // magnitude は u128 で受ける。i128::MIN の絶対値（i128::MAX + 1）は
        // i128 で表現できないため、符号を反映する段階で特別扱いする。
        let magnitude: u128 = format!("{integer_digits}{decimal_digits}")
            .parse()
            .map_err(|_| invalid_amount(format!("数値として解釈できません: \"{s}\"")))?;

        let minor = match (negative, i128::try_from(magnitude)) {
            (false, Ok(m)) => m,
            (true, Ok(m)) => -m,
            (true, Err(_)) if magnitude == i128::MIN.unsigned_abs() => i128::MIN,
            _ => return Err(invalid_amount(format!("数値として解釈できません: \"{s}\""))),
        };
        Ok(Money { minor, currency })
    }

    /// ゼロ金額を作る。
    pub fn zero(currency: Currency) -> Self {
        Money { minor: 0, currency }
    }

    /// 最小通貨単位の整数値を返す。
    pub fn minor(&self) -> i128 {
        self.minor
    }

    /// 通貨を返す。
    pub fn currency(&self) -> Currency {
        self.currency
    }

    /// ゼロかどうか。
    pub fn is_zero(&self) -> bool {
        self.minor == 0
    }

    /// 負値かどうか。
    pub fn is_negative(&self) -> bool {
        self.minor < 0
    }

    /// 加算する。異通貨は `CoreError::CurrencyMismatch`、オーバーフローは
    /// `CoreError::InvalidAmount` を返す。
    pub fn add(&self, other: &Money) -> Result<Money, CoreError> {
        self.ensure_same_currency(other)?;
        let minor =
            self.minor
                .checked_add(other.minor)
                .ok_or_else(|| CoreError::InvalidAmount {
                    reason: format!(
                        "加算結果が範囲を超えました: {} + {}",
                        self.minor, other.minor
                    ),
                })?;
        Ok(Money {
            minor,
            currency: self.currency,
        })
    }

    /// 減算する。異通貨は `CoreError::CurrencyMismatch`、オーバーフローは
    /// `CoreError::InvalidAmount` を返す。
    pub fn sub(&self, other: &Money) -> Result<Money, CoreError> {
        self.ensure_same_currency(other)?;
        let minor =
            self.minor
                .checked_sub(other.minor)
                .ok_or_else(|| CoreError::InvalidAmount {
                    reason: format!(
                        "減算結果が範囲を超えました: {} - {}",
                        self.minor, other.minor
                    ),
                })?;
        Ok(Money {
            minor,
            currency: self.currency,
        })
    }

    /// 符号反転した金額を返す。
    ///
    /// # Panics
    ///
    /// `minor()` が `i128::MIN` の場合（符号反転を表現できない）。
    /// 現実的な会計金額では発生しない前提。
    pub fn neg(&self) -> Money {
        Money {
            minor: self
                .minor
                .checked_neg()
                .expect("Money::neg: i128::MIN の符号反転は表現できません"),
            currency: self.currency,
        }
    }

    /// 絶対値の金額を返す。
    ///
    /// # Panics
    ///
    /// `minor()` が `i128::MIN` の場合（絶対値を表現できない）。
    /// 現実的な会計金額では発生しない前提。
    pub fn abs(&self) -> Money {
        Money {
            minor: self
                .minor
                .checked_abs()
                .expect("Money::abs: i128::MIN の絶対値は表現できません"),
            currency: self.currency,
        }
    }

    /// 按分等に使用する。丸めは `mode` に従う。
    /// 符号付き `minor` に対する標準的な丸め（Floor/Ceil は数直線上の切捨/切上）に従う。
    ///
    /// `minor` 自体が `rust_decimal::Decimal` の表現上限（約7.9×10^28）を超える場合、
    /// または**金額 × 比率の結果**が表現上限を超える場合は `CoreError::InvalidAmount` を返す。
    /// `Ratio::parse_rate` は上限を設けていないため、小さい金額でも巨大な比率と
    /// 掛け合わせれば積が上限を超えうる（`DECISIONS.md` D-018 / D-020）。
    pub fn mul_ratio(&self, ratio: Ratio, mode: RoundMode) -> Result<Money, CoreError> {
        let base = Decimal::try_from_i128_with_scale(self.minor, 0).map_err(|_| {
            CoreError::InvalidAmount {
                reason: format!(
                    "金額が範囲外です（{} は rust_decimal の表現上限を超えています）",
                    self.minor
                ),
            }
        })?;
        let exact = base.checked_mul(ratio.as_decimal()).ok_or_else(|| {
            CoreError::InvalidAmount {
                reason: format!(
                    "金額 {} と比率 {} の積が rust_decimal の表現上限（約7.9×10^28）を超えています。\
                     より小さい金額または比率を指定してください",
                    self.minor,
                    ratio.as_decimal()
                ),
            }
        })?;
        let strategy = match mode {
            RoundMode::Floor => RoundingStrategy::ToNegativeInfinity,
            RoundMode::Ceil => RoundingStrategy::ToPositiveInfinity,
            RoundMode::HalfUp => RoundingStrategy::MidpointAwayFromZero,
        };
        // `round_dp_with_strategy` はスケールを 0 まで縮小する（内部表現を除算で
        // 縮める）だけで、最後に高々 1 を加算するのみ。`exact` は既に
        // `Decimal` として表現可能な値なので、この操作が桁あふれで panic することはない
        // （rust_decimal 1.42.1 の実装を確認済み。乗算のような非 checked な
        // オーバーフローを起こす経路が無い）。
        let rounded = exact.round_dp_with_strategy(0, strategy);
        Ok(Money {
            minor: rounded.mantissa(),
            currency: self.currency,
        })
    }

    /// 表示用文字列。`"1,234.56"` 形式。`minor_unit` に従って小数点位置を変える
    /// （JPY は小数なし）。
    pub fn to_display_string(&self) -> String {
        let minor_unit = self.currency.minor_unit() as u32;
        // `Currency::new` が `minor_unit <= MAX_MINOR_UNIT` を保証するため、
        // 通常この経路は `checked_pow` が必ず `Some` を返す。それでも
        // `Currency` を直接構築できる経路が将来できた場合に備え、桁あふれで
        // panic しないよう多層防御する（失敗時は現実的にありえない巨大な
        // scale にフォールバックし、整数部と小数部の計算自体は破綻させない）。
        let scale = 10u128.checked_pow(minor_unit).unwrap_or(u128::MAX);
        let magnitude = self.minor.unsigned_abs();
        let integer_part = magnitude / scale;
        let fractional_part = magnitude % scale;

        let mut out = String::new();
        if self.is_negative() {
            out.push('-');
        }
        out.push_str(&group_thousands(integer_part));
        if minor_unit > 0 {
            out.push('.');
            write!(
                out,
                "{fractional_part:0width$}",
                width = minor_unit as usize
            )
            .expect("String への write! は失敗しない");
        }
        out
    }

    fn ensure_same_currency(&self, other: &Money) -> Result<(), CoreError> {
        if self.currency != other.currency {
            return Err(CoreError::CurrencyMismatch {
                a: self.currency.code().to_string(),
                b: other.currency.code().to_string(),
            });
        }
        Ok(())
    }
}

/// 整数部の文字列から桁区切りカンマを検証つきで取り除く。
///
/// カンマを含まない場合は数字のみで構成されていることを確認する。
/// カンマを含む場合は「先頭グループが1〜3桁、以降のグループが正確に3桁」
/// という正しい3桁区切りの場合のみ受理する（`to_display_string()` の出力
/// をラウンドトリップさせるため）。`"1234,56"` のような不正な区切りは拒否する。
fn validate_and_strip_thousands_separators(
    integer_part: &str,
    original: &str,
) -> Result<String, CoreError> {
    let invalid = || CoreError::InvalidAmount {
        reason: format!("数値として解釈できません: \"{original}\""),
    };

    if !integer_part.contains(',') {
        if integer_part.is_empty() || !integer_part.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid());
        }
        return Ok(integer_part.to_string());
    }

    let groups: Vec<&str> = integer_part.split(',').collect();
    let is_valid_group = |g: &str| !g.is_empty() && g.bytes().all(|b| b.is_ascii_digit());
    let Some((first, rest)) = groups.split_first() else {
        return Err(invalid());
    };
    let valid = is_valid_group(first)
        && first.len() <= 3
        && rest.iter().all(|g| g.len() == 3 && is_valid_group(g));
    if !valid {
        return Err(invalid());
    }
    Ok(groups.concat())
}

/// 3桁ごとにカンマを挿入する。
fn group_thousands(n: u128) -> String {
    let digits = n.to_string();
    let bytes = digits.as_bytes();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i != 0 && (bytes.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

/// `Money` のイテレータを合算する。空なら `Ok(None)`。異通貨混在は
/// `CoreError::CurrencyMismatch`。
///
/// `std::ops::Add` は `Money` に実装しない（異通貨をパニックさせたくないため）。
/// 合計にはこの専用関数を使う。
pub fn sum_money<'a>(items: impl Iterator<Item = &'a Money>) -> Result<Option<Money>, CoreError> {
    let mut items = items;
    let Some(&first) = items.next() else {
        return Ok(None);
    };
    let mut total = first;
    for &item in items {
        total = total.add(&item)?;
    }
    Ok(Some(total))
}

/// 比率。按分率・税率に使用する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ratio(Decimal);

impl Ratio {
    /// 按分率を解釈する。0 以上 1 以下でなければエラー。
    pub fn parse_fraction(s: &str) -> Result<Self, CoreError> {
        let value = parse_decimal(s)?;
        if value < Decimal::ZERO || value > Decimal::ONE {
            return Err(CoreError::InvalidValue {
                reason: format!("按分率は0以上1以下である必要があります: \"{s}\""),
            });
        }
        Ok(Ratio(value))
    }

    /// 率を解釈する。0 以上でなければエラー（税率は 1 を超えないが、この制約は緩める）。
    pub fn parse_rate(s: &str) -> Result<Self, CoreError> {
        let value = parse_decimal(s)?;
        if value < Decimal::ZERO {
            return Err(CoreError::InvalidValue {
                reason: format!("率は0以上である必要があります: \"{s}\""),
            });
        }
        Ok(Ratio(value))
    }

    /// 内部の `Decimal` 値を返す。
    pub fn as_decimal(&self) -> Decimal {
        self.0
    }
}

fn parse_decimal(s: &str) -> Result<Decimal, CoreError> {
    Decimal::from_str(s).map_err(|_| CoreError::InvalidValue {
        reason: format!("数値として解釈できません: \"{s}\""),
    })
}

/// 端数処理の方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundMode {
    /// 切捨。
    Floor,
    /// 切上。
    Ceil,
    /// 四捨五入。
    HalfUp,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // ---- 生成と表示 ----

    // M-01
    #[test]
    fn money_from_minor_jpy_displays_without_decimal() {
        let m = Money::from_minor(1000, Currency::JPY);
        assert_eq!(m.to_display_string(), "1,000");
    }

    // M-02
    #[test]
    fn money_from_minor_usd_displays_with_two_decimals() {
        let m = Money::from_minor(123_456, Currency::USD);
        assert_eq!(m.to_display_string(), "1,234.56");
    }

    // M-03
    #[test]
    fn money_parse_jpy_integer_succeeds() {
        let m = Money::parse("1000", Currency::JPY).unwrap();
        assert_eq!(m.minor(), 1000);
    }

    // M-04
    #[test]
    fn money_parse_usd_decimal_succeeds() {
        let m = Money::parse("1234.56", Currency::USD).unwrap();
        assert_eq!(m.minor(), 123_456);
    }

    // M-05
    #[test]
    fn money_parse_jpy_with_decimal_is_error() {
        assert!(Money::parse("1000.5", Currency::JPY).is_err());
    }

    // M-06
    #[test]
    fn money_parse_usd_too_many_decimal_digits_is_error() {
        assert!(Money::parse("1.234", Currency::USD).is_err());
    }

    // M-07
    #[test]
    fn money_parse_non_numeric_is_error() {
        assert!(Money::parse("abc", Currency::JPY).is_err());
    }

    // M-08
    #[test]
    fn money_parse_negative_value_succeeds() {
        let m = Money::parse("-500", Currency::JPY).unwrap();
        assert_eq!(m.minor(), -500);
    }

    // M-09
    #[test]
    fn money_zero_is_zero() {
        assert!(Money::zero(Currency::JPY).is_zero());
    }

    // M-10
    #[test]
    fn money_parse_three_decimal_currency_succeeds() {
        let kwd = Currency::new("KWD", 3).unwrap();
        let m = Money::parse("1.234", kwd).unwrap();
        assert_eq!(m.minor(), 1234);
    }

    // ---- Currency ----

    // バグ[B]: 上限値ちょうど（MAX_MINOR_UNIT = 18）は許容する
    #[test]
    fn currency_new_at_max_minor_unit_succeeds() {
        let c = Currency::new("XXX", Currency::MAX_MINOR_UNIT).unwrap();
        assert_eq!(c.minor_unit(), 18);
    }

    // バグ[B]: 上限を1でも超えたら拒否する
    #[test]
    fn currency_new_above_max_minor_unit_is_error() {
        assert!(matches!(
            Currency::new("XXX", Currency::MAX_MINOR_UNIT + 1),
            Err(CoreError::InvalidValue { .. })
        ));
    }

    // バグ[B]: u8 の上限に近い極端な値も拒否する
    #[test]
    fn currency_new_extreme_minor_unit_is_error() {
        assert!(matches!(
            Currency::new("XXX", 255),
            Err(CoreError::InvalidValue { .. })
        ));
    }

    // バグ[B]: 既存の想定値（0/2/3）は引き続き作れる
    #[test]
    fn currency_new_common_minor_units_succeed() {
        assert!(Currency::new("JPY", 0).is_ok());
        assert!(Currency::new("USD", 2).is_ok());
        assert!(Currency::new("KWD", 3).is_ok());
    }

    // ---- 演算 ----

    // M-20
    #[test]
    fn money_add_same_currency_succeeds() {
        let a = Money::from_minor(100, Currency::JPY);
        let b = Money::from_minor(200, Currency::JPY);
        assert_eq!(a.add(&b).unwrap().minor(), 300);
    }

    // M-21
    #[test]
    fn money_add_currency_mismatch_is_error() {
        let a = Money::from_minor(100, Currency::JPY);
        let b = Money::from_minor(100, Currency::USD);
        assert!(matches!(a.add(&b), Err(CoreError::CurrencyMismatch { .. })));
    }

    // M-22
    #[test]
    fn money_sub_same_currency_allows_negative_result() {
        let a = Money::from_minor(100, Currency::JPY);
        let b = Money::from_minor(150, Currency::JPY);
        assert_eq!(a.sub(&b).unwrap().minor(), -50);
    }

    // M-23
    #[test]
    fn money_neg_and_abs_invert_and_take_absolute_value() {
        let a = Money::from_minor(100, Currency::JPY);
        assert_eq!(a.neg().minor(), -100);
        assert_eq!(a.neg().abs().minor(), 100);
    }

    // M-24
    #[test]
    fn sum_money_empty_iterator_returns_none() {
        let items: Vec<Money> = Vec::new();
        assert_eq!(sum_money(items.iter()).unwrap(), None);
    }

    // M-25
    #[test]
    fn sum_money_currency_mismatch_is_error() {
        let items = [
            Money::from_minor(100, Currency::JPY),
            Money::from_minor(100, Currency::USD),
        ];
        assert!(matches!(
            sum_money(items.iter()),
            Err(CoreError::CurrencyMismatch { .. })
        ));
    }

    // M-26
    #[test]
    fn money_mul_ratio_floor_thirty_percent() {
        let m = Money::from_minor(100_000, Currency::JPY);
        let ratio = Ratio::parse_fraction("0.30").unwrap();
        assert_eq!(
            m.mul_ratio(ratio, RoundMode::Floor).unwrap().minor(),
            30_000
        );
    }

    // M-27
    #[test]
    fn money_mul_ratio_floor_repeating_decimal() {
        let m = Money::from_minor(100, Currency::JPY);
        let ratio = Ratio::parse_fraction("0.333").unwrap();
        assert_eq!(m.mul_ratio(ratio, RoundMode::Floor).unwrap().minor(), 33);
    }

    // M-28
    #[test]
    fn money_mul_ratio_ceil_repeating_decimal() {
        let m = Money::from_minor(100, Currency::JPY);
        let ratio = Ratio::parse_fraction("0.333").unwrap();
        assert_eq!(m.mul_ratio(ratio, RoundMode::Ceil).unwrap().minor(), 34);
    }

    // M-29
    #[test]
    fn money_mul_ratio_half_up() {
        let m = Money::from_minor(1000, Currency::JPY);
        let ratio = Ratio::parse_fraction("0.335").unwrap();
        assert_eq!(m.mul_ratio(ratio, RoundMode::HalfUp).unwrap().minor(), 335);
    }

    // レビュー指摘1: 正しい3桁区切りのカンマは受理する
    #[test]
    fn money_parse_correctly_grouped_commas_succeeds() {
        assert_eq!(Money::parse("1,234", Currency::JPY).unwrap().minor(), 1234);
        assert_eq!(
            Money::parse("1,234,567.89", Currency::USD).unwrap().minor(),
            123_456_789
        );
    }

    // レビュー指摘1: 不正な桁区切り（例: "1234,56"）は拒否する
    #[test]
    fn money_parse_incorrectly_grouped_commas_is_error() {
        assert!(Money::parse("1234,56", Currency::USD).is_err());
        assert!(Money::parse("1,23", Currency::JPY).is_err());
        assert!(Money::parse("1,2345", Currency::JPY).is_err());
    }

    // レビュー指摘3: rust_decimal の表現上限を超える金額への mul_ratio は InvalidAmount
    // （変換ガード側で落ちる経路。掛け算そのものには到達しない）
    #[test]
    fn money_mul_ratio_out_of_decimal_range_is_error() {
        let m = Money::from_minor(i128::MAX / 2, Currency::JPY);
        let ratio = Ratio::parse_fraction("0.5").unwrap();
        assert!(matches!(
            m.mul_ratio(ratio, RoundMode::Floor),
            Err(CoreError::InvalidAmount { .. })
        ));
    }

    // バグ[A]: 金額そのものは表現できても「積」が表現上限を超える場合は
    // InvalidAmount を返す（panic しない）。2円という現実的な金額でも、
    // `Ratio::parse_rate` に上限が無いため巨大な比率と組み合わせると到達する経路。
    #[test]
    fn money_mul_ratio_product_overflow_is_error() {
        let m = Money::from_minor(2, Currency::JPY);
        let ratio = Ratio::parse_rate("79228162514264337593543950335").unwrap();
        assert!(matches!(
            m.mul_ratio(ratio, RoundMode::Floor),
            Err(CoreError::InvalidAmount { .. })
        ));
    }

    // M-30
    #[test]
    fn money_add_near_i128_max_does_not_overflow() {
        let a = Money::from_minor(i128::MAX - 1, Currency::JPY);
        let b = Money::from_minor(2, Currency::JPY);
        assert!(matches!(a.add(&b), Err(CoreError::InvalidAmount { .. })));
    }

    // レビュー指摘1: i128::MIN もラウンドトリップできる
    #[test]
    fn money_parse_display_round_trip_handles_i128_min() {
        let original = Money::from_minor(i128::MIN, Currency::JPY);
        let parsed = Money::parse(&original.to_display_string(), Currency::JPY).unwrap();
        assert_eq!(parsed.minor(), i128::MIN);
    }

    // ---- Ratio ----

    // M-40
    #[test]
    fn ratio_parse_fraction_valid() {
        assert!(Ratio::parse_fraction("0.3").is_ok());
    }

    // M-41
    #[test]
    fn ratio_parse_fraction_over_one_is_error() {
        assert!(Ratio::parse_fraction("1.5").is_err());
    }

    // M-42
    #[test]
    fn ratio_parse_fraction_negative_is_error() {
        assert!(Ratio::parse_fraction("-0.1").is_err());
    }

    // M-43
    #[test]
    fn ratio_parse_rate_valid() {
        assert!(Ratio::parse_rate("0.08").is_ok());
    }

    // ---- プロパティテスト ----

    /// PT-04/PT-05 共通。通常のランダム範囲に加え、`i128` の境界値
    /// （`MIN`, `MAX`, `0`）を明示的に含める。今回の `i128::MIN` ラウンドトリップ
    /// バグはこの境界値を生成範囲に含めていれば機械的に発見できた。
    fn any_minor() -> impl Strategy<Value = i128> {
        prop_oneof![
            8 => -1_000_000_000_000_000i128..=1_000_000_000_000_000i128,
            1 => Just(i128::MIN),
            1 => Just(i128::MAX),
            1 => Just(0i128),
        ]
    }

    /// PT-05 用。`mul_ratio` の引数は非負の金額のみを想定するため 0 以上。
    fn any_non_negative_minor() -> impl Strategy<Value = i128> {
        prop_oneof![
            8 => 0i128..=1_000_000_000_000_000i128,
            1 => Just(i128::MAX),
            1 => Just(0i128),
        ]
    }

    /// PT-04 用。境界値を含む複数の `minor_unit`（0, 1, 2, 3, 4, `MAX_MINOR_UNIT`）を
    /// 持つ通貨を生成する。かつては JPY/USD/KWD（0, 2, 3）の3値のみで、
    /// `Currency::new` の `minor_unit` 上限検証（バグ[B]）の境界を
    /// 機械的に踏めなかった。
    fn any_currency() -> impl Strategy<Value = Currency> {
        prop_oneof![
            Just(Currency::JPY),
            Just(Currency::USD),
            Just(Currency::new("KWD", 3).unwrap()),
            Just(Currency::new("ONE", 1).unwrap()),
            Just(Currency::new("FOR", 4).unwrap()),
            Just(Currency::new("MAX", Currency::MAX_MINOR_UNIT).unwrap()),
        ]
    }

    proptest! {
        // PT-04: Money::parse(m.to_display_string()) が元に戻る
        #[test]
        fn money_parse_display_round_trip(
            minor in any_minor(),
            currency in any_currency(),
        ) {
            let original = Money::from_minor(minor, currency);
            let parsed = Money::parse(&original.to_display_string(), currency).unwrap();
            prop_assert_eq!(parsed.minor(), original.minor());
            prop_assert_eq!(parsed.currency(), original.currency());
        }

        // PT-05: mul_ratio の結果は ratio<=1 のとき元金額を超えない。
        // ratio>1 のときはこの性質は成り立たないため、その場合は結果が `Ok` に
        // なること自体は許容し、上限比較は行わない。
        //
        // かつては `parse_fraction`（ratio<=1 限定）のみを生成しており、
        // 「金額 × 比率」の積が表現上限を超えて panic するバグ[A]の経路を
        // 構造的に踏めなかった。`parse_rate` で 1 を超える比率も生成対象に
        // 含めることで、この proptest 自体がバグ[A]の回帰検知にもなる
        // （修正前は特定の組み合わせで panic し、テストが失敗していた）。
        #[test]
        fn money_mul_ratio_does_not_exceed_original_when_ratio_le_one(
            minor in any_non_negative_minor(),
            ratio_str in prop_oneof![
                "0\\.[0-9]{1,3}",
                "[1-9][0-9]{0,2}\\.[0-9]{1,3}",
            ],
            mode_idx in 0u8..3,
        ) {
            let ratio = Ratio::parse_rate(&ratio_str).unwrap();
            let mode = match mode_idx {
                0 => RoundMode::Floor,
                1 => RoundMode::Ceil,
                _ => RoundMode::HalfUp,
            };
            let m = Money::from_minor(minor, Currency::JPY);
            if let Ok(result) = m.mul_ratio(ratio, mode) {
                if ratio.as_decimal() <= Decimal::ONE {
                    prop_assert!(result.minor() <= m.minor());
                }
            }
        }
    }
}
