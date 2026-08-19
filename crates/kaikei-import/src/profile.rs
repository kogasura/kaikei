//! CSV プロファイル（`docs/05-csv-import.md` §5）。
//!
//! 銀行ごとにフォーマットが違う問題に、**コードではなくデータ**で対処する。
//! 新しい金融機関への対応が YAML を1枚足すだけで済む形にする。
//!
//! # 現実の落とし穴
//!
//! | 問題 | 対処 |
//! |---|---|
//! | Shift-JIS | 読み込み側（[`crate::decode_csv`]）が扱う |
//! | 金額のカンマ | `thousands_separator` |
//! | 和暦（`R08/04/15`） | `era: true` |
//! | 入金/出金が別列 | `mode: separate_columns` |
//! | 符号付き1列 | `mode: signed_column` |
//! | 半角カナ | `normalize_kana` |
//! | 末尾の合計行 | `skip_trailing_rows` |
//!
//! # 分からないものを推測しない
//!
//! 列が足りない・日付が読めない・金額が数でない、といった行は**その行だけ**を
//! 失敗にして理由を返す（`docs/05-csv-import.md` §4「部分成功を許す」）。
//! 黙って 0 円や当日の日付で埋めると、帳簿に嘘が入る。

use crate::{Direction, ImportError};
use serde::Deserialize;

/// 金額の表し方。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum AmountSpec {
    /// 入金列と出金列が分かれている（多くの邦銀）。
    SeparateColumns {
        /// 出金（お支払金額）の列。
        debit_column: usize,
        /// 入金（お預り金額）の列。
        credit_column: usize,
        /// `1,234` のようにカンマが入るか。
        #[serde(default)]
        thousands_separator: bool,
    },
    /// 符号付きの1列（一部のカード会社）。
    SignedColumn {
        /// 金額の列。
        column: usize,
        /// 正の値がどちらを意味するか。
        positive_means: Direction,
        #[serde(default)]
        thousands_separator: bool,
    },
}

/// 日付の読み方。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DateSpec {
    /// 列。
    pub column: usize,
    /// `chrono` の書式（`%Y/%m/%d` など）。
    pub format: String,
    /// 和暦か（`R08/04/15` → 2026-04-15）。
    #[serde(default)]
    pub era: bool,
}

/// 摘要の作り方。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DescriptionSpec {
    /// 連結する列。
    pub columns: Vec<usize>,
    /// 連結の区切り。
    #[serde(default = "default_separator")]
    pub separator: String,
    /// 前後の空白を落とすか。
    #[serde(default)]
    pub trim: bool,
    /// 半角カナを全角にするか（検索できるようにするため）。
    #[serde(default)]
    pub normalize_kana: bool,
}

fn default_separator() -> String {
    " ".to_string()
}

/// 取引後残高の読み方。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BalanceSpec {
    /// 列。無ければ残高を読まない。
    #[serde(default)]
    pub column: Option<usize>,
    /// 空欄を許すか。
    #[serde(default)]
    pub optional: bool,
}

/// 1つの CSV フォーマット。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CsvProfile {
    /// 識別子（`mizuho_business` など）。
    pub id: String,
    /// 表示名。
    pub name: String,
    /// 銀行かカードか。
    pub kind: String,
    /// 文字コード（読み込み側への助言。実際の判定は [`crate::decode_csv`]）。
    #[serde(default)]
    pub encoding: Option<String>,
    /// 区切り文字。
    #[serde(default = "default_delimiter")]
    pub delimiter: String,
    /// 先頭で飛ばす行数（見出しなど）。
    #[serde(default)]
    pub skip_rows: usize,
    /// 末尾で飛ばす行数（合計行など）。
    #[serde(default)]
    pub skip_trailing_rows: usize,
    /// 日付。
    pub date: DateSpec,
    /// 金額。
    pub amount: AmountSpec,
    /// 摘要。
    pub description: DescriptionSpec,
    /// 残高。
    #[serde(default)]
    pub balance: Option<BalanceSpec>,
    /// 冪等性キーの構成要素（記録用。実際の組み立ては [`crate::external_key`]）。
    #[serde(default)]
    pub external_key: Vec<String>,
}

fn default_delimiter() -> String {
    ",".to_string()
}

impl CsvProfile {
    /// YAML から読む。**複数のプロファイルを `---` で区切って書ける。**
    pub fn load_all(source: &str) -> Result<Vec<CsvProfile>, ImportError> {
        let mut profiles = Vec::new();
        for document in source.split("\n---") {
            if document.trim().is_empty()
                || document.trim().starts_with('#') && !document.contains("id:")
            {
                continue;
            }
            let profile: CsvProfile =
                serde_norway::from_str(document).map_err(|error| ImportError::InvalidValue {
                    reason: format!("CSV プロファイルを読めません: {error}"),
                })?;
            profiles.push(profile);
        }
        if profiles.is_empty() {
            return Err(ImportError::InvalidValue {
                reason: "CSV プロファイルが1つも入っていません".to_string(),
            });
        }
        Ok(profiles)
    }

    /// 1行を解釈する。
    ///
    /// # Errors
    ///
    /// 列が足りない・日付が読めない・金額が数でない場合は
    /// [`ImportError::InvalidValue`]。**推測で埋めない。**
    pub fn parse_row(&self, cells: &[String]) -> Result<ParsedRow, ImportError> {
        let occurred_on = self.parse_date(cells)?;
        let (amount_minor, direction) = self.parse_amount(cells)?;
        let raw_description = self.build_description(cells)?;
        let balance_after = self.parse_balance(cells)?;
        Ok(ParsedRow {
            occurred_on,
            amount_minor,
            direction,
            raw_description,
            balance_after,
        })
    }

    fn cell<'a>(
        &self,
        cells: &'a [String],
        index: usize,
        what: &str,
    ) -> Result<&'a str, ImportError> {
        cells
            .get(index)
            .map(|text| text.as_str())
            .ok_or_else(|| ImportError::InvalidValue {
                reason: format!(
                    "{what}の列（{index}）がありません。この行は {} 列しかありません",
                    cells.len()
                ),
            })
    }

    fn parse_date(&self, cells: &[String]) -> Result<chrono::NaiveDate, ImportError> {
        let raw = self.cell(cells, self.date.column, "日付")?.trim();
        let text = if self.date.era {
            convert_era_to_western(raw)?
        } else {
            raw.to_string()
        };
        chrono::NaiveDate::parse_from_str(&text, &self.date.format).map_err(|error| {
            ImportError::InvalidValue {
                reason: format!(
                    "日付を読めません: \"{raw}\"（書式 {} を想定。{error}）",
                    self.date.format
                ),
            }
        })
    }

    fn parse_amount(&self, cells: &[String]) -> Result<(i64, Direction), ImportError> {
        match &self.amount {
            AmountSpec::SeparateColumns {
                debit_column,
                credit_column,
                thousands_separator,
            } => {
                let debit = parse_optional_number(
                    self.cell(cells, *debit_column, "出金")?,
                    *thousands_separator,
                )?;
                let credit = parse_optional_number(
                    self.cell(cells, *credit_column, "入金")?,
                    *thousands_separator,
                )?;
                match (debit, credit) {
                    // **両方に値が入っている行を通さない。** どちらが本当かを
                    // 決められないのに、片方を選ぶと金額が変わる。
                    (Some(d), Some(c)) if d != 0 && c != 0 => Err(ImportError::InvalidValue {
                        reason: format!(
                            "出金（{d}）と入金（{c}）の両方に値が入っています。どちらの取引か決められません"
                        ),
                    }),
                    (Some(d), _) if d != 0 => Ok((d.abs(), Direction::Out)),
                    (_, Some(c)) if c != 0 => Ok((c.abs(), Direction::In)),
                    // **0 円の行を「入金0」として通さない。** 合計行や区切り行の
                    // 可能性が高く、取引として帳簿に入れるべきものではない。
                    _ => Err(ImportError::InvalidValue {
                        reason: "出金にも入金にも金額がありません".to_string(),
                    }),
                }
            }
            AmountSpec::SignedColumn {
                column,
                positive_means,
                thousands_separator,
            } => {
                let value = parse_optional_number(
                    self.cell(cells, *column, "金額")?,
                    *thousands_separator,
                )?
                .ok_or_else(|| ImportError::InvalidValue {
                    reason: "金額がありません".to_string(),
                })?;
                if value == 0 {
                    return Err(ImportError::InvalidValue {
                        reason: "金額が 0 です".to_string(),
                    });
                }
                let direction = if value > 0 {
                    *positive_means
                } else {
                    opposite(*positive_means)
                };
                Ok((value.abs(), direction))
            }
        }
    }

    /// 摘要を組み立てる。
    ///
    /// # 列が無ければエラーにする
    ///
    /// 以前は `filter_map` で**黙って飛ばしていた**。プロファイルが指す列が
    /// CSV に無くても「エラー 0件」と出て、**摘要が空のまま取り込まれる。**
    ///
    /// これは現実に起きる——プロファイルを取り違える、銀行が書き出し形式を
    /// 変えて列が1つずれる、など。そして摘要が空の明細は、後から相手先を
    /// 辿れない。実帳簿では摘要が科目名だけの取引が11件あり、通帳とカードの
    /// 明細を見に行くことになった。**それを黙って大量に作る形だった。**
    ///
    /// 日付と金額は `cell()` で列の欠けをエラーにしている。摘要だけ扱いが
    /// 違っていたので揃える。
    ///
    /// **空のセルはエラーにしない。** 列はあるが中身が無い行は普通にある。
    /// 区別しているのは「列そのものが無い」ことである。
    fn build_description(&self, cells: &[String]) -> Result<String, ImportError> {
        let mut found = Vec::with_capacity(self.description.columns.len());
        for index in &self.description.columns {
            found.push(self.cell(cells, *index, "摘要")?.to_string());
        }
        let parts: Vec<String> = found
            .iter()
            .map(|text| {
                let text = if self.description.trim {
                    text.trim()
                } else {
                    text.as_str()
                };
                if self.description.normalize_kana {
                    normalize_halfwidth_kana(text)
                } else {
                    text.to_string()
                }
            })
            .filter(|text| !text.is_empty())
            .collect();
        Ok(parts.join(&self.description.separator))
    }

    fn parse_balance(&self, cells: &[String]) -> Result<Option<i64>, ImportError> {
        let Some(spec) = &self.balance else {
            return Ok(None);
        };
        let Some(column) = spec.column else {
            return Ok(None);
        };
        let Some(raw) = cells.get(column) else {
            // 列が無いのは、`optional` なら許す。
            return if spec.optional {
                Ok(None)
            } else {
                Err(ImportError::InvalidValue {
                    reason: format!("残高の列（{column}）がありません"),
                })
            };
        };
        let value = parse_optional_number(raw, true)?;
        match (value, spec.optional) {
            (Some(value), _) => Ok(Some(value)),
            (None, true) => Ok(None),
            (None, false) => Err(ImportError::InvalidValue {
                reason: "残高が空です".to_string(),
            }),
        }
    }
}

/// 解釈した1行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedRow {
    /// 取引日。
    pub occurred_on: chrono::NaiveDate,
    /// 金額（常に正）。
    pub amount_minor: i64,
    /// 入金か出金か。
    pub direction: Direction,
    /// 摘要。
    pub raw_description: String,
    /// 取引後残高。
    pub balance_after: Option<i64>,
}

fn opposite(direction: Direction) -> Direction {
    match direction {
        Direction::In => Direction::Out,
        Direction::Out => Direction::In,
    }
}

/// 数として読む。空欄は `None`。
///
/// **読めない文字列を 0 にしない。** 0 円の取引として帳簿に入ってしまう。
fn parse_optional_number(raw: &str, thousands_separator: bool) -> Result<Option<i64>, ImportError> {
    let mut text = raw.trim().to_string();
    if text.is_empty() || text == "-" {
        return Ok(None);
    }
    if thousands_separator {
        text = text.replace(',', "");
    }
    // 全角の数字と記号を半角に寄せる（明細によっては全角で入る）。
    text = text
        .chars()
        .map(|ch| match ch {
            '０'..='９' => char::from_u32(ch as u32 - '０' as u32 + '0' as u32).unwrap_or(ch),
            '－' | '−' | 'ー' => '-',
            '＋' => '+',
            '￥' | '¥' | '　' => ' ',
            other => other,
        })
        .collect::<String>()
        .replace(' ', "");
    if text.is_empty() {
        return Ok(None);
    }
    text.parse::<i64>()
        .map(Some)
        .map_err(|_| ImportError::InvalidValue {
            reason: format!("金額を数として読めません: \"{raw}\""),
        })
}

/// 和暦（`R08/04/15`・`令和8年4月15日`）を西暦の文字列にする。
///
/// **元号を取り違えると年が数十年ずれる。** 対応表に無い元号は推測せず拒否する。
fn convert_era_to_western(raw: &str) -> Result<String, ImportError> {
    // 元号の記号と、その元年に対応する西暦。
    const ERAS: &[(&str, i32)] = &[
        ("R", 2018), // 令和元年 = 2019 なので、R1 → 2018 + 1
        ("令和", 2018),
        ("H", 1988), // 平成元年 = 1989
        ("平成", 1988),
        ("S", 1925), // 昭和元年 = 1926
        ("昭和", 1925),
    ];

    for (mark, base) in ERAS {
        let Some(rest) = raw.strip_prefix(mark) else {
            continue;
        };
        let digits: String = rest.chars().take_while(|ch| ch.is_ascii_digit()).collect();
        if digits.is_empty() {
            continue;
        }
        let year: i32 = digits.parse().map_err(|_| ImportError::InvalidValue {
            reason: format!("和暦の年を読めません: \"{raw}\""),
        })?;
        if year == 0 {
            return Err(ImportError::InvalidValue {
                reason: format!("和暦に 0 年はありません: \"{raw}\""),
            });
        }
        let western = base + year;
        return Ok(format!("{western}{}", &rest[digits.len()..]));
    }

    Err(ImportError::InvalidValue {
        reason: format!(
            "和暦として読めません: \"{raw}\"（対応している元号: 令和(R)・平成(H)・昭和(S)）"
        ),
    })
}

/// 半角カナを全角にする。
///
/// **検索できるようにするため。** `ｱﾏｿﾞﾝ` と `アマゾン` が別物として扱われると、
/// 取引先で絞り込めない。濁点・半濁点は前の文字に合成する。
fn normalize_halfwidth_kana(text: &str) -> String {
    const BASE: &[(char, char)] = &[
        ('ｱ', 'ア'),
        ('ｲ', 'イ'),
        ('ｳ', 'ウ'),
        ('ｴ', 'エ'),
        ('ｵ', 'オ'),
        ('ｶ', 'カ'),
        ('ｷ', 'キ'),
        ('ｸ', 'ク'),
        ('ｹ', 'ケ'),
        ('ｺ', 'コ'),
        ('ｻ', 'サ'),
        ('ｼ', 'シ'),
        ('ｽ', 'ス'),
        ('ｾ', 'セ'),
        ('ｿ', 'ソ'),
        ('ﾀ', 'タ'),
        ('ﾁ', 'チ'),
        ('ﾂ', 'ツ'),
        ('ﾃ', 'テ'),
        ('ﾄ', 'ト'),
        ('ﾅ', 'ナ'),
        ('ﾆ', 'ニ'),
        ('ﾇ', 'ヌ'),
        ('ﾈ', 'ネ'),
        ('ﾉ', 'ノ'),
        ('ﾊ', 'ハ'),
        ('ﾋ', 'ヒ'),
        ('ﾌ', 'フ'),
        ('ﾍ', 'ヘ'),
        ('ﾎ', 'ホ'),
        ('ﾏ', 'マ'),
        ('ﾐ', 'ミ'),
        ('ﾑ', 'ム'),
        ('ﾒ', 'メ'),
        ('ﾓ', 'モ'),
        ('ﾔ', 'ヤ'),
        ('ﾕ', 'ユ'),
        ('ﾖ', 'ヨ'),
        ('ﾗ', 'ラ'),
        ('ﾘ', 'リ'),
        ('ﾙ', 'ル'),
        ('ﾚ', 'レ'),
        ('ﾛ', 'ロ'),
        ('ﾜ', 'ワ'),
        ('ｦ', 'ヲ'),
        ('ﾝ', 'ン'),
        ('ｧ', 'ァ'),
        ('ｨ', 'ィ'),
        ('ｩ', 'ゥ'),
        ('ｪ', 'ェ'),
        ('ｫ', 'ォ'),
        ('ｬ', 'ャ'),
        ('ｭ', 'ュ'),
        ('ｮ', 'ョ'),
        ('ｯ', 'ッ'),
        ('ｰ', 'ー'),
        ('｡', '。'),
        ('｢', '「'),
        ('｣', '」'),
        ('､', '、'),
        ('･', '・'),
    ];

    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        let mapped = BASE
            .iter()
            .find(|(half, _)| *half == ch)
            .map(|(_, full)| *full);
        let Some(full) = mapped else {
            out.push(ch);
            index += 1;
            continue;
        };
        // 次が濁点・半濁点なら合成する。
        let next = chars.get(index + 1).copied();
        let combined = match next {
            Some('ﾞ') => compose(full, '\u{3099}'),
            Some('ﾟ') => compose(full, '\u{309a}'),
            _ => None,
        };
        match combined {
            Some(composed) => {
                out.push(composed);
                index += 2;
            }
            None => {
                out.push(full);
                index += 1;
            }
        }
    }
    out
}

/// 清音＋濁点/半濁点を1文字にする。
fn compose(base: char, mark: char) -> Option<char> {
    let voiced = "カキクケコサシスセソタチツテトハヒフヘホ";
    let with_mark = "ガギグゲゴザジズゼゾダヂヅデドバビブベボ";
    let semi = "ハヒフヘホ";
    let with_semi = "パピプペポ";
    if mark == '\u{3099}' {
        let position = voiced.chars().position(|ch| ch == base)?;
        return with_mark.chars().nth(position);
    }
    if mark == '\u{309a}' {
        let position = semi.chars().position(|ch| ch == base)?;
        return with_semi.chars().nth(position);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str =
        include_str!("../../kaikei-import-data/profiles/csv-profile-example.yaml");

    fn bank() -> CsvProfile {
        CsvProfile::load_all(EXAMPLE).unwrap().remove(0)
    }

    fn card() -> CsvProfile {
        CsvProfile::load_all(EXAMPLE).unwrap().remove(1)
    }

    fn cells(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| s.to_string()).collect()
    }

    // PR-1: 同梱のプロファイル例が読める（複数を `---` で区切って書ける）。
    #[test]
    fn the_bundled_example_profiles_parse() {
        let profiles = CsvProfile::load_all(EXAMPLE).unwrap();

        assert_eq!(profiles.len(), 2);
        assert_eq!(profiles[0].id, "example_bank_separate");
        assert_eq!(profiles[1].id, "example_card_signed");
    }

    // PR-2: 入出金が別列の明細を読める。カンマ付きの金額も。
    #[test]
    fn a_bank_row_with_separate_columns_is_parsed() {
        let row = bank()
            .parse_row(&cells(&[
                "2026/06/15",
                "",
                "1,234",
                "",
                "ｶ)ﾋﾞｰﾃｯｸ",
                "振込",
                "50,000",
            ]))
            .unwrap();

        assert_eq!(
            row.occurred_on,
            chrono::NaiveDate::from_ymd_opt(2026, 6, 15).unwrap()
        );
        assert_eq!(row.amount_minor, 1_234);
        assert_eq!(row.direction, Direction::Out);
        assert_eq!(row.balance_after, Some(50_000));
    }

    // PR-3: **本命。** 半角カナが全角になる（濁点も合成される）。
    //
    //       `ｱﾏｿﾞﾝ` と `アマゾン` が別物だと、取引先で絞り込めない。
    #[test]
    fn halfwidth_kana_becomes_fullwidth_with_voiced_marks_composed() {
        let row = bank()
            .parse_row(&cells(&["2026/06/15", "", "100", "", "ｱﾏｿﾞﾝ", "ｼﾞﾔﾊﾟﾝ", "1"]))
            .unwrap();

        assert_eq!(row.raw_description, "アマゾン ジヤパン", "{row:?}");
    }

    // PR-4: 符号付き1列の明細を読める。負なら向きが反転する。
    #[test]
    fn a_signed_column_flips_the_direction_when_negative() {
        let profile = card();

        let spent = profile
            .parse_row(&cells(&["2026/06/15", "ｺﾝﾋﾞﾆ", "1,000"]))
            .unwrap();
        let refunded = profile
            .parse_row(&cells(&["2026/06/15", "ｺﾝﾋﾞﾆ", "-1,000"]))
            .unwrap();

        assert_eq!(spent.direction, Direction::Out);
        assert_eq!(refunded.direction, Direction::In);
        assert_eq!(refunded.amount_minor, 1_000, "金額は常に正");
    }

    // PR-5: **本命。** 出金と入金の両方に値がある行を通さない。
    //
    //       どちらが本当かを決められないのに片方を選ぶと、金額が変わる。
    #[test]
    fn a_row_with_both_debit_and_credit_is_rejected() {
        let error = bank()
            .parse_row(&cells(&["2026/06/15", "", "100", "200", "x", "", "1"]))
            .expect_err("決められないなら拒否すること");

        assert!(format!("{error}").contains("両方に値"), "{error}");
    }

    // PR-6: **本命。** 読めない金額を 0 にしない。
    #[test]
    fn an_unreadable_amount_is_rejected_not_treated_as_zero() {
        let error = bank()
            .parse_row(&cells(&["2026/06/15", "", "たくさん", "", "x", "", "1"]))
            .expect_err("拒否すること");

        assert!(format!("{error}").contains("数として読めません"), "{error}");
    }

    // PR-7: 金額が無い行（合計行など）は取引にしない。
    #[test]
    fn a_row_without_any_amount_is_rejected() {
        let error = bank()
            .parse_row(&cells(&["2026/06/15", "", "", "", "合計", "", ""]))
            .expect_err("拒否すること");

        assert!(format!("{error}").contains("金額がありません"), "{error}");
    }

    // PR-8: 列が足りない行は、どの列が無いかを言って拒否する。
    #[test]
    fn a_short_row_says_which_column_is_missing() {
        let error = bank()
            .parse_row(&cells(&["2026/06/15", ""]))
            .expect_err("拒否すること");

        assert!(format!("{error}").contains("列"), "{error}");
    }

    // PR-9: **本命。** 和暦を西暦に直す。
    #[test]
    fn a_japanese_era_date_is_converted() {
        assert_eq!(convert_era_to_western("R08/04/15").unwrap(), "2026/04/15");
        assert_eq!(convert_era_to_western("H31/04/30").unwrap(), "2019/04/30");
        assert_eq!(
            convert_era_to_western("令和8年4月15日").unwrap(),
            "2026年4月15日"
        );
    }

    // PR-10: 知らない元号は推測しない。**取り違えると年が数十年ずれる。**
    #[test]
    fn an_unknown_era_is_rejected_not_guessed() {
        let error = convert_era_to_western("X08/04/15").expect_err("拒否すること");
        assert!(format!("{error}").contains("対応している元号"), "{error}");

        assert!(convert_era_to_western("R00/01/01").is_err(), "0 年は無い");
    }

    // PR-11: 残高が空でも `optional` なら通る（そのとき None）。
    #[test]
    fn an_optional_balance_may_be_blank() {
        let row = bank()
            .parse_row(&cells(&["2026/06/15", "", "100", "", "x", "", ""]))
            .unwrap();

        assert_eq!(row.balance_after, None, "空欄を 0 にしない");
    }

    // PR-12: 全角の数字も読める（明細によっては全角で入る）。
    #[test]
    fn fullwidth_digits_are_understood() {
        let row = bank()
            .parse_row(&cells(&["2026/06/15", "", "１２３４", "", "x", "", "1"]))
            .unwrap();

        assert_eq!(row.amount_minor, 1_234);
    }
    // ─── 摘要の列が無いとき ─────────────────────────

    /// **本命。** 摘要の列が無ければエラーにする。黙って空にしない。
    ///
    /// 以前は `filter_map` で飛ばしていた。プロファイルが指す列が CSV に
    /// 無くても「エラー 0件」と出て、**摘要が空のまま取り込まれる。**
    ///
    /// プロファイルの取り違えや、銀行が書き出し形式を変えて列が1つずれた
    /// ときに起きる。**摘要が空の明細は、後から相手先を辿れない。**
    #[test]
    fn a_missing_description_column_is_an_error() {
        // 摘要は列 4・5 にあるはずだが、この行は 4 列しかない。
        let error = bank()
            .parse_row(&cells(&["2026/06/15", "", "1,234", ""]))
            .expect_err("エラーになること");

        let message = format!("{error}");
        assert!(message.contains("摘要"), "何の列かを言うこと: {message}");
        assert!(message.contains("4"), "列の番号を言うこと: {message}");
    }

    /// **本命。** 列はあるが空のセルは、エラーにしない。
    ///
    /// 摘要が空の行は普通にある。**区別しているのは「列そのものが無い」
    /// ことである。**
    #[test]
    fn an_empty_description_cell_is_not_an_error() {
        let row = bank()
            .parse_row(&cells(&["2026/06/15", "", "1,234", "", "", "", "50,000"]))
            .expect("空のセルは通ること");

        assert_eq!(row.raw_description, "");
    }

    /// 片方だけ埋まっているときは、埋まっている方だけを使う。
    #[test]
    fn only_the_filled_description_column_is_used() {
        let row = bank()
            .parse_row(&cells(&[
                "2026/06/15",
                "",
                "1,234",
                "",
                "ｶ)ﾋﾞｰﾃｯｸ",
                "",
                "50,000",
            ]))
            .expect("通ること");

        assert_eq!(row.raw_description, "カ)ビーテック", "区切りが余らないこと");
    }
}
