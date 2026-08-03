//! タグスキーマ（`kaikei-jp-data/tags.yaml`）→ `kaikei_core::TagSchema`、
//! および**線上（JSON）の文字列 → `kaikei_core::TagSet`** の変換
//! （[`TagCatalog`]）。
//!
//! `docs/04-jp-tax.md` §4「タグスキーマ」・`CLAUDE.md` §4「TagSet はゴミ箱では
//! ない」に対応する。新しいタグキーが必要になったら `tags.yaml` に登録する
//! （このローダは未登録キーを増やす経路を提供しない。登録されたキーだけを
//! `TagSchema` に変換する）。
//!
//! `TagSchema` も `kaikei-core` の型であり、[`crate::chart`] と同じ理由
//! （orphan rule）で自由関数として公開する。
//!
//! # 線上の `tags` を `TagSet` にする（`DECISIONS.md` D-074 訂正注記4）
//!
//! `docs/07-mcp-server.md` §3 の `post_journal_entry` は
//! `"tags": {"tax_category": "SALES_10", "counterparty": "CP0001"}` という
//! **文字列 → 文字列**のマップを受け取る。一方 `kaikei_core::TagValue` は
//! 型付き（`Code` / `Text` / `Decimal` / `Date`）で、平文から作るにはキーごとの
//! `TagValueType` が要る。`TagSchema` の公開 API（`new` / `empty` /
//! `validate` / `is_aggregatable`）にはそれを引き戻す手段が無い。
//!
//! そこで [`TagCatalog`] が `TagSchema` と**キーごとの [`TagDef`]** の両方を
//! 保持し、[`TagCatalog::parse_tag_set`] が変換を引き受ける。
//! `kaikei-core` は変更していない（`CLAUDE.md` §1。docs/07 §3 が挙げた
//! 3案のうち (a) を採る）。
//!
//! 値の文字列表現は `DECISIONS.md` D-035 の JSONB 表現の `"v"`（型によらず
//! 常に文字列）と**同じ規約**にしてある。線上とDBで別の書き方を発明しない。
//!
//! | `value_type` | 線上の文字列 | 変換 |
//! |---|---|---|
//! | `Code` | `"SALES_10"` | そのまま |
//! | `Text` | `"備考"` | そのまま |
//! | `Decimal` | `"0.30"` | `rust_decimal::Decimal::from_str_exact` |
//! | `Date` | `"2026-04-15"` | `AccountingDate::parse`（ISO 8601） |
//!
//! **未登録キー・型不一致は黙って通さない**（`CLAUDE.md` §4）。
//! エラーには有効なキー一覧や期待する書式を必ず載せる（同 §11）。
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
use kaikei_core::{AccountingDate, TagDef, TagKey, TagSchema, TagSet, TagValue, TagValueType};
use kaikei_jp_data::EmbeddedYaml;
use rust_decimal::Decimal;
use serde::de::{Deserializer, MapAccess, Visitor};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;

/// この PR 時点でこの crate が読める唯一のスキーマ版。
const SUPPORTED_VERSION: u32 = 1;

/// エラーメッセージに載せる入力文字列の上限（文字数）。
///
/// タグの値は自由記述（`Text`）でありうるため、呼び出し元が送ってきた文字列を
/// そのまま載せると応答と `audit_log` が入力次第でいくらでも膨らむ
/// （`kaikei-app` の `id::MAX_ECHOED_INPUT_CHARS` と同じ規律）。
const MAX_ECHOED_INPUT_CHARS: usize = 64;

/// `kaikei-jp-data` の埋め込み YAML からタグスキーマを読み込む。
///
/// 例: `tags::load_embedded(kaikei_jp_data::TAGS)`。
///
/// 線上（JSON）の文字列から `TagSet` を組み立てる必要がある呼び出し元は、
/// この関数ではなく [`TagCatalog::from_embedded`] を使う（`TagSchema` は
/// キーごとの `TagValueType` を引き戻せない）。
pub fn load_embedded(embedded: EmbeddedYaml) -> Result<TagSchema, JpError> {
    Ok(TagCatalog::from_embedded(embedded)?.into_schema())
}

/// 任意のファイルパスからタグスキーマを読み込む（ユーザーが自分のタグ
/// スキーマに差し替える経路）。
pub fn load_from_path(path: &Path) -> Result<TagSchema, JpError> {
    Ok(TagCatalog::from_path(path)?.into_schema())
}

/// YAML 文字列からタグスキーマを読み込む（テスト、および上2つの共通経路）。
pub fn load_from_str(source: &str, label: &str) -> Result<TagSchema, JpError> {
    Ok(TagCatalog::from_yaml_str(source, label)?.into_schema())
}

/// `TagValue` を線上（JSON）の文字列にする（[`TagCatalog::parse_value`] の逆向き）。
///
/// 応答に確定後の明細を載せるとき（`docs/07-mcp-server.md` §3
/// 「確定後の明細を必ず返す」）に使う。表現は `DECISIONS.md` D-035 の
/// JSONB の `"v"` と同じで、**型は値そのものからは線上に出ない**
/// （型はタグスキーマが持っており、キーから引ける）。
pub fn tag_value_to_string(value: &TagValue) -> String {
    match value {
        TagValue::Code(s) | TagValue::Text(s) => s.clone(),
        TagValue::Decimal(d) => d.to_string(),
        TagValue::Date(d) => d.to_iso_string(),
    }
}

/// タグスキーマと、**キーごとの定義**の両方を保持したもの。
///
/// `kaikei_core::TagSchema` は検証（`validate` / `is_aggregatable`）のための
/// 型であり、キーから `TagValueType` を引き戻せない。線上の文字列から
/// `TagSet` を組み立てる層（`kaikei-mcp` / `kaikei-api`）はそれが要るため、
/// ロード時に得られる `Vec<(TagKey, TagDef)>` をここで保持する。
///
/// `TagSchema` は同じ `Vec` から [`TagCatalog`] 構築時に1度だけ組み立てる。
/// 2つの一覧を手で維持しているわけではない（`PROGRESS.md` Phase 1 の教訓6）。
#[derive(Debug, Clone)]
pub struct TagCatalog {
    label: String,
    defs: Vec<(TagKey, TagDef)>,
    schema: TagSchema,
}

impl TagCatalog {
    /// `kaikei-jp-data` の埋め込み YAML から読み込む。
    pub fn from_embedded(embedded: EmbeddedYaml) -> Result<Self, JpError> {
        let raw: TagSchemaRaw = crate::yaml::load_embedded(embedded)?;
        from_raw(embedded.label, raw)
    }

    /// 任意のファイルパスから読み込む（ユーザーが自分のタグスキーマに
    /// 差し替える経路）。
    pub fn from_path(path: &Path) -> Result<Self, JpError> {
        let raw: TagSchemaRaw = crate::yaml::load_from_path(path)?;
        from_raw(&path.display().to_string(), raw)
    }

    /// YAML 文字列から読み込む（テスト、および上2つの共通経路）。
    pub fn from_yaml_str(source: &str, label: &str) -> Result<Self, JpError> {
        let raw: TagSchemaRaw = crate::yaml::load_str(source, label)?;
        from_raw(label, raw)
    }

    /// 読み込み元の識別子（埋め込みYAMLの名称、またはファイルパス）。
    pub fn label(&self) -> &str {
        &self.label
    }

    /// `kaikei_core` の検証に渡すタグスキーマ。
    pub fn schema(&self) -> &TagSchema {
        &self.schema
    }

    /// タグスキーマだけを取り出す。
    pub fn into_schema(self) -> TagSchema {
        self.schema
    }

    /// 登録されているタグ定義を YAML の出現順で返す。
    pub fn defs(&self) -> &[(TagKey, TagDef)] {
        &self.defs
    }

    /// キーに対応するタグ定義。未登録キーは `None`。
    pub fn def(&self, key: &TagKey) -> Option<&TagDef> {
        self.defs.iter().find(|(k, _)| k == key).map(|(_, def)| def)
    }

    /// キーに対応する値の型。未登録キーは `None`。
    pub fn value_type(&self, key: &TagKey) -> Option<TagValueType> {
        self.def(key).map(|def| def.value_type)
    }

    /// 登録されているタグキーを昇順・`", "` 区切りで並べた表示用文字列
    /// （エラーメッセージ用。`CLAUDE.md` §11）。
    pub fn registered_keys_display(&self) -> String {
        let mut keys: Vec<&str> = self.defs.iter().map(|(k, _)| k.as_str()).collect();
        keys.sort_unstable();
        keys.join(", ")
    }

    /// 線上のキーと値（どちらも文字列）を、登録された `value_type` に従って
    /// `TagKey` と `TagValue` にする。
    ///
    /// # Errors
    ///
    /// - 未登録のキー（形式として `TagKey` になれない文字列を含む）→
    ///   [`JpError::UnregisteredTagKey`]（有効なキー一覧を載せる）
    /// - 値がその型として解釈できない → [`JpError::InvalidTagValue`]
    ///   （期待する書式を載せる）
    ///
    /// **必須タグ（`required_for`）の検証はここでは行わない。** それは
    /// 明細の科目種別が決まって初めて判定できるもので、
    /// `kaikei_core::JournalEntry::new` が `TagSchema` で行う
    /// （`DECISIONS.md` D-004。検証を2箇所に分けて持たない）。
    pub fn parse_value(&self, key: &str, text: &str) -> Result<(TagKey, TagValue), JpError> {
        let tag_key = TagKey::parse(key).map_err(|_| self.unregistered_tag_key(key))?;
        let Some(def) = self.def(&tag_key) else {
            return Err(self.unregistered_tag_key(key));
        };
        let value = parse_tag_value(key, def.value_type, text)?;
        Ok((tag_key, value))
    }

    /// 線上の `tags`（文字列 → 文字列のマップ）を `TagSet` にする。
    ///
    /// ```
    /// # use kaikei_jp::tags::TagCatalog;
    /// let catalog = TagCatalog::from_embedded(kaikei_jp_data::TAGS).unwrap();
    /// let tags = catalog
    ///     .parse_tag_set([("tax_category", "SALES_10"), ("business_ratio", "0.3")])
    ///     .unwrap();
    /// assert_eq!(tags.iter().count(), 2);
    /// ```
    ///
    /// # Errors
    ///
    /// [`Self::parse_value`] のエラーに加え、同じキーが入力に2回以上現れた
    /// 場合は [`JpError::DuplicateTagKeyInInput`]。後勝ちで黙って上書きしない
    /// （`CLAUDE.md` §4）。
    pub fn parse_tag_set<I, K, V>(&self, pairs: I) -> Result<TagSet, JpError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let mut tags = TagSet::new();
        for (key, value) in pairs {
            let (tag_key, tag_value) = self.parse_value(key.as_ref(), value.as_ref())?;
            if tags.insert(tag_key.clone(), tag_value).is_some() {
                return Err(JpError::DuplicateTagKeyInInput {
                    key: tag_key.as_str().to_string(),
                });
            }
        }
        Ok(tags)
    }

    fn unregistered_tag_key(&self, key: &str) -> JpError {
        JpError::UnregisteredTagKey {
            key: echo_for_message(key),
            valid: self.registered_keys_display(),
        }
    }
}

/// 線上の文字列を、指定された型の [`TagValue`] にする。
fn parse_tag_value(key: &str, value_type: TagValueType, text: &str) -> Result<TagValue, JpError> {
    let invalid = |reason: String| JpError::InvalidTagValue {
        key: echo_for_message(key),
        value_type_label: value_type.label_ja().to_string(),
        input: echo_for_message(text),
        reason,
    };

    if text.trim().is_empty() {
        return Err(invalid(
            "値が空です。値を指定するか、このタグごと省略してください".to_string(),
        ));
    }

    match value_type {
        TagValueType::Code => Ok(TagValue::Code(text.to_string())),
        TagValueType::Text => Ok(TagValue::Text(text.to_string())),
        // `Decimal::from_str`（`kaikei-core` の `parse_decimal` が使う）ではなく
        // `from_str_exact` を使う。前者は表現できない桁数を黙って丸めるため、
        // 線上から受け取った按分比率が入力と違う値で保存されうる
        // （`business_ratio` は税務調査時の根拠として記録するもの。
        // `kaikei-jp-data/tags.yaml`）。入口では丸めずに拒否する。
        TagValueType::Decimal => Decimal::from_str_exact(text)
            .map(TagValue::Decimal)
            .map_err(|_| {
                invalid(
                    "小数として解釈できる形式で指定してください（例: \"0.30\"）。\
                     丸めずに保存するため、表現できない桁数の値は受け付けません"
                        .to_string(),
                )
            }),
        TagValueType::Date => AccountingDate::parse(text)
            .map(TagValue::Date)
            .map_err(|source| {
                invalid(format!(
                    "ISO 8601 の日付（YYYY-MM-DD）で指定してください: {source}"
                ))
            }),
    }
}

/// エラーメッセージに載せる文字列を [`MAX_ECHOED_INPUT_CHARS`] 文字までに切る。
fn echo_for_message(s: &str) -> String {
    if s.chars().count() <= MAX_ECHOED_INPUT_CHARS {
        return s.to_string();
    }
    let head: String = s.chars().take(MAX_ECHOED_INPUT_CHARS).collect();
    format!("{head}…")
}

fn from_raw(label: &str, raw: TagSchemaRaw) -> Result<TagCatalog, JpError> {
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

    let schema = TagSchema::new(defs.clone());
    Ok(TagCatalog {
        label: label.to_string(),
        defs,
        schema,
    })
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
    use kaikei_core::AccountType;

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

    // ---- TagCatalog: 線上（JSON）の文字列 → TagSet（Phase 3 PR-B） ----

    /// 4つの `value_type` をすべて含むスキーマ（`tags.yaml` の実データには
    /// `Date` 型のキーが無いため、変換の網羅はこちらで行う）。
    const ALL_TYPES_YAML: &str = r#"
version: 1
tags:
  tax_category:
    value_type: Code
    aggregatable: true
    required_for: [Revenue, Expense]
  invoice_reg_no:
    value_type: Text
    aggregatable: false
  business_ratio:
    value_type: Decimal
    aggregatable: false
  delivered_on:
    value_type: Date
    aggregatable: false
"#;

    fn all_types_catalog() -> TagCatalog {
        TagCatalog::from_yaml_str(ALL_TYPES_YAML, "test").unwrap()
    }

    /// ★消費側が「変換経路が無い」と指摘した点★
    /// 文字列→文字列のマップから、型付きの `TagSet` が組み立てられる。
    #[test]
    fn parse_tag_set_builds_typed_values_from_plain_strings() {
        let catalog = all_types_catalog();
        let tags = catalog
            .parse_tag_set([
                ("tax_category", "SALES_10"),
                ("invoice_reg_no", "T1234567890123"),
                ("business_ratio", "0.30"),
                ("delivered_on", "2026-04-15"),
            ])
            .unwrap();

        assert_eq!(
            tags.get(&TagKey::parse("tax_category").unwrap()),
            Some(&TagValue::Code("SALES_10".to_string()))
        );
        assert_eq!(
            tags.get(&TagKey::parse("invoice_reg_no").unwrap()),
            Some(&TagValue::Text("T1234567890123".to_string()))
        );
        assert_eq!(
            tags.get(&TagKey::parse("business_ratio").unwrap()),
            Some(&TagValue::Decimal(Decimal::from_str_exact("0.30").unwrap()))
        );
        assert_eq!(
            tags.get(&TagKey::parse("delivered_on").unwrap()),
            Some(&TagValue::Date(AccountingDate::new(2026, 4, 15).unwrap()))
        );

        // 組み立てた TagSet はそのまま core の検証を通せる。
        catalog
            .schema()
            .validate(&tags, AccountType::Expense)
            .unwrap();
    }

    /// 線上の文字列 → `TagValue` → 線上の文字列 が往復すること
    /// （応答に載せる表記が入力と食い違わない）。
    #[test]
    fn tag_values_round_trip_through_the_wire_string_form() {
        let catalog = all_types_catalog();
        for (key, text) in [
            ("tax_category", "SALES_10"),
            ("invoice_reg_no", "T1234567890123"),
            ("business_ratio", "0.30"),
            ("delivered_on", "2026-04-15"),
        ] {
            let (_, value) = catalog.parse_value(key, text).unwrap();
            assert_eq!(tag_value_to_string(&value), text, "key = {key}");
        }
    }

    /// 実データ（`tags.yaml`）のキーから `value_type` を引き戻せること。
    /// `TagSchema` の公開 API だけでは不可能で、これが `TagCatalog` の存在理由。
    #[test]
    fn value_type_can_be_looked_up_for_every_bundled_key() {
        let catalog = TagCatalog::from_embedded(kaikei_jp_data::TAGS).unwrap();
        assert!(!catalog.defs().is_empty());
        for (key, def) in catalog.defs() {
            assert_eq!(catalog.value_type(key), Some(def.value_type));
        }
        assert_eq!(
            catalog.value_type(&TagKey::parse("tax_category").unwrap()),
            Some(TagValueType::Code)
        );
        assert_eq!(
            catalog.value_type(&TagKey::parse("business_ratio").unwrap()),
            Some(TagValueType::Decimal)
        );
        assert_eq!(catalog.value_type(&TagKey::parse("nope").unwrap()), None);
    }

    /// 未登録キーは黙って通らず、有効なキーを列挙したエラーになる
    /// （`CLAUDE.md` §4・§11）。
    #[test]
    fn parse_tag_set_rejects_unregistered_key_and_lists_valid_keys() {
        let catalog = all_types_catalog();
        let err = catalog
            .parse_tag_set([("tax_cat", "SALES_10")])
            .unwrap_err();
        match err {
            JpError::UnregisteredTagKey { key, valid } => {
                assert_eq!(key, "tax_cat");
                assert!(valid.contains("tax_category"), "valid = {valid}");
                assert!(valid.contains("business_ratio"), "valid = {valid}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    /// `TagKey` の形式を満たさない文字列も未登録キーとして扱う
    /// （次の手は同じ「登録済みの綴りに直す」）。
    #[test]
    fn parse_value_rejects_malformed_key_as_unregistered() {
        let catalog = all_types_catalog();
        let err = catalog.parse_value("TaxCategory", "SALES_10").unwrap_err();
        assert!(matches!(err, JpError::UnregisteredTagKey { .. }));
    }

    #[test]
    fn parse_value_rejects_value_that_does_not_match_the_declared_type() {
        let catalog = all_types_catalog();

        let err = catalog.parse_value("business_ratio", "３割").unwrap_err();
        match err {
            JpError::InvalidTagValue {
                key,
                value_type_label,
                reason,
                ..
            } => {
                assert_eq!(key, "business_ratio");
                assert_eq!(value_type_label, "小数");
                assert!(reason.contains("0.30"), "reason = {reason}");
            }
            other => panic!("unexpected error: {other:?}"),
        }

        let err = catalog
            .parse_value("delivered_on", "2026/04/15")
            .unwrap_err();
        match err {
            JpError::InvalidTagValue {
                value_type_label,
                reason,
                ..
            } => {
                assert_eq!(value_type_label, "日付");
                assert!(reason.contains("YYYY-MM-DD"), "reason = {reason}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    /// 表現できない桁数の小数は、丸めずにエラーにする
    /// （`Decimal::from_str` ではなく `from_str_exact` を使っている根拠）。
    #[test]
    fn parse_value_rejects_decimal_that_cannot_be_represented_without_rounding() {
        let catalog = all_types_catalog();
        let too_precise = format!("0.{}", "1".repeat(40));
        let err = catalog
            .parse_value("business_ratio", &too_precise)
            .unwrap_err();
        assert!(matches!(err, JpError::InvalidTagValue { .. }));
    }

    #[test]
    fn parse_value_rejects_empty_value() {
        let catalog = all_types_catalog();
        for text in ["", "   ", "\u{3000}"] {
            let err = catalog.parse_value("tax_category", text).unwrap_err();
            match err {
                JpError::InvalidTagValue { reason, .. } => {
                    assert!(reason.contains("空"), "reason = {reason}");
                }
                other => panic!("unexpected error: {other:?}"),
            }
        }
    }

    /// 入力に同じキーが2回現れたら、後勝ちで黙って上書きせずエラーにする。
    #[test]
    fn parse_tag_set_rejects_duplicate_key_in_input() {
        let catalog = all_types_catalog();
        let err = catalog
            .parse_tag_set([("tax_category", "SALES_10"), ("tax_category", "SALES_8")])
            .unwrap_err();
        match err {
            JpError::DuplicateTagKeyInInput { key } => assert_eq!(key, "tax_category"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    /// エラーに載る入力は上限で切られる（応答と監査ログが入力次第で
    /// 膨らまないこと）。
    #[test]
    fn error_messages_truncate_long_input() {
        let catalog = all_types_catalog();
        let long = "あ".repeat(500);
        let err = catalog.parse_value("business_ratio", &long).unwrap_err();
        let message = err.to_string();
        assert!(message.chars().count() < 300, "message = {message}");
        assert!(message.contains('…'), "message = {message}");
    }

    /// 空の `tags` は空の `TagSet` になる（エラーにしない）。
    #[test]
    fn parse_tag_set_accepts_an_empty_map() {
        let catalog = all_types_catalog();
        let tags = catalog
            .parse_tag_set(Vec::<(String, String)>::new())
            .unwrap();
        assert!(tags.is_empty());
    }

    /// `load_embedded`（`TagSchema` だけを返す既存の入口）と
    /// `TagCatalog::from_embedded` が同じ内容を返すこと。
    #[test]
    fn load_embedded_and_catalog_agree() {
        let schema = load_embedded(kaikei_jp_data::TAGS).unwrap();
        let catalog = TagCatalog::from_embedded(kaikei_jp_data::TAGS).unwrap();
        assert_eq!(format!("{schema:?}"), format!("{:?}", catalog.schema()));
        assert_eq!(catalog.label(), kaikei_jp_data::TAGS.label);
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
