//! 印刷用 HTML の組み立て。
//!
//! # なぜ HTML で、PDF ではないのか
//!
//! 電子帳簿保存法の見読可能性（施行規則第2条第2項第2号）は、電磁的記録を
//! **「ディスプレイの画面及び書面に、整然とした形式及び明瞭な状態で」**
//! 出力できることを求める。CSV は「書面」の形ではなく、これだけでは足りない
//! （`docs/09-tax-research.md` 項目5、`docs/10-report.md` §1）。
//!
//! PDF を選ばなかったのは、Rust から日本語の PDF を出すには**フォントを
//! バイナリに同梱するか、環境依存のフォント探索を書くか**の二択になるためで、
//! 会計の本質から遠い割に壊れやすい。HTML なら文字列を組み立てるだけで、
//! ブラウザで開けば画面表示になり、印刷すれば書面になる——**1つの出力で
//! 両方を満たす**（`docs/10-report.md` §2-2。判断は人間の承認済み）。
//!
//! **このソフトウェアが法令要件を満たすと名乗ることはしない**（`CLAUDE.md` §10）。
//! 満たしうる形で出力するところまでを担う。
//!
//! # スタイルは埋め込む（外部ファイルにしない）
//!
//! 出力した HTML 1ファイルだけで完結させる。CSS を別ファイルにすると、
//! **帳簿を保存・受け渡しするときに片方だけが残る**——7年保存する書類で
//! それをやると、後から開いたときに崩れた表が出る。

use std::fmt::Write as _;

/// HTML の特殊文字をエスケープする。
///
/// 摘要も勘定科目名も**利用者が自由に書く**欄で、`<` `>` `&` が入りうる。
/// エスケープしないと表示が壊れる（帳簿としては「金額の隣の文字が消える」
/// という形で現れ、印刷するまで気づかないことがある）。
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// 印刷用の HTML 文書を組み立てる。
///
/// `title` は文書の表題（帳簿名）、`subtitle` は期間などの補足、
/// `headers` は表の見出し、`rows` はデータ行。
/// `notes` は表の下に出す注記（空なら出さない）。
pub struct PrintableTable<'a> {
    /// 帳簿名（例: 「仕訳日記帳」）。
    pub title: &'a str,
    /// 表題の下に出す補足（例: 「2026-01-01 〜 2026-12-31」）。
    pub subtitle: &'a str,
    /// 表の見出し。
    pub headers: &'a [&'a str],
    /// データ行。各行のセル数は `headers` と同じであること。
    pub rows: &'a [Vec<String>],
    /// 表の下に出す注記。
    pub notes: &'a [String],
    /// 右寄せにする列の添字（金額の列）。
    pub numeric_columns: &'a [usize],
    /// 用紙を横向きにするか。
    ///
    /// 列が多い帳簿（仕訳日記帳は11列）は A4 縦に収まらず、**印刷すると
    /// 右端が切れる**。切れたことは画面では分からない——刷ってから気づく。
    pub landscape: bool,
}

impl PrintableTable<'_> {
    /// 単体で開ける HTML 文書にする。
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("<!DOCTYPE html>\n<html lang=\"ja\">\n<head>\n");
        out.push_str("<meta charset=\"utf-8\">\n");
        let _ = writeln!(out, "<title>{}</title>", escape(self.title));
        out.push_str(STYLE);
        if self.landscape {
            // 列が多い帳簿は A4 縦に収まらない。切れたことは画面では
            // 分からず、刷ってから気づく。
            out.push_str(
                "<style>@page { size: A4 landscape; }</style>
",
            );
        }
        out.push_str("</head>\n<body>\n");

        let _ = writeln!(out, "<h1>{}</h1>", escape(self.title));
        if !self.subtitle.is_empty() {
            let _ = writeln!(out, "<p class=\"subtitle\">{}</p>", escape(self.subtitle));
        }

        out.push_str("<table>\n<thead>\n<tr>");
        for (index, header) in self.headers.iter().enumerate() {
            let class = if self.numeric_columns.contains(&index) {
                " class=\"num\""
            } else {
                ""
            };
            let _ = write!(out, "<th{class}>{}</th>", escape(header));
        }
        out.push_str("</tr>\n</thead>\n<tbody>\n");

        if self.rows.is_empty() {
            // 0 行でも表を空のまま出さない。**「印刷したら白紙だった」を
            // 「該当が無かった」と読めるようにする**（`CLAUDE.md` §11）。
            let _ = writeln!(
                out,
                "<tr><td class=\"empty\" colspan=\"{}\">この期間に該当する記録はありません</td></tr>",
                self.headers.len()
            );
        }
        for row in self.rows {
            out.push_str("<tr>");
            for (index, cell) in row.iter().enumerate() {
                let class = if self.numeric_columns.contains(&index) {
                    " class=\"num\""
                } else {
                    ""
                };
                let _ = write!(out, "<td{class}>{}</td>", escape(cell));
            }
            out.push_str("</tr>\n");
        }
        out.push_str("</tbody>\n</table>\n");

        for note in self.notes {
            let _ = writeln!(out, "<p class=\"note\">{}</p>", escape(note));
        }

        out.push_str("</body>\n</html>\n");
        out
    }
}

/// 埋め込むスタイル。
///
/// - `@media print` で余白と改ページを指定する
/// - **表の見出しを各ページに繰り返す**（`thead` + `display: table-header-group`）。
///   複数ページに渡る帳簿で見出しが1ページ目にしか無いと、2ページ目以降が
///   「整然とした形式」に見えない
/// - 行の途中で改ページしない（`page-break-inside: avoid`）
/// - 金額は右寄せ・等幅。桁を目で揃えられないと検算ができない
const STYLE: &str = r#"<style>
  body { font-family: "Yu Gothic", "Hiragino Kaku Gothic ProN", sans-serif;
         font-size: 10pt; margin: 16mm 12mm; color: #000; }
  h1 { font-size: 14pt; margin: 0 0 2mm; }
  .subtitle { margin: 0 0 4mm; font-size: 9pt; }
  table { border-collapse: collapse; width: 100%; }
  th, td { border: 1px solid #666; padding: 1.2mm 2mm; text-align: left;
           vertical-align: top; }
  th { background: #eee; font-weight: 600; }
  .num { text-align: right; font-variant-numeric: tabular-nums;
         font-family: "Consolas", "Menlo", monospace; white-space: nowrap; }
  .empty { text-align: center; color: #444; padding: 6mm 2mm; }
  .note { font-size: 9pt; margin: 3mm 0 0; }
  @media print {
    body { margin: 0; }
    @page { margin: 16mm 12mm; }
    thead { display: table-header-group; }
    tr { page-break-inside: avoid; }
  }
</style>
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn table<'a>(rows: &'a [Vec<String>], notes: &'a [String]) -> PrintableTable<'a> {
        PrintableTable {
            title: "仕訳日記帳",
            subtitle: "2026-01-01 〜 2026-12-31",
            headers: &["取引日", "摘要", "金額"],
            rows,
            notes,
            numeric_columns: &[2],
            landscape: false,
        }
    }

    #[test]
    fn the_document_is_self_contained() {
        let html = table(&[], &[]).render();

        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("<meta charset=\"utf-8\">"));
        // スタイルは埋め込む（外部ファイルを参照しない）。
        assert!(html.contains("<style>"));
        assert!(!html.contains("<link"), "外部 CSS を参照している: {html}");
        assert!(!html.contains("<script"), "スクリプトは要らない");
    }

    // 印刷したときに見出しが各ページに出る（規2②二「整然とした形式」）。
    #[test]
    fn the_header_repeats_on_every_printed_page() {
        let html = table(&[], &[]).render();
        assert!(html.contains("display: table-header-group"));
        assert!(html.contains("page-break-inside: avoid"));
    }

    // 0 行でも「白紙」にしない。
    #[test]
    fn an_empty_table_says_so_instead_of_printing_blank() {
        let html = table(&[], &[]).render();
        assert!(
            html.contains("この期間に該当する記録はありません"),
            "{html}"
        );
        assert!(html.contains("colspan=\"3\""));
    }

    // 摘要に HTML の特殊文字が入っても表示が壊れない。
    #[test]
    fn special_characters_in_user_text_are_escaped() {
        let rows = vec![vec![
            "2026-04-15".to_string(),
            "A社 <重要> & B社".to_string(),
            "1000".to_string(),
        ]];
        let html = table(&rows, &[]).render();

        assert!(html.contains("A社 &lt;重要&gt; &amp; B社"), "{html}");
        // 生の < > が本文に残っていないこと（タグとして解釈されてしまう）。
        assert!(!html.contains("<重要>"));
    }

    // 金額の列は右寄せ・等幅（桁を目で揃えられないと検算ができない）。
    #[test]
    fn numeric_columns_are_right_aligned() {
        let rows = vec![vec![
            "2026-04-15".to_string(),
            "売上".to_string(),
            "110000".to_string(),
        ]];
        let html = table(&rows, &[]).render();

        assert!(html.contains("<td class=\"num\">110000</td>"), "{html}");
        // 摘要の列は右寄せにしない。
        assert!(html.contains("<td>売上</td>"), "{html}");
        assert!(html.contains("tabular-nums"));
    }

    #[test]
    fn notes_are_rendered_below_the_table() {
        let notes = vec!["期首残高の仕訳が帳簿にありません".to_string()];
        let html = table(&[], &notes).render();

        assert!(html.contains("期首残高の仕訳が帳簿にありません"));
        let table_end = html.find("</table>").unwrap();
        let note_pos = html.find("期首残高").unwrap();
        assert!(note_pos > table_end, "注記は表の下に出すこと");
    }

    // 列が多い帳簿は横向きで刷る（縦だと右端が切れ、切れたことは画面では分からない）。
    #[test]
    fn landscape_sets_the_page_size() {
        let rows: Vec<Vec<String>> = Vec::new();
        let notes: Vec<String> = Vec::new();
        let mut t = table(&rows, &notes);
        assert!(!t.render().contains("landscape"));
        t.landscape = true;
        assert!(t.render().contains("size: A4 landscape"));
    }

    #[test]
    fn the_title_appears_in_both_the_tab_and_the_page() {
        let html = table(&[], &[]).render();
        assert!(html.contains("<title>仕訳日記帳</title>"));
        assert!(html.contains("<h1>仕訳日記帳</h1>"));
        assert!(html.contains("2026-01-01 〜 2026-12-31"));
    }
}
