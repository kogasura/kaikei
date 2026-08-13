//! 帳簿を CSV / 印刷用 HTML として書き出す CLI。
//!
//! ```text
//! kaikei report --year 2026 --out ./out
//! ```
//!
//! # なぜ MCP ツールではないのか
//!
//! 出力は「ファイルとして保存する」ことが用途であり、AI の応答を経由する
//! 必要がない。応答に載せると量が帳簿の大きさに比例し、append-only の
//! `audit_log` に毎回残る（`docs/10-report.md` §4。判断は人間の承認済み）。
//!
//! # 読む設定は3つだけ
//!
//! 記帳しないので、事業者設定12項目のうち必要なのは接続先・帳簿通貨・
//! 会計年度規則だけである。税区分や決算科目は**出力に関与しない**——
//! 使わない設定を要求すると、帳簿を出したいだけの人が12項目を埋めることに
//! なる。記帳する経路（`kaikei-mcp`）は従来どおり全項目を要求する（D-082）。

use kaikei_app::context::{BookSettings, FiscalYearRule};
use kaikei_app::ports::{ChartRepo, JournalRepo};
use kaikei_app::tx::with_tx_err;
use kaikei_app::usecase::report::{self, ReportInput};
use kaikei_app::usecase::statements::{self, StatementsInput};
use kaikei_core::{AccountingDate, FiscalYear};
use kaikei_jp::statement::JpStatementPolicy;
use kaikei_jp::tags::TagCatalog;
use kaikei_store::pool::{connect_app, PgStore};
use kaikei_store::query::PgTrialBalanceQuery;
use std::path::{Path, PathBuf};

const USAGE: &str = "\
kaikei — 帳簿を CSV と印刷用 HTML で書き出します

使い方:
    kaikei report --year <西暦> --out <出力先ディレクトリ>

引数:
    --year <西暦>    会計年度（暦年）。例: 2026
    --out <パス>     出力先のディレクトリ。無ければ作ります

書き出すもの（それぞれ .csv と .html）:
    journal_book      仕訳日記帳（1行1明細。取り消された仕訳も含みます）
    trial_balance     試算表（借方合計・貸方合計・残高）
    balance_sheet     貸借対照表
    income_statement  損益計算書

環境変数:
    APP_DATABASE_URL          帳簿の接続先（kaikei_app ロール）
    KAIKEI_BOOK_CURRENCY      帳簿通貨（例: JPY）
    KAIKEI_FISCAL_YEAR_RULE   会計年度の区切り（現在は calendar_year のみ）

記帳はしません。読み取りだけなので、税区分や決算科目の設定は要りません。
";

fn main() -> std::process::ExitCode {
    match run() {
        Ok(paths) => {
            for path in paths {
                println!("{}", path.display());
            }
            std::process::ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("{message}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<Vec<PathBuf>, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let options = parse_args(&args)?;

    // 同期の `main` から非同期の本体を回す。CLI は1回走って終わるので、
    // ランタイムをここで作って捨てる。
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|error| format!("非同期ランタイムを作れませんでした: {error}"))?;
    runtime.block_on(write_reports(options))
}

/// 解析済みの引数。
#[derive(Debug)]
struct Options {
    fiscal_year: i32,
    out_dir: PathBuf,
}

/// 引数を解析する。
///
/// **未知の引数を黙って無視しない。** `--yea 2026` のような打ち間違いを
/// 受理すると、既定の年度で出力が作られて「なぜか去年の帳簿が出た」に
/// なる（`CLAUDE.md` §11）。
fn parse_args(args: &[String]) -> Result<Options, String> {
    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        return Err(USAGE.to_string());
    }
    if args[0] != "report" {
        return Err(format!("不明なサブコマンドです: {}\n\n{USAGE}", args[0]));
    }

    let mut fiscal_year: Option<i32> = None;
    let mut out_dir: Option<PathBuf> = None;
    let mut rest = args[1..].iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--year" => {
                let value = rest
                    .next()
                    .ok_or("--year の後に西暦を指定してください（例: --year 2026）")?;
                fiscal_year = Some(value.parse::<i32>().map_err(|_| {
                    format!("--year は西暦の数字で指定してください（受け取った値: {value}）")
                })?);
            }
            "--out" => {
                let value = rest
                    .next()
                    .ok_or("--out の後に出力先ディレクトリを指定してください（例: --out ./out）")?;
                out_dir = Some(PathBuf::from(value));
            }
            other => {
                return Err(format!("不明な引数です: {other}\n\n{USAGE}"));
            }
        }
    }

    Ok(Options {
        fiscal_year: fiscal_year.ok_or("--year を指定してください（例: --year 2026）")?,
        out_dir: out_dir.ok_or("--out を指定してください（例: --out ./out）")?,
    })
}

/// 環境変数を1つ読む。**未設定は既定値で埋めない。**
fn env_var(name: &str) -> Result<String, String> {
    std::env::var(name)
        .map_err(|_| format!("環境変数 {name} が未設定です（.env.example を参照してください）"))
}

async fn write_reports(options: Options) -> Result<Vec<PathBuf>, String> {
    let database_url = env_var("APP_DATABASE_URL")?;
    let currency_code = env_var("KAIKEI_BOOK_CURRENCY")?;
    let fiscal_year_rule = env_var("KAIKEI_FISCAL_YEAR_RULE")?;

    let book_currency = kaikei_app::currency::currency_from_code(&currency_code)
        .map_err(|error| format!("KAIKEI_BOOK_CURRENCY が不正です: {error}"))?;
    if fiscal_year_rule != "calendar_year" {
        return Err(format!(
            "KAIKEI_FISCAL_YEAR_RULE は現在 calendar_year のみ対応しています\
             （受け取った値: {fiscal_year_rule}）"
        ));
    }
    let settings = BookSettings {
        fiscal_year_rule: FiscalYearRule::CalendarYear,
        book_currency,
    };

    let fiscal_year = FiscalYear::calendar_year(options.fiscal_year);
    let from = fiscal_year.start();
    let to = fiscal_year.end();
    let period_label = format!("{} 〜 {}", from.to_iso_string(), to.to_iso_string());

    let catalog = TagCatalog::from_embedded(kaikei_jp_data::TAGS)
        .map_err(|error| format!("同梱のタグ定義を読めませんでした: {error}"))?;

    let pool = connect_app(&database_url)
        .await
        .map_err(|error| format!("PostgreSQL に接続できませんでした: {error}"))?;
    let store = PgStore::new(pool.clone());

    // 仕訳と勘定科目表を1つのトランザクションで読む。**間に記帳が入ると、
    // 仕訳日記帳と財務諸表が別の帳簿を映す。**
    let schema = catalog.schema().clone();
    let (entries, chart, statements) = with_tx_err(&store, move |tx| {
        let schema = schema.clone();
        Box::pin(async move {
            let chart = tx.load_chart().await?;
            let entries = tx.list_entries_in_period(from, to).await?;
            let policy = JpStatementPolicy::new(chart.clone());
            let statements =
                statements::execute(tx, &policy, &schema, StatementsInput { from, to }).await?;
            Ok((entries, chart, statements))
        })
    })
    .await
    .map_err(|error: kaikei_app::error::AppError| format!("帳簿を読めませんでした: {error}"))?;

    // 試算表は read model（SQL 集計）から取る。財務諸表とは経路が違う
    // （`DECISIONS.md` D-093 の住み分け）。
    let query = PgTrialBalanceQuery::new(pool);
    let trial_balance = report::execute(
        &query,
        catalog.schema(),
        &settings,
        ReportInput {
            from,
            to,
            group_by: Vec::new(),
        },
    )
    .await
    .map_err(|error| format!("試算表を集計できませんでした: {error}"))?;

    // 貸借対照表が期首残高を欠いている疑いは、出力にも載せる
    // （画面で見た人と印刷した人が違うものを見ないように）。
    let notes = opening_balance_notes(&statements, from);

    std::fs::create_dir_all(&options.out_dir).map_err(|error| {
        format!(
            "出力先を作れませんでした: {}（{error}）",
            options.out_dir.display()
        )
    })?;

    let mut written = Vec::new();
    written.extend(write_pair(
        &options.out_dir,
        "journal_book",
        &kaikei_report::journal_book::to_csv(&entries, &chart),
        &kaikei_report::journal_book::to_html(&entries, &chart, &period_label, &[]),
    )?);
    written.extend(write_pair(
        &options.out_dir,
        "trial_balance",
        &kaikei_report::trial_balance::to_csv(&trial_balance, &chart),
        &kaikei_report::trial_balance::to_html(&trial_balance, &chart, &period_label, &[]),
    )?);
    written.extend(write_pair(
        &options.out_dir,
        "balance_sheet",
        &kaikei_report::statement::to_csv(&statements.balance_sheet),
        &kaikei_report::statement::to_html(&statements.balance_sheet, &period_label, &notes),
    )?);
    written.extend(write_pair(
        &options.out_dir,
        "income_statement",
        &kaikei_report::statement::to_csv(&statements.income_statement),
        &kaikei_report::statement::to_html(&statements.income_statement, &period_label, &[]),
    )?);

    if statements.entry_count == 0 {
        eprintln!(
            "注意: {} にはこの会計年度の仕訳が1件もありません。\
             年度の指定を確認してください",
            period_label
        );
    }
    Ok(written)
}

/// 期首残高が落ちている疑いの注記（`get_statements` と同じ判定）。
///
/// **同じ疑いを MCP の応答と CLI の出力の両方で伝える。** 片方にしか無いと、
/// 画面で見た人と印刷した人が違うものを見る。
fn opening_balance_notes(
    output: &statements::StatementsOutput,
    from: AccountingDate,
) -> Vec<String> {
    if output.entry_count == 0 {
        return Vec::new();
    }
    match output.first_entry_date {
        Some(first) if first > from => vec![format!(
            "集計対象で最も古い仕訳は {} で、会計年度の開始日（{}）から離れています。\
             その間に取引が無かっただけであれば問題ありませんが、期首残高の仕訳が\
             帳簿に無い場合、貸借対照表には前期から繰り越した残高が含まれません。",
            first.to_iso_string(),
            from.to_iso_string()
        )],
        _ => Vec::new(),
    }
}

/// CSV と HTML を1組書き出す。
fn write_pair(dir: &Path, name: &str, csv: &str, html: &str) -> Result<Vec<PathBuf>, String> {
    let csv_path = dir.join(format!("{name}.csv"));
    let html_path = dir.join(format!("{name}.html"));
    std::fs::write(&csv_path, csv)
        .map_err(|error| format!("書き出せませんでした: {}（{error}）", csv_path.display()))?;
    std::fs::write(&html_path, html)
        .map_err(|error| format!("書き出せませんでした: {}（{error}）", html_path.display()))?;
    Ok(vec![csv_path, html_path])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_a_complete_command() {
        let options = parse_args(&args(&["report", "--year", "2026", "--out", "./out"])).unwrap();
        assert_eq!(options.fiscal_year, 2026);
        assert_eq!(options.out_dir, PathBuf::from("./out"));
    }

    // 打ち間違いを黙って無視しない。既定値で埋めると「なぜか違う年度の
    // 帳簿が出た」になる。
    #[test]
    fn an_unknown_argument_is_rejected_with_the_usage() {
        let error = parse_args(&args(&["report", "--yea", "2026", "--out", "./out"])).unwrap_err();
        assert!(error.contains("--yea"), "{error}");
        assert!(error.contains("使い方"), "{error}");
    }

    #[test]
    fn a_missing_year_is_rejected() {
        let error = parse_args(&args(&["report", "--out", "./out"])).unwrap_err();
        assert!(error.contains("--year"), "{error}");
    }

    #[test]
    fn a_missing_out_is_rejected() {
        let error = parse_args(&args(&["report", "--year", "2026"])).unwrap_err();
        assert!(error.contains("--out"), "{error}");
    }

    #[test]
    fn a_non_numeric_year_is_rejected_showing_what_was_received() {
        let error =
            parse_args(&args(&["report", "--year", "令和8年", "--out", "./out"])).unwrap_err();
        assert!(error.contains("令和8年"), "受け取った値を示すこと: {error}");
    }

    #[test]
    fn no_arguments_shows_the_usage() {
        let error = parse_args(&[]).unwrap_err();
        assert!(error.contains("使い方"));
        assert!(error.contains("--year"));
    }

    #[test]
    fn an_unknown_subcommand_is_rejected() {
        let error = parse_args(&args(&["export", "--year", "2026"])).unwrap_err();
        assert!(error.contains("export"), "{error}");
    }

    // 使い方に、書き出すファイル名と要る環境変数が載っている
    // （読んだ人がそのまま実行できる。D-091 と同じ基準）。
    #[test]
    fn the_usage_names_the_files_and_the_environment_variables() {
        for expected in [
            "journal_book",
            "trial_balance",
            "balance_sheet",
            "income_statement",
            "APP_DATABASE_URL",
            "KAIKEI_BOOK_CURRENCY",
            "KAIKEI_FISCAL_YEAR_RULE",
        ] {
            assert!(USAGE.contains(expected), "使い方に無い: {expected}");
        }
    }
}
