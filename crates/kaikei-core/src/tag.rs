//! 仕訳明細に付く分類情報（タグ）。
//!
//! **設計の急所。** 消費税区分などの「意味」を core に持ち込まずに運ぶための仕組み。
//! `TagSet` は core が中身を解釈しない不透明な袋だが、`TagSchema` による
//! 形式（未登録キー・型・必須）検証は core の責務にする（`DECISIONS.md` D-004）。
//! `tax_category` のような具体的なキー名は core の実装に一切ハードコードしない。

use crate::account::AccountType;
use crate::error::CoreError;
use crate::period::AccountingDate;
use std::collections::BTreeMap;

/// タグのキー。
///
/// 中身の意味（`tax_category` が消費税区分であること等）に core は関与しない。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TagKey(String);

impl TagKey {
    /// snake_case のタグキーを解釈する。1〜64文字。
    ///
    /// 受理する文法（概略）:
    /// `[a-z] ([a-z0-9] | _[a-z0-9])*`
    ///
    /// - 使える文字は英小文字・数字・アンダースコアのみ（大文字・非ASCII文字は不可）
    /// - 先頭は英小文字でなければならない（数字・アンダースコア始まりは不可）
    /// - 末尾はアンダースコアにできない
    /// - アンダースコアを連続させられない（`__` は不可）
    /// - 全体の文字数は1〜64文字
    pub fn parse(s: &str) -> Result<Self, CoreError> {
        let invalid = || CoreError::InvalidValue {
            reason: format!(
                "タグキーは snake_case で1〜64文字である必要があります\
                 （先頭は英小文字、以降は英小文字・数字・アンダースコア、\
                 先頭/末尾/連続のアンダースコアは不可）: \"{s}\""
            ),
        };

        let chars: Vec<char> = s.chars().collect();
        if chars.is_empty() || chars.len() > 64 {
            return Err(invalid());
        }
        if !chars
            .iter()
            .all(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        {
            return Err(invalid());
        }
        if !chars[0].is_ascii_lowercase() {
            return Err(invalid());
        }
        if *chars.last().expect("空文字列は上で弾いている") == '_' {
            return Err(invalid());
        }
        if chars.windows(2).any(|w| w[0] == '_' && w[1] == '_') {
            return Err(invalid());
        }
        Ok(TagKey(s.to_string()))
    }

    /// タグキーの文字列表現を返す。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// タグの値。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagValue {
    /// コード値（例: 区分コード）。
    Code(String),
    /// 自由記述のテキスト。
    Text(String),
    /// 小数値（例: 按分比率）。
    Decimal(rust_decimal::Decimal),
    /// 日付。
    Date(AccountingDate),
}

impl TagValue {
    /// この値が持つ `TagValueType` を返す。
    fn value_type(&self) -> TagValueType {
        match self {
            TagValue::Code(_) => TagValueType::Code,
            TagValue::Text(_) => TagValueType::Text,
            TagValue::Decimal(_) => TagValueType::Decimal,
            TagValue::Date(_) => TagValueType::Date,
        }
    }
}

/// タグの値の型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagValueType {
    /// コード値。
    Code,
    /// テキスト。
    Text,
    /// 小数値。
    Decimal,
    /// 日付。
    Date,
}

impl TagValueType {
    /// 値の種類の日本語ラベルを返す（コード・テキスト・小数・日付）。
    ///
    /// エラーメッセージを人間可読にするために使う。`CoreError` の `Debug`
    /// 表示（バリアント名がそのまま出る英語表記）を避けるためのもの。
    pub fn label_ja(&self) -> &'static str {
        match self {
            TagValueType::Code => "コード",
            TagValueType::Text => "テキスト",
            TagValueType::Decimal => "小数",
            TagValueType::Date => "日付",
        }
    }
}

/// 仕訳明細に付く分類情報の袋。core は意味を解釈しない。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TagSet(BTreeMap<TagKey, TagValue>);

impl TagSet {
    /// 空のタグ集合を作る。
    pub fn new() -> Self {
        TagSet(BTreeMap::new())
    }

    /// タグを追加する。同じキーが既にあれば置き換え、古い値を返す。
    pub fn insert(&mut self, key: TagKey, value: TagValue) -> Option<TagValue> {
        self.0.insert(key, value)
    }

    /// キーに対応する値を取得する。
    pub fn get(&self, key: &TagKey) -> Option<&TagValue> {
        self.0.get(key)
    }

    /// 全タグを巡回する。順序はキーの昇順で決定的。
    pub fn iter(&self) -> impl Iterator<Item = (&TagKey, &TagValue)> {
        self.0.iter()
    }

    /// タグが1つも無いかどうか。
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// タグ定義。`kaikei-jp-data/tags.yaml` に対応する。
#[derive(Debug, Clone)]
pub struct TagDef {
    /// 値の型。
    pub value_type: TagValueType,
    /// 集計軸（`group_by`）として使えるか。
    pub aggregatable: bool,
    /// この科目種別の明細では必須であることを表す。
    pub required_for: Vec<AccountType>,
}

/// タグのスキーマ。`kaikei-jp` が提供し、core が検証に使う。
#[derive(Debug, Clone)]
pub struct TagSchema {
    defs: BTreeMap<TagKey, TagDef>,
}

impl TagSchema {
    /// タグ定義の一覧からスキーマを作る。
    pub fn new(defs: Vec<(TagKey, TagDef)>) -> Self {
        TagSchema {
            defs: defs.into_iter().collect(),
        }
    }

    /// 何も登録されていない空のスキーマを作る。
    pub fn empty() -> Self {
        TagSchema {
            defs: BTreeMap::new(),
        }
    }

    /// タグ集合がこのスキーマに適合するか検証する。
    ///
    /// 以下を検出する:
    /// 1. 未登録キー → `CoreError::UnknownTagKey`
    /// 2. 型不一致 → `CoreError::TagTypeMismatch`
    /// 3. `account_type` に対して必須のキーが欠落 → `CoreError::MissingRequiredTag`
    pub fn validate(&self, tags: &TagSet, account_type: AccountType) -> Result<(), CoreError> {
        for (key, value) in tags.iter() {
            let def = self.defs.get(key).ok_or_else(|| CoreError::UnknownTagKey {
                key: key.as_str().to_string(),
            })?;
            let actual = value.value_type();
            if actual != def.value_type {
                return Err(CoreError::TagTypeMismatch {
                    key: key.as_str().to_string(),
                    expected: def.value_type,
                });
            }
        }

        for (key, def) in &self.defs {
            if def.required_for.contains(&account_type) && tags.get(key).is_none() {
                return Err(CoreError::MissingRequiredTag {
                    key: key.as_str().to_string(),
                    account_type,
                });
            }
        }

        Ok(())
    }

    /// 指定したキーが集計軸として使えるか。未登録キーは `false`。
    pub fn is_aggregatable(&self, key: &TagKey) -> bool {
        self.defs.get(key).is_some_and(|def| def.aggregatable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> TagKey {
        TagKey::parse(s).unwrap()
    }

    // T-01
    #[test]
    fn tag_key_parse_snake_case_succeeds() {
        assert!(TagKey::parse("tax_category").is_ok());
    }

    // T-02
    #[test]
    fn tag_key_parse_upper_camel_case_is_error() {
        assert!(TagKey::parse("TaxCategory").is_err());
    }

    // T-03
    #[test]
    fn tag_key_parse_empty_is_error() {
        assert!(TagKey::parse("").is_err());
    }

    // 拒否ケース: 先頭アンダースコア
    #[test]
    fn tag_key_parse_leading_underscore_is_error() {
        assert!(TagKey::parse("_foo").is_err());
    }

    // 拒否ケース: 末尾アンダースコア
    #[test]
    fn tag_key_parse_trailing_underscore_is_error() {
        assert!(TagKey::parse("foo_").is_err());
    }

    // 拒否ケース: 連続アンダースコア
    #[test]
    fn tag_key_parse_consecutive_underscore_is_error() {
        assert!(TagKey::parse("foo__bar").is_err());
    }

    // 拒否ケース: 先頭が数字
    #[test]
    fn tag_key_parse_leading_digit_is_error() {
        assert!(TagKey::parse("1foo").is_err());
    }

    // 拒否ケース: マルチバイト文字を含む
    #[test]
    fn tag_key_parse_multibyte_characters_is_error() {
        assert!(TagKey::parse("タグ").is_err());
        assert!(TagKey::parse("tag_タグ").is_err());
    }

    // 拒否ケース: 65文字
    #[test]
    fn tag_key_parse_65_chars_is_error() {
        let s = "a".repeat(65);
        assert!(TagKey::parse(&s).is_err());
    }

    // 境界値: 64文字は許容される
    #[test]
    fn tag_key_parse_64_chars_succeeds() {
        let s = "a".repeat(64);
        assert!(TagKey::parse(&s).is_ok());
    }

    // T-04
    #[test]
    fn tag_schema_validate_unknown_key_is_error() {
        let schema = TagSchema::empty();
        let mut tags = TagSet::new();
        tags.insert(key("unregistered"), TagValue::Text("x".to_string()));
        assert!(matches!(
            schema.validate(&tags, AccountType::Expense),
            Err(CoreError::UnknownTagKey { .. })
        ));
    }

    // T-05
    #[test]
    fn tag_schema_validate_type_mismatch_is_error() {
        let schema = TagSchema::new(vec![(
            key("tax_category"),
            TagDef {
                value_type: TagValueType::Code,
                aggregatable: true,
                required_for: vec![],
            },
        )]);
        let mut tags = TagSet::new();
        tags.insert(
            key("tax_category"),
            TagValue::Decimal(rust_decimal::Decimal::ONE),
        );
        assert!(matches!(
            schema.validate(&tags, AccountType::Expense),
            Err(CoreError::TagTypeMismatch { .. })
        ));
    }

    // T-06
    #[test]
    fn tag_schema_validate_missing_required_tag_is_error() {
        let schema = TagSchema::new(vec![(
            key("tax_category"),
            TagDef {
                value_type: TagValueType::Code,
                aggregatable: true,
                required_for: vec![AccountType::Expense],
            },
        )]);
        let tags = TagSet::new();
        assert!(matches!(
            schema.validate(&tags, AccountType::Expense),
            Err(CoreError::MissingRequiredTag { .. })
        ));
    }

    // T-07
    #[test]
    fn tag_schema_validate_missing_tag_not_required_for_account_type_succeeds() {
        let schema = TagSchema::new(vec![(
            key("tax_category"),
            TagDef {
                value_type: TagValueType::Code,
                aggregatable: true,
                required_for: vec![AccountType::Expense],
            },
        )]);
        let tags = TagSet::new();
        assert!(schema.validate(&tags, AccountType::Asset).is_ok());
    }

    // T-08
    #[test]
    fn tag_schema_empty_validate_empty_tag_set_succeeds() {
        let schema = TagSchema::empty();
        let tags = TagSet::new();
        assert!(schema.validate(&tags, AccountType::Expense).is_ok());
    }

    // T-09
    #[test]
    fn tag_schema_empty_validate_any_key_is_error() {
        let schema = TagSchema::empty();
        let mut tags = TagSet::new();
        tags.insert(key("anything"), TagValue::Text("x".to_string()));
        assert!(matches!(
            schema.validate(&tags, AccountType::Expense),
            Err(CoreError::UnknownTagKey { .. })
        ));
    }

    // T-10
    #[test]
    fn tag_schema_is_aggregatable_matches_declaration() {
        let schema = TagSchema::new(vec![
            (
                key("counterparty"),
                TagDef {
                    value_type: TagValueType::Code,
                    aggregatable: true,
                    required_for: vec![],
                },
            ),
            (
                key("business_ratio"),
                TagDef {
                    value_type: TagValueType::Decimal,
                    aggregatable: false,
                    required_for: vec![],
                },
            ),
        ]);
        assert!(schema.is_aggregatable(&key("counterparty")));
        assert!(!schema.is_aggregatable(&key("business_ratio")));
        assert!(!schema.is_aggregatable(&key("unregistered")));
    }
}
