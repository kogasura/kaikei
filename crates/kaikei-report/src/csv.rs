//! CSV の組み立て（RFC 4180）。
//!
//! # 依存を足さない
//!
//! CSV ライブラリ（`csv` crate 等）を入れない。書き出す形が固定で、
//! **エスケープ規則は RFC 4180 の3行で済む**（引用符を二重にし、
//! 区切り・引用符・改行を含む値を引用符で囲む）。読み込みが要るようになったら
//! そのとき検討する——読み込みは推測が要るので難しさが桁違いだが、
//! 書き出しはそうではない。
//!
//! # 既定を決めた理由
//!
//! - **UTF-8 BOM 付き**: BOM が無いと Excel が Shift_JIS として開いて文字化けする。
//!   利用者が最初に踏むのがこれで、しかも「文字化けした」としか見えない。
//!   BOM を嫌う取り込み先（弥生等）向けの出力は、そちらの仕様に従って別に作る
//!   （`docs/10-report.md` §6）。
//! - **CRLF**: RFC 4180 の規定。
//! - **金額は桁区切り無し**: MCP の応答と同じ（`docs/07-mcp-server.md` §5）。

/// UTF-8 の BOM。
///
/// Excel に「これは UTF-8 だ」と伝える唯一の手段である（拡張子でも
/// Content-Type でもなく、先頭バイトを見ている）。
pub const UTF8_BOM: &str = "\u{feff}";

/// CSV を1行ずつ組み立てる。
///
/// 行数ぶんのメモリを使う（帳簿1年分＝数千行を想定。`docs/10-report.md`）。
/// ストリームにする必要が出たら、そのとき `std::io::Write` を取る形へ変える。
#[derive(Debug, Default)]
pub struct CsvBuilder {
    rows: Vec<String>,
}

impl CsvBuilder {
    /// 空の状態から始める。
    pub fn new() -> Self {
        Self::default()
    }

    /// 1行追加する。各セルは必要に応じて引用符で囲まれる。
    pub fn push_row<I, S>(&mut self, cells: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let line: Vec<String> = cells
            .into_iter()
            .map(|cell| escape_cell(cell.as_ref()))
            .collect();
        self.rows.push(line.join(","));
    }

    /// 行数（ヘッダを含む）。
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// BOM 付き・CRLF 区切りの文字列にする。
    ///
    /// 末尾にも改行を置く（POSIX のテキストファイルの慣習であり、
    /// 追記したときに行が繋がらない）。
    pub fn finish(self) -> String {
        let mut out = String::from(UTF8_BOM);
        for row in &self.rows {
            out.push_str(row);
            out.push_str("\r\n");
        }
        out
    }
}

/// RFC 4180 のエスケープ。
///
/// 引用符で囲むのは、区切り（`,`）・引用符（`"`）・改行（CR/LF）のいずれかを
/// 含むときだけ。**含まない値は囲まない**——全部囲むと、取り込み先によっては
/// 数値まで文字列として解釈され、しかもそれが目視では分からない。
fn escape_cell(cell: &str) -> String {
    let needs_quotes =
        cell.contains(',') || cell.contains('"') || cell.contains('\n') || cell.contains('\r');
    if !needs_quotes {
        return cell.to_string();
    }
    let mut escaped = String::with_capacity(cell.len() + 2);
    escaped.push('"');
    for ch in cell.chars() {
        if ch == '"' {
            escaped.push('"'); // 引用符は二重にする
        }
        escaped.push(ch);
    }
    escaped.push('"');
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_values_are_not_quoted() {
        let mut csv = CsvBuilder::new();
        csv.push_row(["2026-04-15", "110000", "現金"]);
        let out = csv.finish();
        assert!(out.ends_with("2026-04-15,110000,現金\r\n"), "{out:?}");
    }

    #[test]
    fn the_output_starts_with_a_bom() {
        let mut csv = CsvBuilder::new();
        csv.push_row(["a"]);
        assert!(csv.finish().starts_with(UTF8_BOM));
    }

    #[test]
    fn rows_are_separated_by_crlf() {
        let mut csv = CsvBuilder::new();
        csv.push_row(["a"]);
        csv.push_row(["b"]);
        let out = csv.finish();
        assert_eq!(out, format!("{UTF8_BOM}a\r\nb\r\n"));
    }

    // 摘要には区切り・引用符・改行が入りうる（利用者が自由に書く欄である）。
    #[test]
    fn a_comma_forces_quoting() {
        let mut csv = CsvBuilder::new();
        csv.push_row(["A社, B社 分"]);
        assert!(csv.finish().contains("\"A社, B社 分\""));
    }

    #[test]
    fn a_quote_is_doubled_inside_quotes() {
        let mut csv = CsvBuilder::new();
        csv.push_row(["いわゆる\"訂正\"分"]);
        // "いわゆる""訂正""分"
        assert!(csv.finish().contains("\"いわゆる\"\"訂正\"\"分\""));
    }

    #[test]
    fn a_newline_forces_quoting() {
        let mut csv = CsvBuilder::new();
        csv.push_row(["1行目\n2行目"]);
        let out = csv.finish();
        assert!(out.contains("\"1行目\n2行目\""), "{out:?}");
        // 引用符の中の改行は行区切りではない。データ行は1行のまま。
        assert_eq!(out.matches("\r\n").count(), 1, "{out:?}");
    }

    // 空文字は囲まない（囲むと「空文字」と「値なし」の区別が付かなくなる
    // 取り込み先がある）。
    #[test]
    fn an_empty_cell_is_left_bare() {
        let mut csv = CsvBuilder::new();
        csv.push_row(["a", "", "b"]);
        assert!(csv.finish().contains("a,,b"));
    }

    #[test]
    fn row_count_includes_every_pushed_row() {
        let mut csv = CsvBuilder::new();
        assert_eq!(csv.row_count(), 0);
        csv.push_row(["header"]);
        csv.push_row(["data"]);
        assert_eq!(csv.row_count(), 2);
    }
}
