//! 線上（JSON）の型。
//!
//! `kaikei-core` / `kaikei-app` の型は serde を実装しない（`kaikei-core` への
//! serde 参照は `.github/workflows/architecture.yml` が機械的に禁じており、
//! `kaikei-app` の `Cargo.toml` にも serde は無い）。したがって MCP の入出力
//! DTO はこの crate が自前で持ち、詰め替える（`docs/07-mcp-server.md` §4）。
//!
//! # ここは「詰め替え」だけを行う
//!
//! 値の**書き方**（金額の文字列形式・列挙型の機械可読名・仕訳IDの表記・
//! タグ値の表現）は PR-B が `kaikei-app` / `kaikei-jp` に置いた。
//! このモジュールはそれらを呼ぶだけで、同じ整形をここに書き直さない
//! （`DECISIONS.md` D-072、`docs/07-mcp-server.md` §5 の
//! 「`kaikei-mcp` 側に `amount.rs` を作らないこと」）。

use std::fmt;

use kaikei_core::{CoreError, Currency, Money};
use serde::de;
use serde::{Deserialize, Deserializer, Serialize};

/// JSON の number で金額を渡されたときのエラーメッセージ。
///
/// 素の `String` フィールドにしておくと、serde が組み立てる
/// `invalid type: integer 110000, expected a string` という**英語の型エラー**
/// しか AI に届かない（`CLAUDE.md` §11 を満たさない。`DECISIONS.md` D-019 が
/// `{:?}` の英語バリアント名を禁じたのと同型の問題）。
const AMOUNT_MUST_BE_STRING: &str = "金額は文字列で渡してください（例: \"110000\"、\
     USD なら \"1234.56\"）。JSON の number は倍精度浮動小数点のため、\
     会計金額には使えません";

/// 線上の金額（**文字列**）。
///
/// # number は常にエラー（整数でも受理しない）
///
/// サーバに届いた時点では、クライアント側で既に倍精度に丸められたかどうかを
/// **サーバから検出できない**。受理すれば「警告付きで壊れた金額を記帳する」
/// ことになる（`DECISIONS.md` D-013 の訂正注記、`docs/07-mcp-server.md` §5）。
///
/// ```
/// use kaikei_mcp::wire::AmountStr;
///
/// let ok: AmountStr = serde_json::from_str("\"110000\"").unwrap();
/// assert_eq!(ok.as_str(), "110000");
///
/// let err = serde_json::from_str::<AmountStr>("110000").unwrap_err();
/// assert!(err.to_string().contains("金額は文字列で渡してください"));
/// ```
///
/// # `JsonSchema` は `"type": "string"` を出す
///
/// スキーマ上 number も許されているように見えると、AI が number を送る動機を
/// 作る（`docs/07-mcp-server.md` §5）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(transparent)]
#[schemars(
    with = "String",
    description = "金額。文字列で指定します（例: \"110000\"）。JSON の number は受け付けません"
)]
pub struct AmountStr(String);

impl AmountStr {
    /// 文字列から作る（応答を組み立てるとき用）。
    ///
    /// 入力の検証は行わない。線上に出す値は
    /// [`kaikei_app::amount::money_to_plain_string`] を通した文字列である。
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    /// [`Money`] から線上の文字列を作る（**区切り無し**）。
    ///
    /// 整形は `kaikei-app` に委ねる。ここで `format!` を書かないこと
    /// （同じ整形が2箇所に育つ。`docs/07-mcp-server.md` §5）。
    pub fn from_money(money: &Money) -> Self {
        Self(kaikei_app::amount::money_to_plain_string(money))
    }

    /// 受け取った文字列そのもの。
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 指定した通貨の [`Money`] として解釈する。
    ///
    /// 通貨ごとに小数桁が違う（JPY=0 / USD=2）ため、通貨を伴わずに金額は
    /// 作れない（`CLAUDE.md` §8）。通貨の決め方は
    /// `docs/07-mcp-server.md` §5（明細で省略されたら
    /// `BookSettings::book_currency`、明示されたら
    /// [`kaikei_app::currency::currency_from_code`]）。
    ///
    /// # Errors
    ///
    /// 金額として解釈できない文字列、またはその通貨で表現できない小数桁の
    /// 場合に `CoreError::InvalidAmount` を返す。
    pub fn to_money(&self, currency: Currency) -> Result<Money, CoreError> {
        Money::parse(&self.0, currency)
    }
}

impl fmt::Display for AmountStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for AmountStr {
    /// **文字列以外は必ずエラー**にする（`docs/07-mcp-server.md` §5）。
    ///
    /// # なぜ一度 [`serde_json::Value`] を経由するのか
    ///
    /// `deserializer.deserialize_str(..)` にすると、number を受け取った時点で
    /// **serde 側が** `invalid_type` エラーを組み立ててしまい、こちらの
    /// Visitor に制御が渡らない。結果として下の日本語メッセージは無視され、
    /// 英語の `invalid type: integer 110000, expected a string` が返る
    /// （調査で実測済み）。自己記述的な形式（JSON）として値を受け取ってから
    /// 判定すれば、メッセージをこちらが決められる。
    ///
    /// 独自 Visitor に `visit_i64` / `visit_f64` … を並べる形でも同じことは
    /// できるが、その場合ソースに浮動小数点の型名が現れ、
    /// `.github/workflows/architecture.yml` の「f64 が金額に使われていない」
    /// ステップ（コメント行以外の該当語をすべて落とす）に引っかかる。
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match serde_json::Value::deserialize(deserializer)? {
            serde_json::Value::String(text) => Ok(Self(text)),
            _ => Err(de::Error::custom(AMOUNT_MUST_BE_STRING)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 線上の文字列はそのまま受理される。
    #[test]
    fn amount_accepts_a_json_string() {
        let parsed: AmountStr = serde_json::from_str("\"110000\"").unwrap();
        assert_eq!(parsed.as_str(), "110000");
    }

    // MC-09: number は整数でもエラーにする。しかも**日本語**で。
    #[test]
    fn amount_rejects_json_numbers_with_a_japanese_message() {
        for json in ["110000", "-110000", "1234.56", "0"] {
            let err = serde_json::from_str::<AmountStr>(json).unwrap_err();
            let message = err.to_string();
            assert!(
                message.contains("金額は文字列で渡してください"),
                "{json} → {message}"
            );
            // 次の手（正しい書き方の例）が含まれる（`CLAUDE.md` §11）。
            assert!(message.contains("110000"), "{json} → {message}");
            // 英語の型エラーに落ちていない。
            assert!(!message.contains("invalid type"), "{json} → {message}");
        }
    }

    // null / bool / 配列も同じ扱い（黙って 0 円にしない）。
    #[test]
    fn amount_rejects_null_bool_and_array_with_a_japanese_message() {
        for json in ["null", "true", "[]", "{}"] {
            let err = serde_json::from_str::<AmountStr>(json).unwrap_err();
            assert!(
                err.to_string().contains("金額は文字列で渡してください"),
                "{json} → {err}"
            );
        }
    }

    // 構造体のフィールドとして使っても日本語のままであること
    // （実際の入力は `{"amount": 110000}` の形で届く）。
    #[test]
    fn amount_in_a_struct_field_keeps_the_japanese_message() {
        #[derive(Debug, Deserialize)]
        struct Line {
            #[allow(dead_code)]
            amount: AmountStr,
        }
        let err = serde_json::from_str::<Line>("{\"amount\": 110000}").unwrap_err();
        assert!(
            err.to_string().contains("金額は文字列で渡してください"),
            "{err}"
        );
    }

    // 出力側の整形は `kaikei-app` に委ねている（ここで再実装していない）。
    #[test]
    fn amount_from_money_uses_the_app_layer_formatting() {
        let jpy = Currency::new("JPY", 0).unwrap();
        let money = Money::parse("110000", jpy).unwrap();
        assert_eq!(AmountStr::from_money(&money).as_str(), "110000");
        assert_eq!(
            AmountStr::from_money(&money).as_str(),
            kaikei_app::amount::money_to_plain_string(&money)
        );
    }

    // 入力の文字列は通貨を伴って `Money` になる（小数桁は通貨が決める）。
    #[test]
    fn amount_to_money_validates_the_minor_units_of_the_currency() {
        let jpy = Currency::new("JPY", 0).unwrap();
        assert!(AmountStr::new("110000").to_money(jpy).is_ok());
        // JPY に小数は無い。
        assert!(AmountStr::new("1000.5").to_money(jpy).is_err());

        let usd = Currency::new("USD", 2).unwrap();
        assert!(AmountStr::new("1234.56").to_money(usd).is_ok());
    }

    // `JsonSchema` は number を許さない（AI に number を送る動機を作らない）。
    #[test]
    fn amount_schema_is_a_string() {
        let schema = schemars::schema_for!(AmountStr);
        let json = serde_json::to_value(&schema).unwrap();
        assert_eq!(json.get("type").and_then(|t| t.as_str()), Some("string"));
    }
}
