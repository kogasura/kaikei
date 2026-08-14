//! 証憑の一覧（`index.csv`）と、人間が読めるファイル名の組み立て。
//!
//! `docs/06-documents.md` §5。
//!
//! # 保存はハッシュ、閲覧は人間が読める名前
//!
//! 証憑の実体は内容の SHA-256 で保存されている（改変が見つかるように）。
//! 一方、税務調査で提示するには**日付順に並び、取引先が分かる**名前が要る。
//! 保存構造と閲覧構造を分け、閲覧側はエクスポートで作る。
//!
//! `{日付}_{取引先}_{金額}_{種別}.pdf` という並びは、日付順のソートと取引先の
//! 識別を同時に満たす。
//!
//! # ファイル名は必ず安全化する
//!
//! 取引先名は利用者が自由に書く欄なので、パス区切りや制御文字が入りうる。
//! **そのままファイル名にすると、書き出し先が意図しない場所になる**
//! （`../` を含む取引先名など）。
//!
//! # 元の名前とハッシュは index.csv に必ず残す
//!
//! 安全化で名前が変わっても、`index.csv` を見れば元のファイル名と内容の
//! ハッシュに辿り着ける。**変換で情報を捨てない。**

use crate::csv::CsvBuilder;
use kaikei_app::view::DocumentView;

/// ファイル名全体の上限（文字数）。
///
/// 拡張子と重複回避の連番を足しても収まるようにしておく。
const MAX_FILE_NAME_CHARS: usize = 200;

/// 取引先名に使える長さの上限（文字数）。
const MAX_COUNTERPARTY_CHARS: usize = 40;

/// `index.csv` の見出し。
const INDEX_HEADERS: &[&str] = &[
    "エクスポート名",
    "取引年月日",
    "取引金額",
    "取引先",
    "種別",
    "授受の経路",
    "元のファイル名",
    "内容のSHA-256",
    "バイト数",
    "備考",
];

/// 証憑1件のエクスポート先。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportEntry {
    /// 書き出す先のファイル名（安全化済み・重複解消済み）。
    pub file_name: String,
    /// 元の証憑。
    pub document: DocumentView,
}

/// 証憑の一覧から、書き出す先の名前を決める。
///
/// **同じ名前になったら連番を付ける。** 同じ日・同じ取引先・同じ金額の証憑は
/// 普通にあるので、黙って上書きしない。
pub fn plan_export(documents: &[DocumentView]) -> Vec<ExportEntry> {
    let mut used: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
    documents
        .iter()
        .map(|document| {
            let base = human_file_name(document);
            let count = used.entry(base.clone()).or_insert(0);
            *count += 1;
            let file_name = if *count == 1 {
                base
            } else {
                with_suffix(&base, *count)
            };
            ExportEntry {
                file_name,
                document: document.clone(),
            }
        })
        .collect()
}

/// `{日付}_{取引先}_{金額}_{種別}.{拡張子}`。
fn human_file_name(document: &DocumentView) -> String {
    let counterparty = document
        .counterparty
        .as_deref()
        .map(sanitize)
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "取引先なし".to_string());
    let counterparty = truncate_chars(&counterparty, MAX_COUNTERPARTY_CHARS);

    // 金額の無い証憑（契約書など）は「金額なし」と書く。**0 と書かない**
    // ——「金額が無い」と「0円」は違う。
    let amount = match document.amount_minor {
        Some(value) => value.to_string(),
        None => "金額なし".to_string(),
    };

    let kind = sanitize(&document.doc_type);
    let extension = extension_of(&document.original_name);

    let stem = format!(
        "{}_{}_{}_{}",
        document.doc_date.to_iso_string(),
        counterparty,
        amount,
        kind
    );
    let stem = truncate_chars(&stem, MAX_FILE_NAME_CHARS - extension.len() - 1);
    match extension.is_empty() {
        true => stem,
        false => format!("{stem}.{extension}"),
    }
}

/// 重複したときの連番を付ける（拡張子の前に入れる）。
fn with_suffix(file_name: &str, count: u32) -> String {
    match file_name.rsplit_once('.') {
        Some((stem, extension)) => format!("{stem}_{count}.{extension}"),
        None => format!("{file_name}_{count}"),
    }
}

/// ファイル名に使えない文字を落とす。
///
/// **パス区切りと制御文字を必ず除く。** 取引先名は利用者が自由に書く欄なので、
/// `../` のような値が入ると書き出し先が意図しない場所になる。
fn sanitize(text: &str) -> String {
    text.chars()
        .map(|ch| match ch {
            // パス区切りと、Windows が予約している文字。
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            // 制御文字（改行を含む）。
            ch if ch.is_control() => '_',
            ch => ch,
        })
        .collect::<String>()
        // **連続するピリオドを潰す。** `/` を `_` にしても `..` が残ると、
        // 組み立てたパスが親ディレクトリを指しうる（実際にテストで見つけた）。
        .replace("..", "_")
        .trim()
        // 末尾のピリオドと空白は Windows で扱いが変わる。
        .trim_end_matches('.')
        .trim()
        .to_string()
}

/// 文字数で切り詰める（バイト境界で壊さない）。
fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect()
}

/// 元のファイル名から拡張子を取る（安全化済み）。
fn extension_of(original_name: &str) -> String {
    original_name
        .rsplit_once('.')
        .map(|(_, extension)| sanitize(extension))
        .filter(|extension| {
            !extension.is_empty()
                && extension.chars().count() <= 10
                && extension.chars().all(|ch| ch.is_alphanumeric())
        })
        .unwrap_or_default()
}

/// 証憑の一覧（`index.csv`）。
///
/// **検索要件の代替として提出できる一覧**である（`docs/06-documents.md` §5）。
/// 元のファイル名と内容のハッシュを必ず載せる。
pub fn index_to_csv(entries: &[ExportEntry]) -> String {
    let mut csv = CsvBuilder::new();
    csv.push_row(INDEX_HEADERS);
    for entry in entries {
        let document = &entry.document;
        csv.push_row(vec![
            entry.file_name.clone(),
            document.doc_date.to_iso_string(),
            document
                .amount_minor
                .map(|value| value.to_string())
                .unwrap_or_default(),
            document.counterparty.clone().unwrap_or_default(),
            document.doc_type.clone(),
            document.received_via.clone(),
            document.original_name.clone(),
            document.blob_hash.clone(),
            document.byte_size.to_string(),
            document.note.clone().unwrap_or_default(),
        ]);
    }
    csv.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_core::AccountingDate;

    fn date(year: i32, month: u8, day: u8) -> AccountingDate {
        AccountingDate::new(year, month, day).unwrap()
    }

    fn document(counterparty: Option<&str>, amount: Option<i64>, name: &str) -> DocumentView {
        DocumentView {
            id: "00000000-0000-0000-0000-000000000001".to_string(),
            blob_hash: "a".repeat(64),
            original_name: name.to_string(),
            mime_type: "application/pdf".to_string(),
            byte_size: 1024,
            doc_date: date(2026, 4, 15),
            amount_minor: amount,
            counterparty: counterparty.map(|s| s.to_string()),
            doc_type: "invoice".to_string(),
            received_via: "email".to_string(),
            note: None,
        }
    }

    // EX-1: 日付順に並び、取引先が読める名前になる。
    #[test]
    fn the_file_name_sorts_by_date_and_shows_the_counterparty() {
        let planned = plan_export(&[document(Some("株式会社ABC"), Some(110_000), "invoice.pdf")]);

        assert_eq!(
            planned[0].file_name,
            "2026-04-15_株式会社ABC_110000_invoice.pdf"
        );
    }

    // EX-2: **本命。** 取引先名にパス区切りが入っていても、書き出し先が
    //       意図しない場所にならない。
    #[test]
    fn a_counterparty_name_cannot_escape_the_output_directory() {
        let planned = plan_export(&[document(Some("../../etc/passwd"), Some(1), "invoice.pdf")]);

        let name = &planned[0].file_name;
        assert!(!name.contains('/'), "{name}");
        assert!(!name.contains('\\'), "{name}");
        assert!(!name.contains(".."), "{name}");
    }

    // EX-3: 制御文字（改行など）も落とす。
    #[test]
    fn control_characters_are_removed() {
        let planned = plan_export(&[document(Some("A社\n改行\tタブ"), Some(1), "x.pdf")]);

        let name = &planned[0].file_name;
        assert!(!name.contains('\n'), "{name:?}");
        assert!(!name.contains('\t'), "{name:?}");
    }

    // EX-4: 同じ名前になったら連番を付ける（黙って上書きしない）。
    //
    //       同じ日・同じ取引先・同じ金額の証憑は普通にある。
    #[test]
    fn documents_that_would_collide_get_a_number() {
        let planned = plan_export(&[
            document(Some("A社"), Some(1_000), "x.pdf"),
            document(Some("A社"), Some(1_000), "y.pdf"),
            document(Some("A社"), Some(1_000), "z.pdf"),
        ]);

        let names: Vec<&str> = planned.iter().map(|e| e.file_name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "2026-04-15_A社_1000_invoice.pdf",
                "2026-04-15_A社_1000_invoice_2.pdf",
                "2026-04-15_A社_1000_invoice_3.pdf",
            ]
        );
    }

    // EX-5: 金額の無い証憑を 0 と書かない。
    #[test]
    fn a_document_without_an_amount_is_not_named_zero() {
        let planned = plan_export(&[document(Some("A社"), None, "契約書.pdf")]);

        assert!(
            planned[0].file_name.contains("金額なし"),
            "{}",
            planned[0].file_name
        );
        assert!(!planned[0].file_name.contains("_0_"), "0 と書かないこと");
    }

    // EX-6: 取引先が無くても名前が作れる。
    #[test]
    fn a_document_without_a_counterparty_still_gets_a_name() {
        let planned = plan_export(&[document(None, Some(1), "x.pdf")]);

        assert!(planned[0].file_name.contains("取引先なし"));
    }

    // EX-7: 長い取引先名でもファイル名が上限に収まる。
    #[test]
    fn a_very_long_counterparty_name_is_truncated() {
        let long = "あ".repeat(300);
        let planned = plan_export(&[document(Some(&long), Some(1), "x.pdf")]);

        assert!(
            planned[0].file_name.chars().count() <= MAX_FILE_NAME_CHARS,
            "{} 文字",
            planned[0].file_name.chars().count()
        );
    }

    // EX-8: **本命。** index.csv に元のファイル名とハッシュが必ず残る。
    //
    //        安全化で名前が変わっても、元に辿り着けること。
    #[test]
    fn the_index_keeps_the_original_name_and_hash() {
        let planned = plan_export(&[document(Some("../危険な/名前"), Some(1), "元の名前.pdf")]);

        let csv = index_to_csv(&planned);

        assert!(csv.contains("元の名前.pdf"), "{csv}");
        assert!(csv.contains(&"a".repeat(64)), "{csv}");
        // エクスポート名も載る（照合できるように）。
        assert!(csv.contains(&planned[0].file_name), "{csv}");
    }

    // EX-9: 金額の無い証憑は index.csv でも空欄（0 で埋めない）。
    #[test]
    fn the_index_leaves_a_missing_amount_blank() {
        let planned = plan_export(&[document(Some("A社"), None, "x.pdf")]);

        let csv = index_to_csv(&planned);
        let line = csv.lines().nth(1).unwrap();

        // 「エクスポート名,日付,金額,...」の金額が空。
        let cells: Vec<&str> = line.split(',').collect();
        assert_eq!(cells[2], "", "0 で埋めないこと: {line}");
    }

    // EX-10: 拡張子として怪しいものは使わない。
    #[test]
    fn a_suspicious_extension_is_not_used() {
        for name in ["x.pdf/../y", "x.とても長すぎる拡張子です", "x."] {
            let planned = plan_export(&[document(Some("A社"), Some(1), name)]);
            let file_name = &planned[0].file_name;
            assert!(!file_name.contains('/'), "{file_name}");
            assert!(!file_name.contains(".."), "{file_name}");
        }
    }
}
