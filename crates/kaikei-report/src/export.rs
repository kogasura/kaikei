//! 帳簿の全件 JSON エクスポート。
//!
//! `docs/03-database.md` §8、`docs/08-compliance.md` §8。
//!
//! # 何のための出力か
//!
//! **「このソフトが消えてもデータは残る」と言えるようにするため**である。
//! 帳簿書類は7年（欠損金の繰越があれば10年）保存が要る。PostgreSQL の
//! バージョンにも、このプロジェクトの存続にも依存しない形で残せる必要がある。
//!
//! そのため、この出力は次を満たす。
//!
//! - **欠けが無い。** 仕訳の全項目（明細・タグ・摘要・メモ・証憑参照・
//!   訂正の関係・記帳日時）を出す。表示用に丸めた値は入れない
//! - **それ自体で読める。** スキーマ版と、各項目が何かを説明する `_readme`
//!   を先頭に置く。7年後にコードが手元に無くても意味が取れるように
//! - **決定的。** 同じ帳簿からは同じバイト列が出る（並び順を固定する）。
//!   差分を取って「変わっていないこと」を確かめられる
//!
//! # 金額は文字列で出す
//!
//! JSON の数値は倍精度浮動小数点として読まれることがある（多くの実装が
//! そうする）。**金額を数値で書くと、読み直したときに 1 円ずれうる**。
//! 最小単位の整数を文字列で出し、通貨と小数桁数を添える。

use kaikei_core::{ChartOfAccounts, JournalEntry, Side, TagValue};
use serde_json::{json, Map, Value};

/// この出力のスキーマ版。**形を変えたら上げる。**
///
/// 読む側が「知らない版なら勝手に解釈しない」と判断できるようにする。
pub const SCHEMA_VERSION: u32 = 1;

/// 出力の先頭に置く説明。
///
/// **7年後にコードが手元に無くても意味が取れるように**、各項目が何かを
/// 日本語で書く。JSON の中に説明を混ぜるのは冗長だが、この出力の目的は
/// 「独立して読めること」なので、冗長さより読めることを取る。
const README: &[(&str, &str)] = &[
    ("この出力について", "会計帳簿の全件エクスポート。仕訳を1件も落とさずに出しています"),
    ("amount_minor", "金額。通貨の最小単位（日本円なら1円）の整数を文字列で書いています。JSONの数値にすると読み直しで誤差が出るため文字列です"),
    ("currency_minor_unit", "最小単位の小数桁数。日本円は0（1円が最小）"),
    ("side", "debit=借方、credit=貸方"),
    ("entry_no", "会計年度ごとの連番。年度をまたぐと1に戻ります"),
    ("reverses", "この仕訳が訂正している元の仕訳のID。訂正（逆仕訳）でなければ null"),
    ("recorded_at", "帳簿に記録した日時（UNIX時刻のナノ秒）。取引日ではありません"),
    ("tags", "明細に付けた分類。tax_category は消費税区分"),
];

/// 帳簿を JSON にする。
///
/// `entries` は出力したい仕訳、`chart` は勘定科目表。**科目表も一緒に出す**
/// ——科目コードだけ残っても、それが何の科目かが分からなければ帳簿として
/// 読めない。
///
/// 並びは呼び出し側が渡した順を保つ（`list_entries_in_period` は
/// `(取引日, 仕訳番号)` 昇順を約束している）。
pub fn to_json(entries: &[JournalEntry], chart: &ChartOfAccounts) -> String {
    let mut readme = Map::new();
    for (key, text) in README {
        readme.insert((*key).to_string(), json!(text));
    }

    let accounts: Vec<Value> = chart
        .iter()
        .map(|def| {
            json!({
                "code": def.code.as_str(),
                "name": def.name,
                "type": format!("{:?}", def.account_type),
                "parent": def.parent.as_ref().map(|code| code.as_str()),
                "postable": def.postable,
            })
        })
        .collect();

    let entries: Vec<Value> = entries.iter().map(entry_to_json).collect();

    let document = json!({
        "schema_version": SCHEMA_VERSION,
        "_readme": Value::Object(readme),
        // 件数を書いておくと、読む側が「途中で切れていないか」を確かめられる。
        "entry_count": entries.len(),
        "account_count": accounts.len(),
        "accounts": accounts,
        "entries": entries,
    });

    // `serde_json` のオブジェクトはキー順が挿入順（preserve_order 無しなら
    // BTreeMap 順）で決まる。どちらにせよ同じ入力からは同じ出力になる。
    serde_json::to_string_pretty(&document).unwrap_or_else(|error| {
        // ここで失敗するのは JSON にできない値がある場合だけで、
        // 上で組み立てているのは文字列・数値・真偽値・配列・オブジェクトのみ。
        // **黙って空文字を返さない**——欠けたことに気づけなくなる。
        panic!("帳簿を JSON にできませんでした（この出力は文字列と数値しか含まないため、通常は起こりません）: {error}")
    })
}

fn entry_to_json(entry: &JournalEntry) -> Value {
    let lines: Vec<Value> = entry
        .lines()
        .iter()
        .map(|line| {
            json!({
                "account": line.account().as_str(),
                "side": match line.side() {
                    Side::Debit => "debit",
                    Side::Credit => "credit",
                },
                "amount_minor": line.amount().minor().to_string(),
                "currency": line.amount().currency().code(),
                "currency_minor_unit": line.amount().currency().minor_unit(),
                "tags": tags_to_json(line.tags()),
                "memo": line.memo(),
            })
        })
        .collect();

    let document_refs: Vec<Value> = entry
        .document_refs()
        .iter()
        .map(|reference| json!(format!("{reference:?}")))
        .collect();

    json!({
        "id": entry_id_to_uuid(entry.id()),
        "entry_no": entry.entry_no().as_u32(),
        "fiscal_year": entry.fiscal_year(),
        "entry_date": entry.entry_date().to_iso_string(),
        "description": entry.description(),
        "lines": lines,
        "document_refs": document_refs,
        "reverses": entry.reverses().map(entry_id_to_uuid),
        "reverse_reason": entry.reverse_reason(),
        "recorded_at_unix_nanos": entry.recorded_at().as_unix_nanos().to_string(),
    })
}

/// 仕訳IDを UUID の表記にする。
///
/// `EntryId` は内部では 128 ビットの整数だが、DB の列も MCP の応答も UUID
/// 表記である。**同じ表記で出さないと、他の出力と突き合わせられない。**
fn entry_id_to_uuid(id: kaikei_core::EntryId) -> String {
    let value = id.as_u128();
    let hex = format!("{value:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn tags_to_json(tags: &kaikei_core::TagSet) -> Value {
    let mut map = Map::new();
    for (key, value) in tags.iter() {
        let rendered = match value {
            TagValue::Code(code) => json!({"type": "code", "value": code}),
            TagValue::Text(text) => json!({"type": "text", "value": text}),
            // 小数も文字列で出す（金額と同じ理由）。
            TagValue::Decimal(decimal) => json!({"type": "decimal", "value": decimal.to_string()}),
            TagValue::Date(date) => json!({"type": "date", "value": date.to_iso_string()}),
        };
        map.insert(key.as_str().to_string(), rendered);
    }
    Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_core::{
        AccountCode, AccountDef, AccountType, AccountingDate, Currency, EntryId, EntryNumber,
        FiscalYear, FixedClock, JournalLine, Money, NewEntry, PeriodGuard, PeriodStatus, TagDef,
        TagKey, TagSchema, TagSet, TagValueType, Timestamp,
    };

    struct AllOpen;
    impl PeriodGuard for AllOpen {
        fn status(&self, _date: AccountingDate) -> PeriodStatus {
            PeriodStatus::Open
        }
    }

    fn chart() -> ChartOfAccounts {
        ChartOfAccounts::new(vec![
            AccountDef {
                code: AccountCode::parse("110").unwrap(),
                name: "普通預金".to_string(),
                account_type: AccountType::Asset,
                parent: None,
                postable: true,
            },
            AccountDef {
                code: AccountCode::parse("500").unwrap(),
                name: "売上高".to_string(),
                account_type: AccountType::Revenue,
                parent: None,
                postable: true,
            },
        ])
        .unwrap()
    }

    fn schema() -> TagSchema {
        TagSchema::new(vec![(
            TagKey::parse("tax_category").unwrap(),
            TagDef {
                value_type: TagValueType::Code,
                aggregatable: true,
                required_for: Vec::new(),
            },
        )])
    }

    fn sample_entry() -> JournalEntry {
        let mut tags = TagSet::new();
        tags.insert(
            TagKey::parse("tax_category").unwrap(),
            kaikei_core::TagValue::Code("SALES_10".to_string()),
        );
        JournalEntry::new(
            NewEntry {
                id: EntryId::new(1),
                entry_no: EntryNumber::new(1),
                entry_date: AccountingDate::new(2026, 6, 15).unwrap(),
                description: "ビーテック 5月分 請求".to_string(),
                lines: vec![
                    JournalLine::new(
                        AccountCode::parse("110").unwrap(),
                        Side::Debit,
                        Money::from_minor(550_000, Currency::JPY),
                        TagSet::new(),
                        Some("入金".to_string()),
                    )
                    .unwrap(),
                    JournalLine::new(
                        AccountCode::parse("500").unwrap(),
                        Side::Credit,
                        Money::from_minor(550_000, Currency::JPY),
                        tags,
                        None,
                    )
                    .unwrap(),
                ],
                document_refs: Vec::new(),
            },
            &FiscalYear::calendar_year(2026),
            &chart(),
            &schema(),
            &AllOpen,
            &FixedClock(Timestamp::from_unix_nanos(1_700_000_000_000_000)),
        )
        .unwrap()
    }

    fn parsed() -> Value {
        serde_json::from_str(&to_json(&[sample_entry()], &chart())).unwrap()
    }

    // EX-1: スキーマ版と件数が入る（読む側が途中で切れていないか確かめられる）。
    #[test]
    fn the_export_states_its_schema_version_and_counts() {
        let value = parsed();

        assert_eq!(value["schema_version"], json!(SCHEMA_VERSION));
        assert_eq!(value["entry_count"], json!(1));
        assert_eq!(value["account_count"], json!(2));
    }

    // EX-2: **本命。** 金額を JSON の数値にしない。
    //
    //       数値で書くと倍精度で読まれて 1 円ずれうる。最小単位の整数を
    //       文字列で出し、通貨と小数桁数を添える。
    #[test]
    fn amounts_are_strings_not_json_numbers() {
        let value = parsed();
        let line = &value["entries"][0]["lines"][0];

        assert_eq!(line["amount_minor"], json!("550000"));
        assert!(
            line["amount_minor"].is_string(),
            "数値にすると読み直しで誤差が出る"
        );
        assert_eq!(line["currency"], json!("JPY"));
        assert_eq!(line["currency_minor_unit"], json!(0));
    }

    // EX-3: 仕訳の項目が欠けない。
    #[test]
    fn an_entry_keeps_every_field() {
        let value = parsed();
        let entry = &value["entries"][0];

        assert_eq!(entry["entry_no"], json!(1));
        assert_eq!(entry["fiscal_year"], json!(2026));
        assert_eq!(entry["entry_date"], json!("2026-06-15"));
        assert_eq!(entry["description"], json!("ビーテック 5月分 請求"));
        assert_eq!(entry["reverses"], Value::Null);
        assert!(entry["id"].is_string());
        assert!(entry["recorded_at_unix_nanos"].is_string());
        assert_eq!(entry["lines"].as_array().unwrap().len(), 2);
    }

    // EX-4: 明細のタグ・メモが残る。**税区分が落ちると消費税が再現できない。**
    #[test]
    fn line_tags_and_memos_survive() {
        let value = parsed();
        let lines = value["entries"][0]["lines"].as_array().unwrap();

        assert_eq!(lines[0]["memo"], json!("入金"));
        assert_eq!(lines[1]["tags"]["tax_category"]["value"], json!("SALES_10"));
        assert_eq!(lines[1]["tags"]["tax_category"]["type"], json!("code"));
    }

    // EX-5: 勘定科目表も一緒に出る。
    //
    //       科目コードだけ残っても、それが何の科目かが分からなければ
    //       帳簿として読めない。
    #[test]
    fn the_chart_of_accounts_is_exported_too() {
        let value = parsed();
        let accounts = value["accounts"].as_array().unwrap();

        let deposit = accounts
            .iter()
            .find(|a| a["code"] == json!("110"))
            .expect("科目が出ているはず");
        assert_eq!(deposit["name"], json!("普通預金"));
        assert_eq!(deposit["type"], json!("Asset"));
    }

    // EX-6: 説明が入る（7年後にコードが無くても意味が取れるように）。
    #[test]
    fn the_export_explains_itself() {
        let value = parsed();

        let readme = value["_readme"].as_object().unwrap();
        assert!(readme.contains_key("amount_minor"), "{readme:?}");
        assert!(readme.contains_key("side"), "{readme:?}");
        assert!(
            readme["amount_minor"].as_str().unwrap().contains("文字列"),
            "なぜ文字列なのかを書くこと"
        );
    }

    // EX-7: **本命。** 同じ帳簿からは同じバイト列が出る。
    //
    //       差分を取って「変わっていないこと」を確かめられる。
    #[test]
    fn the_export_is_deterministic() {
        let first = to_json(&[sample_entry()], &chart());
        let second = to_json(&[sample_entry()], &chart());

        assert_eq!(first, second);
    }

    // EX-8: 仕訳が0件でも、形の揃った出力を返す（空文字にしない）。
    #[test]
    fn an_empty_book_still_produces_a_well_formed_export() {
        let value: Value = serde_json::from_str(&to_json(&[], &chart())).unwrap();

        assert_eq!(value["entry_count"], json!(0));
        assert_eq!(value["entries"].as_array().unwrap().len(), 0);
        assert_eq!(value["account_count"], json!(2), "科目表は出す");
    }
}
