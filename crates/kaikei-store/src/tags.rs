//! `kaikei_core::TagSet` と JSONB（`journal_lines.tags` 列）の相互変換。
//!
//! PR-5 本体と PR-6（read model）の両方が参照する共有基盤（`DECISIONS.md`
//! D-034 / D-035）。
//!
//! # JSONB 表現（`DECISIONS.md` D-035）
//!
//! `TagSet` 全体は JSON オブジェクトとして表現する。キーはタグキー文字列
//! （[`TagKey::as_str`]）、値は `{"t": <型>, "v": <値>}` という2フィールドの
//! オブジェクトにする。
//!
//! ```json
//! {
//!   "tax_category": {"t": "code", "v": "10"},
//!   "business_ratio": {"t": "decimal", "v": "0.8"},
//!   "memo_date": {"t": "date", "v": "2026-04-15"},
//!   "memo": {"t": "text", "v": "備考"}
//! }
//! ```
//!
//! `"t"` は `"code"` / `"text"` / `"decimal"` / `"date"` のいずれか
//! （[`kaikei_core::TagValueType`] の4バリアントに対応）。**`"v"` は常に
//! JSON 文字列にする**（`DECISIONS.md` D-013「JSON では金額を文字列で扱う」
//! と同じ理由。`Decimal` を JSON の number にすると IEEE754 の丸め誤差を
//! 経由しうるため、文字列のまま `rust_decimal::Decimal` の `Display`/
//! `FromStr` で往復させる。`Date` も同様に ISO 文字列にする）。
//! 空の `TagSet` は空の JSON オブジェクト `{}` になり、`journal_lines.tags`
//! 列の `DEFAULT '{}'` と一致する。
//!
//! `"v"` を常に文字列にすることで、PR-6 の read model が
//! `l.tags -> k ->> 'v'`（`->` でキー `k` に対応する `{"t","v"}` オブジェクトを
//! 取り出し、`->>'v'` でその `"v"` フィールドをテキストとして取り出す）という
//! 素直な SQL でグルーピング用の値を取り出せる（型ごとに SQL 側の分岐が
//! 不要になる）。

use kaikei_app::error::RepoError;
use kaikei_core::{AccountingDate, TagKey, TagSet, TagValue};
use rust_decimal::Decimal;
use serde_json::{Map, Value};
use std::str::FromStr;

const TYPE_CODE: &str = "code";
const TYPE_TEXT: &str = "text";
const TYPE_DECIMAL: &str = "decimal";
const TYPE_DATE: &str = "date";

/// `TagSet` を `journal_lines.tags` 列に保存する JSONB 値に変換する。
pub fn tag_set_to_json(tags: &TagSet) -> Value {
    let mut map = Map::new();
    for (key, value) in tags.iter() {
        map.insert(key.as_str().to_string(), tag_value_to_json(value));
    }
    Value::Object(map)
}

fn tag_value_to_json(value: &TagValue) -> Value {
    let (t, v) = match value {
        TagValue::Code(s) => (TYPE_CODE, s.clone()),
        TagValue::Text(s) => (TYPE_TEXT, s.clone()),
        TagValue::Decimal(d) => (TYPE_DECIMAL, d.to_string()),
        TagValue::Date(d) => (TYPE_DATE, d.to_iso_string()),
    };
    let mut obj = Map::new();
    obj.insert("t".to_string(), Value::String(t.to_string()));
    obj.insert("v".to_string(), Value::String(v));
    Value::Object(obj)
}

/// `journal_lines.tags` 列から読み出した JSONB 値を `TagSet` に変換する。
///
/// 保存されているデータは通常 [`tag_set_to_json`] が書いた形をしているが、
/// 想定外の形（オブジェクトでない、`"t"`/`"v"` が無い、未知の型タグ、
/// パースできない小数・日付、不正なタグキー）に対しては panic せず
/// [`RepoError::Corrupt`] を返す（永続化層からの復元は無検証であっては
/// ならない。`kaikei_core::JournalEntry` の復元専用コンストラクタに対する
/// 規律と同じ）。
pub fn tag_set_from_json(value: &Value) -> Result<TagSet, RepoError> {
    let obj = value.as_object().ok_or_else(|| RepoError::Corrupt {
        reason: format!("tags 列がJSONオブジェクトではありません: {value}"),
    })?;

    let mut tags = TagSet::new();
    for (key_str, tag_json) in obj {
        let key = TagKey::parse(key_str).map_err(|e| RepoError::Corrupt {
            reason: format!("tags 列のキーが不正です: \"{key_str}\"（{e}）"),
        })?;
        let tag_value = tag_value_from_json(tag_json)?;
        tags.insert(key, tag_value);
    }
    Ok(tags)
}

fn tag_value_from_json(value: &Value) -> Result<TagValue, RepoError> {
    let obj = value.as_object().ok_or_else(|| RepoError::Corrupt {
        reason: format!("タグの値がJSONオブジェクトではありません: {value}"),
    })?;

    let t = obj
        .get("t")
        .and_then(Value::as_str)
        .ok_or_else(|| RepoError::Corrupt {
            reason: format!("タグに \"t\"（型）フィールドがありません: {value}"),
        })?;
    let v = obj
        .get("v")
        .and_then(Value::as_str)
        .ok_or_else(|| RepoError::Corrupt {
            reason: format!("タグの \"v\"（値）フィールドが文字列ではありません: {value}"),
        })?;

    match t {
        TYPE_CODE => Ok(TagValue::Code(v.to_string())),
        TYPE_TEXT => Ok(TagValue::Text(v.to_string())),
        TYPE_DECIMAL => {
            Decimal::from_str(v)
                .map(TagValue::Decimal)
                .map_err(|_| RepoError::Corrupt {
                    reason: format!("タグの小数値を解釈できません: \"{v}\""),
                })
        }
        TYPE_DATE => AccountingDate::parse(v)
            .map(TagValue::Date)
            .map_err(|_| RepoError::Corrupt {
                reason: format!("タグの日付を解釈できません: \"{v}\""),
            }),
        other => Err(RepoError::Corrupt {
            reason: format!("未知のタグ型です: \"{other}\""),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn key(s: &str) -> TagKey {
        TagKey::parse(s).unwrap()
    }

    #[test]
    fn empty_tag_set_round_trips_as_empty_object() {
        let tags = TagSet::new();
        let json = tag_set_to_json(&tags);
        assert_eq!(json, serde_json::json!({}));
        let restored = tag_set_from_json(&json).unwrap();
        assert!(restored.is_empty());
    }

    #[test]
    fn code_value_round_trips() {
        let mut tags = TagSet::new();
        tags.insert(key("tax_category"), TagValue::Code("10".to_string()));
        let json = tag_set_to_json(&tags);
        assert_eq!(
            json,
            serde_json::json!({"tax_category": {"t": "code", "v": "10"}})
        );
        assert_eq!(tag_set_from_json(&json).unwrap(), tags);
    }

    #[test]
    fn text_value_round_trips() {
        let mut tags = TagSet::new();
        tags.insert(key("memo"), TagValue::Text("備考".to_string()));
        let json = tag_set_to_json(&tags);
        assert_eq!(tag_set_from_json(&json).unwrap(), tags);
    }

    #[test]
    fn decimal_value_round_trips_as_string_not_number() {
        let mut tags = TagSet::new();
        tags.insert(
            key("business_ratio"),
            TagValue::Decimal(Decimal::from_str("0.8").unwrap()),
        );
        let json = tag_set_to_json(&tags);
        // 浮動小数点を経由しないことを、JSON上で "v" が文字列であることで確認する
        // （もし number にしていたら Value::Number になり、この assert が失敗する）。
        assert!(json["business_ratio"]["v"].is_string());
        assert_eq!(tag_set_from_json(&json).unwrap(), tags);
    }

    #[test]
    fn date_value_round_trips() {
        let mut tags = TagSet::new();
        tags.insert(
            key("memo_date"),
            TagValue::Date(AccountingDate::new(2026, 4, 15).unwrap()),
        );
        let json = tag_set_to_json(&tags);
        assert_eq!(json["memo_date"]["v"], "2026-04-15");
        assert_eq!(tag_set_from_json(&json).unwrap(), tags);
    }

    #[test]
    fn multiple_tags_round_trip_together() {
        let mut tags = TagSet::new();
        tags.insert(key("tax_category"), TagValue::Code("10".to_string()));
        tags.insert(
            key("business_ratio"),
            TagValue::Decimal(Decimal::from_str("0.333").unwrap()),
        );
        tags.insert(
            key("memo_date"),
            TagValue::Date(AccountingDate::new(2026, 1, 1).unwrap()),
        );
        tags.insert(key("memo"), TagValue::Text("x".to_string()));
        let json = tag_set_to_json(&tags);
        assert_eq!(tag_set_from_json(&json).unwrap(), tags);
    }

    #[test]
    fn unknown_type_tag_is_corrupt_not_panic() {
        let json = serde_json::json!({"tax_category": {"t": "unknown", "v": "x"}});
        assert!(matches!(
            tag_set_from_json(&json),
            Err(RepoError::Corrupt { .. })
        ));
    }

    #[test]
    fn non_object_top_level_json_is_corrupt() {
        for broken in [
            serde_json::json!([1, 2, 3]),
            serde_json::json!("not an object"),
            serde_json::json!(42),
            serde_json::json!(null),
        ] {
            assert!(matches!(
                tag_set_from_json(&broken),
                Err(RepoError::Corrupt { .. })
            ));
        }
    }

    #[test]
    fn tag_value_missing_t_field_is_corrupt() {
        let json = serde_json::json!({"tax_category": {"v": "10"}});
        assert!(matches!(
            tag_set_from_json(&json),
            Err(RepoError::Corrupt { .. })
        ));
    }

    #[test]
    fn tag_value_missing_v_field_is_corrupt() {
        let json = serde_json::json!({"tax_category": {"t": "code"}});
        assert!(matches!(
            tag_set_from_json(&json),
            Err(RepoError::Corrupt { .. })
        ));
    }

    #[test]
    fn tag_value_v_as_number_is_corrupt() {
        let json = serde_json::json!({"business_ratio": {"t": "decimal", "v": 0.8}});
        assert!(matches!(
            tag_set_from_json(&json),
            Err(RepoError::Corrupt { .. })
        ));
    }

    #[test]
    fn tag_value_not_object_is_corrupt() {
        let json = serde_json::json!({"tax_category": "10"});
        assert!(matches!(
            tag_set_from_json(&json),
            Err(RepoError::Corrupt { .. })
        ));
    }

    #[test]
    fn invalid_decimal_string_is_corrupt() {
        let json = serde_json::json!({"business_ratio": {"t": "decimal", "v": "not-a-number"}});
        assert!(matches!(
            tag_set_from_json(&json),
            Err(RepoError::Corrupt { .. })
        ));
    }

    #[test]
    fn invalid_date_string_is_corrupt() {
        let json = serde_json::json!({"memo_date": {"t": "date", "v": "2026/04/15"}});
        assert!(matches!(
            tag_set_from_json(&json),
            Err(RepoError::Corrupt { .. })
        ));
    }

    #[test]
    fn invalid_tag_key_is_corrupt() {
        let json = serde_json::json!({"TaxCategory": {"t": "code", "v": "10"}});
        assert!(matches!(
            tag_set_from_json(&json),
            Err(RepoError::Corrupt { .. })
        ));
    }

    // ---- プロパティテスト（Phase 0の教訓: 境界値をprop_oneof!で明示的に含める）----

    /// Decimal の scale 0〜28（rust_decimal の最大スケール）を境界込みで生成する。
    fn any_decimal() -> impl Strategy<Value = Decimal> {
        prop_oneof![
            (-1_000_000i64..=1_000_000i64, 0u32..=6u32)
                .prop_map(|(mantissa, scale)| Decimal::new(mantissa, scale)),
            Just(Decimal::ZERO),
            Just(Decimal::new(1, 0)),
            Just(Decimal::new(123, 28)),
            Just(Decimal::new(-123, 28)),
        ]
    }

    proptest! {
        #[test]
        fn decimal_tag_round_trips_for_various_scales(decimal in any_decimal()) {
            let mut tags = TagSet::new();
            tags.insert(key("business_ratio"), TagValue::Decimal(decimal));
            let json = tag_set_to_json(&tags);
            let restored = tag_set_from_json(&json).unwrap();
            prop_assert_eq!(restored, tags);
        }
    }
}
