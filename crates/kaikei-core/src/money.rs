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

        // to_display_string() が挿入する桁区切りカンマを許容する（ラウンドトリップのため）。
        let integer_digits: String = integer_part.chars().filter(|&c| c != ',').collect();
        if integer_digits.is_empty() || !integer_digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid_amount(format!("数値として解釈できません: \"{s}\"")));
        }

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
    pub fn neg(&self) -> Money {
        Money {
            minor: -self.minor,
            currency: self.currency,
        }
    }

    /// 絶対値の金額を返す。
    pub fn abs(&self) -> Money {
        Money {
            minor: self.minor.abs(),
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
