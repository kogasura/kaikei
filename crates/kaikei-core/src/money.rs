//! 金額と通貨。
//!
//! 内部表現は最小通貨単位の整数（`i128`）。`f64` は使わない（`CLAUDE.md` §8）。
//! 異通貨の加減算は型では防がず `Result` で弾く（`DECISIONS.md` D-003）。

use crate::error::CoreError;
use rust_decimal::{Decimal, RoundingStrategy};
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

    /// 通貨を作る。コードは英大文字3文字でなければならない。
    pub fn new(code: &str, minor_unit: u8) -> Result<Self, CoreError> {
        let bytes = code.as_bytes();
        if bytes.len() != 3 || !bytes.iter().all(u8::is_ascii_uppercase) {
            return Err(CoreError::InvalidValue {
                reason: format!("通貨コードは英大文字3文字である必要があります: \"{code}\""),
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

        let integer_digits = parse_integer_digits(integer_part, s)?;

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

        let magnitude: i128 = format!("{integer_digits}{decimal_digits}")
            .parse()
            .map_err(|_| invalid_amount(format!("数値として解釈できません: \"{s}\"")))?;

        let minor = if negative { -magnitude } else { magnitude };
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
    pub fn mul_ratio(&self, ratio: Ratio, mode: RoundMode) -> Money {
        let exact = Decimal::from(self.minor) * ratio.as_decimal();
        let strategy = match mode {
            RoundMode::Floor => RoundingStrategy::ToNegativeInfinity,
            RoundMode::Ceil => RoundingStrategy::ToPositiveInfinity,
            RoundMode::HalfUp => RoundingStrategy::MidpointAwayFromZero,
        };
        let rounded = exact.round_dp_with_strategy(0, strategy);
        Money {
            minor: rounded.mantissa(),
            currency: self.currency,
        }
    }

    /// 表示用文字列。`"1,234.56"` 形式。`minor_unit` に従って小数点位置を変える
    /// （JPY は小数なし）。
    pub fn to_display_string(&self) -> String {
        let minor_unit = self.currency.minor_unit() as u32;
        let scale = 10u128.pow(minor_unit);
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
            out.push_str(&format!(
                "{fractional_part:0width$}",
                width = minor_unit as usize
            ));
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
fn parse_integer_digits(integer_part: &str, original: &str) -> Result<String, CoreError> {
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
        assert_eq!(m.mul_ratio(ratio, RoundMode::Floor).minor(), 30_000);
    }

    // M-27
    #[test]
    fn money_mul_ratio_floor_repeating_decimal() {
        let m = Money::from_minor(100, Currency::JPY);
        let ratio = Ratio::parse_fraction("0.333").unwrap();
        assert_eq!(m.mul_ratio(ratio, RoundMode::Floor).minor(), 33);
    }

    // M-28
    #[test]
    fn money_mul_ratio_ceil_repeating_decimal() {
        let m = Money::from_minor(100, Currency::JPY);
        let ratio = Ratio::parse_fraction("0.333").unwrap();
        assert_eq!(m.mul_ratio(ratio, RoundMode::Ceil).minor(), 34);
    }

    // M-29
    #[test]
    fn money_mul_ratio_half_up() {
        let m = Money::from_minor(1000, Currency::JPY);
        let ratio = Ratio::parse_fraction("0.335").unwrap();
        assert_eq!(m.mul_ratio(ratio, RoundMode::HalfUp).minor(), 335);
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

    // M-30
    #[test]
    fn money_add_near_i128_max_does_not_overflow() {
        let a = Money::from_minor(i128::MAX - 1, Currency::JPY);
        let b = Money::from_minor(2, Currency::JPY);
        assert!(matches!(a.add(&b), Err(CoreError::InvalidAmount { .. })));
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

    proptest! {
        // PT-04: Money::parse(m.to_display_string()) が元に戻る
        #[test]
        fn money_parse_display_round_trip(
            minor in -1_000_000_000_000_000i128..=1_000_000_000_000_000i128,
            currency_idx in 0u8..3,
        ) {
            let currency = match currency_idx {
                0 => Currency::JPY,
                1 => Currency::USD,
                _ => Currency::new("KWD", 3).unwrap(),
            };
            let original = Money::from_minor(minor, currency);
            let parsed = Money::parse(&original.to_display_string(), currency).unwrap();
            prop_assert_eq!(parsed.minor(), original.minor());
            prop_assert_eq!(parsed.currency(), original.currency());
        }

        // PT-05: mul_ratio の結果が ratio<=1 のとき元金額を超えない
        #[test]
        fn money_mul_ratio_does_not_exceed_original_when_ratio_le_one(
            minor in 0i128..=1_000_000_000_000_000i128,
            ratio_str in "0\\.[0-9]{1,3}",
            mode_idx in 0u8..3,
        ) {
            let ratio = Ratio::parse_fraction(&ratio_str).unwrap();
            let mode = match mode_idx {
                0 => RoundMode::Floor,
                1 => RoundMode::Ceil,
                _ => RoundMode::HalfUp,
            };
            let m = Money::from_minor(minor, Currency::JPY);
            let result = m.mul_ratio(ratio, mode);
            prop_assert!(result.minor() <= m.minor());
        }
    }
}
