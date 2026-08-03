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

// `PolicyNote` は `kaikei-app` の再エクスポート経由で参照する。
// `kaikei-policy` を直接 `use` すると、この crate の `Cargo.toml` に
// `kaikei-policy` を足すことになり MC-30（依存の許可リスト）に反する
// （`DECISIONS.md` D-047 と同型の問題。`kaikei-app` のクレート doc
// 「`kaikei-policy` 型の再エクスポート」を参照）。
use kaikei_app::PolicyNote;
use kaikei_core::{CoreError, Currency, JournalLine, Money, TagSet};
use serde::de;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Map, Value};

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

/// 線上の `tags`（キーも値も文字列のマップ）。**重複キーを保持する。**
///
/// # なぜ `Map<String, String>` にしないのか
///
/// `serde_json` の `Map` は同じキーが2回現れると**後勝ちで黙って上書き**する。
/// `{"tags": {"tax_category": "SALES_10", "tax_category": "SALES_8"}}` を
/// `Map` として受けた時点で片方が消えるので、`kaikei-jp` が用意している
/// [`kaikei_jp::error::JpError::DuplicateTagKeyInInput`]（「重複した指定を
/// 1つにまとめてください」）に**到達する経路が無くなる**。
/// `CLAUDE.md` §4「`TagSet` はゴミ箱ではない。黙って落とさない」に反する。
///
/// そこで出現順のペアをそのまま保持し、重複の検出は
/// [`kaikei_jp::tags::TagCatalog::parse_tag_set`] に委ねる（判定を MCP 層に
/// 書き直さない。`DECISIONS.md` D-072）。
///
/// `JsonSchema` 上は素直なオブジェクト（`{"キー": "値"}`）として見える。
#[derive(Debug, Clone, Default, PartialEq, Eq, schemars::JsonSchema)]
#[schemars(
    with = "std::collections::BTreeMap<String, String>",
    description = "タグ。キーも値も文字列で指定します（例: {\"tax_category\": \"SALES_10\"}）"
)]
pub struct TagPairs(Vec<(String, String)>);

impl TagPairs {
    /// 出現順のキーと値。
    pub fn as_slice(&self) -> &[(String, String)] {
        &self.0
    }

    /// 空かどうか。
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<'de> Deserialize<'de> for TagPairs {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct PairsVisitor;

        impl<'de> de::Visitor<'de> for PairsVisitor {
            type Value = TagPairs;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("タグのオブジェクト（キーも値も文字列）")
            }

            fn visit_map<M>(self, mut access: M) -> Result<TagPairs, M::Error>
            where
                M: de::MapAccess<'de>,
            {
                let mut pairs = Vec::new();
                // `MapAccess` は入力に現れた順で1組ずつ渡してくるので、
                // 重複キーもここでは失われない（畳み込むのは呼び出し先）。
                while let Some((key, value)) = access.next_entry::<String, String>()? {
                    pairs.push((key, value));
                }
                Ok(TagPairs(pairs))
            }
        }

        deserializer.deserialize_map(PairsVisitor)
    }
}

/// [`TagSet`] を線上の `tags`（文字列マップ）にする。
///
/// 値の文字列化は [`kaikei_jp::tags::tag_value_to_string`] に委ねる
/// （線上とDBで別の書き方を発明しない。`docs/07-mcp-server.md` §3）。
pub fn tag_set_to_json(tags: &TagSet) -> Value {
    let mut object = Map::new();
    for (key, value) in tags.iter() {
        object.insert(
            key.as_str().to_string(),
            json!(kaikei_jp::tags::tag_value_to_string(value)),
        );
    }
    Value::Object(object)
}

/// 確定後の明細1行を線上の JSON にする。
///
/// 金額は**区切り無しの文字列**（`docs/07-mcp-server.md` §5）。
/// `side` は [`kaikei_app::wire::side_code`]。`memo` は指定されていた場合だけ
/// 現れる（`null` を置くと「メモが空文字である」と区別できない）。
pub fn line_to_json(line: &JournalLine) -> Value {
    let mut object = Map::new();
    object.insert("account".to_string(), json!(line.account().as_str()));
    object.insert(
        "side".to_string(),
        json!(kaikei_app::wire::side_code(line.side())),
    );
    object.insert(
        "amount".to_string(),
        json!(AmountStr::from_money(line.amount()).as_str()),
    );
    object.insert(
        "currency".to_string(),
        json!(line.amount().currency().code()),
    );
    object.insert("tags".to_string(), tag_set_to_json(line.tags()));
    if let Some(memo) = line.memo() {
        object.insert("memo".to_string(), json!(memo));
    }
    Value::Object(object)
}

/// 明細の一覧を線上の JSON 配列にする。
pub fn lines_to_json(lines: &[JournalLine]) -> Value {
    Value::Array(lines.iter().map(line_to_json).collect())
}

/// `PolicyNote` の一覧を線上の JSON 配列にする。
///
/// **文言は `kaikei-policy` の実装が組み立てたものをそのまま素通しする**
/// （税務判断を断定する言い換えをしない。`CLAUDE.md` §10）。
/// `severity` は [`kaikei_app::wire::note_severity_code`]。
pub fn policy_notes_to_json(notes: &[PolicyNote]) -> Value {
    Value::Array(
        notes
            .iter()
            .map(|note| {
                json!({
                    "severity": kaikei_app::wire::note_severity_code(note.severity),
                    "message": note.message,
                })
            })
            .collect(),
    )
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

    // ★重複キーが畳み込まれない★
    //
    // `Map<String, String>` で受けていると後勝ちで1件に潰れ、
    // `JpError::DuplicateTagKeyInInput` に到達する経路が消える。
    #[test]
    fn tag_pairs_keep_duplicate_keys_in_input_order() {
        let pairs: TagPairs =
            serde_json::from_str(r#"{"tax_category":"SALES_10","tax_category":"SALES_8"}"#)
                .unwrap();
        assert_eq!(
            pairs.as_slice(),
            [
                ("tax_category".to_string(), "SALES_10".to_string()),
                ("tax_category".to_string(), "SALES_8".to_string()),
            ]
        );

        // 対照: `Map` で受けると 1 件に潰れる（この差がこの型の存在理由）。
        let collapsed: Map<String, Value> =
            serde_json::from_str(r#"{"tax_category":"SALES_10","tax_category":"SALES_8"}"#)
                .unwrap();
        assert_eq!(collapsed.len(), 1);
    }

    #[test]
    fn tag_pairs_schema_is_an_object_of_strings() {
        let schema = serde_json::to_value(schemars::schema_for!(TagPairs)).unwrap();
        assert_eq!(schema.get("type").and_then(|t| t.as_str()), Some("object"));
    }

    // 出力側の整形は `kaikei-app` / `kaikei-jp` に委ねている。
    #[test]
    fn line_to_json_uses_the_frozen_wire_vocabulary() {
        use kaikei_core::{AccountCode, Side, TagKey, TagValue};

        let mut tags = TagSet::new();
        tags.insert(
            TagKey::parse("tax_category").unwrap(),
            TagValue::Code("SALES_10".to_string()),
        );
        let line = JournalLine::new(
            AccountCode::parse("135").unwrap(),
            Side::Debit,
            Money::from_minor(110_000, Currency::JPY),
            tags,
            Some("4月分".to_string()),
        )
        .unwrap();

        let value = line_to_json(&line);
        assert_eq!(value["account"], json!("135"));
        assert_eq!(
            value["side"],
            json!(kaikei_app::wire::side_code(Side::Debit))
        );
        // 金額は区切り無しの文字列（number にしない。§5）。
        assert_eq!(value["amount"], json!("110000"));
        assert!(value["amount"].is_string());
        assert_eq!(value["currency"], json!("JPY"));
        assert_eq!(value["tags"]["tax_category"], json!("SALES_10"));
        assert_eq!(value["memo"], json!("4月分"));
    }

    // メモが無ければキーごと出さない（`null` と空文字を混同させない）。
    #[test]
    fn line_to_json_omits_the_memo_key_when_absent() {
        use kaikei_core::{AccountCode, Side};

        let line = JournalLine::new(
            AccountCode::parse("100").unwrap(),
            Side::Credit,
            Money::from_minor(1, Currency::JPY),
            TagSet::new(),
            None,
        )
        .unwrap();
        assert!(line_to_json(&line).get("memo").is_none());
    }

    // 注記の文言は素通しし、severity は凍結済みの語彙を使う。
    #[test]
    fn policy_notes_to_json_passes_the_message_through_verbatim() {
        use kaikei_app::NoteSeverity;

        let notes = [PolicyNote {
            severity: NoteSeverity::Info,
            message: "税込経理の設定のため税額行を生成していません".to_string(),
        }];
        let value = policy_notes_to_json(&notes);
        assert_eq!(value[0]["severity"], json!("info"));
        assert_eq!(
            value[0]["message"],
            json!("税込経理の設定のため税額行を生成していません")
        );
    }
}
