//! ★契約凍結点★ 金額の**線上文字列**（区切り無し）への整形。
//!
//! `docs/07-mcp-server.md` §5 は、機械可読フィールド（`amount` /
//! `debit_total` 等）を**区切り無しの文字列**（`"110000"`、USD なら
//! `"1234.56"`）に固定し、3桁区切り付きの表記（`"110,000"`）は `message` の
//! 文中でのみ使う、と定めている。
//!
//! `kaikei_core::Money` が持つ唯一の文字列化は `to_display_string()` で、
//! これは**3桁区切り付き**を返す。区切り無しの整形手段は core に無い。
//!
//! **`kaikei-core` は変更しない。** 必要な部品（`Money::minor()` と
//! `Currency::minor_unit()`）はどちらも公開されており、この2つから区切り無しの
//! 文字列を組み立てられる（`Money::parse` とのラウンドトリップも通る。
//! 下部のテストで実測）。`DECISIONS.md` D-072 の「線上に出る表現は
//! `kaikei-app` に1箇所だけ置く」と同じ方針で、presentation 層
//! （`kaikei-mcp` / 将来の `kaikei-api`）が各自で再実装しないようにする。
//!
//! # 入力側（文字列 → `Money`）はここに無い
//!
//! `kaikei_core::Money::parse(s, currency)` がそのまま使える
//! （区切り無しも、正しい3桁区切り付きも受理する）。通貨の解決は
//! [`crate::currency::currency_from_code`]。

use kaikei_core::Money;
use std::fmt::Write as _;

/// 金額を**区切り無し**の文字列にする（`"110000"` / USD `"1234.56"`）。
///
/// - 小数点は `.`。桁区切りは入れない。
/// - 小数桁数は `currency.minor_unit()` に従って**必ず**その桁数まで埋める
///   （USD の 5 セントは `"0.05"`。`"0.5"` にしない）。
/// - 負値は先頭に `-` を付ける（`"-110000"`）。
/// - `minor_unit == 0` の通貨（JPY 等）では小数点そのものを出さない。
///
/// 戻り値は `Money::parse(&s, money.currency())` で元の値に戻る
/// （ラウンドトリップはテストで担保している）。
///
/// **通貨コードは含めない。** 「いくらか」と「何建てか」は別のフィールドに
/// 分ける（`{"amount": "110000", "currency": "JPY"}`）。1つの文字列に混ぜると
/// 呼び出し側が必ずパースを書くことになる。
pub fn money_to_plain_string(money: &Money) -> String {
    let minor_unit = money.currency().minor_unit() as u32;
    // `Currency::new` は `minor_unit <= Currency::MAX_MINOR_UNIT`（18）を
    // 保証するので `checked_pow` は必ず `Some` を返す。それでも
    // `Money::to_display_string` と同じく、桁あふれで panic しない形にしておく。
    let scale = 10u128.checked_pow(minor_unit).unwrap_or(u128::MAX);
    let magnitude = money.minor().unsigned_abs();
    let integer_part = magnitude / scale;
    let fractional_part = magnitude % scale;

    let mut out = String::new();
    if money.is_negative() {
        out.push('-');
    }
    out.push_str(&integer_part.to_string());
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

/// 3桁区切り付きの金額文字列から**区切りだけ**を取り除く。
///
/// `kaikei_core::CoreError::Unbalanced` / [`crate::error::AppError::Inconsistent`]
/// が持つのは `Money` ではなく**整形済みの `String`**（`"110,000"`）である。
/// エラー応答の `debit_total` / `credit_total` / `difference` を
/// [`money_to_plain_string`] と同じ形式に揃えるための入口。
///
/// 桁区切りは `,`、小数点は `.` と決まっている（`Money::to_display_string` の
/// 出力仕様）ので、**通貨の小数桁数を知る必要が無い**。
/// `"-1,234.56"` → `"-1234.56"`、`"110,000"` → `"110000"`。
///
/// 入力が既に区切り無しでも安全（変化しない）。
pub fn strip_thousands_separators(display: &str) -> String {
    display.replace(',', "")
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_core::Currency;

    // AM-1: JPY（minor_unit = 0）は小数点を出さない。
    #[test]
    fn jpy_is_formatted_without_a_decimal_point() {
        assert_eq!(
            money_to_plain_string(&Money::from_minor(110_000, Currency::JPY)),
            "110000"
        );
        assert_eq!(
            money_to_plain_string(&Money::from_minor(-110_000, Currency::JPY)),
            "-110000"
        );
        assert_eq!(money_to_plain_string(&Money::zero(Currency::JPY)), "0");
    }

    // AM-2: USD（minor_unit = 2）は必ず2桁まで埋める。
    #[test]
    fn usd_is_formatted_with_exactly_two_decimals() {
        assert_eq!(
            money_to_plain_string(&Money::from_minor(123_456, Currency::USD)),
            "1234.56"
        );
        assert_eq!(
            money_to_plain_string(&Money::from_minor(-5, Currency::USD)),
            "-0.05"
        );
        assert_eq!(money_to_plain_string(&Money::zero(Currency::USD)), "0.00");
    }

    // AM-3: 区切り無し文字列は `Money::parse` で元の値に戻る（★ラウンドトリップ★）。
    #[test]
    fn plain_string_round_trips_through_money_parse() {
        let cases = [
            (110_000i128, Currency::JPY),
            (-110_000, Currency::JPY),
            (0, Currency::JPY),
            (123_456, Currency::USD),
            (-5, Currency::USD),
            (0, Currency::USD),
            (1, Currency::JPY),
            (i64::MAX as i128, Currency::JPY),
            (i64::MIN as i128, Currency::JPY),
        ];
        for (minor, currency) in cases {
            let money = Money::from_minor(minor, currency);
            let text = money_to_plain_string(&money);
            let parsed = Money::parse(&text, currency)
                .unwrap_or_else(|err| panic!("\"{text}\" を parse できない: {err}"));
            assert_eq!(parsed.minor(), minor, "text={text}");
            assert_eq!(parsed.currency(), currency);
        }
    }

    // AM-4: 区切り無し表記は `to_display_string`（区切り付き）と別物である。
    #[test]
    fn plain_string_differs_from_the_display_string_when_grouping_applies() {
        let money = Money::from_minor(110_000, Currency::JPY);
        assert_eq!(money.to_display_string(), "110,000");
        assert_eq!(money_to_plain_string(&money), "110000");
    }

    // AM-5: エラー本文の整形済み文字列から区切りだけを外せる（通貨の知識は不要）。
    #[test]
    fn thousands_separators_are_stripped_without_knowing_the_currency() {
        assert_eq!(strip_thousands_separators("110,000"), "110000");
        assert_eq!(strip_thousands_separators("-1,234.56"), "-1234.56");
        assert_eq!(strip_thousands_separators("10,000"), "10000");
        // 既に区切り無しなら変化しない。
        assert_eq!(strip_thousands_separators("110000"), "110000");
        assert_eq!(strip_thousands_separators("0.00"), "0.00");
    }

    // AM-6: `CoreError::Unbalanced` の3つの文字列は、区切りを外すと
    // `money_to_plain_string` と同じ形式になる（応答の中で表記が混ざらない）。
    #[test]
    fn stripping_the_unbalanced_error_strings_matches_the_plain_format() {
        for (minor, currency) in [(110_000i128, Currency::JPY), (123_456, Currency::USD)] {
            let money = Money::from_minor(minor, currency);
            assert_eq!(
                strip_thousands_separators(&money.to_display_string()),
                money_to_plain_string(&money)
            );
        }
    }
}
