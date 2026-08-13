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
use kaikei_app::usecase::ledger::{self, LedgerInput, MAX_LIMIT};
use kaikei_app::usecase::report::{self, ReportInput};
use kaikei_app::usecase::statements::{self, StatementsInput};
use kaikei_app::usecase::verify::{self, VerifyInput};
use kaikei_app::view::LedgerPageView;
use kaikei_core::{AccountingDate, FiscalYear};
use kaikei_jp::statement::JpStatementPolicy;
use kaikei_jp::tags::TagCatalog;
use kaikei_store::pool::{connect_app, PgStore};
use kaikei_store::query::{PgLedgerQuery, PgTrialBalanceQuery};
use std::path::{Path, PathBuf};

/// `--deduction` を省略したときの青色申告特別控除額（円）。
///
/// 複式簿記に加えて e-Tax申告または優良な電子帳簿保存の要件を満たす場合の額。
/// **このソフトは要件を判定しない**ので、既定値を使ったときも実行時に
/// 「何円を適用したか」と「要件は判定していない」ことを画面に出す。
const DEFAULT_BLUE_RETURN_DEDUCTION: i128 = 650_000;

const USAGE: &str = "\
kaikei — 帳簿を CSV と印刷用 HTML で書き出します

使い方:
    kaikei report --year <西暦> --out <出力先ディレクトリ>
    kaikei verify --year <西暦>

report は帳簿をファイルに書き出します。
verify は帳簿の整合性を検査します（書き出しません）。

引数:
    --year <西暦>       会計年度（暦年）。例: 2026
    --out <パス>        出力先のディレクトリ。無ければ作ります（report のみ）
    --deduction <円>    青色申告特別控除額。省略時は 650000（report のみ）
                        要件（複式簿記・e-Tax申告・優良な電子帳簿保存）を
                        満たすかどうかは、このソフトでは判定しません

書き出すもの（それぞれ .csv と .html）:
    journal_book      仕訳日記帳（1行1明細。取り消された仕訳も含みます）
    general_ledger    総勘定元帳（科目ごとの明細と残高の推移）
    trial_balance     試算表（借方合計・貸方合計・残高）
    balance_sheet     貸借対照表
    income_statement  損益計算書
    blue_return       青色申告決算書（損益計算書）の各欄のデータ
                      ※ 国税庁の様式そのものではありません

    このほか blue_return_not_on_form.csv に、決算書のどの欄にも
    載らなかった科目を理由付きで書き出します。

環境変数:
    APP_DATABASE_URL          帳簿の接続先（kaikei_app ロール）
    KAIKEI_BOOK_CURRENCY      帳簿通貨（例: JPY）
    KAIKEI_FISCAL_YEAR_RULE   会計年度の区切り（現在は calendar_year のみ）

verify が見るもの:
    同じ帳簿を2つの経路（仕訳からの集計と SQL 集計）で計算し、
    残高が一致するかを突き合わせます。あわせて赤伝の訂正元と
    仕訳番号の重複を確認します。

どちらも記帳はしません。読み取りだけなので、税区分や決算科目の設定は要りません。
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
    let command = parse_args(&args)?;

    // 同期の `main` から非同期の本体を回す。CLI は1回走って終わるので、
    // ランタイムをここで作って捨てる。
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|error| format!("非同期ランタイムを作れませんでした: {error}"))?;
    match command {
        Command::Report {
            fiscal_year,
            out_dir,
            deduction,
        } => runtime.block_on(write_reports(fiscal_year, out_dir, deduction)),
        Command::Verify { fiscal_year } => runtime.block_on(run_verify(fiscal_year)),
    }
}

/// 解析済みの引数。
#[derive(Debug)]
enum Command {
    /// 帳簿をファイルに書き出す。
    Report {
        fiscal_year: i32,
        out_dir: PathBuf,
        /// 青色申告特別控除額（円）。**帳簿からは決まらない**ので受け取る。
        deduction: i128,
    },
    /// 帳簿の整合性を検査する（書き出さない）。
    Verify { fiscal_year: i32 },
}

/// 引数を解析する。
///
/// **未知の引数を黙って無視しない。** `--yea 2026` のような打ち間違いを
/// 受理すると、既定の年度で出力が作られて「なぜか去年の帳簿が出た」に
/// なる（`CLAUDE.md` §11）。
fn parse_args(args: &[String]) -> Result<Command, String> {
    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        return Err(USAGE.to_string());
    }
    let subcommand = args[0].as_str();
    if subcommand != "report" && subcommand != "verify" {
        return Err(format!("不明なサブコマンドです: {subcommand}\n\n{USAGE}"));
    }

    let mut fiscal_year: Option<i32> = None;
    let mut out_dir: Option<PathBuf> = None;
    let mut deduction: Option<i128> = None;
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
            "--deduction" => {
                let value = rest.next().ok_or(
                    "--deduction の後に青色申告特別控除額を円で指定してください（例: --deduction 650000）",
                )?;
                let parsed = value.parse::<i128>().map_err(|_| {
                    format!("--deduction は円の数字で指定してください（受け取った値: {value}）")
                })?;
                // **負の控除額は受け取らない。** 所得金額が過大になる。
                if parsed < 0 {
                    return Err(format!(
                        "--deduction に負の値は指定できません（受け取った値: {value}）"
                    ));
                }
                deduction = Some(parsed);
            }
            other => {
                return Err(format!("不明な引数です: {other}\n\n{USAGE}"));
            }
        }
    }

    let fiscal_year = fiscal_year.ok_or("--year を指定してください（例: --year 2026）")?;
    match subcommand {
        "report" => Ok(Command::Report {
            fiscal_year,
            out_dir: out_dir.ok_or("--out を指定してください（例: --out ./out）")?,
            // 省略時は 65 万円（複式簿記 + e-Tax申告 または優良な電子帳簿保存）。
            // **要件を満たすかはソフトが判定しない**ので、適用した額は実行時に
            // 必ず画面へ出す（`write_blue_return` を参照）。
            deduction: deduction.unwrap_or(DEFAULT_BLUE_RETURN_DEDUCTION),
        }),
        // verify は書き出さないので --out を受け取らない。**黙って無視しない**
        // ——「出力先を指定したのに何も出なかった」と読まれる。
        _ => match (out_dir, deduction) {
            (Some(_), _) => Err("verify は書き出さないので --out は指定できません".to_string()),
            (_, Some(_)) => {
                Err("verify は決算書を出さないので --deduction は指定できません".to_string())
            }
            (None, None) => Ok(Command::Verify { fiscal_year }),
        },
    }
}

/// 帳簿を検査して結果を表示する。
///
/// **不整合が見つかったら終了コードを 1 にする。** 「検査が走った」ことと
/// 「異常が無かった」ことは別で、シェルから使うときに区別できる必要がある。
async fn run_verify(fiscal_year: i32) -> Result<Vec<PathBuf>, String> {
    let database_url = env_var("APP_DATABASE_URL")?;
    let catalog = TagCatalog::from_embedded(kaikei_jp_data::TAGS)
        .map_err(|error| format!("同梱のタグ定義を読めませんでした: {error}"))?;

    let pool = connect_app(&database_url)
        .await
        .map_err(|error| format!("PostgreSQL に接続できませんでした: {error}"))?;
    let store = PgStore::new(pool.clone());

    let schema = catalog.schema().clone();
    let output = with_tx_err(&store, move |tx| {
        let schema = schema.clone();
        // read model のクエリはクロージャの中で作る。外で作った参照を
        // 持ち込むと、`with_tx_err` が要求する 'static を満たせない。
        let query = PgTrialBalanceQuery::new(pool.clone());
        Box::pin(
            async move { verify::execute(tx, &query, &schema, VerifyInput { fiscal_year }).await },
        )
    })
    .await
    .map_err(|error: kaikei_app::error::AppError| format!("検査できませんでした: {error}"))?;

    println!("検査した仕訳: {} 件", output.entry_count);
    if output.is_clean() {
        println!("不整合は見つかりませんでした");
        return Ok(Vec::new());
    }

    eprintln!("不整合が {} 件見つかりました:", output.findings.len());
    for finding in &output.findings {
        eprintln!("  [{}] {}", finding.kind.as_code(), finding.detail);
    }
    Err(String::new())
}

/// 環境変数を1つ読む。**未設定は既定値で埋めない。**
fn env_var(name: &str) -> Result<String, String> {
    std::env::var(name)
        .map_err(|_| format!("環境変数 {name} が未設定です（.env.example を参照してください）"))
}

async fn write_reports(
    fiscal_year_label: i32,
    out_dir: PathBuf,
    deduction: i128,
) -> Result<Vec<PathBuf>, String> {
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

    let fiscal_year = FiscalYear::calendar_year(fiscal_year_label);
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

    // 総勘定元帳。科目ごとに引き、ページングを辿り切る。
    let ledger_query = PgLedgerQuery::new(pool.clone());
    let mut ledger_pages = Vec::new();
    for account in chart.iter() {
        let page = fetch_whole_ledger(&ledger_query, &account.code, from, to, &settings).await?;
        if kaikei_report::ledger::is_worth_printing(&page) {
            ledger_pages.push(page);
        }
    }

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

    std::fs::create_dir_all(&out_dir)
        .map_err(|error| format!("出力先を作れませんでした: {}（{error}）", out_dir.display()))?;

    let mut written = Vec::new();
    written.extend(write_pair(
        &out_dir,
        "journal_book",
        &kaikei_report::journal_book::to_csv(&entries, &chart),
        &kaikei_report::journal_book::to_html(&entries, &chart, &period_label, &[]),
    )?);
    written.extend(write_pair(
        &out_dir,
        "general_ledger",
        &kaikei_report::ledger::to_csv(&ledger_pages),
        &kaikei_report::ledger::to_html(&ledger_pages, &period_label, &[]),
    )?);
    written.extend(write_pair(
        &out_dir,
        "trial_balance",
        &kaikei_report::trial_balance::to_csv(&trial_balance, &chart),
        &kaikei_report::trial_balance::to_html(&trial_balance, &chart, &period_label, &[]),
    )?);
    written.extend(write_pair(
        &out_dir,
        "balance_sheet",
        &kaikei_report::statement::to_csv(&statements.balance_sheet),
        &kaikei_report::statement::to_html(&statements.balance_sheet, &period_label, &notes),
    )?);
    written.extend(write_pair(
        &out_dir,
        "income_statement",
        &kaikei_report::statement::to_csv(&statements.income_statement),
        &kaikei_report::statement::to_html(&statements.income_statement, &period_label, &[]),
    )?);

    written.extend(write_blue_return(
        &out_dir,
        &statements.income_statement,
        &period_label,
        deduction,
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

/// 青色申告決算書（損益計算書）のデータを書き出す。
///
/// **控除額はソフトが決めない。** 65万・55万・10万のどれになるかは複式簿記・
/// e-Tax申告・優良な電子帳簿保存といった要件で決まり、帳簿からは判定できない
/// （`docs/10-report.md` §5）。呼び出し側が渡した額をそのまま使い、**何円を
/// 適用したかを必ず画面に出す**——黙って既定値が入ると、要件を満たさない
/// 控除額のまま決算書が出る。
fn write_blue_return(
    out_dir: &Path,
    income_statement: &kaikei_app::policy::Statement,
    period_label: &str,
    deduction: i128,
) -> Result<Vec<PathBuf>, String> {
    let form = kaikei_jp::blue_return::load_embedded(kaikei_jp_data::STATEMENT_BLUE_RETURN_GENERAL)
        .map_err(|error| format!("決算書の当てはめ表を読めませんでした: {error}"))?;

    let mut inputs = std::collections::BTreeMap::new();
    inputs.insert(
        "blue_return_deduction".to_string(),
        kaikei_core::Money::from_minor(deduction, income_statement.total.currency()),
    );

    let filled = kaikei_jp::blue_return_fill::fill(&form, income_statement, &inputs)
        .map_err(|error| format!("決算書の金額を計算できませんでした: {error}"))?;

    let fields: Vec<kaikei_report::blue_return::FormRow> = filled
        .fields
        .iter()
        .map(|field| kaikei_report::blue_return::FormRow {
            no: field.no,
            label: field.label.clone(),
            amount: field.amount,
        })
        .collect();

    let not_on_form: Vec<kaikei_report::blue_return::NotOnFormRow> = filled
        .not_on_form
        .iter()
        .map(|entry| kaikei_report::blue_return::NotOnFormRow {
            account: entry.account.as_str().to_string(),
            label: entry.label.clone(),
            amount: entry.amount,
            reason: match &entry.reason {
                kaikei_jp::blue_return_fill::NotOnFormReason::Excluded(reason) => reason.clone(),
                kaikei_jp::blue_return_fill::NotOnFormReason::Unmapped => {
                    "当てはめ表にこの科目がありません。決算書のどの欄に入れるかを\
                     決めてください（黙って雑費には入れません）"
                        .to_string()
                }
            },
        })
        .collect();

    let title = format!("{}（{}）", filled.form, filled.part);
    let notes = vec![format!(
        "青色申告特別控除額として {} 円を適用しています。\
         控除額の要件（複式簿記・e-Tax申告・優良な電子帳簿保存）を\
         満たすかどうかは、このソフトでは判定していません。",
        deduction
    )];

    let mut written = write_pair(
        out_dir,
        "blue_return",
        &kaikei_report::blue_return::to_csv(&fields),
        &kaikei_report::blue_return::to_html(&title, period_label, &fields, &not_on_form, &notes),
    )?;

    // 載らなかった科目は本表と別の CSV にする（1つの CSV に2つの表を
    // 入れると表計算で読めなくなる）。0 件でも見出しだけのファイルを書く。
    let aside = out_dir.join("blue_return_not_on_form.csv");
    std::fs::write(
        &aside,
        kaikei_report::blue_return::not_on_form_to_csv(&not_on_form),
    )
    .map_err(|error| format!("書き出せませんでした: {}（{error}）", aside.display()))?;
    written.push(aside);

    // **適用した控除額を必ず出す。** 出力ファイルを見ない人にも伝わるように。
    println!("青色申告特別控除額 {deduction} 円を適用しました（要件の判定はしていません）");

    if !not_on_form.is_empty() {
        eprintln!(
            "注意: 決算書のどの欄にも載らなかった科目が {} 件あります。\
             blue_return_not_on_form.csv を確認してください",
            not_on_form.len()
        );
    }

    Ok(written)
}

/// 1科目の元帳を、ページングを辿り切って1つにまとめる。
///
/// **ここで取りこぼすと帳簿が静かに欠ける。** `get_ledger`（MCP）は1回で
/// 返せる上限を超えたら `next_cursor` を添えて切るが、帳簿として書き出す
/// ときに切ってよい理由は無い——読み手は「これで全部だ」と思って印刷する。
///
/// 集計値（期首・期末残高、借方・貸方合計、総行数）は**期間全体の値**なので
/// 1ページ目のものをそのまま使う（`LedgerPageView` の doc）。ページごとに
/// 足し直さないこと。
async fn fetch_whole_ledger(
    query: &PgLedgerQuery,
    account: &kaikei_core::AccountCode,
    from: AccountingDate,
    to: AccountingDate,
    settings: &BookSettings,
) -> Result<LedgerPageView, String> {
    let mut cursor = None;
    let mut merged: Option<LedgerPageView> = None;

    loop {
        let page = ledger::execute(
            query,
            settings,
            LedgerInput {
                account: account.clone(),
                from,
                to,
                cursor,
                limit: MAX_LIMIT,
            },
        )
        .await
        .map_err(|error| {
            format!(
                "元帳を読めませんでした（科目 {}）: {error}",
                account.as_str()
            )
        })?;

        cursor = page.next_cursor;
        match merged.as_mut() {
            // 2ページ目以降は行だけを継ぎ足す。集計値は期間全体の値なので
            // 1ページ目のものが正しい。
            Some(acc) => acc.rows.extend(page.rows),
            None => merged = Some(page),
        }
        if cursor.is_none() {
            break;
        }
    }

    let mut page = merged.expect("最低1ページは返る");
    // 全ページを辿り終えたので、切れ残りは無い。
    page.next_cursor = None;

    // 読み終えた行数が、read model が数えた総行数と一致すること。
    // **ページングの辿り漏れをここで検出する**——黙って短い元帳が出ると、
    // 印刷した人は気づけない。
    let collected = page.rows.len() as u64;
    if collected != page.total_lines {
        return Err(format!(
            "元帳の行を取りこぼしました（科目 {}）: 読めたのは {collected} 行ですが、             この期間の明細は {} 行あります",
            account.as_str(),
            page.total_lines
        ));
    }
    Ok(page)
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
    fn parses_a_complete_report_command() {
        let command = parse_args(&args(&["report", "--year", "2026", "--out", "./out"])).unwrap();
        match command {
            Command::Report {
                fiscal_year,
                out_dir,
                deduction,
            } => {
                assert_eq!(fiscal_year, 2026);
                assert_eq!(out_dir, PathBuf::from("./out"));
                assert_eq!(
                    deduction, DEFAULT_BLUE_RETURN_DEDUCTION,
                    "--deduction 省略時は既定値"
                );
            }
            other => panic!("report として解釈されるはず: {other:?}"),
        }
    }

    // 控除額は指定できる（65万の要件を満たさない場合に 55万・10万を選べる）。
    #[test]
    fn the_deduction_can_be_given_explicitly() {
        let command = parse_args(&args(&[
            "report",
            "--year",
            "2026",
            "--out",
            "./out",
            "--deduction",
            "100000",
        ]))
        .unwrap();
        match command {
            Command::Report { deduction, .. } => assert_eq!(deduction, 100_000),
            other => panic!("report として解釈されるはず: {other:?}"),
        }
    }

    // **負の控除額は拒否する。** 通すと所得金額が過大になる。
    #[test]
    fn a_negative_deduction_is_rejected() {
        let err = parse_args(&args(&[
            "report",
            "--year",
            "2026",
            "--out",
            "./out",
            "--deduction",
            "-1",
        ]))
        .expect_err("負の控除額は拒否されるはず");
        assert!(err.contains("負の値"), "{err}");
    }

    // verify は決算書を出さないので --deduction を黙って無視しない
    // （「指定したのに効かなかった」と読まれる）。
    #[test]
    fn verify_rejects_a_deduction_instead_of_ignoring_it() {
        let err = parse_args(&args(&[
            "verify",
            "--year",
            "2026",
            "--deduction",
            "650000",
        ]))
        .expect_err("verify では拒否されるはず");
        assert!(err.contains("--deduction"), "{err}");
    }

    #[test]
    fn parses_a_verify_command() {
        let command = parse_args(&args(&["verify", "--year", "2026"])).unwrap();
        match command {
            Command::Verify { fiscal_year } => assert_eq!(fiscal_year, 2026),
            other => panic!("verify として解釈されるはず: {other:?}"),
        }
    }

    // verify は書き出さないので --out を受け取らない。**黙って無視しない**
    // ——「出力先を指定したのに何も出なかった」と読まれる。
    #[test]
    fn verify_rejects_an_out_directory_instead_of_ignoring_it() {
        let error = parse_args(&args(&["verify", "--year", "2026", "--out", "./out"])).unwrap_err();
        assert!(error.contains("--out"), "{error}");
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
