//! `crates/kaikei-core/tests/common/mod.rs` の `test_chart()` と
//! `kaikei-jp-data/chart/sole_proprietor.yaml` の乖離を機械的に検出する。
//!
//! `kaikei-core` は依存を増やせない（`CLAUDE.md` §1）ため `kaikei-jp-data`
//! を直接読めず、乖離検出は `kaikei-core` 側では実装できない。
//! `kaikei-jp-data` をワークスペースに組み込む本 PR で、`kaikei-jp` 側の
//! テストとして消化する（`PROGRESS.md` Phase 0 申し送り (b)、
//! `DECISIONS.md` D-051）。
//!
//! # なぜ `test_chart()` の中身をここに書き写さないか
//!
//! 科目一覧を定数として**手で複製**すると、`test_chart()` を変更したときに
//! この複製が追随せず、「乖離を検出するための仕組みが乖離する」という
//! 一段まずい状態になる。Phase 1 で `kaikei-app` の再エクスポート漏れを
//! 「表を手で維持する」運用ルールで防ごうとして3回失敗した教訓
//! （`DECISIONS.md` D-047）をここでも適用する。
//!
//! そこで、`test_chart()` の**ソースそのもの**を読んで科目定義を抽出する。
//! `kaikei-core` の当該ファイルはリポジトリ内に実在するので、
//! `CARGO_MANIFEST_DIR` からの相対パスで読める（crate 依存は発生しない）。

use kaikei_jp::yaml::load_embedded;
use serde::Deserialize;
use std::path::PathBuf;

/// `sole_proprietor.yaml` の科目1件分のスキーマ。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountYaml {
    code: String,
    name: String,
    #[serde(rename = "type")]
    account_type: String,
    #[allow(dead_code)] // 比較には使わないが deny_unknown_fields のため受け取る
    sort: i64,
}

/// `sole_proprietor.yaml` 全体のスキーマ。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChartYaml {
    #[allow(dead_code)]
    version: u32,
    #[allow(dead_code)]
    name: String,
    accounts: Vec<AccountYaml>,
}

/// `test_chart()` のソースから抽出した科目1件。
#[derive(Debug, PartialEq)]
struct CoreAccount {
    code: String,
    name: String,
    account_type: String,
    postable: bool,
}

/// `sole_proprietor.yaml` に存在しないことが正しい科目コード。
///
/// `"999"` は `test_chart()` が「見出し科目（`postable: false`）は記帳できない」
/// ことを検証するために持つ core のテスト専用ダミーであり、実データの
/// 標準科目テンプレートには含まれない。無言の除外リストにせず、ここに理由を
/// 明示する（除外が妥当であり続けることは
/// [`code_999_is_intentionally_absent_from_yaml`] が別途検証する）。
const EXCLUDED_FROM_YAML: &[&str] = &["999"];

/// 抽出できる科目数の下限。
///
/// `test_chart()` の書き方が変わって抽出パターンに当たらなくなると、
/// 「0件を全部比較して全部一致した」という形で**テストが黙って通ってしまう**。
/// それを防ぐための番人。実際の件数（現在12件）を書くのではなく下限にするのは、
/// 科目が1件増えただけでこのテストが落ちるのを避けるため。
const MIN_EXTRACTED_ACCOUNTS: usize = 10;

/// `crates/kaikei-core/tests/common/mod.rs` のソースを読む。
fn core_test_common_source() -> String {
    // env!("CARGO_MANIFEST_DIR") は crates/kaikei-jp を指す。
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("kaikei-core")
        .join("tests")
        .join("common")
        .join("mod.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "kaikei-core のテスト共通モジュールを読めませんでした（{}）: {e}\n\
             このテストは test_chart() のソースを直接読んで科目定義を抽出します。\
             ファイルを移動・改名した場合はこのパスも追随させてください。",
            path.display()
        )
    })
}

/// `test_chart()` のソースから `account(...)` 呼び出しを抽出する。
///
/// 正規表現クレートは使わない（`kaikei-jp` に依存を1つ増やしてまで得る価値が
/// 無い）。対象は自リポジトリ内の既知の書式なので手書きで十分。
///
/// # 行単位で見ない理由（レビューで実際に踏んだ2つの脱落）
///
/// 当初は「1行が `account(` で始まり `),` で終わる」ことを条件に行単位で
/// 抽出していたが、次の2つで**その1行だけが黙って抽出から漏れた**。
/// 件数は減るが下限（[`MIN_EXTRACTED_ACCOUNTS`]）は満たすため、
/// 「該当科目の乖離だけが永久に検出されない」という発見しにくい形になる。
///
/// 1. 行末コメント: `account("615", "地代家賃", AccountType::Expense, true), // 注記`
///    （`sole_proprietor.yaml` は実際にデータ行へ行末コメントを付けている）
/// 2. rustfmt による折返し: 行が長くなると `account(` と引数と `),` が別行に割れる
///
/// そこで**行コメントを除去してから全体を1本の文字列にまとめ**、
/// `account(` から対応する `)` までを括弧の対応で切り出す方式にした。
/// どちらの書き方でも同じ結果になる。
fn extract_test_chart_accounts(source: &str) -> Vec<CoreAccount> {
    let stripped = strip_line_comments(source);
    let call_sites = count_account_call_sites(&stripped);
    let out = parse_account_calls(&stripped);

    assert!(
        call_sites >= MIN_EXTRACTED_ACCOUNTS,
        "test_chart() の中に `account(` の呼び出しが {call_sites} 箇所しか見つかりません（下限 {MIN_EXTRACTED_ACCOUNTS}）。
         test_chart() が `account(...)` ヘルパを使わない書き方に変わった可能性があります。
         抽出ロジック（extract_test_chart_accounts）を追随させてください。
         このまま通すと「0件を比較して全件一致」という形で乖離検出が無言で機能停止します。"
    );

    assert_eq!(
        out.len(),
        call_sites,
        "`account(` の呼び出しが {call_sites} 箇所ある一方、解析できたのは {} 件でした。
         解析できなかった呼び出しがあります（引数の並びが
         `account(\"コード\", \"名称\", AccountType::種別, 記帳可否)` から変わった等）。
         この差を放置すると、解析できなかった科目の乖離だけが検出されないまま緑になります。
         解析できたもの: {:?}",
        out.len(),
        out.iter().map(|a| a.code.as_str()).collect::<Vec<_>>()
    );

    out
}

/// `//` 以降を行ごとに落とす。
///
/// 対象は科目定義という短い式の並びで、文字列リテラルの中に `//` が現れる
/// ことは実質無いため、素朴な除去で足りる。
fn strip_line_comments(source: &str) -> String {
    source
        .lines()
        .map(|line| match line.split_once("//") {
            Some((code, _)) => code,
            None => line,
        })
        .collect::<Vec<_>>()
        .join(
            "
",
        )
}

/// `account(` の呼び出し箇所を数える（`fn account(` の定義行は除く）。
///
/// 折返しがあっても `account(` という並び自体は残るため、
/// 「呼び出しは存在するのに解析できなかった」を検出する基準として使える。
fn count_account_call_sites(source: &str) -> usize {
    source.matches("account(").count() - source.matches("fn account(").count()
}

/// `account(` から対応する `)` までを括弧の対応で切り出して解析する。
fn parse_account_calls(source: &str) -> Vec<CoreAccount> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut search_from = 0;

    while let Some(rel) = source[search_from..].find("account(") {
        let call_start = search_from + rel;
        let args_start = call_start + "account(".len();
        search_from = args_start;

        // `fn account(` の定義は対象外。
        if source[..call_start].trim_end().ends_with("fn") {
            continue;
        }

        // 対応する閉じ括弧を探す（引数に括弧は現れない想定だが、対応を数えておく）。
        let mut depth = 1usize;
        let mut i = args_start;
        while i < bytes.len() {
            match bytes[i] {
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        if depth != 0 {
            continue; // 閉じ括弧が見つからない（ソースが途中で切れている等）
        }

        if let Some(account) = parse_account_args(&source[args_start..i]) {
            out.push(account);
        }
    }

    out
}

/// `"100", "現金", AccountType::Asset, true` を解析する。
///
/// 折返しで改行や余分な空白が入っていてもよいように、各要素を `trim` する。
/// 末尾カンマ（rustfmt が折返し時に付ける）も許容する。
fn parse_account_args(args: &str) -> Option<CoreAccount> {
    let parts: Vec<&str> = args
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let [code, name, ty, postable] = parts.as_slice() else {
        return None;
    };

    Some(CoreAccount {
        code: unquote(code)?.to_string(),
        name: unquote(name)?.to_string(),
        account_type: ty.strip_prefix("AccountType::")?.to_string(),
        postable: match *postable {
            "true" => true,
            "false" => false,
            _ => return None,
        },
    })
}

/// `"現金"` のような二重引用符で囲まれた文字列から中身を取り出す。
fn unquote(s: &str) -> Option<&str> {
    s.strip_prefix('"')?.strip_suffix('"')
}

fn load_chart_yaml() -> ChartYaml {
    load_embedded(kaikei_jp_data::CHART_SOLE_PROPRIETOR)
        .expect("sole_proprietor.yaml は常にパース可能であるべき")
}

#[test]
fn test_chart_matches_sole_proprietor_yaml() {
    let core_accounts = extract_test_chart_accounts(&core_test_common_source());
    let chart = load_chart_yaml();

    let mut mismatches = Vec::new();

    for account in &core_accounts {
        if EXCLUDED_FROM_YAML.contains(&account.code.as_str()) {
            continue;
        }

        match chart.accounts.iter().find(|a| a.code == account.code) {
            None => {
                mismatches.push(format!(
                    "{}: test_chart() には存在しますが sole_proprietor.yaml に存在しません",
                    account.code
                ));
            }
            Some(found) => {
                if found.name != account.name {
                    mismatches.push(format!(
                        "{}: 名称が不一致です（test_chart()=\"{}\" / \
                         sole_proprietor.yaml=\"{}\"）",
                        account.code, account.name, found.name
                    ));
                }
                if found.account_type != account.account_type {
                    mismatches.push(format!(
                        "{}: 種別が不一致です（test_chart()={} / sole_proprietor.yaml={}）",
                        account.code, account.account_type, found.account_type
                    ));
                }
            }
        }
    }

    assert!(
        mismatches.is_empty(),
        "test_chart()（crates/kaikei-core/tests/common/mod.rs）と \
         sole_proprietor.yaml が乖離しています:\n{}",
        mismatches.join("\n")
    );
}

/// `"999"` が実際に `sole_proprietor.yaml` に存在しないことを確認する。
///
/// 万が一将来 `sole_proprietor.yaml` に `"999"` が追加された場合、
/// [`EXCLUDED_FROM_YAML`] による除外が「存在するのに無視している」という
/// 逆方向の乖離を隠してしまわないようにするための対抗テスト。
#[test]
fn code_999_is_intentionally_absent_from_yaml() {
    let chart = load_chart_yaml();

    assert!(
        chart.accounts.iter().all(|a| a.code != "999"),
        "\"999\" は test_chart() 専用のダミー科目のはずですが、\
         sole_proprietor.yaml に実在しています。EXCLUDED_FROM_YAML の除外理由を見直してください"
    );
}

/// 抽出ロジックが4項目すべてを正しく読むことを、既知の入力で確認する。
///
/// **本体（`parse_account_calls`）をそのまま呼ぶ。** 検証側で抽出を再実装すると
/// 「複製は本体の変更に追随しない」という、D-051 自身が否定したのと同じ形を
/// テストの中で再演してしまうため（レビュー指摘）。
#[test]
fn extractor_reads_code_name_type_and_postable() {
    let source = r#"
        account("100", "現金", AccountType::Asset, true),
        account("999", "見出し科目", AccountType::Expense, false),
    "#;

    assert_eq!(
        parse_account_calls(&strip_line_comments(source)),
        vec![
            CoreAccount {
                code: "100".to_string(),
                name: "現金".to_string(),
                account_type: "Asset".to_string(),
                postable: true,
            },
            CoreAccount {
                code: "999".to_string(),
                name: "見出し科目".to_string(),
                account_type: "Expense".to_string(),
                postable: false,
            },
        ]
    );
}

/// 行末コメントが付いていても抽出できること。
///
/// レビューで実際に踏んだ脱落パターン1。`sole_proprietor.yaml` は
/// データ行に行末コメントを付ける書き方を実際に使っており、`test_chart()`
/// に同種の注記が付くのは十分現実的。
#[test]
fn extractor_tolerates_trailing_line_comments() {
    let source = r#"
        account("615", "地代家賃", AccountType::Expense, true), // TODO: 名称要確認
    "#;

    let extracted = parse_account_calls(&strip_line_comments(source));
    assert_eq!(
        extracted.len(),
        1,
        "行末コメントで脱落しています: {extracted:?}"
    );
    assert_eq!(extracted[0].name, "地代家賃");
}

/// rustfmt が折返した形でも抽出できること。
///
/// レビューで実際に踏んだ脱落パターン2。行が長くなると rustfmt が
/// `account(` と引数と `),` を別行に割る（末尾カンマも付く）。
#[test]
fn extractor_tolerates_rustfmt_line_wrapping() {
    let source = r#"
        account(
            "615", "地代家賃", AccountType::Expense, true,
        ),
    "#;

    let extracted = parse_account_calls(&strip_line_comments(source));
    assert_eq!(extracted.len(), 1, "折返しで脱落しています: {extracted:?}");
    assert_eq!(extracted[0].code, "615");
    assert!(extracted[0].postable, "折返し時に記帳可否が読めていません");
}

/// 呼び出しが存在するのに解析できなかったら**無言で通らず落ちる**こと。
///
/// 「一部だけ脱落する」が最も発見しにくい失敗モードなので、
/// 呼び出し箇所の数と解析できた件数の一致を要求している。その番人が
/// 実際に働くかをここで確認する。
#[test]
#[should_panic(expected = "解析できなかった呼び出しがあります")]
fn extractor_panics_when_some_calls_cannot_be_parsed() {
    // 12箇所の呼び出しのうち1つだけ引数の並びが違う（種別が AccountType:: でない）。
    let mut source = String::new();
    for i in 0..11 {
        source.push_str(&format!(
            "        account(\"{:03}\", \"科目{i}\", AccountType::Asset, true),
",
            100 + i
        ));
    }
    source.push_str(
        "        account(\"999\", \"壊れた行\", SomethingElse::Asset, true),
",
    );

    let _ = extract_test_chart_accounts(&source);
}

/// 呼び出しが1つも見つからなくなったら**無言で通らず落ちる**こと。
///
/// `test_chart()` が `account(...)` ヘルパを使わない書き方にリファクタされた
/// 場合。件数一致のチェックだけでは 0 == 0 で通ってしまうため、下限の番人が要る。
#[test]
#[should_panic(expected = "呼び出しが 0 箇所しか見つかりません")]
fn extractor_panics_loudly_when_pattern_no_longer_matches() {
    // `test_chart()` が別の書き方にリファクタされた状況を模したソース。
    let source = r#"
        fn test_chart() -> ChartOfAccounts {
            ChartOfAccounts::new(ACCOUNT_TABLE.iter().map(to_def).collect()).unwrap()
        }
    "#;
    let _ = extract_test_chart_accounts(source);
}
