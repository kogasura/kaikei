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
//!
//! # 電子取引とスキャナ保存は別の場所に置く
//!
//! 電子帳簿保存法では**電子取引データの保存**と**スキャナ保存**が別の制度で、
//! 要件も違う。1つのフォルダに混ぜると、提示のときにどれがどちらか分からない。
//!
//! 授受の経路（`received_via`）で分ける:
//!
//! | 経路 | 置き場所 |
//! |---|---|
//! | email / download | `電子取引/` |
//! | scan | `スキャン/` |
//! | それ以外 | `その他/` |
//!
//! **区分が決まらないものを電子取引に混ぜない。** 制度が違うものを混ぜると、
//! 電子取引データとして要件を満たしていないものが混ざったまま提示される。

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
    "置き場所",
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
    /// 書き出す先のフォルダ名（`電子取引` / `スキャン` / `その他`）。
    ///
    /// **電子取引とスキャナ保存は制度が違う**ので分ける（モジュール doc）。
    pub folder: String,
    /// 書き出す先のファイル名（安全化済み・重複解消済み）。
    pub file_name: String,
    /// 元の証憑。
    pub document: DocumentView,
}

/// 授受の経路から置き場所を決める。
///
/// **知らない経路を電子取引にしない。** 制度が違うものを混ぜると、要件を
/// 満たしていないものが電子取引データとして提示される。
pub fn folder_for(received_via: &str) -> &'static str {
    match received_via {
        // 電子的に授受したデータ。
        "email" | "download" => "電子取引",
        // 紙をスキャンしたもの。
        "scan" => "スキャン",
        // 手で登録したものは、どちらとも決まらない。
        _ => "その他",
    }
}

/// 証憑の一覧から、書き出す先の名前を決める。
///
/// **同じ名前になったら連番を付ける。** 同じ日・同じ取引先・同じ金額の証憑は
/// 普通にあるので、黙って上書きしない。
pub fn plan_export(documents: &[DocumentView]) -> Vec<ExportEntry> {
    // 重複はフォルダごとに数える。別のフォルダに同じ名前があっても、
    // 上書きは起きないので連番を付ける理由が無い。
    let mut used: std::collections::BTreeMap<(String, String), u32> =
        std::collections::BTreeMap::new();
    documents
        .iter()
        .map(|document| {
            let folder = folder_for(&document.received_via).to_string();
            let base = human_file_name(document);
            let count = used.entry((folder.clone(), base.clone())).or_insert(0);
            *count += 1;
            let file_name = if *count == 1 {
                base
            } else {
                with_suffix(&base, *count)
            };
            ExportEntry {
                folder,
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

/// 書き出せなかった証憑の1件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotExported {
    /// 置き場所（`index.csv` と同じ）。
    pub folder: String,
    /// エクスポート名（`index.csv` と同じ）。
    pub file_name: String,
    /// 元のファイル名。
    pub original_name: String,
    /// 内容の SHA-256。
    pub blob_hash: String,
    /// 書き出せなかった理由。
    pub reason: String,
}

/// `not_exported.csv` の見出し。
const NOT_EXPORTED_HEADERS: &[&str] = &[
    "置き場所",
    "エクスポート名",
    "元のファイル名",
    "内容のSHA-256",
    "書き出せなかった理由",
];

/// 書き出せなかった証憑を CSV にする。**0 件でも見出しだけのファイルを書く。**
///
/// # なぜ要るのか
///
/// `index.csv` は**書き出せたかどうかに関わらず全件**載せる。`checksums.txt`
/// には書き出せたものしか載らない。つまり受け取った側は、**2つを突き合わせて
/// 初めて欠けに気づく。**
///
/// 失敗は画面にも出るが、**画面は渡す一式に残らない。** 税理士が受け取る
/// のはフォルダだけである。
///
/// 0 件でもファイルを書くのは、`yayoi_skipped.csv` と同じ理由——無いのと
/// 「1件も無かった」は違う。前者は「出し忘れたのでは」と疑う余地が残る。
pub fn not_exported_to_csv(rows: &[NotExported]) -> String {
    let mut csv = CsvBuilder::new();
    csv.push_row(NOT_EXPORTED_HEADERS);
    for row in rows {
        csv.push_row(vec![
            row.folder.clone(),
            row.file_name.clone(),
            row.original_name.clone(),
            row.blob_hash.clone(),
            row.reason.clone(),
        ]);
    }
    csv.finish()
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
            entry.folder.clone(),
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

    /// `index.csv` の1行目から、見出しで列を引く。
    ///
    /// **位置で引かない。** 列を1つ足しただけで、無関係なテストが落ちる
    /// （実際に「置き場所」を足したときに落ちた）。
    fn index_cell(csv: &str, header: &str, row: usize) -> String {
        // **BOM を剥がす。** Excel で開けるように先頭に付いているので、
        // そのまま比べると最初の見出しだけ一致しない。
        let first = csv
            .lines()
            .next()
            .expect("見出し行")
            .trim_start_matches('\u{feff}');
        let headers: Vec<&str> = first.split(',').collect();
        let column = headers
            .iter()
            .position(|h| *h == header)
            .unwrap_or_else(|| panic!("見出しに {header} がありません: {headers:?}"));
        csv.lines()
            .nth(row)
            .expect("行があること")
            .split(',')
            .nth(column)
            .expect("列があること")
            .to_string()
    }

    // EX-9: 金額の無い証憑は index.csv でも空欄（0 で埋めない）。
    #[test]
    fn the_index_leaves_a_missing_amount_blank() {
        let planned = plan_export(&[document(Some("A社"), None, "x.pdf")]);

        let csv = index_to_csv(&planned);

        assert_eq!(
            index_cell(&csv, "取引金額", 1),
            "",
            "0 で埋めないこと: {csv}"
        );
    }

    /// **本命。** 電子取引とスキャナ保存を混ぜない。
    ///
    /// 電子帳簿保存法では別の制度で要件も違う。1つのフォルダに混ぜると、
    /// 提示のときにどれがどちらか分からない。
    #[test]
    fn electronic_and_scanned_documents_go_to_different_folders() {
        assert_eq!(folder_for("email"), "電子取引");
        assert_eq!(folder_for("download"), "電子取引");
        assert_eq!(folder_for("scan"), "スキャン");
    }

    /// **本命。** 区分が決まらないものを電子取引に混ぜない。
    ///
    /// 混ぜると、要件を満たしていないものが電子取引データとして提示される。
    #[test]
    fn an_unknown_route_does_not_land_in_the_electronic_folder() {
        assert_eq!(folder_for("manual"), "その他");
        assert_eq!(folder_for("なにか知らない経路"), "その他");
    }

    /// 置き場所は index.csv にも載る。
    ///
    /// フォルダを見なくても、一覧だけでどちらの制度かが分かる。
    #[test]
    fn the_index_says_where_each_document_went() {
        let planned = plan_export(&[document(Some("A社"), Some(1_000), "x.pdf")]);

        let csv = index_to_csv(&planned);

        assert_eq!(index_cell(&csv, "置き場所", 1), "電子取引", "{csv}");
    }

    /// 同じ名前でもフォルダが違えば連番を付けない。
    ///
    /// 上書きが起きないので、付ける理由が無い。
    #[test]
    fn the_same_name_in_different_folders_keeps_its_name() {
        let mut scanned = document(Some("A社"), Some(1_000), "x.pdf");
        scanned.received_via = "scan".to_string();
        let electronic = document(Some("A社"), Some(1_000), "x.pdf");

        let planned = plan_export(&[electronic, scanned]);

        assert_eq!(planned[0].folder, "電子取引");
        assert_eq!(planned[1].folder, "スキャン");
        assert_eq!(
            planned[0].file_name, planned[1].file_name,
            "別のフォルダなので連番は要らない"
        );
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
    // ─── 書き出せなかった証憑 ───────────────────────

    fn not_exported_row() -> NotExported {
        NotExported {
            folder: "電子取引".to_string(),
            file_name: "2026-03-24_グランデ_515720_invoice.txt".to_string(),
            original_name: "inv1.txt".to_string(),
            blob_hash: "9dbaa4f7".to_string(),
            reason: "中身が保存時から変わっています".to_string(),
        }
    }

    /// **本命。** どの証憑がなぜ落ちたかを渡す一式に残す。
    ///
    /// `index.csv` は全件を載せ、`checksums.txt` は書き出せたものしか
    /// 載らない。受け取った側は2つを突き合わせて初めて欠けに気づく。
    /// **画面の警告は一式に残らない。**
    #[test]
    fn the_reason_and_the_document_are_both_recorded() {
        let csv = not_exported_to_csv(&[not_exported_row()]);

        assert!(csv.contains("inv1.txt"), "元のファイル名: {csv}");
        assert!(csv.contains("9dbaa4f7"), "ハッシュ: {csv}");
        assert!(
            csv.contains("中身が保存時から変わっています"),
            "理由: {csv}"
        );
        assert!(csv.contains("電子取引"), "置き場所: {csv}");
    }

    /// **本命。** 0 件でも見出しだけのファイルを書く。
    ///
    /// 無いのと「1件も無かった」は違う。前者は「出し忘れたのでは」と
    /// 疑う余地が残る（`yayoi_skipped.csv` と同じ）。
    #[test]
    fn an_empty_list_still_has_a_header() {
        let csv = not_exported_to_csv(&[]);

        assert!(csv.contains("書き出せなかった理由"), "{csv}");
        assert_eq!(csv.lines().count(), 1, "見出しだけ: {csv}");
    }
}
