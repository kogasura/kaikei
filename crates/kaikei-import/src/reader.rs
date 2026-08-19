//! CSV ファイル全体を [`ImportedTransaction`] の並びにする
//! （`docs/05-csv-import.md` §4）。
//!
//! 1行の読み方は [`crate::profile::CsvProfile::parse_row`] が持つ。ここが担うのは
//! **ファイル全体を通したときにしか決まらないこと**である:
//!
//! - どの行を飛ばすか（見出し・合計行）
//! - 同じ内容の行が複数あるとき、何番目かを数える（冪等性キーに効く）
//! - 1行の失敗で全体を止めない
//!
//! # 行を自分で分割しない
//!
//! 摘要にカンマが入った明細（`"振込 ﾔﾏﾀﾞ,ﾀﾛｳ"` など）は珍しくない。区切り文字で
//! 素朴に分けると列がずれ、**金額の列が摘要の一部を読む**——桁が変わったまま
//! 帳簿に入る。`csv` クレートに引用符の解釈を任せる。

use std::collections::HashMap;

use crate::profile::{CsvProfile, ParsedRow};
use crate::{external_key, ImportError, ImportedTransaction, RowError, SourceId};

/// CSV を読んだ結果。
///
/// **部分成功を許す**（`docs/05-csv-import.md` §4）。1行のパースに失敗しても
/// 全体を止めず、失敗した行は理由付きで返す。取り込めた分と取り込めなかった分を
/// 同時に返さないと、利用者は「何件入って何件落ちたか」を確かめられない。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedCsv {
    /// 読めた明細。
    pub transactions: Vec<ImportedTransaction>,
    /// 読めなかった行。
    pub errors: Vec<RowError>,
}

/// プロファイルに従って CSV の文字列を読む。
///
/// 文字コードの解決は [`crate::decode_csv`] が先に済ませている前提。
///
/// # Errors
///
/// CSV として全く読めない場合は [`ImportError`]。**行ごとの失敗はエラーに
/// しない**——[`ParsedCsv::errors`] に入れて返す。
pub fn parse_csv(
    profile: &CsvProfile,
    source: &SourceId,
    text: &str,
) -> Result<ParsedCsv, ImportError> {
    let delimiter = single_byte_delimiter(&profile.delimiter)?;

    let mut reader = csv::ReaderBuilder::new()
        // 見出しの扱いは profile.skip_rows が決める。csv クレートに任せると
        // 「見出しが2行ある」形式を扱えない。
        .has_headers(false)
        .delimiter(delimiter)
        // **列数の違いで止めない。** 合計行だけ列が少ない明細は普通にある。
        .flexible(true)
        .from_reader(text.as_bytes());

    // 行番号を保つために、まず全行を読む。飛ばす行数が末尾から数えるもので
    // ある以上、末尾が分かるまで確定しない。
    let mut records = Vec::new();
    for (index, record) in reader.records().enumerate() {
        match record {
            Ok(record) => records.push((index + 1, record)),
            Err(source) => {
                return Err(ImportError::InvalidValue {
                    reason: format!("CSV として読めません（{index}行目付近）: {source}"),
                })
            }
        }
    }

    let body = slice_body(&records, profile.skip_rows, profile.skip_trailing_rows);

    let mut parsed = ParsedCsv::default();
    // 同じ (日付・金額・向き・摘要) が何度出たかを数える。同日同額同摘要の
    // 取引（コンビニで2回買った等）を、残高が無くても区別するため。
    let mut seen: HashMap<String, u32> = HashMap::new();

    for (line, record) in body {
        let cells: Vec<String> = record.iter().map(str::to_string).collect();
        match profile.parse_row(&cells) {
            Ok(row) => {
                let group = group_key(&row);
                let occurrence = seen.entry(group).or_insert(0);
                *occurrence += 1;
                parsed
                    .transactions
                    .push(to_transaction(source, &row, &cells, *occurrence));
            }
            Err(err) => parsed.errors.push(RowError {
                line: *line,
                reason: err.to_string(),
            }),
        }
    }

    Ok(parsed)
}

/// 見出しと末尾を落とした本体を返す。
///
/// **飛ばす行数が行数を超えても panic させない。** 空の明細（見出しだけの
/// ファイル）を渡されるのは、月の途中で口座に動きが無かったときの普通の姿で
/// ある。
fn slice_body<T>(records: &[T], skip_rows: usize, skip_trailing_rows: usize) -> &[T] {
    let start = skip_rows.min(records.len());
    let end = records.len().saturating_sub(skip_trailing_rows).max(start);
    &records[start..end]
}

/// 同じ取引として数える単位。
fn group_key(row: &ParsedRow) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}",
        row.occurred_on,
        row.amount_minor,
        row.direction.as_key(),
        row.raw_description
    )
}

fn to_transaction(
    source: &SourceId,
    row: &ParsedRow,
    cells: &[String],
    occurrence: u32,
) -> ImportedTransaction {
    ImportedTransaction {
        source: source.clone(),
        external_key: external_key(
            row.occurred_on,
            row.amount_minor,
            row.direction,
            &row.raw_description,
            row.balance_after,
            occurrence,
        ),
        occurred_on: row.occurred_on,
        amount_minor: row.amount_minor,
        currency: "JPY".to_string(),
        direction: row.direction,
        raw_description: row.raw_description.clone(),
        balance_after: row.balance_after,
        // **元の行をそのまま残す。** 解釈を間違えたと後で分かったとき、元が
        // 無ければ直せない。
        raw_row: serde_json::Value::Array(
            cells
                .iter()
                .map(|cell| serde_json::Value::String(cell.clone()))
                .collect(),
        ),
    }
}

/// 区切り文字を1バイトにする。
///
/// タブ区切りの明細があるので `\t` の表記も受ける。
fn single_byte_delimiter(delimiter: &str) -> Result<u8, ImportError> {
    let resolved = match delimiter {
        "\\t" => "\t",
        other => other,
    };
    let bytes = resolved.as_bytes();
    if bytes.len() != 1 {
        return Err(ImportError::InvalidValue {
            reason: format!("区切り文字は1バイトである必要があります: {delimiter:?}"),
        });
    }
    Ok(bytes[0])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Direction;

    fn profile(extra: &str) -> CsvProfile {
        let yaml = format!(
            "id: test_bank\n\
             name: テスト銀行\n\
             kind: bank\n\
             skip_rows: 1\n\
             date:\n  column: 0\n  format: \"%Y/%m/%d\"\n\
             amount:\n  mode: separate_columns\n  debit_column: 1\n  credit_column: 2\n  thousands_separator: true\n\
             description:\n  columns: [3]\n\
             {extra}"
        );
        CsvProfile::load_all(&yaml).unwrap().pop().unwrap()
    }

    fn source() -> SourceId {
        SourceId::parse("test_bank").unwrap()
    }

    /// **本命。** 摘要にカンマが入っていても列がずれない。
    ///
    /// 素朴に区切ると金額の列が摘要の一部を読み、桁が変わったまま帳簿に入る。
    #[test]
    fn a_comma_inside_a_quoted_description_does_not_shift_the_columns() {
        let csv = "日付,出金,入金,摘要\n\
                   2026/06/15,1980,,\"振込 ﾔﾏﾀﾞ,ﾀﾛｳ\"\n";

        let parsed = parse_csv(&profile(""), &source(), csv).unwrap();

        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        assert_eq!(parsed.transactions.len(), 1);
        assert_eq!(parsed.transactions[0].amount_minor, 1980);
        assert_eq!(parsed.transactions[0].raw_description, "振込 ﾔﾏﾀﾞ,ﾀﾛｳ");
    }

    /// **本命。** 同日同額同摘要の取引が別々の明細になる。
    ///
    /// 同じキーになると、2件目が重複として捨てられて帳簿から消える。
    #[test]
    fn two_identical_looking_rows_get_different_keys() {
        let csv = "日付,出金,入金,摘要\n\
                   2026/06/15,500,,ｺﾝﾋﾞﾆ\n\
                   2026/06/15,500,,ｺﾝﾋﾞﾆ\n";

        let parsed = parse_csv(&profile(""), &source(), csv).unwrap();

        assert_eq!(parsed.transactions.len(), 2);
        assert_ne!(
            parsed.transactions[0].external_key, parsed.transactions[1].external_key,
            "同じ内容でも別の取引として扱えること"
        );
    }

    /// 別のファイルでも同じ行なら同じキーになる（再取込が重複しない）。
    #[test]
    fn the_same_row_gets_the_same_key_every_time() {
        let csv = "日付,出金,入金,摘要\n2026/06/15,500,,ｺﾝﾋﾞﾆ\n";

        let first = parse_csv(&profile(""), &source(), csv).unwrap();
        let second = parse_csv(&profile(""), &source(), csv).unwrap();

        assert_eq!(
            first.transactions[0].external_key,
            second.transactions[0].external_key
        );
    }

    /// **本命。** 1行の失敗で全体を止めない。
    ///
    /// 止めると、合計行が1行あるだけでその月が丸ごと取り込めなくなる。
    #[test]
    fn one_bad_row_does_not_stop_the_rest() {
        let csv = "日付,出金,入金,摘要\n\
                   2026/06/15,500,,ｺﾝﾋﾞﾆ\n\
                   これは日付ではない,500,,こわれた行\n\
                   2026/06/16,700,,ｽｰﾊﾟｰ\n";

        let parsed = parse_csv(&profile(""), &source(), csv).unwrap();

        assert_eq!(parsed.transactions.len(), 2, "残り2件は取り込めること");
        assert_eq!(parsed.errors.len(), 1);
        // 行番号は見出しを含めて数える（利用者が CSV を開いて探せるように）。
        assert_eq!(parsed.errors[0].line, 3);
    }

    /// 末尾の合計行を落とせる。
    #[test]
    fn trailing_rows_can_be_dropped() {
        let csv = "日付,出金,入金,摘要\n\
                   2026/06/15,500,,ｺﾝﾋﾞﾆ\n\
                   合計,500,,\n";

        let parsed = parse_csv(&profile("skip_trailing_rows: 1\n"), &source(), csv).unwrap();

        assert_eq!(parsed.transactions.len(), 1);
        assert!(parsed.errors.is_empty(), "落とした行は失敗にもしない");
    }

    /// 見出しだけのファイルでも落ちない。
    ///
    /// 月の途中で口座に動きが無ければ、こうなるのが普通である。
    #[test]
    fn a_file_with_only_a_header_is_empty_not_an_error() {
        let parsed = parse_csv(&profile(""), &source(), "日付,出金,入金,摘要\n").unwrap();

        assert_eq!(parsed, ParsedCsv::default());
    }

    /// 飛ばす行数がファイルより多くても panic しない。
    #[test]
    fn skipping_more_rows_than_exist_is_not_a_panic() {
        let parsed = parse_csv(
            &profile("skip_trailing_rows: 99\n"),
            &source(),
            "日付,出金,入金,摘要\n2026/06/15,500,,ｺﾝﾋﾞﾆ\n",
        )
        .unwrap();

        assert!(parsed.transactions.is_empty());
    }

    /// 元の行が残る。
    #[test]
    fn the_original_row_is_kept() {
        let csv = "日付,出金,入金,摘要\n2026/06/15,1980,,ｻﾝﾌﾟﾙ\n";

        let parsed = parse_csv(&profile(""), &source(), csv).unwrap();

        assert_eq!(
            parsed.transactions[0].raw_row,
            serde_json::json!(["2026/06/15", "1980", "", "ｻﾝﾌﾟﾙ"])
        );
    }

    /// 入金と出金が入れ替わらない。
    #[test]
    fn money_in_and_money_out_do_not_swap() {
        let csv = "日付,出金,入金,摘要\n\
                   2026/06/15,1980,,支払\n\
                   2026/06/16,,50000,入金\n";

        let parsed = parse_csv(&profile(""), &source(), csv).unwrap();

        assert_eq!(parsed.transactions[0].direction, Direction::Out);
        assert_eq!(parsed.transactions[1].direction, Direction::In);
    }

    /// タブ区切りも読める。
    #[test]
    fn a_tab_delimiter_is_understood() {
        let csv = "日付\t出金\t入金\t摘要\n2026/06/15\t500\t\tｺﾝﾋﾞﾆ\n";

        let parsed = parse_csv(&profile("delimiter: \"\\\\t\"\n"), &source(), csv).unwrap();

        assert_eq!(parsed.transactions.len(), 1);
        assert_eq!(parsed.transactions[0].raw_description, "ｺﾝﾋﾞﾆ");
    }

    #[test]
    fn a_multi_byte_delimiter_is_rejected() {
        let err = single_byte_delimiter("、").expect_err("1バイトでない区切りは拒否");
        assert!(matches!(err, ImportError::InvalidValue { .. }));
    }
}
