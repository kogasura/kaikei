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

// ---------------------------------------------------------------------------
// 線上の `tags`
// ---------------------------------------------------------------------------
//
// ★MCP 経由では重複キーを検出できない★（PR-F レビュー B-2。D-085 の訂正注記）
//
// 当初ここには `TagPairs`（出現順のペアを保持する手書き `Deserialize`）が
// あり、「重複キーを畳み込まないので `JpError::DuplicateTagKeyInInput` に
// 到達できる」と主張していた。**それは成立していなかった。**
//
// rmcp の `CallToolRequestParams::arguments` は `Option<JsonObject>`
// （＝ `serde_json::Map`）であり、JSON-RPC メッセージ全体が `serde_json` で
// パースされる時点——つまり `dispatch::call` に入るより前——で重複キーは
// **後勝ちで畳み込まれている**。ツール側の受け型を何にしても、届く時点で
// 既に1件になっている。`wire.rs` の単体テストが緑だったのは、本番と違う
// 入口（`serde_json::from_str` に生のテキストを渡す）を通していたためで、
// 本番経路を再現していなかった。
//
// 生の JSON テキストを MCP 層まで持ち込むには rmcp の stdio トランスポートを
// 自前に置き換えることになり、得るものに対して代償が大きい。**提供できない
// 保証を主張しない**ほうを採り、型ごと削除した（`tags` は
// `BTreeMap<String, String>` で受ける）。制約は
// `docs/07-mcp-server.md` §3 と D-085 に明記してある。

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

    // ★MCP 経由では `tags` の重複キーを検出できない★（B-2）
    //
    // 本番経路では、JSON-RPC メッセージ全体が `serde_json` でパースされる
    // 時点で重複キーが後勝ちで畳み込まれ、ツール層には既に1件になった
    // `serde_json::Map` が届く。**このテストはその事実を固定するもので、
    // 「重複を検出できる」ことを検証してはいない。**
    //
    // ここで `serde_json::from_str` に生のテキストを渡しているのは、
    // 畳み込みが**どこで起きるか**（＝ MCP 層より前）を示すためである。
    // 以前ここにあった `TagPairs` のテストは、同じ入口を使いながら
    // 「重複キーが保持される」ことを主張しており、本番経路で成立しない
    // 保証を緑のテストで裏書きしていた（誤診は誤値と同じ実害を持つ。
    // `PROGRESS.md` Phase 1 の教訓3）。
    #[test]
    fn duplicate_tag_keys_are_already_collapsed_before_reaching_this_layer() {
        let collapsed: Map<String, Value> =
            serde_json::from_str(r#"{"tax_category":"SALES_10","tax_category":"SALES_8_REDUCED"}"#)
                .unwrap();

        assert_eq!(collapsed.len(), 1, "重複キーは畳み込まれる: {collapsed:?}");
        // 後勝ち。前の指定は跡形も無い（＝MCP 層からは検出しようがない）。
        assert_eq!(collapsed["tax_category"], json!("SALES_8_REDUCED"));
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
    //
    // 文言そのものは `kaikei-policy` の実装（`kaikei-jp`）が決める。
    // ここで**実在する文言をリテラルで置くと、grep したときに
    // 「その文言が実装されている」ように見えてしまう**ので、
    // 明らかに検査用と分かる文字列を使う（実在する注記の生成経路と文言は
    // `crates/kaikei-jp/src/tax/policy.rs` のテストが持つ）。
    #[test]
    fn policy_notes_to_json_passes_the_message_through_verbatim() {
        use kaikei_app::NoteSeverity;

        let notes = [PolicyNote {
            severity: NoteSeverity::Info,
            message: "（検査用の注記本文）".to_string(),
        }];
        let value = policy_notes_to_json(&notes);
        assert_eq!(value[0]["severity"], json!("info"));
        assert_eq!(value[0]["message"], json!("（検査用の注記本文）"));
    }
}
