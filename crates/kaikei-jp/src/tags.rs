//! タグスキーマ（`kaikei-jp-data/tags.yaml`）→ `kaikei_core::TagSchema`。
//!
//! `docs/04-jp-tax.md` §4「タグスキーマ」・`CLAUDE.md` §4「TagSet はゴミ箱では
//! ない」に対応する。新しいタグキーが必要になったら `tags.yaml` に登録する
//! （このローダは未登録キーを増やす経路を提供しない。登録されたキーだけを
//! `TagSchema` に変換する）。
//!
//! `TagSchema` も `kaikei-core` の型であり、[`crate::chart`] と同じ理由
//! （orphan rule）で自由関数として公開する。
//!
//! # `description` の扱い（`DECISIONS.md` D-062）
//!
//! `tags.yaml` の各タグ定義は `description` を持つが、`kaikei_core::TagDef`
//! に対応するフィールドが無い。ドメイン変換では**破棄する**（YAML 上は
//! `deny_unknown_fields` によるスキーマ完全性検証のために受け取る）。
//! `description` は YAML を編集する人間向けの注記であり、現時点で
//! `TagSchema` / `TagDef` の利用側（`kaikei-core::JournalEntry::new` の
//! タグ検証等）がこれを必要としていないため（YAGNI）。将来 MCP 経由で
//! タグの意味を AI に説明する用途が生じたら、`kaikei_core::TagDef` に
//! `description` を追加するかどうかを別途検討する。
//!
//! # `tags` マッピングの読み方と決定性（`DECISIONS.md` D-062）
//!
//! `tags.yaml` の `tags:` は YAML マッピング（キー = タグキー）。素朴に
//! `BTreeMap<String, TagDefRaw>` 等の `serde` 標準のマップ型へ読み込むと、
//! **重複キーがあってもエラーにならず、後勝ちで黙って上書きされる**
//! （`serde_norway` 0.9.42 で実際に確認済み: `"a: 1\nb: 2\na: 3\n"` を
//! `BTreeMap<String, i32>` に読ませると `{"a": 3, "b": 2}` になり、
//! `a` の最初の定義がエラーなく失われる）。`tags.yaml` は「任意パスからの
//! 差し替え」（[`load_from_path`]）でユーザー自身が編集するファイルでもあり、
//! コピペミス等で重複キーが入り込んでも黙って1件消えるのは
//! `CLAUDE.md` §4 の精神に反する。
//!
//! そこで `tags` フィールドは [`ordered_pairs`] という `MapAccess` を直接
//! 読む deserializer で `Vec<(String, TagDefRaw)>`（出現順・重複を保持した
//! ペア列）として受け取り、[`from_raw`] 側で明示的に重複キーを検出して
//! `JpError::InvalidTagSchema` を返す。
//!
//! なお、最終的な `TagSchema`（`kaikei_core` 側）は内部で `BTreeMap` に
//! 詰め直されるため、`Vec` の並び順自体は `TagSchema` の挙動
//! （`validate` / `is_aggregatable`）には影響しない。並び順を保存する
//! 理由は重複検出のためだけである。

use crate::account_type::parse_account_type;
use crate::error::JpError;
use kaikei_core::{TagDef, TagKey, TagSchema, TagValueType};
use kaikei_jp_data::EmbeddedYaml;
use serde::de::{Deserializer, MapAccess, Visitor};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;

/// この PR 時点でこの crate が読める唯一のスキーマ版。
const SUPPORTED_VERSION: u32 = 1;

/// `kaikei-jp-data` の埋め込み YAML からタグスキーマを読み込む。
///
/// 例: `tags::load_embedded(kaikei_jp_data::TAGS)`。
pub fn load_embedded(embedded: EmbeddedYaml) -> Result<TagSchema, JpError> {
    let raw: TagSchemaRaw = crate::yaml::load_embedded(embedded)?;
    from_raw(embedded.label, raw)
}

/// 任意のファイルパスからタグスキーマを読み込む（ユーザーが自分のタグ
/// スキーマに差し替える経路）。
pub fn load_from_path(path: &Path) -> Result<TagSchema, JpError> {
    let raw: TagSchemaRaw = crate::yaml::load_from_path(path)?;
    from_raw(&path.display().to_string(), raw)
}

/// YAML 文字列からタグスキーマを読み込む（テスト、および上2つの共通経路）。
pub fn load_from_str(source: &str, label: &str) -> Result<TagSchema, JpError> {
    let raw: TagSchemaRaw = crate::yaml::load_str(source, label)?;
    from_raw(label, raw)
}

fn from_raw(label: &str, raw: TagSchemaRaw) -> Result<TagSchema, JpError> {
    let invalid = |reason: String| JpError::InvalidTagSchema {
        label: label.to_string(),
        reason,
    };

    if raw.version != SUPPORTED_VERSION {
        return Err(invalid(format!(
            "対応していないスキーマバージョンです: {}（対応: {}）。新しいバージョンの \
             スキーマを読むには kaikei-jp 側の対応が必要です",
            raw.version, SUPPORTED_VERSION
        )));
    }

    let mut seen_keys = BTreeSet::new();
    let mut defs = Vec::with_capacity(raw.tags.len());
    for (key, def_raw) in raw.tags {
        if !seen_keys.insert(key.clone()) {
            return Err(invalid(format!(
                "タグキーが重複しています: \"{key}\"。tags.yaml 内でキーは一意である必要があります"
            )));
        }
        let def = tag_def_from_raw(&key, def_raw).map_err(&invalid)?;
        let tag_key = TagKey::parse(&key)
            .map_err(|source| invalid(format!("タグキーが不正です: \"{key}\": {source}")))?;
        defs.push((tag_key, def));
    }

    Ok(TagSchema::new(defs))
}

fn tag_def_from_raw(key: &str, raw: TagDefRaw) -> Result<TagDef, String> {
    let value_type =
        parse_value_type(&raw.value_type).map_err(|reason| format!("key={key}: {reason}"))?;
    let required_for = raw
        .required_for
        .iter()
        .map(|s| parse_account_type("required_for", s))
        .collect::<Result<Vec<_>, String>>()
        .map_err(|reason| format!("key={key}: {reason}"))?;

    Ok(TagDef {
        value_type,
        aggregatable: raw.aggregatable,
        required_for,
    })
}

/// YAML の `value_type` フィールド（`Code` | `Text` | `Decimal` | `Date`）を
/// `kaikei_core::TagValueType` に写像する。
fn parse_value_type(s: &str) -> Result<TagValueType, String> {
    match s {
        "Code" => Ok(TagValueType::Code),
        "Text" => Ok(TagValueType::Text),
        "Decimal" => Ok(TagValueType::Decimal),
        "Date" => Ok(TagValueType::Date),
        other => Err(format!(
            "value_type の値が不正です: \"{other}\"（有効な値: Code, Text, Decimal, Date）"
        )),
    }
}

/// [`TagSchema`] の YAML 上の生の形。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TagSchemaRaw {
    version: u32,
    #[serde(deserialize_with = "ordered_pairs")]
    tags: Vec<(String, TagDefRaw)>,
}

/// タグ1件分の YAML 上の生の形。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TagDefRaw {
    value_type: String,
    aggregatable: bool,
    #[serde(default)]
    required_for: Vec<String>,
    /// 人間向けの注記。ドメイン変換では破棄する（`DECISIONS.md` D-062）。
    #[allow(dead_code)]
    #[serde(default)]
    description: Option<String>,
}

/// YAML マッピングを、出現順・重複キーを保持したまま `Vec<(K, V)>` として
/// 読む（モジュール doc の「`tags` マッピングの読み方と決定性」を参照）。
///
/// `serde::Deserializer::deserialize_map` が渡す `MapAccess` を直接
/// 読み進める。標準の `BTreeMap`/`HashMap` へ読み込む場合と異なり、
/// 重複キーがあってもここでは上書きされず、両方とも `Vec` に残る
/// （重複の検出自体は呼び出し側 [`from_raw`] が行う）。
fn ordered_pairs<'de, D, V>(deserializer: D) -> Result<Vec<(String, V)>, D::Error>
where
    D: Deserializer<'de>,
    V: Deserialize<'de>,
{
    struct OrderedPairsVisitor<V>(std::marker::PhantomData<V>);

    impl<'de, V: Deserialize<'de>> Visitor<'de> for OrderedPairsVisitor<V> {
        type Value = Vec<(String, V)>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a YAML mapping of tag key to tag definition")
        }

        fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
        where
            A: MapAccess<'de>,
        {
            let mut entries = Vec::with_capacity(map.size_hint().unwrap_or(0));
            while let Some(entry) = map.next_entry::<String, V>()? {
                entries.push(entry);
            }
            Ok(entries)
        }
    }

    deserializer.deserialize_map(OrderedPairsVisitor(std::marker::PhantomData))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempFile;
    use kaikei_core::{AccountType, TagSet, TagValue};

    const VALID_YAML: &str = r#"
version: 1
tags:
  tax_category:
    value_type: Code
    aggregatable: true
    required_for: [Revenue, Expense]
    description: "消費税区分"
  project:
    value_type: Code
    aggregatable: true
    description: "案件コード"
"#;

    /// 実データ（`kaikei_jp_data::TAGS`）がパースできること。
    #[test]
    fn load_embedded_parses_the_bundled_schema() {
        let schema = load_embedded(kaikei_jp_data::TAGS).unwrap();

        // tax_category は Revenue/Expense で必須（tags.yaml の実データ）。
        let mut tags = TagSet::new();
        let err = schema
            .validate(&tags, AccountType::Expense)
            .expect_err("tax_category が未設定なのでエラーになるはず");
        assert!(matches!(
            err,
            kaikei_core::CoreError::MissingRequiredTag { .. }
        ));

        tags.insert(
            TagKey::parse("tax_category").unwrap(),
            TagValue::Code("SALES_10".to_string()),
        );
        schema.validate(&tags, AccountType::Expense).unwrap();
    }

    #[test]
    fn load_from_str_parses_valid_schema() {
        let schema = load_from_str(VALID_YAML, "test").unwrap();
        let mut tags = TagSet::new();
        tags.insert(
            TagKey::parse("project").unwrap(),
            TagValue::Code("P1".to_string()),
        );
        // project は required_for が無いので Asset でも検証を通る。
        schema.validate(&tags, AccountType::Asset).unwrap();
    }

    #[test]
    fn load_from_str_rejects_unknown_top_level_field() {
        let yaml = format!("{VALID_YAML}\nextra_field: true\n");
        let err = load_from_str(&yaml, "test").unwrap_err();
        assert!(matches!(err, JpError::YamlParse { .. }));
    }

    #[test]
    fn load_from_str_rejects_unknown_tag_field() {
        let yaml = VALID_YAML.replace(
            "    description: \"案件コード\"\n",
            "    description: \"案件コード\"\n    unexpected: true\n",
        );
        let err = load_from_str(&yaml, "test").unwrap_err();
        assert!(matches!(err, JpError::YamlParse { .. }));
    }

    #[test]
    fn load_from_str_unsupported_version_is_error() {
        let yaml = VALID_YAML.replace("version: 1", "version: 2");
        let err = load_from_str(&yaml, "test").unwrap_err();
        match err {
            JpError::InvalidTagSchema { reason, .. } => {
                assert!(reason.contains('2'), "reason = {reason}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn load_from_str_unknown_value_type_lists_valid_values() {
        let yaml = VALID_YAML.replace("value_type: Code", "value_type: Number");
        let err = load_from_str(&yaml, "test").unwrap_err();
        match err {
            JpError::InvalidTagSchema { reason, .. } => {
                assert!(reason.contains("Number"), "reason = {reason}");
                assert!(reason.contains("Code"), "reason = {reason}");
                assert!(reason.contains("Text"), "reason = {reason}");
                assert!(reason.contains("Decimal"), "reason = {reason}");
                assert!(reason.contains("Date"), "reason = {reason}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn load_from_str_unknown_required_for_value_lists_valid_values() {
        let yaml = VALID_YAML.replace("required_for: [Revenue, Expense]", "required_for: [Foo]");
        let err = load_from_str(&yaml, "test").unwrap_err();
        match err {
            JpError::InvalidTagSchema { reason, .. } => {
                assert!(reason.contains("Foo"), "reason = {reason}");
                assert!(reason.contains("Asset"), "reason = {reason}");
                assert!(reason.contains("Expense"), "reason = {reason}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn load_from_str_invalid_tag_key_is_error() {
        // TagKey::parse は大文字始まりを拒否する。
        let yaml = VALID_YAML.replace("tax_category:", "TaxCategory:");
        let err = load_from_str(&yaml, "test").unwrap_err();
        assert!(matches!(err, JpError::InvalidTagSchema { .. }));
    }

    /// 重複するタグキーは、後勝ちで黙って上書きされず、明示的にエラーになる。
    ///
    /// `serde_norway` の `BTreeMap`/`HashMap` デシリアライズは重複キーを
    /// エラーにしない（モジュール doc 参照）ため、`ordered_pairs` が
    /// 重複を保持したまま渡し、ここで検出できることを確認する。
    #[test]
    fn load_from_str_duplicate_tag_key_is_error() {
        let yaml = r#"
version: 1
tags:
  tax_category:
    value_type: Code
    aggregatable: true
  tax_category:
    value_type: Text
    aggregatable: false
"#;
        let err = load_from_str(yaml, "test").unwrap_err();
        match err {
            JpError::InvalidTagSchema { reason, .. } => {
                assert!(reason.contains("tax_category"), "reason = {reason}");
                assert!(reason.contains("重複"), "reason = {reason}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    /// 埋め込みと差し替えで検証の強さが変わらないこと。
    #[test]
    fn load_from_path_rejects_unknown_fields_just_like_embedded() {
        let file = TempFile::with_contents(&format!(
            "{VALID_YAML}
extra_field: true
"
        ));
        let err = load_from_path(file.path()).unwrap_err();
        assert!(matches!(err, JpError::YamlParse { .. }));
    }

    /// ロード結果が決定的であること（同じ YAML を複数回ロードして同じ順序・内容になる）。
    ///
    /// `TagSchema` は `Debug` を導出しているため、内部の `BTreeMap` の並びを
    /// 含めて `{:?}` で比較できる。実データ（`tags.yaml`）を使い、複数回
    /// ロードした結果が常に同一の表現になることを確認する。
    #[test]
    fn load_embedded_is_deterministic_across_repeated_loads() {
        let snapshots: Vec<String> = (0..5)
            .map(|_| format!("{:?}", load_embedded(kaikei_jp_data::TAGS).unwrap()))
            .collect();

        for snapshot in &snapshots[1..] {
            assert_eq!(snapshot, &snapshots[0]);
        }
    }
}
