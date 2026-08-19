//! 青色申告決算書の貸借対照表への当てはめ（[`BalanceSheetForm`] / [`fill`]）。
//!
//! `docs/10-report.md` §5。同梱の当てはめ表は
//! `kaikei_jp_data::STATEMENT_BLUE_RETURN_GENERAL_BS`。
//!
//! # 損益計算書と構造が違う
//!
//! 損益計算書（[`crate::blue_return`]）は丸番号の欄が並ぶ1列の表だが、
//! 貸借対照表は**番号が無く、期首と期末の2列**を持つ。行の並びが意味を持つ
//! ので、この表は様式の印字順をそのまま保つ。
//!
//! # 2つの試算表を受け取る
//!
//! 期首列と期末列で別の残高が要るため、[`fill`] は貸借対照表を2つ
//! （期首時点・期末時点）受け取る。**どちらも「その時点までの累計」**で
//! なければならない——会計年度中の増減を渡すと、期首残高を含まない
//! 貸借対照表になる（`usecase::statements` のモジュール doc）。
//!
//! # 貸借が合うかを必ず検算する
//!
//! 様式の書き方が「損益計算書と貸借対照表の青色申告特別控除前の所得金額は、
//! 必ず一致します。一致しない場合には、記帳誤りや計算誤りがあると思われます」
//! と明記している。[`FilledBalanceSheet::imbalance`] が資産合計と負債・資本
//! 合計の差を返すので、**呼び出し側はこれを利用者に見せること**。
//!
//! 差が出る典型は、損益計算書から除いた収益（受取利息など）の相手科目が
//! 資産に残っている場合である。除くと決めた収益は、帳簿の側で事業主借へ
//! 振り替えないと貸借対照表が合わない。

use crate::error::JpError;
use kaikei_core::{AccountCode, Money};
use kaikei_jp_data::EmbeddedYaml;
use kaikei_policy::Statement;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// この crate が読める唯一のスキーマ版（`chart.rs` と同じ方針）。
const SUPPORTED_VERSION: u32 = 1;

/// 様式の行1つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormRow {
    /// 様式に印字されている科目名。空欄の行では `None`。
    pub label: Option<String>,
    /// この行に足す勘定科目。
    pub accounts: Vec<AccountCode>,
    /// 期首欄を記入しない（様式で斜線が引かれている行）。
    pub omit_opening: bool,
    /// 期首と期末に同じ金額を入れる（様式の書き方が指示している行）。
    pub same_in_both_columns: bool,
    /// 損益計算書の欄から転記する行（青色申告特別控除前の所得金額）。
    pub from_income_statement: Option<u32>,
    /// 区分の合計行。
    pub total: bool,
    /// 残高の符号を反転する。
    ///
    /// 試算表は科目をその**科目種別の自然な貸借の向き**で返す。様式が
    /// その科目を反対側の区分に置いている場合（事業主貸は純資産科目だが
    /// 資産の部にある）、反転しないと区分の合計が狂う。
    pub negate: bool,
}

/// 様式の区分（資産の部 / 負債・資本の部）。
#[derive(Debug, Clone)]
pub struct FormSection {
    /// 区分名。
    pub title: String,
    /// 行（様式の印字順）。
    pub rows: Vec<FormRow>,
}

/// 貸借対照表への当てはめ表。
#[derive(Debug, Clone)]
pub struct BalanceSheetForm {
    form: String,
    part: String,
    sections: Vec<FormSection>,
}

impl BalanceSheetForm {
    /// 様式名。
    pub fn form(&self) -> &str {
        &self.form
    }

    /// 様式のどの部分か。
    pub fn part(&self) -> &str {
        &self.part
    }

    /// 区分（様式の印字順）。
    pub fn sections(&self) -> &[FormSection] {
        &self.sections
    }

    /// この表がどこかの行に当てはめている科目。
    fn mapped_accounts(&self) -> BTreeSet<&AccountCode> {
        self.sections
            .iter()
            .flat_map(|section| section.rows.iter())
            .flat_map(|row| row.accounts.iter())
            .collect()
    }
}

/// 金額を埋めた行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilledRow {
    /// 行名。空欄の行では、そこに当てはめた科目の名前（無ければ `None`）。
    pub label: Option<String>,
    /// 期首の金額。様式で斜線の行は `None`。
    pub opening: Option<Money>,
    /// 期末の金額。
    pub closing: Money,
}

/// 金額を埋めた区分。
#[derive(Debug, Clone)]
pub struct FilledSection {
    /// 区分名。
    pub title: String,
    /// 行。
    pub rows: Vec<FilledRow>,
}

/// 金額を埋めた貸借対照表。
#[derive(Debug, Clone)]
pub struct FilledBalanceSheet {
    /// 様式名。
    pub form: String,
    /// 様式のどの部分か。
    pub part: String,
    /// 区分。
    pub sections: Vec<FilledSection>,
    /// 貸借対照表のどの行にも載らなかった科目。
    ///
    /// **呼び出し側はこれを利用者に見せること。**
    pub not_on_form: Vec<NotOnForm>,
    /// 「期首と期末は同額」の行で、帳簿上は同額でなかったもの
    /// （行名・帳簿の期首・帳簿の期末）。
    ///
    /// 元入金がこれにあたる。様式は同額を前提にしているので、この表は期末の
    /// 値を両列に入れる——**だからこそ、食い違いを検出して知らせないと
    /// 黙って隠すことになる**。
    ///
    /// 典型は**決算振替を記帳した後に決算書を出した**場合である。損益が
    /// 元入金へ振り替わって期末の元入金が動くが、決算書は振替前の姿を
    /// 前提にしている（所得金額を別の行に書くため）。
    pub same_column_mismatches: Vec<(String, Money, Money)>,
}

/// 貸借対照表に載らなかった科目1件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotOnForm {
    /// 科目コード。
    pub account: AccountCode,
    /// 科目名。
    pub label: String,
    /// 期末の金額。
    pub closing: Money,
}

impl FilledBalanceSheet {
    /// 区分の合計（期首・期末）。
    fn section_totals(&self, index: usize) -> (Option<Money>, Money) {
        let section = &self.sections[index];
        let row = section
            .rows
            .iter()
            .rev()
            .find(|row| row.label.as_deref() == Some("合計"));
        match row {
            Some(row) => (row.opening, row.closing),
            None => (None, Money::from_minor(0, self.currency())),
        }
    }

    fn currency(&self) -> kaikei_core::Currency {
        self.sections
            .first()
            .and_then(|section| section.rows.first())
            .map(|row| row.closing.currency())
            .unwrap_or(kaikei_core::Currency::JPY)
    }

    /// 期末の「資産合計 − 負債・資本合計」。
    ///
    /// **0 でなければ帳簿か当てはめに誤りがある。** 様式の書き方が
    /// 「一致しない場合には、記帳誤りや計算誤りがあると思われます」と
    /// 明記している。呼び出し側はこれを利用者に見せること。
    ///
    /// 区分が2つ揃っていなければ `None`（検算しようがない）。
    pub fn imbalance(&self) -> Option<Money> {
        if self.sections.len() != 2 {
            return None;
        }
        let (_, assets) = self.section_totals(0);
        let (_, liabilities_and_equity) = self.section_totals(1);
        assets.sub(&liabilities_and_equity).ok()
    }
}

/// 埋め込み YAML から当てはめ表を読み込む。
pub fn load_embedded(embedded: EmbeddedYaml) -> Result<BalanceSheetForm, JpError> {
    let raw: FormRaw = crate::yaml::load_embedded(embedded)?;
    from_raw(embedded.label, raw)
}

/// 任意のファイルパスから読み込む。
pub fn load_from_path(path: &Path) -> Result<BalanceSheetForm, JpError> {
    let raw: FormRaw = crate::yaml::load_from_path(path)?;
    from_raw(&path.display().to_string(), raw)
}

/// YAML 文字列から読み込む。
pub fn load_from_str(source: &str, label: &str) -> Result<BalanceSheetForm, JpError> {
    let raw: FormRaw = crate::yaml::load_str(source, label)?;
    from_raw(label, raw)
}

fn from_raw(label: &str, raw: FormRaw) -> Result<BalanceSheetForm, JpError> {
    let invalid = |reason: String| JpError::InvalidChart {
        label: label.to_string(),
        reason,
    };

    if raw.version != SUPPORTED_VERSION {
        return Err(invalid(format!(
            "対応していないスキーマバージョンです: {}（対応: {}）",
            raw.version, SUPPORTED_VERSION
        )));
    }

    let mut sections = Vec::with_capacity(raw.sections.len());
    let mut seen_accounts: BTreeSet<String> = BTreeSet::new();
    for section in raw.sections {
        let mut rows = Vec::with_capacity(section.rows.len());
        for row in section.rows {
            // 金額の出どころが2つある行は、どちらを使うかが決まらない。
            let sources = [
                !row.accounts.is_empty(),
                row.from_income_statement.is_some(),
                row.total,
            ]
            .iter()
            .filter(|present| **present)
            .count();
            if sources > 1 {
                return Err(invalid(format!(
                    "行「{}」が accounts / from_income_statement / total を複数持っています。\
                     金額の出どころは1つにしてください",
                    row.label.as_deref().unwrap_or("（空欄）")
                )));
            }

            let accounts = row
                .accounts
                .iter()
                .map(|code| {
                    // **同じ科目を2つの行に当てはめない。** 貸借対照表に
                    // 二重計上され、貸借が合わなくなる。
                    if !seen_accounts.insert(code.clone()) {
                        return Err(format!("科目 {code} が複数の行に当てはめられています"));
                    }
                    AccountCode::parse(code)
                        .map_err(|source| format!("科目コードが不正です: \"{code}\": {source}"))
                })
                .collect::<Result<Vec<_>, String>>()
                .map_err(invalid)?;

            rows.push(FormRow {
                label: row.label,
                accounts,
                omit_opening: row.omit_opening,
                same_in_both_columns: row.same_in_both_columns,
                from_income_statement: row.from_income_statement,
                total: row.total,
                negate: row.negate,
            });
        }
        sections.push(FormSection {
            title: section.title,
            rows,
        });
    }

    Ok(BalanceSheetForm {
        form: raw.form,
        part: raw.part,
        sections,
    })
}

/// 当てはめ表に金額を埋める。
///
/// `opening` は期首時点、`closing` は期末時点の貸借対照表。**どちらも
/// 「その時点までの累計」**であること（モジュール doc「2つの試算表を
/// 受け取る」）。`income_statement_fields` は損益計算書の欄番号 → 金額で、
/// 青色申告特別控除前の所得金額（㊸）の転記に使う。
///
/// # Errors
///
/// 転記元の欄が渡されていない場合は [`JpError::InvalidChart`]。
/// **確定できない行を 0 として通すことはしない。**
pub fn fill(
    form: &BalanceSheetForm,
    opening: &Statement,
    closing: &Statement,
    income_statement_fields: &BTreeMap<u32, Money>,
) -> Result<FilledBalanceSheet, JpError> {
    let invalid = |reason: String| JpError::InvalidChart {
        label: format!("{}（{}）", form.form(), form.part()),
        reason,
    };

    let opening_by_account = index_by_account(opening);
    let closing_by_account = index_by_account(closing);
    let zero = Money::from_minor(0, closing.total.currency());

    let mut sections = Vec::with_capacity(form.sections().len());
    let mut same_column_mismatches = Vec::new();
    for section in form.sections() {
        // 合計は、その区分の**合計行より前の行**を足したもの。合計行を
        // 含めると二重になる。
        let mut opening_total = zero;
        let mut closing_total = zero;
        let mut rows = Vec::with_capacity(section.rows.len());

        for row in &section.rows {
            if row.total {
                rows.push(FilledRow {
                    label: row.label.clone(),
                    opening: Some(opening_total),
                    closing: closing_total,
                });
                continue;
            }

            let (opening_amount, closing_amount, label) =
                if let Some(field) = row.from_income_statement {
                    let amount = *income_statement_fields.get(&field).ok_or_else(|| {
                        invalid(format!(
                            "行「{}」は損益計算書の欄 {} を転記しますが、その欄が\
                             渡されていません。この行を 0 として出すことはしません",
                            row.label.as_deref().unwrap_or("（空欄）"),
                            field
                        ))
                    })?;
                    (zero, amount, row.label.clone())
                } else {
                    let mut opening_amount = zero;
                    let mut closing_amount = zero;
                    let mut names: Vec<&str> = Vec::new();
                    for code in &row.accounts {
                        if let Some((_, amount)) = opening_by_account.get(code) {
                            opening_amount = opening_amount.add(amount).map_err(|source| {
                                invalid(format!("期首の合算に失敗しました: {source}"))
                            })?;
                        }
                        if let Some((name, amount)) = closing_by_account.get(code) {
                            closing_amount = closing_amount.add(amount).map_err(|source| {
                                invalid(format!("期末の合算に失敗しました: {source}"))
                            })?;
                            names.push(name);
                        }
                    }
                    let label = match &row.label {
                        Some(printed) => Some(printed.clone()),
                        None if names.is_empty() => None,
                        None => Some(names.join("・")),
                    };
                    (opening_amount, closing_amount, label)
                };

            // 様式がその科目を反対側の区分に置いている行は符号を反転する。
            let (mut opening_amount, closing_amount) = if row.negate {
                (opening_amount.neg(), closing_amount.neg())
            } else {
                (opening_amount, closing_amount)
            };

            // 様式が「期首と期末は同じ金額」と指示している行。
            if row.same_in_both_columns {
                // **食い違いを黙って揃えない。** 帳簿上で動いていたら、
                // 決算振替を記帳した後に決算書を出した疑いがある。
                if opening_amount != closing_amount {
                    same_column_mismatches.push((
                        row.label.clone().unwrap_or_else(|| "（空欄）".to_string()),
                        opening_amount,
                        closing_amount,
                    ));
                }
                opening_amount = closing_amount;
            }

            opening_total = opening_total
                .add(&opening_amount)
                .map_err(|source| invalid(format!("期首合計の合算に失敗しました: {source}")))?;
            closing_total = closing_total
                .add(&closing_amount)
                .map_err(|source| invalid(format!("期末合計の合算に失敗しました: {source}")))?;

            rows.push(FilledRow {
                label,
                // 様式で斜線の行は期首を出さない。**合計には足した後**で
                // 落とす——様式は期首欄が空でも合計は成り立つ形になって
                // いる（事業主貸・事業主借は期首残高を持たないので 0）。
                opening: if row.omit_opening {
                    None
                } else {
                    Some(opening_amount)
                },
                closing: closing_amount,
            });
        }

        sections.push(FilledSection {
            title: section.title.clone(),
            rows,
        });
    }

    Ok(FilledBalanceSheet {
        form: form.form().to_string(),
        part: form.part().to_string(),
        sections,
        not_on_form: collect_not_on_form(form, &closing_by_account),
        same_column_mismatches,
    })
}

fn index_by_account(statement: &Statement) -> BTreeMap<&AccountCode, (&str, Money)> {
    let mut map = BTreeMap::new();
    for section in &statement.sections {
        for line in &section.lines {
            map.insert(&line.account, (line.label.as_str(), line.amount));
        }
    }
    map
}

fn collect_not_on_form(
    form: &BalanceSheetForm,
    closing_by_account: &BTreeMap<&AccountCode, (&str, Money)>,
) -> Vec<NotOnForm> {
    let mapped = form.mapped_accounts();
    closing_by_account
        .iter()
        .filter(|(code, _)| !mapped.contains(*code))
        .map(|(code, (label, amount))| NotOnForm {
            account: (*code).clone(),
            label: (*label).to_string(),
            closing: *amount,
        })
        .collect()
}

/// 当てはめ表の YAML 上の生の形。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FormRaw {
    version: u32,
    form: String,
    part: String,
    #[allow(dead_code)]
    source: String,
    sections: Vec<SectionRaw>,
    /// 意図して載せない科目。貸借対照表では今のところ空だが、スキーマとして
    /// 受け取る（損益計算書側と形を揃える）。
    #[serde(default)]
    #[allow(dead_code)]
    excluded: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SectionRaw {
    title: String,
    rows: Vec<RowRaw>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RowRaw {
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    accounts: Vec<String>,
    #[serde(default)]
    omit_opening: bool,
    #[serde(default)]
    same_in_both_columns: bool,
    #[serde(default)]
    from_income_statement: Option<u32>,
    #[serde(default)]
    total: bool,
    #[serde(default)]
    negate: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn form() -> BalanceSheetForm {
        load_embedded(kaikei_jp_data::STATEMENT_BLUE_RETURN_GENERAL_BS).unwrap()
    }

    fn statement(sections: Vec<StatementSection>, total: i128) -> Statement {
        Statement {
            title: "貸借対照表".to_string(),
            sections,
            total: yen(total),
        }
    }

    /// 検証帳簿の期首（2025-12-31 時点）。
    fn opening() -> Statement {
        statement(
            vec![
                StatementSection {
                    title: "資産".to_string(),
                    lines: vec![
                        line("100", "現金", 483_610),
                        line("110", "普通預金", -762_415),
                        line("205", "機械装置", 97_200),
                        line("210", "工具器具備品", 140_405),
                        line("220", "車両運搬具", 105_600),
                    ],
                    subtotal: yen(64_400),
                },
                StatementSection {
                    title: "負債".to_string(),
                    lines: vec![line("325", "未払金", 1_502_385)],
                    subtotal: yen(1_502_385),
                },
                StatementSection {
                    title: "純資産".to_string(),
                    lines: vec![line("400", "元入金", -1_437_985)],
                    subtotal: yen(-1_437_985),
                },
            ],
            64_400,
        )
    }

    /// 検証帳簿の期末（2026-12-31 時点）。
    fn closing() -> Statement {
        statement(
            vec![
                StatementSection {
                    title: "資産".to_string(),
                    lines: vec![
                        line("100", "現金", 479_105),
                        line("110", "普通預金", -840_905),
                        line("205", "機械装置", 97_200),
                        line("210", "工具器具備品", 140_405),
                        line("220", "車両運搬具", 105_600),
                        line("410", "事業主貸", -8_100_472),
                    ],
                    subtotal: yen(-8_119_067),
                },
                StatementSection {
                    title: "負債".to_string(),
                    lines: vec![line("325", "未払金", 2_013_470)],
                    subtotal: yen(2_013_470),
                },
                StatementSection {
                    title: "純資産".to_string(),
                    lines: vec![
                        line("400", "元入金", -1_437_985),
                        line("420", "事業主借", 820_000),
                    ],
                    subtotal: yen(-617_985),
                },
            ],
            -8_119_067,
        )
    }

    fn pl_fields(income: i128) -> BTreeMap<u32, Money> {
        let mut map = BTreeMap::new();
        map.insert(43, yen(income));
        map
    }

    fn row_of<'a>(filled: &'a FilledBalanceSheet, label: &str) -> &'a FilledRow {
        filled
            .sections
            .iter()
            .flat_map(|section| section.rows.iter())
            .find(|row| row.label.as_deref() == Some(label))
            .unwrap_or_else(|| panic!("行「{label}」が無い"))
    }

    // BS-1: 同梱の当てはめ表が読める。
    #[test]
    fn the_embedded_form_parses() {
        let form = form();
        assert_eq!(form.part(), "貸借対照表（資産負債調）");
        assert_eq!(form.sections().len(), 2);
        assert_eq!(form.sections()[0].title, "資産の部");
        assert_eq!(form.sections()[1].title, "負債・資本の部");
    }

    // BS-2: 様式どおり両側とも25行ある。
    #[test]
    fn both_sides_have_the_twenty_five_rows_of_the_official_layout() {
        let form = form();
        assert_eq!(form.sections()[0].rows.len(), 25, "資産の部");
        assert_eq!(form.sections()[1].rows.len(), 25, "負債・資本の部");
    }

    // BS-3: 普通預金は「その他の預金」に入る（様式に普通預金の欄が無い）。
    #[test]
    fn the_ordinary_deposit_account_goes_to_the_other_deposits_row() {
        let filled = fill(&form(), &opening(), &closing(), &pl_fields(6_685_880)).unwrap();

        let row = row_of(&filled, "その他の預金");
        assert_eq!(row.closing, yen(-840_905));
        assert_eq!(row.opening, Some(yen(-762_415)));
    }

    // BS-4: 期首欄に斜線がある行は期首を出さない。
    #[test]
    fn rows_struck_through_on_the_form_have_no_opening_amount() {
        let filled = fill(&form(), &opening(), &closing(), &pl_fields(6_685_880)).unwrap();

        for label in ["事業主貸", "事業主借", "青色申告特別控除前の所得金額"]
        {
            assert_eq!(
                row_of(&filled, label).opening,
                None,
                "{label} の期首欄は様式で斜線"
            );
        }
    }

    // BS-5: 元入金は期首と期末が同額（様式の書き方が明記している）。
    #[test]
    fn the_owners_capital_is_the_same_in_both_columns() {
        let filled = fill(&form(), &opening(), &closing(), &pl_fields(6_685_880)).unwrap();

        let row = row_of(&filled, "元入金");
        assert_eq!(row.opening, Some(row.closing));
        assert_eq!(row.closing, yen(-1_437_985));
    }

    // BS-6: 所得金額は損益計算書の㊸を転記する。
    #[test]
    fn the_income_amount_is_carried_over_from_the_income_statement() {
        let filled = fill(&form(), &opening(), &closing(), &pl_fields(6_685_880)).unwrap();

        assert_eq!(
            row_of(&filled, "青色申告特別控除前の所得金額").closing,
            yen(6_685_880)
        );
    }

    // BS-7: 転記元の欄が渡されていなければ拒否する（0 として通さない）。
    #[test]
    fn a_missing_income_statement_field_is_rejected_not_defaulted_to_zero() {
        let err = fill(&form(), &opening(), &closing(), &BTreeMap::new())
            .expect_err("転記元が無ければ拒否されるはず");

        let message = format!("{err}");
        assert!(message.contains("欄 43"), "{message}");
        assert!(message.contains("0 として出すことはしません"), "{message}");
    }

    // BS-8: **本命。** 貸借が合うかを検算できる。
    //
    //       様式の書き方が「一致しない場合には、記帳誤りや計算誤りがあると
    //       思われます」と明記している。差を返せないと、利用者は誤った
    //       決算書をそのまま提出する。
    #[test]
    fn the_form_can_check_that_it_balances() {
        let filled = fill(&form(), &opening(), &closing(), &pl_fields(6_685_880)).unwrap();

        // 資産合計 = 現金479,105 + その他の預金-840,905 + 機械装置97,200
        //          + 工具140,405 + 車両105,600 + 事業主貸8,100,472（符号反転）
        //          = 8,081,877
        // 負債・資本合計 = 未払金2,013,470 + 事業主借820,000
        //                + 元入金-1,437,985 + 所得金額6,685,880
        //                = 8,081,365
        let imbalance = filled.imbalance().expect("2区分あるので検算できる");
        assert_eq!(
            imbalance,
            yen(512),
            "この帳簿は貸借が合わない。決算書から除いた受取利息512の相手科目が\
             資産（普通預金）に残っているため、ちょうどその分だけ資産が多い。\
             差額そのものを固定しておくと、原因の特定に使える"
        );
    }

    // BS-9: 貸借が合う帳簿では差が 0 になる。
    #[test]
    fn a_balanced_book_reports_no_imbalance() {
        // 事業主借を 512 増やして、除いた受取利息の分を吸収した帳簿。
        let mut balanced = closing();
        balanced.sections[2].lines[1] = line("420", "事業主借", 820_512);

        let filled = fill(&form(), &opening(), &balanced, &pl_fields(6_685_880)).unwrap();

        assert_eq!(
            filled.imbalance(),
            Some(yen(0)),
            "受取利息512を事業主借へ振り替えると貸借が合う"
        );
    }

    // BS-9b: **本命。** 「期首と期末は同額」の行が帳簿上で動いていたら
    //        検出する。
    //
    //        決算振替を記帳した後に決算書を出すと、損益が元入金へ振り替わって
    //        期末の元入金が動く。この表は期末の値を両列に入れるので、
    //        検出しないと**食い違いを黙って隠す**ことになる。
    #[test]
    fn a_capital_account_that_moved_during_the_year_is_detected() {
        // 決算振替を記帳した後の姿（元入金が当期純利益の分だけ増えた）。
        let mut after_closing = closing();
        after_closing.sections[2].lines[0] = line("400", "元入金", 6_554_453);

        let filled = fill(&form(), &opening(), &after_closing, &pl_fields(6_685_880)).unwrap();

        assert_eq!(
            filled.same_column_mismatches.len(),
            1,
            "元入金が動いていることを検出すること"
        );
        let (label, book_opening, book_closing) = &filled.same_column_mismatches[0];
        assert_eq!(label, "元入金");
        assert_eq!(*book_opening, yen(-1_437_985));
        assert_eq!(*book_closing, yen(6_554_453));
    }

    // BS-9c: 動いていなければ検出しない。
    #[test]
    fn a_capital_account_that_did_not_move_is_not_flagged() {
        let filled = fill(&form(), &opening(), &closing(), &pl_fields(6_685_880)).unwrap();

        assert!(filled.same_column_mismatches.is_empty());
    }

    // BS-10: 同じ科目を2つの行に当てはめたら拒否する（二重計上になる）。
    #[test]
    fn an_account_mapped_to_two_rows_is_rejected() {
        let source = r#"
version: 1
form: "test"
part: "test"
source: "test"
sections:
  - title: "資産の部"
    rows:
      - { label: "現金", accounts: ["100"] }
      - { label: "その他", accounts: ["100"] }
"#;
        let err = load_from_str(source, "test").expect_err("二重の当てはめは拒否されるはず");
        assert!(format!("{err}").contains("複数の行"), "{err}");
    }

    // BS-11: 金額の出どころが2つある行は拒否する。
    #[test]
    fn a_row_with_two_sources_of_its_amount_is_rejected() {
        let source = r#"
version: 1
form: "test"
part: "test"
source: "test"
sections:
  - title: "資産の部"
    rows:
      - { label: "現金", accounts: ["100"], total: true }
"#;
        let err = load_from_str(source, "test").expect_err("拒否されるはず");
        assert!(format!("{err}").contains("金額の出どころ"), "{err}");
    }

    // BS-12: 当てはめ表に無い貸借科目は報告する（黙って落とさない）。
    #[test]
    fn a_balance_account_missing_from_the_form_is_reported() {
        let mut with_extra = closing();
        with_extra.sections[0]
            .lines
            .push(line("260", "差入保証金", 30_000));

        let filled = fill(&form(), &opening(), &with_extra, &pl_fields(6_685_880)).unwrap();

        let reported = filled
            .not_on_form
            .iter()
            .find(|entry| entry.account.as_str() == "260")
            .expect("当てはめ表に無い科目は報告するはず");
        assert_eq!(reported.closing, yen(30_000));
        assert_eq!(reported.label, "差入保証金");
    }

    // BS-13: 当てはめ済みの科目は報告に出ない。
    #[test]
    fn accounts_that_are_on_the_form_are_not_reported() {
        let filled = fill(&form(), &opening(), &closing(), &pl_fields(6_685_880)).unwrap();

        assert!(
            filled.not_on_form.is_empty(),
            "この帳簿の貸借科目はすべて当てはめ済みのはず: {:?}",
            filled
                .not_on_form
                .iter()
                .map(|e| e.account.as_str())
                .collect::<Vec<_>>()
        );
    }

    // BS-14: 未知のスキーマ版は拒否する。
    #[test]
    fn an_unsupported_schema_version_is_rejected() {
        let source = r#"
version: 99
form: "test"
part: "test"
source: "test"
sections: []
"#;
        let err = load_from_str(source, "test").expect_err("拒否されるはず");
        assert!(format!("{err}").contains("バージョン"), "{err}");
    }
}
