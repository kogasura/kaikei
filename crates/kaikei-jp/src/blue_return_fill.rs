//! 当てはめ表に金額を埋める（[`fill`]）。
//!
//! [`crate::blue_return`] が読んだ表（どの科目がどの欄か）と、損益計算書の
//! [`Statement`] を突き合わせて、決算書の各欄の金額を出す。
//!
//! # 出すのはデータまで
//!
//! 戻り値は欄番号・欄名・金額の [`FilledForm`] であり、国税庁の様式を模した
//! 帳票ではない（`docs/10-report.md` §5、`CLAUDE.md` §10）。整形は
//! `kaikei-report` が行う。
//!
//! # 足りないものは黙って 0 にしない
//!
//! この module で最も危ないのは、**欄の金額が本当は不明なのに 0 が出る**
//! ことである。0 は「その欄に該当が無い」という正当な値でもあるため、
//! 読む人には区別がつかない。そこで次を全部エラーにする。
//!
//! - 計算式が実在しない欄を参照している
//! - 計算式が、まだ計算していない欄を参照している（前方参照）
//! - `from_input` が指す値が渡されていない
//! - 計算式が解釈できない
//!
//! 逆に「当てはめる科目はあるが、その科目が試算表に無い」のは 0 でよい
//! （その科目に取引が無かっただけである）。
//!
//! # 符号
//!
//! [`Statement`] の各行の金額は、その科目の自然な貸借の向きで正の値になって
//! いる（売上高も通信費も正）。決算書の欄も様式上そのまま正の数を書くため、
//! **符号の反転はしない**。差引や控除は計算式（`33-44` のような引き算）が
//! 表す。

use crate::blue_return::BlueReturnForm;
use crate::error::JpError;
use kaikei_core::{AccountCode, Money};
use kaikei_policy::Statement;
use std::collections::BTreeMap;

/// 金額を埋めた欄1つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilledField {
    /// 様式の丸番号。
    pub no: u32,
    /// 欄名。
    ///
    /// 様式に印字されている欄はその名前。**空欄の行は、そこに当てはめた
    /// 科目の名前**（利用者が様式に書き込む文字列にあたる）。当てはめる
    /// 科目が無い空欄では `None`。
    pub label: Option<String>,
    /// 金額。
    pub amount: Money,
}

/// 金額を埋めた決算書。
#[derive(Debug, Clone)]
pub struct FilledForm {
    /// 様式名。
    pub form: String,
    /// 様式のどの部分か。
    pub part: String,
    /// 欄（当てはめ表に書かれた順）。
    pub fields: Vec<FilledField>,
    /// 決算書のどの欄にも載らなかった科目。
    ///
    /// **呼び出し側はこれを利用者に見せること。** 空でないのは異常ではなく、
    /// 利用者が独自に足した科目や、載せないと決めた科目（受取利息など）が
    /// ここに出る。黙って捨てると、決算書に載らない金額があることに決算まで
    /// 気づけない。
    pub not_on_form: Vec<NotOnForm>,
}

/// 決算書に載らなかった科目1件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotOnForm {
    /// 科目コード。
    pub account: AccountCode,
    /// 科目名（損益計算書の行から取る）。
    pub label: String,
    /// 金額。
    pub amount: Money,
    /// 載らなかった理由。
    pub reason: NotOnFormReason,
}

/// 決算書に載らなかった理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotOnFormReason {
    /// 当てはめ表が「載せない」と決めている（理由付き）。
    Excluded(String),
    /// 当てはめ表にその科目が無い（利用者が足した科目など）。
    ///
    /// **これは「まだ当てはめていない」であって「載せなくてよい」ではない。**
    Unmapped,
}

/// 当てはめ表に金額を埋める。
///
/// `income_statement` は [`crate::statement::JpStatementPolicy`] が返した
/// 損益計算書。`inputs` は帳簿から決まらない値（青色申告特別控除額など）を
/// 名前で渡す。
///
/// # Errors
///
/// 金額が確定できない欄があれば [`JpError::InvalidChart`]。**確定できない欄を
/// 0 として通すことはしない**（モジュール doc「足りないものは黙って 0 に
/// しない」）。
pub fn fill(
    form: &BlueReturnForm,
    income_statement: &Statement,
    inputs: &BTreeMap<String, Money>,
) -> Result<FilledForm, JpError> {
    let invalid = |reason: String| JpError::InvalidChart {
        label: format!("{}（{}）", form.form(), form.part()),
        reason,
    };

    // 損益計算書の全行を科目コードで引けるようにする。
    let mut by_account: BTreeMap<&AccountCode, (&str, Money)> = BTreeMap::new();
    for section in &income_statement.sections {
        for line in &section.lines {
            by_account.insert(&line.account, (line.label.as_str(), line.amount));
        }
    }

    let zero = Money::from_minor(0, income_statement.total.currency());

    let mut amounts: BTreeMap<u32, Money> = BTreeMap::new();
    let mut fields = Vec::with_capacity(form.fields().len());
    let mut used: Vec<&AccountCode> = Vec::new();

    for field in form.fields() {
        let (amount, label) = if let Some(expr) = &field.computed {
            let amount = evaluate(expr, &amounts, field.no).map_err(&invalid)?;
            (amount, field.label.clone())
        } else if let Some(name) = &field.from_input {
            let amount = *inputs.get(name.as_str()).ok_or_else(|| {
                invalid(format!(
                    "欄 {} の金額は帳簿から決まらないため、\"{}\" を渡す必要があります。\
                     この欄を 0 として出すことはしません（0 は「該当なし」と\
                     区別がつかないため）",
                    field.no, name
                ))
            })?;
            (amount, field.label.clone())
        } else {
            // 科目から集める欄。当てはめた科目が試算表に無いのは 0 でよい
            // （その科目に取引が無かっただけ）。
            let mut total = zero;
            let mut names: Vec<&str> = Vec::new();
            for code in &field.accounts {
                used.push(code);
                if let Some((name, amount)) = by_account.get(code) {
                    total = total.add(amount).map_err(|source| {
                        invalid(format!("欄 {} の合算に失敗しました: {source}", field.no))
                    })?;
                    names.push(name);
                }
            }
            // 空欄の行は、そこに当てはめた科目の名前を欄名にする
            // （利用者が様式に書き込む文字列にあたる）。
            let label = match &field.label {
                Some(printed) => Some(printed.clone()),
                None if names.is_empty() => None,
                None => Some(names.join("・")),
            };
            (total, label)
        };

        amounts.insert(field.no, amount);
        fields.push(FilledField {
            no: field.no,
            label,
            amount,
        });
    }

    Ok(FilledForm {
        form: form.form().to_string(),
        part: form.part().to_string(),
        fields,
        not_on_form: collect_not_on_form(form, &by_account, &used),
    })
}

/// 決算書に載らなかった科目を集める。
fn collect_not_on_form(
    form: &BlueReturnForm,
    by_account: &BTreeMap<&AccountCode, (&str, Money)>,
    used: &[&AccountCode],
) -> Vec<NotOnForm> {
    let excluded: BTreeMap<&AccountCode, &str> = form
        .excluded()
        .iter()
        .map(|entry| (&entry.account, entry.reason.as_str()))
        .collect();

    by_account
        .iter()
        .filter(|(code, _)| !used.contains(code))
        .map(|(code, (label, amount))| NotOnForm {
            account: (*code).clone(),
            label: (*label).to_string(),
            amount: *amount,
            reason: match excluded.get(code) {
                Some(reason) => NotOnFormReason::Excluded((*reason).to_string()),
                None => NotOnFormReason::Unmapped,
            },
        })
        .collect()
}

/// 計算式を評価する。
///
/// 受け付けるのは様式に印字されている形だけ。
///
/// - `2+3` / `33+37-42` — 欄番号の加減
/// - `sum(8..31)` — 欄番号の範囲（両端を含む）の合計
///
/// 参照先は**すでに計算した欄**でなければならない。前方参照を許すと、
/// 表の並び順によって結果が変わる。
fn evaluate(expr: &str, amounts: &BTreeMap<u32, Money>, field_no: u32) -> Result<Money, String> {
    let expr = expr.trim();

    let lookup = |no: u32| -> Result<Money, String> {
        amounts.get(&no).copied().ok_or_else(|| {
            format!(
                "欄 {field_no} の計算式 \"{expr}\" が欄 {no} を参照していますが、\
                 その欄は当てはめ表に無いか、まだ計算していません（前方参照）。\
                 参照先が無い欄を 0 として計算することはしません"
            )
        })
    };

    if let Some(range) = expr.strip_prefix("sum(").and_then(|s| s.strip_suffix(')')) {
        let (from, to) = range.split_once("..").ok_or_else(|| {
            format!("欄 {field_no} の計算式 \"{expr}\" が sum(開始..終了) の形ではありません")
        })?;
        let from: u32 = from.trim().parse().map_err(|_| {
            format!("欄 {field_no} の計算式 \"{expr}\" の開始が欄番号ではありません")
        })?;
        let to: u32 = to.trim().parse().map_err(|_| {
            format!("欄 {field_no} の計算式 \"{expr}\" の終了が欄番号ではありません")
        })?;
        if from > to {
            return Err(format!(
                "欄 {field_no} の計算式 \"{expr}\" の範囲が逆です（{from} > {to}）"
            ));
        }

        let mut total = lookup(from)?;
        for no in (from + 1)..=to {
            total = total
                .add(&lookup(no)?)
                .map_err(|source| format!("欄 {field_no} の合算に失敗しました: {source}"))?;
        }
        return Ok(total);
    }

    // 加減算。先頭の項は符号なし（様式の印字がそうなっている）。
    let mut total: Option<Money> = None;
    let mut rest = expr;
    let mut op = '+';
    loop {
        let split = rest.find(['+', '-']);
        let (term, next) = match split {
            Some(index) => (
                &rest[..index],
                Some((rest.as_bytes()[index] as char, &rest[index + 1..])),
            ),
            None => (rest, None),
        };
        let no: u32 = term.trim().parse().map_err(|_| {
            format!(
                "欄 {field_no} の計算式 \"{expr}\" を解釈できません。\
                 受け付けるのは欄番号の加減（例: 33+37-42）と sum(開始..終了) です"
            )
        })?;
        let value = lookup(no)?;
        total = Some(match total {
            None => value,
            Some(current) => match op {
                '+' => current.add(&value),
                _ => current.sub(&value),
            }
            .map_err(|source| format!("欄 {field_no} の計算に失敗しました: {source}"))?,
        });
        match next {
            Some((next_op, remainder)) => {
                op = next_op;
                rest = remainder;
            }
            None => break,
        }
    }
    total.ok_or_else(|| format!("欄 {field_no} の計算式が空です"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blue_return;
    use kaikei_core::Currency;
    use kaikei_policy::{StatementLine, StatementSection};

    fn yen(minor: i128) -> Money {
        Money::from_minor(minor, Currency::JPY)
    }

    fn line(code: &str, label: &str, minor: i128) -> StatementLine {
        StatementLine {
            account: AccountCode::parse(code).unwrap(),
            label: label.to_string(),
            amount: yen(minor),
        }
    }

    fn form() -> BlueReturnForm {
        blue_return::load_embedded(kaikei_jp_data::STATEMENT_BLUE_RETURN_GENERAL).unwrap()
    }

    /// WeBanana.SP の 2026 年 1〜8 月の実績に近い損益計算書。
    fn income_statement() -> Statement {
        Statement {
            title: "損益計算書".to_string(),
            sections: vec![
                StatementSection {
                    title: "収益".to_string(),
                    lines: vec![
                        line("500", "売上高", 11_435_380),
                        line("530", "受取利息", 434),
                    ],
                    subtotal: yen(11_435_814),
                },
                StatementSection {
                    title: "費用".to_string(),
                    lines: vec![
                        line("602", "水道光熱費", 77_261),
                        line("603", "旅費交通費", 98_093),
                        line("604", "通信費", 439_852),
                        line("607", "損害保険料", 13_000),
                        line("609", "消耗品費", 145_057),
                        line("611", "福利厚生費", 2_968),
                        line("613", "外注工賃", 385_000),
                        line("615", "地代家賃", 1_845_720),
                        line("620", "支払手数料", 7_693),
                        line("621", "新聞図書費", 17_782),
                        line("622", "研修費", 4_784),
                        line("623", "会議費", 16_476),
                        line("625", "諸会費", 12_980),
                    ],
                    subtotal: yen(3_066_666),
                },
            ],
            total: yen(8_369_148),
        }
    }

    fn inputs(deduction: i128) -> BTreeMap<String, Money> {
        let mut map = BTreeMap::new();
        map.insert("blue_return_deduction".to_string(), yen(deduction));
        map
    }

    fn amount_of(filled: &FilledForm, no: u32) -> Money {
        filled
            .fields
            .iter()
            .find(|field| field.no == no)
            .unwrap_or_else(|| panic!("欄 {no} が無い"))
            .amount
    }

    // FL-1: **本命。** 実データで決算書の各欄が埋まり、所得金額まで通ること。
    #[test]
    fn it_fills_the_form_down_to_the_income_amount() {
        let filled = fill(&form(), &income_statement(), &inputs(650_000)).unwrap();

        // ① 売上（収入）金額。**受取利息 434 は入らない**（利子所得）。
        assert_eq!(amount_of(&filled, 1), yen(11_435_380));
        // ⑦ 差引金額（① − ⑥）。売上原価が無いので売上と同額。
        assert_eq!(amount_of(&filled, 7), yen(11_435_380));
        // ㉜ 経費の計。
        assert_eq!(amount_of(&filled, 32), yen(3_066_666));
        // ㉝ 差引金額（⑦ − ㉜）。
        assert_eq!(amount_of(&filled, 33), yen(8_368_714));
        // ㊸ 青色申告特別控除前の所得金額（㉝ + ㊲ − ㊷）。引当金は 0。
        assert_eq!(amount_of(&filled, 43), yen(8_368_714));
        // ㊺ 所得金額（㊸ − ㊹）。
        assert_eq!(amount_of(&filled, 45), yen(7_718_714));
    }

    // FL-2: 空欄の行には、当てはめた科目の名前が入る。
    #[test]
    fn a_blank_row_takes_its_label_from_the_account_mapped_to_it() {
        let filled = fill(&form(), &income_statement(), &inputs(650_000)).unwrap();

        let field = filled.fields.iter().find(|f| f.no == 25).unwrap();
        assert_eq!(field.label.as_deref(), Some("支払手数料"));
        assert_eq!(field.amount, yen(7_693));

        // 取引が無い科目を当てはめた空欄は、金額 0 で名前も出ない
        // （㉙ は車両費。この帳簿には取引が無い）。
        let unused = filled.fields.iter().find(|f| f.no == 29).unwrap();
        assert_eq!(unused.label, None);
        assert_eq!(unused.amount, yen(0));
    }

    // FL-3: **決算書に載らない科目を必ず持ち帰る。** 受取利息は「載せないと
    //       決めた」側、利用者が足した科目は「まだ当てはめていない」側。
    #[test]
    fn accounts_that_do_not_appear_on_the_form_are_reported_with_the_reason() {
        let mut statement = income_statement();
        statement.sections[1]
            .lines
            .push(line("630", "研究開発費", 50_000));

        let filled = fill(&form(), &statement, &inputs(650_000)).unwrap();

        let interest = filled
            .not_on_form
            .iter()
            .find(|entry| entry.account.as_str() == "530")
            .expect("受取利息が報告されるはず");
        assert_eq!(interest.amount, yen(434));
        assert!(matches!(interest.reason, NotOnFormReason::Excluded(_)));

        let added = filled
            .not_on_form
            .iter()
            .find(|entry| entry.account.as_str() == "630")
            .expect("利用者が足した科目が報告されるはず");
        assert_eq!(added.amount, yen(50_000));
        assert_eq!(added.reason, NotOnFormReason::Unmapped);
        assert_eq!(added.label, "研究開発費");

        // **決算書に載った科目は報告に出ない。** ここを見ていないと、
        // 「載らなかった科目」の判定が壊れて全科目が並んでも気づけない
        // （利用者は毎回大量の科目を見せられ、本当の未マッピングを見落とす）。
        let reported: Vec<&str> = filled
            .not_on_form
            .iter()
            .map(|entry| entry.account.as_str())
            .collect();
        assert_eq!(
            reported,
            vec!["530", "630"],
            "決算書のどこかの欄に載った科目は報告しないこと"
        );
    }

    // FL-4: 帳簿から決まらない値が渡されていなければ拒否する。
    //       **0 として通さない**——0 は「控除を受けない」という正当な値でも
    //       あるため、渡し忘れと区別がつかない。
    #[test]
    fn a_missing_user_supplied_input_is_rejected_not_defaulted_to_zero() {
        let err = fill(&form(), &income_statement(), &BTreeMap::new())
            .expect_err("控除額が無ければ拒否されるはず");

        let message = format!("{err}");
        assert!(message.contains("blue_return_deduction"), "{message}");
        assert!(message.contains("0 として出すことはしません"), "{message}");
    }

    // FL-5: 実在しない欄を参照する計算式は拒否する（黙って 0 にしない）。
    #[test]
    fn a_formula_referring_to_a_missing_field_is_rejected() {
        let source = r#"
version: 1
form: "test"
part: "test"
source: "test"
fields:
  - no: 1
    label: "売上"
    accounts: ["500"]
  - no: 2
    label: "計"
    computed: "1+99"
"#;
        let form = blue_return::load_from_str(source, "test").unwrap();

        let err = fill(&form, &income_statement(), &BTreeMap::new()).expect_err("拒否されるはず");

        let message = format!("{err}");
        assert!(message.contains("欄 99"), "{message}");
        assert!(
            message.contains("0 として計算することはしません"),
            "{message}"
        );
    }

    // FL-6: 前方参照（まだ計算していない欄）も拒否する。表の並び順で結果が
    //       変わるのを防ぐ。
    #[test]
    fn a_forward_reference_is_rejected() {
        let source = r#"
version: 1
form: "test"
part: "test"
source: "test"
fields:
  - no: 1
    label: "計"
    computed: "2"
  - no: 2
    label: "売上"
    accounts: ["500"]
"#;
        let form = blue_return::load_from_str(source, "test").unwrap();

        let err = fill(&form, &income_statement(), &BTreeMap::new()).expect_err("拒否されるはず");
        assert!(format!("{err}").contains("前方参照"), "{err}");
    }

    // FL-7: 解釈できない計算式は拒否する。
    #[test]
    fn an_unparsable_formula_is_rejected() {
        let source = r#"
version: 1
form: "test"
part: "test"
source: "test"
fields:
  - no: 1
    label: "売上"
    accounts: ["500"]
  - no: 2
    label: "変な式"
    computed: "1 * 2"
"#;
        let form = blue_return::load_from_str(source, "test").unwrap();

        let err = fill(&form, &income_statement(), &BTreeMap::new()).expect_err("拒否されるはず");
        assert!(format!("{err}").contains("解釈できません"), "{err}");
    }

    // FL-8: sum の範囲が逆なら拒否する（黙って 0 件の合計にしない）。
    #[test]
    fn a_reversed_sum_range_is_rejected() {
        let source = r#"
version: 1
form: "test"
part: "test"
source: "test"
fields:
  - no: 1
    label: "売上"
    accounts: ["500"]
  - no: 2
    label: "計"
    computed: "sum(5..1)"
"#;
        let form = blue_return::load_from_str(source, "test").unwrap();

        let err = fill(&form, &income_statement(), &BTreeMap::new()).expect_err("拒否されるはず");
        assert!(format!("{err}").contains("範囲が逆"), "{err}");
    }

    // FL-9: 控除額を変えれば所得金額が動く（控除の要件判定はしない）。
    #[test]
    fn the_deduction_comes_from_the_caller_not_from_the_book() {
        let with_650k = fill(&form(), &income_statement(), &inputs(650_000)).unwrap();
        let with_100k = fill(&form(), &income_statement(), &inputs(100_000)).unwrap();

        assert_eq!(amount_of(&with_650k, 45), yen(7_718_714));
        assert_eq!(amount_of(&with_100k, 45), yen(8_268_714));
    }
}
