//! 通貨コードから `Currency` を解決する（[`currency_from_code`]）。
//!
//! `kaikei_core::Currency::new` は `minor_unit`（小数桁数）を必須の引数として
//! 要求する。ユースケースの入力（例: ユーザーが入力する金額と通貨コードの
//! 文字列）には多くの場合 `minor_unit` が付いてこないため、既知の通貨コードに
//! 対して正しい桁数を引くための最小限のホワイトリストをここに置く。
//!
//! **未知のコードは絶対に桁数0と推測しない**（`CLAUDE.md` §8「金額=セントの
//! 前提を置かない」）。推測すると、例えば `minor_unit` が2桁の通貨を0桁として
//! 扱ってしまい、金額が100倍ズレて記帳される。
//!
//! store 層（DB からの復元）はこの関数を使わないこと。DB には `currency` と
//! `currency_minor_unit` の両方の列があるため、`Currency::new(currency,
//! currency_minor_unit)` に実際に保存されている桁数をそのまま渡せばよく、
//! ホワイトリストによる推測は不要（推測するとホワイトリストに無い通貨で
//! 保存済みデータを復元できなくなる）。

use kaikei_core::{CoreError, Currency};

/// 既知の通貨コードから `Currency` を解決する。
///
/// 未知のコードは `CoreError::InvalidValue` を返す（桁数を推測しない）。
pub fn currency_from_code(code: &str) -> Result<Currency, CoreError> {
    match code {
        "JPY" => Ok(Currency::JPY),
        "USD" => Ok(Currency::USD),
        _ => Err(CoreError::InvalidValue {
            reason: format!(
                "未知の通貨コードです: \"{code}\"。既知のコード（JPY, USD）以外は\
                 小数桁数を推測できません。Currency::new(code, minor_unit) を\
                 明示的に指定してください"
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jpy_resolves_to_zero_minor_unit() {
        let currency = currency_from_code("JPY").unwrap();
        assert_eq!(currency.minor_unit(), 0);
    }

    #[test]
    fn usd_resolves_to_two_minor_units() {
        let currency = currency_from_code("USD").unwrap();
        assert_eq!(currency.minor_unit(), 2);
    }

    #[test]
    fn unknown_code_is_an_error_rather_than_a_guess() {
        assert!(matches!(
            currency_from_code("KWD"),
            Err(CoreError::InvalidValue { .. })
        ));
        assert!(matches!(
            currency_from_code(""),
            Err(CoreError::InvalidValue { .. })
        ));
    }
}
