//! 仕訳化ルールを YAML から読む（`docs/05-csv-import.md` §6）。
//!
//! ルールそのもの（照合と仕訳の組み立て）は
//! [`kaikei_app::journalize`] が持つ。ここが担うのは**外の書式から中の型への
//! 翻訳だけ**である。
//!
//! # なぜ `kaikei-app` に YAML を持ち込まないか
//!
//! `kaikei-app` は列挙型と文字列の対応を自分で持ち（`wire.rs`）、serde に
//! 依存しない作りになっている。ルールの書式が変わっても中の型が揺れないよう、
//! 読み取りは端（この CLI）に置く。

use kaikei_app::journalize::{DescriptionPattern, JournalizeRule};
use kaikei_app::ports::ImportDirection;
use kaikei_core::AccountCode;
use serde::Deserialize;

/// YAML に書く1件のルール。
///
/// **未知のキーを拒否する**（`deny_unknown_fields`）。`acount:` のような
/// 打ち間違いを黙って無視すると、既定の科目で仕訳が提案されて「なぜか
/// 違う科目になる」になる。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleDoc {
    id: String,
    /// 評価の順。小さいほど先。
    #[serde(default = "default_priority")]
    priority: i32,
    /// この取り込み元にだけ適用する。
    #[serde(default)]
    source: Option<String>,
    /// `in` / `out`。省略すると入金・出金の両方。
    #[serde(default)]
    direction: Option<String>,
    /// 摘要の条件。
    #[serde(rename = "match")]
    pattern: PatternDoc,
    #[serde(default)]
    amount_min: Option<i64>,
    #[serde(default)]
    amount_max: Option<i64>,
    /// 立てる科目。
    account: String,
    /// 相手科目（口座）。
    counter_account: String,
    #[serde(default)]
    tax_category: Option<String>,
    /// 取引先コード（`CP0001` など）。
    #[serde(default)]
    counterparty: Option<String>,
    #[serde(default = "default_active")]
    active: bool,
}

/// 摘要の条件。**1つだけ書く。**
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PatternDoc {
    #[serde(default)]
    contains: Option<String>,
    #[serde(default)]
    starts_with: Option<String>,
    #[serde(default)]
    equals: Option<String>,
}

fn default_priority() -> i32 {
    100
}

fn default_active() -> bool {
    true
}

/// YAML を読んでルールにする。
///
/// # Errors
///
/// 書式が読めない場合、科目コードが不正な場合、条件の書き方が不正な場合は
/// 理由を返す。**IDの重複も拒否する**——同じ名前のルールが2つあると、
/// 「どちらが当たったか」を人に見せても区別が付かない。
pub fn load_rules(yaml: &str) -> Result<Vec<JournalizeRule>, String> {
    let docs: Vec<RuleDoc> = serde_norway::from_str(yaml)
        .map_err(|error| format!("ルールを読めませんでした: {error}"))?;

    let mut rules = Vec::with_capacity(docs.len());
    let mut seen: Vec<String> = Vec::new();
    for doc in docs {
        if seen.contains(&doc.id) {
            return Err(format!("ルールのIDが重複しています: {}", doc.id));
        }
        seen.push(doc.id.clone());
        rules.push(to_rule(doc)?);
    }
    Ok(rules)
}

fn to_rule(doc: RuleDoc) -> Result<JournalizeRule, String> {
    let id = doc.id;
    let account = parse_code(&doc.account, &id, "account")?;
    let counter_account = parse_code(&doc.counter_account, &id, "counter_account")?;
    let direction = doc
        .direction
        .as_deref()
        .map(|text| parse_direction(text, &id))
        .transpose()?;
    let pattern = to_pattern(doc.pattern, &id)?;

    // **範囲が逆なら止める。** min > max は何にも当たらないルールであり、
    // 書いた人は「当たるはず」と思っている。黙って通すと、なぜ自動化されない
    // のかが分からないまま残る。
    if let (Some(min), Some(max)) = (doc.amount_min, doc.amount_max) {
        if min > max {
            return Err(format!(
                "ルール {id}: 金額の範囲が逆です（amount_min={min} > amount_max={max}）"
            ));
        }
    }

    Ok(JournalizeRule {
        id,
        priority: doc.priority,
        source: doc.source,
        direction,
        pattern,
        amount_min: doc.amount_min,
        amount_max: doc.amount_max,
        account,
        counter_account,
        tax_category: doc.tax_category,
        counterparty: doc.counterparty,
        active: doc.active,
    })
}

fn to_pattern(doc: PatternDoc, id: &str) -> Result<DescriptionPattern, String> {
    let given: Vec<DescriptionPattern> = [
        doc.contains.map(DescriptionPattern::Contains),
        doc.starts_with.map(DescriptionPattern::StartsWith),
        doc.equals.map(DescriptionPattern::Equals),
    ]
    .into_iter()
    .flatten()
    .collect();

    match given.len() {
        1 => Ok(given.into_iter().next().expect("1件ある")),
        // **2つ書かれていたら止める。** 片方を黙って捨てると、書いた人が
        // 意図した条件と違うルールが動く。
        0 => Err(format!(
            "ルール {id}: match に contains / starts_with / equals のどれかを書いてください"
        )),
        _ => Err(format!(
            "ルール {id}: match に書けるのは contains / starts_with / equals のうち1つだけです"
        )),
    }
}

fn parse_direction(text: &str, id: &str) -> Result<ImportDirection, String> {
    match text {
        "in" => Ok(ImportDirection::In),
        "out" => Ok(ImportDirection::Out),
        other => Err(format!(
            "ルール {id}: direction は in / out で書いてください（受け取った値: {other}）"
        )),
    }
}

fn parse_code(text: &str, id: &str, field: &str) -> Result<AccountCode, String> {
    AccountCode::parse(text).map_err(|error| format!("ルール {id} の {field} が不正です: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE_RULE: &str = "\
- id: amazon
  match:
    contains: アマゾン
  account: \"500\"
  counter_account: \"100\"
";

    #[test]
    fn a_minimal_rule_gets_sensible_defaults() {
        let rules = load_rules(ONE_RULE).unwrap();

        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].id, "amazon");
        assert_eq!(rules[0].priority, 100);
        assert!(rules[0].active, "既定で有効");
        assert_eq!(rules[0].direction, None, "既定では入金・出金の両方");
        assert_eq!(
            rules[0].pattern,
            DescriptionPattern::Contains("アマゾン".to_string())
        );
    }

    #[test]
    fn every_field_can_be_written() {
        let yaml = "\
- id: konbini
  priority: 10
  source: mizuho
  direction: out
  match:
    starts_with: ｾﾌﾞﾝ
  amount_min: 100
  amount_max: 5000
  account: \"501\"
  counter_account: \"100\"
  tax_category: TAXABLE_PURCHASE_10_QUALIFIED
  counterparty: CP0001
  active: false
";
        let rules = load_rules(yaml).unwrap();
        let rule = &rules[0];

        assert_eq!(rule.priority, 10);
        assert_eq!(rule.source.as_deref(), Some("mizuho"));
        assert_eq!(rule.direction, Some(ImportDirection::Out));
        assert_eq!(rule.amount_min, Some(100));
        assert_eq!(rule.amount_max, Some(5_000));
        assert_eq!(
            rule.tax_category.as_deref(),
            Some("TAXABLE_PURCHASE_10_QUALIFIED")
        );
        assert_eq!(rule.counterparty.as_deref(), Some("CP0001"));
        assert!(!rule.active);
    }

    /// **本命。** キーの打ち間違いを黙って無視しない。
    ///
    /// 無視すると既定の科目で仕訳が提案され、「なぜか違う科目になる」になる。
    #[test]
    fn a_misspelled_key_is_rejected() {
        let yaml = "\
- id: amazon
  match:
    contains: アマゾン
  acount: \"500\"
  account: \"500\"
  counter_account: \"100\"
";
        let error = load_rules(yaml).unwrap_err();
        assert!(error.contains("acount"), "{error}");
    }

    /// **本命。** 条件を2つ書いたら止める。
    ///
    /// 片方を黙って捨てると、書いた人が意図した条件と違うルールが動く。
    #[test]
    fn writing_two_conditions_is_rejected_instead_of_picking_one() {
        let yaml = "\
- id: amazon
  match:
    contains: アマゾン
    starts_with: カ)
  account: \"500\"
  counter_account: \"100\"
";
        let error = load_rules(yaml).unwrap_err();
        assert!(error.contains("1つだけ"), "{error}");
    }

    #[test]
    fn writing_no_condition_is_rejected() {
        let yaml = "\
- id: amazon
  match: {}
  account: \"500\"
  counter_account: \"100\"
";
        let error = load_rules(yaml).unwrap_err();
        assert!(error.contains("contains"), "{error}");
    }

    /// **本命。** IDが重複していたら止める。
    ///
    /// 同じ名前のルールが2つあると、どちらが当たったかを人に見せても
    /// 区別が付かない。
    #[test]
    fn duplicate_ids_are_rejected() {
        let yaml = format!("{ONE_RULE}{ONE_RULE}");
        let error = load_rules(&yaml).unwrap_err();
        assert!(error.contains("重複"), "{error}");
        assert!(error.contains("amazon"), "{error}");
    }

    /// 金額の範囲が逆なら止める。
    ///
    /// 何にも当たらないルールであり、書いた人は「当たるはず」と思っている。
    #[test]
    fn a_reversed_amount_range_is_rejected() {
        let yaml = "\
- id: amazon
  match:
    contains: アマゾン
  amount_min: 5000
  amount_max: 100
  account: \"500\"
  counter_account: \"100\"
";
        let error = load_rules(yaml).unwrap_err();
        assert!(error.contains("逆"), "{error}");
    }

    #[test]
    fn an_unknown_direction_is_rejected() {
        let yaml = "\
- id: amazon
  direction: 入金
  match:
    contains: アマゾン
  account: \"500\"
  counter_account: \"100\"
";
        let error = load_rules(yaml).unwrap_err();
        assert!(error.contains("in / out"), "{error}");
    }

    #[test]
    fn a_bad_account_code_names_the_rule_and_the_field() {
        let yaml = "\
- id: amazon
  match:
    contains: アマゾン
  account: \"\"
  counter_account: \"100\"
";
        let error = load_rules(yaml).unwrap_err();
        assert!(error.contains("amazon"), "{error}");
        assert!(error.contains("account"), "{error}");
    }

    #[test]
    fn an_empty_file_has_no_rules() {
        assert_eq!(load_rules("[]").unwrap().len(), 0);
    }
}
