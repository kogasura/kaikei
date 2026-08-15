//! 弥生の仕訳データインポート形式（25項目・Shift-JIS）。
//!
//! `docs/10-report.md` §6。
//!
//! # 単純な写しではない
//!
//! `kaikei` の仕訳は N 本の明細を持つが、弥生は「借方1つ＋貸方1つ」で1行を
//! 作る。3本以上は識別フラグ 2110 → 2100 → 2101 の**複数行伝票**として表す。
//!
//! 割り方が自明でないもの（借方も貸方も2本以上）は**変換せずに一覧する**。
//! 黙って近い形に丸めると、取り込んだ側では元の仕訳が分からない。
//!
//! # Shift-JIS で表せない文字を黙って捨てない
//!
//! 弥生は Shift-JIS 以外を取り込めない（§6-3）。摘要や科目名に Shift-JIS に
//! 無い文字（絵文字など）があると、変換で置換文字に化ける。**化けたことを
//! 知らせる**——摘要が静かに壊れたことに、取り込んだ側は気づけない。
//!
//! # 行数の上限
//!
//! 弥生会計 オンラインは 1000 行・1.0MB を超えるファイルを取り込めない
//! （デスクトップ版の記述には無い）。取り込む側の製品が分からないので、
//! **超えたら知らせる**。黙って1本の巨大なファイルを出すと相手側で弾かれる。

use encoding_rs::SHIFT_JIS;
use kaikei_app::amount::money_to_plain_string;
use kaikei_core::{ChartOfAccounts, JournalEntry, Side};
use std::collections::BTreeMap;

/// 弥生会計 オンラインが取り込める行数の上限。
pub const ONLINE_MAX_ROWS: usize = 1000;

/// 弥生会計 オンラインが取り込めるファイルサイズの上限（バイト）。
pub const ONLINE_MAX_BYTES: usize = 1_000_000;

/// 摘要の上限（半角桁）。超えるとインポート時に切り捨てられる。
const DESCRIPTION_LIMIT_HALF_WIDTH: usize = 64;

/// 税区分が付いていない明細に使う弥生の区分。
///
/// `kaikei` は貸借科目に税区分を求めないが、弥生は税区分を必須にしている。
/// 貸借科目の振替は消費税の対象外なので「対象外」を使う。
const NO_TAX_CATEGORY: &str = "対象外";

/// 変換できなかった仕訳。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    /// 仕訳番号。
    pub entry_no: u32,
    /// 取引日。
    pub date: String,
    /// 摘要。
    pub description: String,
    /// 変換できなかった理由。
    pub reason: String,
}

/// 変換の結果。
#[derive(Debug, Clone, Default)]
pub struct Conversion {
    /// 弥生の1行にあたる 25 項目の並び。
    pub rows: Vec<Vec<String>>,
    /// 変換できなかった仕訳。**呼び出し側は必ず利用者に見せること。**
    pub skipped: Vec<Skipped>,
    /// 摘要が上限を超えていた仕訳の番号（インポート時に切り捨てられる）。
    pub truncated_descriptions: Vec<u32>,
    /// Shift-JIS で表せない文字を含む仕訳（番号と、その文字）。
    ///
    /// **どの仕訳かを言えないと直しようがない。** 「化けました」とだけ
    /// 知らせても、687 行の中から探すことになる。
    pub unmappable_characters: Vec<(u32, String)>,
}

impl Conversion {
    /// 弥生会計 オンラインの行数上限を超えているか。
    pub fn exceeds_online_row_limit(&self) -> bool {
        self.rows.len() > ONLINE_MAX_ROWS
    }
}

/// 仕訳を弥生の行に変換する。
///
/// `tax_map` は kaikei の税区分 → 弥生の税区分名。**表に無い区分は変換せず、
/// その仕訳を [`Conversion::skipped`] に入れる**（近い区分に丸めない）。
pub fn convert(
    entries: &[JournalEntry],
    chart: &ChartOfAccounts,
    tax_map: &BTreeMap<String, YayoiCategory>,
) -> Conversion {
    let mut out = Conversion::default();

    for entry in entries {
        let entry_no = entry.entry_no().as_u32();
        let date = entry.entry_date().to_iso_string();
        let description = entry.description().to_string();

        let fail = |reason: String| Skipped {
            entry_no,
            date: date.clone(),
            description: description.clone(),
            reason,
        };

        let debits: Vec<_> = entry
            .lines()
            .iter()
            .filter(|line| line.side() == Side::Debit)
            .collect();
        let credits: Vec<_> = entry
            .lines()
            .iter()
            .filter(|line| line.side() == Side::Credit)
            .collect();

        // 借方も貸方も2本以上あると、どう組み合わせるかが決まらない。
        // **勝手に組み合わせない**——取り込んだ側では元の仕訳が分からなくなる。
        if debits.len() > 1 && credits.len() > 1 {
            out.skipped.push(fail(format!(
                "借方 {} 本・貸方 {} 本の仕訳は、弥生の「借方1つ＋貸方1つ」への\
                 割り方が一意に決まりません。手で分けてください",
                debits.len(),
                credits.len()
            )));
            continue;
        }
        if debits.is_empty() || credits.is_empty() {
            out.skipped.push(fail(
                "借方または貸方の明細がありません（弥生は両側が必要です）".to_string(),
            ));
            continue;
        }

        // 多い側を1行ずつ、少ない側（1本）をその都度あてる。
        let (many, one, many_is_debit) = if debits.len() >= credits.len() {
            (&debits, credits[0], true)
        } else {
            (&credits, debits[0], false)
        };
        let one: &kaikei_core::JournalLine = one;

        let mut rows = Vec::with_capacity(many.len());
        let mut failed: Option<String> = None;
        for (index, line) in many.iter().enumerate() {
            let (debit_line, credit_line): (&kaikei_core::JournalLine, &kaikei_core::JournalLine) =
                if many_is_debit {
                    (*line, one)
                } else {
                    (one, *line)
                };

            let debit_account = match chart.get(debit_line.account()) {
                Some(def) => def.name.clone(),
                None => {
                    failed = Some(format!(
                        "科目 {} が勘定科目表にありません",
                        debit_line.account().as_str()
                    ));
                    break;
                }
            };
            let credit_account = match chart.get(credit_line.account()) {
                Some(def) => def.name.clone(),
                None => {
                    failed = Some(format!(
                        "科目 {} が勘定科目表にありません",
                        credit_line.account().as_str()
                    ));
                    break;
                }
            };

            let debit_tax = match tax_category_of(debit_line, chart, tax_map) {
                Ok(value) => value,
                Err(reason) => {
                    failed = Some(reason);
                    break;
                }
            };
            let credit_tax = match tax_category_of(credit_line, chart, tax_map) {
                Ok(value) => value,
                Err(reason) => {
                    failed = Some(reason);
                    break;
                }
            };

            // 金額は多い側の明細の額。少ない側はその都度同額をあてる
            // （合計すると少ない側の額に一致する）。
            let amount = money_to_plain_string(line.amount());

            let flag = flag_for(index, many.len());
            rows.push(vec![
                flag.to_string(),     // A 識別フラグ
                String::new(),        // B 伝票No.
                String::new(),        // C 決算
                to_yayoi_date(&date), // D 取引日付
                debit_account,        // E 借方勘定科目
                String::new(),        // F 借方補助科目
                String::new(),        // G 借方部門
                debit_tax,            // H 借方税区分
                amount.clone(),       // I 借方金額
                String::new(),        // J 借方税金額（税込経理なので空）
                credit_account,       // K 貸方勘定科目
                String::new(),        // L 貸方補助科目
                String::new(),        // M 貸方部門
                credit_tax,           // N 貸方税区分
                amount,               // O 貸方金額
                String::new(),        // P 貸方税金額
                description.clone(),  // Q 摘要
                String::new(),        // R 番号
                String::new(),        // S 期日
                "0".to_string(),      // T タイプ（0 = 仕訳）
                String::new(),        // U 生成元
                String::new(),        // V 仕訳メモ
                String::new(),        // W 付箋1
                String::new(),        // X 付箋2
                "no".to_string(),     // Y 調整
            ]);
        }

        match failed {
            Some(reason) => out.skipped.push(fail(reason)),
            None => {
                if half_width_len(&description) > DESCRIPTION_LIMIT_HALF_WIDTH {
                    out.truncated_descriptions.push(entry_no);
                }
                // Shift-JIS に無い文字は置換文字に化ける。**どの仕訳かを
                // 記録する**——化けたことだけ知らせても直しようがない。
                let unmappable: String = rows
                    .iter()
                    .flat_map(|row| row.iter())
                    .flat_map(|cell| unmappable_chars(cell).into_iter())
                    .collect::<std::collections::BTreeSet<char>>()
                    .into_iter()
                    .collect();
                if !unmappable.is_empty() {
                    out.unmappable_characters.push((entry_no, unmappable));
                }
                out.rows.extend(rows);
            }
        }
    }

    out
}

/// 書き出した行を読み直して、科目ごとの借方・貸方を集計する。
///
/// # なぜ読み直すのか
///
/// **税理士が取り込んだ数字が決算書と違えば、そこで気づけない。** 列を1つ
/// ずらす、金額を取り違える、行を落とす——どれも書き出したファイルを
/// 見ないと分からない。組み立てた構造体をそのまま数えても、**書き出しの
/// 誤りは見つからない**（同じ誤りを共有するため）。
///
/// 行は `convert` が作った 25 項目の並びである。ここでは
/// **列の位置を決め打ちで読む**——それがずれていれば、集計が帳簿と合わなく
/// なって表面化する。
///
/// 借方・貸方が同じ科目に立つ仕訳（振替など）も、それぞれ足す。
pub fn sum_by_account(rows: &[Vec<String>]) -> BTreeMap<String, (i128, i128)> {
    let mut totals: BTreeMap<String, (i128, i128)> = BTreeMap::new();
    for row in rows {
        if row.len() < 25 {
            continue;
        }
        // E=借方勘定科目(4) I=借方金額(8) K=貸方勘定科目(10) O=貸方金額(14)
        if let Ok(amount) = row[8].parse::<i128>() {
            totals.entry(row[4].clone()).or_default().0 += amount;
        }
        if let Ok(amount) = row[14].parse::<i128>() {
            totals.entry(row[10].clone()).or_default().1 += amount;
        }
    }
    totals
}

/// 弥生の税区分名。売上側と仕入側で分かれるものがある。
///
/// **文字列1つにしない。** 非課税のように売上にも仕入にも立つ区分があり、
/// 片方だけを持つと向きを取り違える。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YayoiCategory {
    /// 売上側、または向きを問わない区分名。
    pub sales: String,
    /// 仕入側の区分名。`None` なら向きを問わない。
    pub purchase: Option<String>,
}

/// 明細の税区分を弥生の名称に直す。
///
/// # 向きで区分が変わる
///
/// 非課税のように**売上にも仕入にも立つ**区分がある。弥生は売上側と仕入側で
/// 区分が分かれているので、売上側だけを使うと**非課税の仕入が「非課売上」
/// として出力される**（住宅の家賃・支払利息・保険料など、個人事業主に
/// 普通にある取引で起きる）。
///
/// どちらを使うかは**明細の科目の種別**で決める。収益なら売上側、それ以外
/// （費用・資産・負債・純資産）なら仕入側の区分がある場合はそれを使う。
/// 仕入側を持たない区分は向きを問わないので、そのまま使う。
fn tax_category_of(
    line: &kaikei_core::JournalLine,
    chart: &kaikei_core::ChartOfAccounts,
    tax_map: &BTreeMap<String, YayoiCategory>,
) -> Result<String, String> {
    let key = match kaikei_core::TagKey::parse("tax_category") {
        Ok(key) => key,
        // 定数なので失敗しないが、`expect` で落とすより黙って対象外にしない。
        Err(source) => return Err(format!("タグキーを作れませんでした: {source}")),
    };
    match line.tags().get(&key) {
        None => Ok(NO_TAX_CATEGORY.to_string()),
        Some(kaikei_core::TagValue::Code(code)) => {
            let code = code.clone();
            let mapping = tax_map.get(&code).ok_or_else(|| {
                format!(
                    "税区分 {code} に対応する弥生の区分がありません。\
                     近い区分に置き換えることはしません（消費税の申告額が変わります）"
                )
            })?;
            // 収益の明細だけを売上側とみなす。**科目が勘定科目表に無い場合は
            // 売上側と決めつけない**——仕入側の区分があるならそちらを使う
            // （非課税の仕入を売上として出す方が実害が大きい）。
            let is_sales = chart
                .get(line.account())
                .map(|def| def.account_type == kaikei_core::AccountType::Revenue)
                .unwrap_or(false);
            Ok(match (&mapping.purchase, is_sales) {
                (Some(purchase), false) => purchase.clone(),
                _ => mapping.sales.clone(),
            })
        }
        // 税区分はコード値で入る想定。別の型なら**黙って対象外にしない**。
        Some(other) => Err(format!("税区分のタグがコード値ではありません（{other:?}）")),
    }
}

/// 識別フラグ。
///
/// 1行で済む仕訳は 2000（伝票以外）、複数行は 2110 → 2100 → 2101。
fn flag_for(index: usize, total: usize) -> &'static str {
    if total == 1 {
        "2000"
    } else if index == 0 {
        "2110"
    } else if index + 1 == total {
        "2101"
    } else {
        "2100"
    }
}

/// `2026-08-13` → `2026/08/13`。
///
/// 弥生の様式に日付の形式の明記が無かったため、弥生で一般的な
/// スラッシュ区切りにしている（`docs/10-report.md` §6-3b）。
fn to_yayoi_date(iso: &str) -> String {
    iso.replace('-', "/")
}

/// Shift-JIS で表せない文字を拾う。
fn unmappable_chars(text: &str) -> Vec<char> {
    text.chars()
        .filter(|ch| {
            let mut buffer = [0u8; 4];
            let (_, _, had_errors) = SHIFT_JIS.encode(ch.encode_utf8(&mut buffer));
            had_errors
        })
        .collect()
}

/// 半角換算の桁数。全角は2桁として数える。
fn half_width_len(text: &str) -> usize {
    text.chars()
        .map(|ch| if ch.is_ascii() { 1 } else { 2 })
        .sum()
}

/// Shift-JIS のバイト列にする。
///
/// 戻り値は `(バイト列, 変換できなかった文字があったか)`。
/// **化けたことを黙らない**——摘要が静かに壊れたことに、取り込んだ側は
/// 気づけない。
pub fn to_shift_jis_csv(rows: &[Vec<String>]) -> (Vec<u8>, bool) {
    let mut text = String::new();
    for row in rows {
        let cells: Vec<String> = row.iter().map(|cell| quote(cell)).collect();
        text.push_str(&cells.join(","));
        // 弥生は「行末が改行コード」とだけ書いている。Windows の製品なので
        // CRLF にする。
        text.push_str("\r\n");
    }
    let (bytes, _, had_errors) = SHIFT_JIS.encode(&text);
    (bytes.into_owned(), had_errors)
}

/// 値にカンマかダブルクォートが含まれる場合はダブルクォートで囲む。
fn quote(cell: &str) -> String {
    if cell.contains(',') || cell.contains('"') {
        format!("\"{}\"", cell.replace('"', "\"\""))
    } else {
        cell.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_core::{
        AccountCode, AccountDef, AccountType, AccountingDate, Currency, EntryId, EntryNumber,
        FiscalYear, FixedClock, JournalLine, Money, NewEntry, PeriodGuard, PeriodStatus, TagDef,
        TagKey, TagSchema, TagSet, TagValue, TagValueType, Timestamp,
    };

    /// `tax_category` を登録したスキーマ。`TagSchema::empty()` だと
    /// 未登録キーとして弾かれる（core がタグを検証するため）。
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

    struct AllOpen;
    impl PeriodGuard for AllOpen {
        fn status(&self, _date: AccountingDate) -> PeriodStatus {
            PeriodStatus::Open
        }
    }

    fn chart() -> ChartOfAccounts {
        ChartOfAccounts::new(vec![
            def("100", "現金", AccountType::Asset),
            def("110", "普通預金", AccountType::Asset),
            def("500", "売上高", AccountType::Revenue),
            def("604", "通信費", AccountType::Expense),
            def("609", "消耗品費", AccountType::Expense),
            def("615", "地代家賃", AccountType::Expense),
        ])
        .unwrap()
    }

    /// **本命。** 非課税の仕入が「非課売上」として出力されない。
    ///
    /// 非課税は売上にも仕入にも立つ。売上側の区分だけを持っていると、
    /// 住宅の家賃・支払利息・保険料のような**非課税の仕入が売上として
    /// 出力される**。税理士に渡す CSV で経費が売上に化ける。
    ///
    /// 実際に weBanana.SP の帳簿で、非課税仕入の地代家賃 205,000 円が
    /// 「非課売上」として出力されていた。
    #[test]
    fn a_tax_free_purchase_is_not_written_as_a_tax_free_sale() {
        let entries = vec![entry(
            1,
            "地代家賃",
            vec![
                line("615", Side::Debit, 205_000, Some("TAX_FREE")),
                line("110", Side::Credit, 205_000, None),
            ],
        )];

        let result = convert(&entries, &chart(), &tax_map());

        assert!(result.skipped.is_empty(), "{:?}", result.skipped);
        let row = &result.rows[0];
        assert_eq!(row[4], "地代家賃");
        assert_eq!(row[7], "非課仕入", "費用の明細を売上側の区分で出さないこと");
    }

    /// 非課税の売上は売上側の区分のままである。
    ///
    /// 仕入側を足したことで売上side が変わっていないことを確かめる。
    #[test]
    fn a_tax_free_sale_still_uses_the_sales_category() {
        let entries = vec![entry(
            1,
            "非課税売上",
            vec![
                line("110", Side::Debit, 50_000, None),
                line("500", Side::Credit, 50_000, Some("TAX_FREE")),
            ],
        )];

        let result = convert(&entries, &chart(), &tax_map());

        assert!(result.skipped.is_empty(), "{:?}", result.skipped);
        assert_eq!(result.rows[0][13], "非課売上");
    }

    fn def(code: &str, name: &str, account_type: AccountType) -> AccountDef {
        AccountDef {
            code: AccountCode::parse(code).unwrap(),
            name: name.to_string(),
            account_type,
            parent: None,
            postable: true,
        }
    }

    fn both(sales: &str, purchase: Option<&str>) -> YayoiCategory {
        YayoiCategory {
            sales: sales.to_string(),
            purchase: purchase.map(str::to_string),
        }
    }

    fn tax_map() -> BTreeMap<String, YayoiCategory> {
        let mut map = BTreeMap::new();
        map.insert("SALES_10".to_string(), both("課税売上込10%", None));
        map.insert(
            "PURCHASE_10_QUALIFIED".to_string(),
            both("課対仕入込10%適格", None),
        );
        // 非課税は売上にも仕入にも立つ。
        map.insert("TAX_FREE".to_string(), both("非課売上", Some("非課仕入")));
        map
    }

    fn line(code: &str, side: Side, minor: i128, tax: Option<&str>) -> JournalLine {
        let mut tags = TagSet::new();
        if let Some(tax) = tax {
            tags.insert(
                TagKey::parse("tax_category").unwrap(),
                TagValue::Code(tax.to_string()),
            );
        }
        JournalLine::new(
            AccountCode::parse(code).unwrap(),
            side,
            Money::from_minor(minor, Currency::JPY),
            tags,
            None,
        )
        .unwrap()
    }

    fn entry(no: u32, description: &str, lines: Vec<JournalLine>) -> JournalEntry {
        let date = AccountingDate::new(2026, 8, 13).unwrap();
        JournalEntry::new(
            NewEntry {
                id: EntryId::new(no as u128),
                entry_no: EntryNumber::new(no),
                entry_date: date,
                description: description.to_string(),
                lines,
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

    /// **本命。** 書き出した行を読み直すと、帳簿と同じ集計になる。
    ///
    /// 税理士が取り込んだ数字が決算書と違えば、そこで気づけない。
    #[test]
    fn reading_the_written_rows_back_gives_the_same_totals() {
        let entries = vec![entry(
            1,
            "通信費",
            vec![
                line("604", Side::Debit, 1_000, Some("PURCHASE_10_QUALIFIED")),
                line("110", Side::Credit, 1_000, None),
            ],
        )];

        let result = convert(&entries, &chart(), &tax_map());
        let totals = sum_by_account(&result.rows);

        assert_eq!(totals.get("通信費"), Some(&(1_000, 0)));
        assert_eq!(totals.get("普通預金"), Some(&(0, 1_000)));
    }

    /// 同じ科目が借方にも貸方にも立つ仕訳を、両方とも数える。
    ///
    /// 片方しか数えないと、振替の仕訳で集計が合わなくなる。
    #[test]
    fn an_account_on_both_sides_is_counted_on_both() {
        let entries = vec![entry(
            1,
            "振替",
            vec![
                line("110", Side::Debit, 500, None),
                line("110", Side::Credit, 500, None),
            ],
        )];

        let totals = sum_by_account(&convert(&entries, &chart(), &tax_map()).rows);

        assert_eq!(totals.get("普通預金"), Some(&(500, 500)));
    }

    /// 25項目に満たない行は数えない。
    ///
    /// 壊れた行を数えると、集計が合わない理由が分からなくなる。
    #[test]
    fn a_short_row_is_not_counted() {
        let totals = sum_by_account(&[vec!["2000".to_string(), "x".to_string()]]);
        assert!(totals.is_empty(), "{totals:?}");
    }

    // YA-1: 借方1・貸方1 の仕訳は1行になる（識別フラグ 2000）。
    #[test]
    fn a_simple_entry_becomes_one_row() {
        let entries = vec![entry(
            1,
            "通信費",
            vec![
                line("604", Side::Debit, 1_000, Some("PURCHASE_10_QUALIFIED")),
                line("110", Side::Credit, 1_000, None),
            ],
        )];

        let result = convert(&entries, &chart(), &tax_map());

        assert!(result.skipped.is_empty(), "{:?}", result.skipped);
        assert_eq!(result.rows.len(), 1);
        let row = &result.rows[0];
        assert_eq!(row[0], "2000", "1行で済む仕訳は伝票以外");
        assert_eq!(row[3], "2026/08/13");
        assert_eq!(row[4], "通信費");
        assert_eq!(row[7], "課対仕入込10%適格");
        assert_eq!(row[8], "1000");
        assert_eq!(row[10], "普通預金");
        assert_eq!(row[13], "対象外", "税区分の無い明細は対象外");
        assert_eq!(row.len(), 25, "25項目であること");
    }

    // YA-2: 借方2・貸方1 は複数行伝票になる（2110 → 2101）。
    #[test]
    fn an_entry_with_two_debits_becomes_a_multi_row_voucher() {
        let entries = vec![entry(
            2,
            "まとめ払い",
            vec![
                line("604", Side::Debit, 1_000, Some("PURCHASE_10_QUALIFIED")),
                line("609", Side::Debit, 2_000, Some("PURCHASE_10_QUALIFIED")),
                line("110", Side::Credit, 3_000, None),
            ],
        )];

        let result = convert(&entries, &chart(), &tax_map());

        assert!(result.skipped.is_empty(), "{:?}", result.skipped);
        assert_eq!(result.rows.len(), 2);
        assert_eq!(result.rows[0][0], "2110", "複数行の1行目");
        assert_eq!(result.rows[1][0], "2101", "複数行の最終行");
        // 貸方は同じ科目で、金額は借方に合わせて分かれる（合計 3,000）。
        assert_eq!(result.rows[0][8], "1000");
        assert_eq!(result.rows[1][8], "2000");
        assert_eq!(result.rows[0][10], "普通預金");
        assert_eq!(result.rows[1][10], "普通預金");
    }

    // YA-3: 3本以上の借方は 2110 → 2100 → 2101 になる。
    #[test]
    fn three_debits_get_the_middle_flag() {
        let entries = vec![entry(
            3,
            "3本",
            vec![
                line("604", Side::Debit, 100, Some("PURCHASE_10_QUALIFIED")),
                line("609", Side::Debit, 200, Some("PURCHASE_10_QUALIFIED")),
                line("100", Side::Debit, 300, None),
                line("110", Side::Credit, 600, None),
            ],
        )];

        let result = convert(&entries, &chart(), &tax_map());

        let flags: Vec<&str> = result.rows.iter().map(|row| row[0].as_str()).collect();
        assert_eq!(flags, vec!["2110", "2100", "2101"]);
    }

    // YA-4: **本命。** 借方も貸方も2本以上なら変換せず一覧する。
    //
    //       割り方が一意に決まらないので、勝手に組み合わせない。
    #[test]
    fn an_entry_with_many_on_both_sides_is_listed_not_guessed() {
        let entries = vec![entry(
            4,
            "期首残高",
            vec![
                line("100", Side::Debit, 100, None),
                line("110", Side::Debit, 200, None),
                line("500", Side::Credit, 150, Some("SALES_10")),
                line("604", Side::Credit, 150, Some("PURCHASE_10_QUALIFIED")),
            ],
        )];

        let result = convert(&entries, &chart(), &tax_map());

        assert!(result.rows.is_empty(), "勝手に組み合わせないこと");
        assert_eq!(result.skipped.len(), 1);
        assert_eq!(result.skipped[0].entry_no, 4);
        assert!(
            result.skipped[0].reason.contains("一意に決まりません"),
            "{:?}",
            result.skipped[0]
        );
    }

    // YA-5: **本命。** 写像に無い税区分は変換せず一覧する。
    //
    //       近い区分に丸めると、消費税の申告額が変わったことに気づけない。
    #[test]
    fn an_unmapped_tax_category_is_listed_not_rounded() {
        let entries = vec![entry(
            5,
            "非適格の仕入",
            vec![
                line("604", Side::Debit, 1_000, Some("PURCHASE_10_NON_QUALIFIED")),
                line("110", Side::Credit, 1_000, None),
            ],
        )];

        let result = convert(&entries, &chart(), &tax_map());

        assert!(result.rows.is_empty());
        assert_eq!(result.skipped.len(), 1);
        assert!(
            result.skipped[0]
                .reason
                .contains("近い区分に置き換えることはしません"),
            "{:?}",
            result.skipped[0]
        );
    }

    // YA-6: 摘要が上限を超えていたら知らせる（インポート時に切り捨てられる）。
    #[test]
    fn a_description_that_will_be_truncated_is_reported() {
        let long = "あ".repeat(40); // 半角換算 80 桁
        let entries = vec![entry(
            6,
            &long,
            vec![
                line("604", Side::Debit, 1_000, Some("PURCHASE_10_QUALIFIED")),
                line("110", Side::Credit, 1_000, None),
            ],
        )];

        let result = convert(&entries, &chart(), &tax_map());

        assert_eq!(result.truncated_descriptions, vec![6]);
    }

    // YA-6b: **本命。** 化けた仕訳がどれかを言えること。
    //
    //        「化けました」とだけ知らせても、687 行の中から探すことになる。
    #[test]
    fn an_entry_with_characters_shift_jis_cannot_represent_is_identified() {
        let entries = vec![entry(
            7,
            "打合せ🙂",
            vec![
                line("604", Side::Debit, 1_000, Some("PURCHASE_10_QUALIFIED")),
                line("110", Side::Credit, 1_000, None),
            ],
        )];

        let result = convert(&entries, &chart(), &tax_map());

        assert_eq!(result.unmappable_characters.len(), 1);
        assert_eq!(result.unmappable_characters[0].0, 7, "仕訳番号を言うこと");
        assert_eq!(
            result.unmappable_characters[0].1, "🙂",
            "どの文字かも言うこと"
        );
    }

    // YA-6c: 通常の日本語だけの仕訳は報告に出ない。
    #[test]
    fn an_ordinary_entry_is_not_reported_as_unmappable() {
        let entries = vec![entry(
            8,
            "通信費の支払",
            vec![
                line("604", Side::Debit, 1_000, Some("PURCHASE_10_QUALIFIED")),
                line("110", Side::Credit, 1_000, None),
            ],
        )];

        let result = convert(&entries, &chart(), &tax_map());

        assert!(result.unmappable_characters.is_empty());
    }

    // YA-7: Shift-JIS で表せない文字があったら知らせる。
    #[test]
    fn characters_that_shift_jis_cannot_represent_are_reported() {
        let rows = vec![vec!["絵文字🙂".to_string()]];

        let (_, had_errors) = to_shift_jis_csv(&rows);

        assert!(had_errors, "化けたことを黙らないこと");
    }

    // YA-8: 通常の日本語は Shift-JIS に変換できる。
    #[test]
    fn ordinary_japanese_encodes_cleanly() {
        let rows = vec![vec!["通信費".to_string(), "課対仕入込10%適格".to_string()]];

        let (bytes, had_errors) = to_shift_jis_csv(&rows);

        assert!(!had_errors);
        // UTF-8 ではないこと（同じ文字列の UTF-8 バイト列と一致しない）。
        assert_ne!(bytes, "通信費,課対仕入込10%適格\r\n".as_bytes());
        assert!(bytes.ends_with(b"\r\n"), "行末は CRLF");
    }

    // YA-9: カンマを含む値はダブルクォートで囲む。
    #[test]
    fn a_cell_containing_a_comma_is_quoted() {
        let rows = vec![vec!["A,B".to_string(), "C".to_string()]];

        let (bytes, _) = to_shift_jis_csv(&rows);
        let text = SHIFT_JIS.decode(&bytes).0.into_owned();

        assert_eq!(text, "\"A,B\",C\r\n");
    }

    // YA-10: 行数の上限を超えたことを判別できる。
    #[test]
    fn exceeding_the_online_row_limit_can_be_detected() {
        let mut conversion = Conversion::default();
        assert!(!conversion.exceeds_online_row_limit());

        conversion.rows = vec![Vec::new(); ONLINE_MAX_ROWS + 1];
        assert!(conversion.exceeds_online_row_limit());
    }
}
