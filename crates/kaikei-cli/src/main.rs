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

mod rules;

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
use kaikei_store::documents::PgDocumentQuery;
use kaikei_store::imported::PgImportedTxQuery;
use kaikei_store::pool::{connect_app, PgStore};
use kaikei_store::query::{PgLedgerQuery, PgTrialBalanceQuery};
use std::path::{Path, PathBuf};

/// `--deduction` を省略したときの青色申告特別控除額（円）。
///
/// 複式簿記に加えて e-Tax申告または優良な電子帳簿保存の要件を満たす場合の額。
/// **このソフトは要件を判定しない**ので、既定値を使ったときも実行時に
/// 「何円を適用したか」と「要件は判定していない」ことを画面に出す。
const DEFAULT_BLUE_RETURN_DEDUCTION: i128 = 650_000;

/// 検査の「疑い」を画面に出す件数の上限。
///
/// 正当な重複は普通にあるので、全部並べると本当の不整合が埋もれる。
const SUSPICIONS_TO_SHOW: usize = 5;

const USAGE: &str = "\
kaikei — 帳簿を CSV と印刷用 HTML で書き出します

使い方:
    kaikei report --year <西暦> --out <出力先ディレクトリ>
    kaikei verify --year <西暦>
    kaikei attach --file <ファイル> --date <取引年月日> --type <種別> --via <経路>
    kaikei import --profile <プロファイル.yaml> --file <明細.csv> [--commit]
    kaikei journalize --rules <ルール.yaml> [--year <西暦>]

report は帳簿をファイルに書き出します。
verify は帳簿の整合性を検査します（書き出しません）。
import は銀行・カードの明細 CSV を取り込みます。
journalize は取り込んだ明細にルールを当てて、仕訳の案を見せます。

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

    export.json       帳簿の全件エクスポート（このソフトが無くても読める形）
    blue_return       青色申告決算書（損益計算書）の各欄のデータ
                      ※ 国税庁の様式そのものではありません
    blue_return_balance_sheet
                      青色申告決算書（貸借対照表）の各行のデータ
                      期首・期末の2列。貸借が一致するかも確認します

    このほか blue_return_not_on_form.csv に、決算書のどの欄にも
    載らなかった科目を理由付きで書き出します。

    証憑が登録されていれば documents/ にも書き出します（KAIKEI_BLOB_ROOT が
    要ります）。保存はハッシュ、閲覧は「日付_取引先_金額_種別」の名前です。
    index.csv に元のファイル名とハッシュ、checksums.txt に各ファイルの
    SHA-256 が入ります。中身が変わっている証憑は書き出さず、理由を出します。

環境変数:
    APP_DATABASE_URL          帳簿の接続先（kaikei_app ロール）
    KAIKEI_BOOK_CURRENCY      帳簿通貨（例: JPY）
    KAIKEI_FISCAL_YEAR_RULE   会計年度の区切り（現在は calendar_year のみ）
    KAIKEI_BLOB_ROOT          証憑ファイルの保存先（verify で中身を検証する
                              ときに使う。未設定なら検証を行わず、その旨を出す）

verify が見るもの:
    同じ帳簿を2つの経路（仕訳からの集計と SQL 集計）で計算し、
    残高が一致するかを突き合わせます。あわせて赤伝の訂正元と
    仕訳番号の重複を確認します。
    証憑が登録されていれば、保存されているファイルの中身が
    帳簿の記録と一致するかも確かめます。

attach の引数:
    --file <パス>        取り込むファイル（必須）
    --date <YYYY-MM-DD>  取引年月日（必須。検索要件の1つ）
    --type <種別>        invoice / receipt / contract / other（必須）
    --via <経路>         email / download / scan / manual（必須）
    --amount <円>        取引金額（検索要件。無い証憑もあるので任意）
    --counterparty <名>  取引先（検索要件）
    --entry <UUID>       紐付ける仕訳のID
    --mime <型>          MIME タイプ（省略時は拡張子から決める）
    --note <文>          備考

    証憑は内容の SHA-256 で保存します。同じ内容を2回入れてもファイルは1つです。
    KAIKEI_BLOB_ROOT の指定が要ります。

import の引数:
    --profile <パス>     CSV プロファイル（YAML。列の対応を書いたもの。必須）
    --file <パス>        取り込む明細 CSV（必須）
    --profile-id <ID>    プロファイルが複数書かれている YAML から1つ選ぶ
    --source <ID>        取込元の名前。省略時はプロファイルのIDを使います
    --commit             実際に取り込みます

    **既定は下見です。** 引数に --commit が無ければ、読み取った内容を表示
    するだけで保存しません。取り込んだ明細は消せない（何を取り込んだかが
    追えなくなるため）ので、列の対応が合っているかを先に目で確かめます。

    同じ明細を2回取り込んでも重複しません。追記された行だけを取り込む
    使い方ができます。

    取り込んだだけでは帳簿に入りません。仕訳にするのは別の操作です。

journalize の引数:
    --rules <パス>       仕訳化ルール（YAML。必須）
    --year <西暦>        この年の明細だけを見ます
    --source <ID>        この取り込み元の明細だけを見ます

    **見せるだけで、まだ記帳しません。** どの明細にどのルールが当たるかと、
    ルールが無い明細を出します。ルールを書く手がかりにしてください。
    記帳するには MCP の post_journal_entry を使います。

report と verify は記帳しません。読み取りだけなので、税区分や決算科目の設定は
要りません。
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
            yayoi,
        } => runtime.block_on(write_reports(fiscal_year, out_dir, deduction, yayoi)),
        Command::Verify { fiscal_year } => runtime.block_on(run_verify(fiscal_year)),
        Command::Attach(args) => runtime.block_on(run_attach(args)),
        Command::Import(args) => runtime.block_on(run_import(args)),
        Command::Journalize(args) => runtime.block_on(run_journalize(args)),
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
        /// 弥生インポート形式の CSV も書き出すか。
        ///
        /// **既定では出さない。** 税込経理を前提にした出力なので、経理方式の
        /// 設定（`KAIKEI_TAX_MODE`）を確かめる必要があり、読む設定が増える。
        /// 必要な人だけが指定する。
        yayoi: bool,
    },
    /// 帳簿の整合性を検査する（書き出さない）。
    Verify { fiscal_year: i32 },
    /// 証憑を保存して帳簿に登録する。
    Attach(AttachArgs),
    /// 銀行・カードの明細 CSV を取り込む。
    Import(ImportArgs),
    /// 取り込んだ明細にルールを当てて、仕訳の案を見せる。
    Journalize(JournalizeArgs),
}

/// `kaikei journalize` の引数。
#[derive(Debug)]
struct JournalizeArgs {
    /// 仕訳化ルール（YAML）。
    rules: PathBuf,
    /// この年の明細だけを見る。
    fiscal_year: Option<i32>,
    /// この取り込み元の明細だけを見る。
    source: Option<String>,
}

/// `kaikei import` の引数。
#[derive(Debug)]
struct ImportArgs {
    /// CSV プロファイル（YAML）。
    profile: PathBuf,
    /// 取り込む明細 CSV。
    file: PathBuf,
    /// プロファイルが複数あるとき、どれを使うか。
    profile_id: Option<String>,
    /// 取込元の名前。省略時はプロファイルのID。
    source: Option<String>,
    /// 実際に保存するか。
    ///
    /// **既定は false（下見）。** 取り込んだ明細は消せないので、列の対応が
    /// 合っているかを先に目で確かめられるようにする。
    commit: bool,
}

/// `kaikei attach` の引数。
#[derive(Debug)]
struct AttachArgs {
    /// 取り込むファイル。
    file: PathBuf,
    /// 取引年月日（検索要件）。
    doc_date: AccountingDate,
    /// 取引金額（検索要件）。**無い証憑があるので Option**。
    amount_minor: Option<i64>,
    /// 取引先（検索要件）。
    counterparty: Option<String>,
    /// 種別（invoice / receipt / contract / other）。
    doc_type: String,
    /// 授受の経路（email / download / scan / manual）。
    received_via: String,
    /// MIME タイプ。省略時は拡張子から決める。
    mime_type: Option<String>,
    /// 紐付ける仕訳のID。
    entry_id: Option<String>,
    /// 備考。
    note: Option<String>,
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
    if subcommand == "attach" {
        return parse_attach(&args[1..]);
    }
    if subcommand == "import" {
        return parse_import(&args[1..]);
    }
    if subcommand == "journalize" {
        return parse_journalize(&args[1..]);
    }
    if subcommand != "report" && subcommand != "verify" {
        return Err(format!("不明なサブコマンドです: {subcommand}\n\n{USAGE}"));
    }

    let mut fiscal_year: Option<i32> = None;
    let mut out_dir: Option<PathBuf> = None;
    let mut deduction: Option<i128> = None;
    let mut yayoi = false;
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
            "--yayoi" => {
                yayoi = true;
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
            yayoi,
        }),
        // verify は書き出さないので --out を受け取らない。**黙って無視しない**
        // ——「出力先を指定したのに何も出なかった」と読まれる。
        _ => match (out_dir, deduction, yayoi) {
            (Some(_), _, _) => Err("verify は書き出さないので --out は指定できません".to_string()),
            (_, Some(_), _) => {
                Err("verify は決算書を出さないので --deduction は指定できません".to_string())
            }
            (_, _, true) => Err("verify は書き出さないので --yayoi は指定できません".to_string()),
            (None, None, false) => Ok(Command::Verify { fiscal_year }),
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

    // 証憑の検査に使う。**クロージャへ移す前に取っておく。**
    let documents = PgDocumentQuery::new(pool.clone());

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

    // 疑いは不整合と分けて出す。**混ぜると本当の不整合が埋もれる。**
    // 正当な重複（同じ日に同額の交通費など）は普通にあるので、疑いの件数は
    // 多くなりうる。全部並べず、件数と先頭だけ出す。
    let suspicions: Vec<_> = output.suspicions().collect();
    if !suspicions.is_empty() {
        println!("確認する価値のある点が {} 件あります:", suspicions.len());
        for finding in suspicions.iter().take(SUSPICIONS_TO_SHOW) {
            println!("  [{}] {}", finding.kind.as_code(), finding.detail);
        }
        if suspicions.len() > SUSPICIONS_TO_SHOW {
            println!(
                "  （ほか {} 件。いずれも誤りとは限りません）",
                suspicions.len() - SUSPICIONS_TO_SHOW
            );
        }
    }

    // 証憑の検証。**保存先が設定されていなければ、検証したふりをしない。**
    let document_findings = verify_documents(&documents).await?;
    for message in &document_findings {
        eprintln!("  [document] {message}");
    }

    if output.is_clean() && document_findings.is_empty() {
        println!("不整合は見つかりませんでした");
        return Ok(Vec::new());
    }

    let inconsistencies: Vec<_> = output.inconsistencies().collect();
    eprintln!(
        "不整合が {} 件見つかりました:",
        inconsistencies.len() + document_findings.len()
    );
    for finding in inconsistencies {
        eprintln!("  [{}] {}", finding.kind.as_code(), finding.detail);
    }
    Err(String::new())
}

/// 証憑を人間が読める名前で書き出す（`docs/06-documents.md` §5）。
///
/// 税務調査の「ダウンロードの求め」に応じられる形にする。保存はハッシュ、
/// 閲覧は人間が読める名前、という分担の閲覧側にあたる。
///
/// # 保存先が無いときに黙って飛ばさない
///
/// `KAIKEI_BLOB_ROOT` が未設定なら、証憑を書き出していないことを画面に出す
/// （`verify` と同じ方針）。証憑が1件も無ければ何も言わない。
///
/// # 書けなかったものを黙って落とさない
///
/// ファイルが保存先に無い、中身が変わっている、といった証憑は**書き出さずに
/// 知らせる**。欠けたまま「これで全部です」と提出されるのが最も困る。
async fn write_document_export(
    out_dir: &Path,
    query: &PgDocumentQuery,
    from: AccountingDate,
    to: AccountingDate,
) -> Result<Vec<PathBuf>, String> {
    use kaikei_app::ports::DocumentQueryPort;
    use kaikei_blob::BlobStore;

    let documents = query
        .search_documents(
            &kaikei_app::view::DocumentQuery {
                date_from: Some(from),
                date_to: Some(to),
                ..Default::default()
            },
            u32::MAX,
        )
        .await
        .map_err(|error| format!("証憑を読めませんでした: {error}"))?;

    if documents.is_empty() {
        return Ok(Vec::new());
    }

    let Ok(blob_root) = std::env::var("KAIKEI_BLOB_ROOT") else {
        eprintln!(
            "注意: この期間の証憑が {} 件ありますが、KAIKEI_BLOB_ROOT が未設定のため書き出していません",
            documents.len()
        );
        return Ok(Vec::new());
    };

    let store = kaikei_blob::LocalBlobStore::new(blob_root);
    let planned = kaikei_report::documents::plan_export(&documents);

    let dir = out_dir.join("documents");
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("作れませんでした: {}（{error}）", dir.display()))?;

    let mut written = Vec::new();
    let mut checksums = String::new();
    let mut failed: Vec<String> = Vec::new();

    for entry in &planned {
        let hash = match kaikei_core::BlobHash::parse_hex(&entry.document.blob_hash) {
            Ok(hash) => hash,
            Err(error) => {
                failed.push(format!(
                    "{}: ハッシュが不正です（{error}）",
                    entry.document.original_name
                ));
                continue;
            }
        };
        let bytes = match store.get(&hash).await {
            Ok(bytes) => bytes,
            Err(error) => {
                failed.push(format!("{}: {error}", entry.document.original_name));
                continue;
            }
        };
        // **書き出す前に中身を確かめる。** 変わっているものをそのまま提出用の
        // フォルダへ入れると、改変に気づかないまま提出することになる。
        if kaikei_blob::hash_of(&bytes) != hash {
            failed.push(format!(
                "{}: 中身が保存時から変わっています",
                entry.document.original_name
            ));
            continue;
        }

        let path = dir.join(&entry.file_name);
        std::fs::write(&path, &bytes)
            .map_err(|error| format!("書き出せませんでした: {}（{error}）", path.display()))?;
        checksums.push_str(&format!(
            "{}  {}\n",
            entry.document.blob_hash, entry.file_name
        ));
        written.push(path);
    }

    // 一覧は**書き出せたかどうかに関わらず全件**載せる。載っていない証憑が
    // あることを、受け取った側が index から気づけるようにする。
    let index_path = dir.join("index.csv");
    std::fs::write(
        &index_path,
        kaikei_report::documents::index_to_csv(&planned),
    )
    .map_err(|error| format!("書き出せませんでした: {}（{error}）", index_path.display()))?;
    written.push(index_path);

    let checksums_path = dir.join("checksums.txt");
    std::fs::write(&checksums_path, checksums).map_err(|error| {
        format!(
            "書き出せませんでした: {}（{error}）",
            checksums_path.display()
        )
    })?;
    written.push(checksums_path);

    println!(
        "証憑を書き出しました: {} 件（全 {} 件）",
        written.len() - 2,
        planned.len()
    );
    if !failed.is_empty() {
        eprintln!("注意: 書き出せなかった証憑が {} 件あります:", failed.len());
        for message in &failed {
            eprintln!("  {message}");
        }
    }
    Ok(written)
}

/// `kaikei attach` の引数を解析する。
///
/// **必須のものを既定値で埋めない。** 取引年月日・種別・授受の経路は後から
/// 復元できない情報なので、指定が無ければ止める。
fn parse_attach(args: &[String]) -> Result<Command, String> {
    let mut file: Option<PathBuf> = None;
    let mut doc_date: Option<AccountingDate> = None;
    let mut amount_minor: Option<i64> = None;
    let mut counterparty: Option<String> = None;
    let mut doc_type: Option<String> = None;
    let mut received_via: Option<String> = None;
    let mut mime_type: Option<String> = None;
    let mut entry_id: Option<String> = None;
    let mut note: Option<String> = None;

    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let key = arg.as_str();
        let mut take = || {
            rest.next()
                .cloned()
                .ok_or_else(|| format!("{key} の後に値を指定してください"))
        };
        match key {
            "--file" => file = Some(PathBuf::from(take()?)),
            "--date" => {
                let text = take()?;
                doc_date = Some(AccountingDate::parse(&text).map_err(|error| {
                    format!("--date は YYYY-MM-DD で指定してください（{text}: {error}）")
                })?);
            }
            "--amount" => {
                let text = take()?;
                amount_minor = Some(text.parse::<i64>().map_err(|_| {
                    format!("--amount は円の数字で指定してください（受け取った値: {text}）")
                })?);
            }
            "--counterparty" => counterparty = Some(take()?),
            "--type" => doc_type = Some(take()?),
            "--via" => received_via = Some(take()?),
            "--mime" => mime_type = Some(take()?),
            "--entry" => entry_id = Some(take()?),
            "--note" => note = Some(take()?),
            other => return Err(format!("不明な引数です: {other}")),
        }
    }

    Ok(Command::Attach(AttachArgs {
        file: file.ok_or("--file を指定してください")?,
        doc_date: doc_date.ok_or("--date を指定してください（例: --date 2026-06-15）")?,
        amount_minor,
        counterparty,
        doc_type: doc_type
            .ok_or("--type を指定してください（invoice / receipt / contract / other）")?,
        received_via: received_via
            .ok_or("--via を指定してください（email / download / scan / manual）")?,
        mime_type,
        entry_id,
        note,
    }))
}

/// 拡張子から MIME タイプを決める。
///
/// **分からなければ推測しない。** 誤った型で保存すると、後から中身が何かを
/// 判別できなくなる。`--mime` で明示してもらう。
fn mime_from_extension(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match extension.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "csv" => "text/csv",
        "txt" => "text/plain",
        "html" | "htm" => "text/html",
        "xml" => "application/xml",
        "json" => "application/json",
        "zip" => "application/zip",
        _ => return None,
    })
}

/// 証憑を保存して帳簿に登録する。
///
/// **保存と登録を必ず両方行う。** ファイルだけ保存して帳簿に登録しないと、
/// 中身はあるのに誰も辿れない証憑が残る。
async fn run_attach(args: AttachArgs) -> Result<Vec<PathBuf>, String> {
    use kaikei_app::ports::{DocumentQueryPort, DocumentRepo, NewDocument};
    use kaikei_blob::BlobStore;

    let mime_type = match &args.mime_type {
        Some(mime) => mime.clone(),
        None => mime_from_extension(&args.file)
            .ok_or_else(|| {
                format!(
                    "{} の MIME タイプを拡張子から決められません。--mime で指定してください",
                    args.file.display()
                )
            })?
            .to_string(),
    };

    let bytes = std::fs::read(&args.file)
        .map_err(|error| format!("読めませんでした: {}（{error}）", args.file.display()))?;
    let original_name = args
        .file
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("ファイル名を取れません: {}", args.file.display()))?
        .to_string();

    let blob_root = env_var("KAIKEI_BLOB_ROOT")?;
    let store = kaikei_blob::LocalBlobStore::new(blob_root);
    store
        .prepare()
        .await
        .map_err(|error| format!("証憑の保存先を用意できませんでした: {error}"))?;
    let hash = store
        .put(&bytes)
        .await
        .map_err(|error| format!("証憑を保存できませんでした: {error}"))?;

    let database_url = env_var("APP_DATABASE_URL")?;
    let pool = connect_app(&database_url)
        .await
        .map_err(|error| format!("PostgreSQL に接続できませんでした: {error}"))?;

    // **同じ内容が既に登録されていれば知らせる。** 同じ請求書を別の取引の
    // 証憑にすることはあるので止めはしないが、二重登録に気づけるようにする。
    let query = PgDocumentQuery::new(pool.clone());
    let existing = query
        .search_documents(&kaikei_app::view::DocumentQuery::default(), 200)
        .await
        .map_err(|error| format!("既存の証憑を調べられませんでした: {error}"))?;
    let same: Vec<&str> = existing
        .iter()
        .filter(|doc| doc.blob_hash == hash.to_hex())
        .map(|doc| doc.original_name.as_str())
        .collect();
    if !same.is_empty() {
        eprintln!(
            "注意: 同じ内容の証憑が既に {} 件登録されています（{}）。別の取引の証憑にするなら問題ありません",
            same.len(),
            same.join("・")
        );
    }

    let document = NewDocument {
        id: uuid::Uuid::now_v7().to_string(),
        blob_hash: hash.to_hex(),
        original_name,
        mime_type,
        byte_size: bytes.len() as i64,
        doc_date: args.doc_date,
        amount_minor: args.amount_minor,
        counterparty: args.counterparty,
        doc_type: args.doc_type,
        received_via: args.received_via,
        received_at: kaikei_core::Timestamp::from_unix_nanos(now_unix_nanos()?),
        note: args.note,
    };
    let document_id = document.id.clone();
    let entry = args.entry_id.as_deref().map(parse_entry_id).transpose()?;

    let store_pg = PgStore::new(pool);
    with_tx_err(&store_pg, move |tx| {
        let document = document.clone();
        let document_id = document_id.clone();
        Box::pin(async move {
            tx.insert_document(&document).await?;
            // **登録と紐付けを同じトランザクションで行う。** 片方だけ残ると
            // 帳簿から証憑への道筋が壊れる。
            if let Some(entry) = entry {
                tx.link_document(entry, &document_id).await?;
            }
            Ok::<(), kaikei_app::error::AppError>(())
        })
    })
    .await
    .map_err(|error: kaikei_app::error::AppError| format!("証憑を登録できませんでした: {error}"))?;

    println!("証憑を登録しました: {}", hash.to_hex());
    if entry.is_some() {
        println!("  仕訳に紐付けました");
    }
    Ok(Vec::new())
}

/// `kaikei import` の引数を解析する。
fn parse_import(args: &[String]) -> Result<Command, String> {
    let mut profile = None;
    let mut file = None;
    let mut profile_id = None;
    let mut source = None;
    let mut commit = false;

    let mut index = 0;
    while index < args.len() {
        let key = args[index].as_str();
        // 値を取る引数と、取らない引数を混ぜない。`--commit` の次を値として
        // 食べてしまうと、`--commit --file x` が黙って通る。
        if key == "--commit" {
            commit = true;
            index += 1;
            continue;
        }
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("{key} の値がありません"))?
            .clone();
        match key {
            "--profile" => profile = Some(PathBuf::from(value)),
            "--file" => file = Some(PathBuf::from(value)),
            "--profile-id" => profile_id = Some(value),
            "--source" => source = Some(value),
            other => return Err(format!("不明な引数です: {other}\n\n{USAGE}")),
        }
        index += 2;
    }

    Ok(Command::Import(ImportArgs {
        profile: profile.ok_or("--profile を指定してください（列の対応を書いた YAML）")?,
        file: file.ok_or("--file を指定してください（取り込む明細 CSV）")?,
        profile_id,
        source,
        commit,
    }))
}

/// 下見で並べて見せる明細の件数。
///
/// 全部出すと画面が流れて、肝心の先頭（列がずれていれば真っ先に分かる）が
/// 見えなくなる。
const IMPORT_PREVIEW_ROWS: usize = 10;

/// 銀行・カードの明細 CSV を取り込む。
///
/// # 既定では保存しない
///
/// 取り込んだ明細は消せない（何を取り込んだかが追えなくなるため、
/// `imported_transactions` の DELETE は与えていない）。プロファイルの列指定を
/// 間違えたまま保存すると、桁の狂った明細が残り続ける。**`--commit` が無ければ
/// 読んで見せるだけにする。**
async fn run_import(args: ImportArgs) -> Result<Vec<PathBuf>, String> {
    use kaikei_app::ports::{
        ImportDirection, ImportOutcome, ImportedTxRepo, NewImportedTransaction,
    };

    let yaml = std::fs::read_to_string(&args.profile).map_err(|error| {
        format!(
            "プロファイルを読めませんでした: {}（{error}）",
            args.profile.display()
        )
    })?;
    let profiles = kaikei_import::profile::CsvProfile::load_all(&yaml)
        .map_err(|error| format!("プロファイルを読めませんでした: {error}"))?;
    let profile = choose_profile(profiles, args.profile_id.as_deref())?;

    // **文字コードを推測で決めない。** 邦銀の明細は Shift-JIS が多いが、
    // 読めないものを置換文字で埋めると摘要が壊れたまま帳簿に入る。
    let bytes = std::fs::read(&args.file)
        .map_err(|error| format!("読めませんでした: {}（{error}）", args.file.display()))?;
    let text = kaikei_import::decode_csv(&bytes)
        .map_err(|error| format!("{}: {error}", args.file.display()))?;

    let source_id = args.source.clone().unwrap_or_else(|| profile.id.clone());
    let source = kaikei_import::SourceId::parse(&source_id)
        .map_err(|error| format!("--source が不正です: {error}"))?;

    let parsed = kaikei_import::reader::parse_csv(&profile, &source, &text)
        .map_err(|error| format!("{}: {error}", args.file.display()))?;

    println!(
        "読み取り: {}件（エラー {}件）  プロファイル: {}",
        parsed.transactions.len(),
        parsed.errors.len(),
        profile.name
    );
    for row in parsed.transactions.iter().take(IMPORT_PREVIEW_ROWS) {
        println!(
            "  {}  {}  {:>12}  {}",
            row.occurred_on,
            match row.direction {
                kaikei_import::Direction::In => "入金",
                kaikei_import::Direction::Out => "出金",
            },
            group_digits(row.amount_minor),
            row.raw_description
        );
    }
    if parsed.transactions.len() > IMPORT_PREVIEW_ROWS {
        println!(
            "  ...（残り {}件）",
            parsed.transactions.len() - IMPORT_PREVIEW_ROWS
        );
    }
    // **読めなかった行は必ず全部出す。** 件数だけ出して中身を隠すと、
    // 「エラー3件」を見なかったことにして先へ進んでしまう。
    for error in &parsed.errors {
        eprintln!("  {}行目: {}", error.line, error.reason);
    }

    if !args.commit {
        println!("※ 下見です。取り込むには --commit を付けてください");
        return Ok(Vec::new());
    }

    let database_url = env_var("APP_DATABASE_URL")?;
    let pool = connect_app(&database_url)
        .await
        .map_err(|error| format!("PostgreSQL に接続できませんでした: {error}"))?;
    let store = PgStore::new(pool);

    let imported_at = kaikei_core::Timestamp::from_unix_nanos(now_unix_nanos()?);
    let rows: Vec<NewImportedTransaction> = parsed
        .transactions
        .iter()
        .map(|row| {
            Ok(NewImportedTransaction {
                id: uuid::Uuid::now_v7().to_string(),
                source: row.source.as_str().to_string(),
                external_key: row.external_key.clone(),
                occurred_on: to_accounting_date(row.occurred_on)?,
                amount_minor: row.amount_minor,
                direction: match row.direction {
                    kaikei_import::Direction::In => ImportDirection::In,
                    kaikei_import::Direction::Out => ImportDirection::Out,
                },
                raw_description: row.raw_description.clone(),
                balance_after: row.balance_after,
                raw_row: row.raw_row.to_string(),
                imported_at,
            })
        })
        .collect::<Result<_, String>>()?;

    // **1つのトランザクションで入れる。** 途中で落ちたときに半分だけ入ると、
    // どこまで取り込んだかを人が数え直すことになる（消せないので余計に困る）。
    let outcomes = with_tx_err(&store, move |tx| {
        let rows = rows.clone();
        Box::pin(async move {
            let mut inserted = 0usize;
            let mut skipped = 0usize;
            for row in &rows {
                match tx.insert_imported(row).await? {
                    ImportOutcome::Inserted => inserted += 1,
                    ImportOutcome::SkippedDuplicate => skipped += 1,
                }
            }
            Ok::<(usize, usize), kaikei_app::error::AppError>((inserted, skipped))
        })
    })
    .await
    .map_err(|error: kaikei_app::error::AppError| format!("取り込めませんでした: {error}"))?;

    println!(
        "取り込みました: 新規 {}件 / 重複 {}件",
        outcomes.0, outcomes.1
    );
    println!("※ まだ仕訳ではありません。帳簿に入れるには仕訳化が要ります");
    Ok(Vec::new())
}

/// 使うプロファイルを1つ選ぶ。
///
/// **どれを使ったか分からないまま進めない。** 複数あるのに指定が無ければ、
/// 候補を並べて止める。勝手に先頭を使うと、別の銀行の列の対応で読んで
/// 桁が狂う。
fn choose_profile(
    profiles: Vec<kaikei_import::profile::CsvProfile>,
    wanted: Option<&str>,
) -> Result<kaikei_import::profile::CsvProfile, String> {
    let available = || {
        profiles
            .iter()
            .map(|p| p.id.as_str())
            .collect::<Vec<_>>()
            .join("・")
    };
    match wanted {
        Some(id) => profiles
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .ok_or_else(|| {
                format!(
                    "プロファイル {id} がありません（あるのは: {}）",
                    available()
                )
            }),
        None if profiles.len() == 1 => Ok(profiles.into_iter().next().expect("1件ある")),
        None if profiles.is_empty() => Err("プロファイルが1つも書かれていません".to_string()),
        None => Err(format!(
            "プロファイルが複数あります。--profile-id で選んでください（{}）",
            available()
        )),
    }
}

/// `chrono` の日付を帳簿の日付に直す。
fn to_accounting_date(date: chrono::NaiveDate) -> Result<AccountingDate, String> {
    use chrono::Datelike;
    let month = u8::try_from(date.month()).map_err(|_| format!("月が範囲外です: {date}"))?;
    let day = u8::try_from(date.day()).map_err(|_| format!("日が範囲外です: {date}"))?;
    AccountingDate::new(date.year(), month, day)
        .map_err(|error| format!("取り込めない日付です（{date}）: {error}"))
}

/// 3桁ごとに区切る。
fn group_digits(amount: i64) -> String {
    let digits = amount.abs().to_string();
    let mut out = String::new();
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    if amount < 0 {
        format!("-{out}")
    } else {
        out
    }
}

/// `kaikei journalize` の引数を解析する。
fn parse_journalize(args: &[String]) -> Result<Command, String> {
    let mut rules = None;
    let mut fiscal_year = None;
    let mut source = None;

    let mut index = 0;
    while index < args.len() {
        let key = args[index].as_str();
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("{key} の値がありません"))?
            .clone();
        match key {
            "--rules" => rules = Some(PathBuf::from(value)),
            "--year" => {
                fiscal_year = Some(
                    value
                        .parse::<i32>()
                        .map_err(|_| format!("--year は西暦で指定してください: {value}"))?,
                )
            }
            "--source" => source = Some(value),
            other => return Err(format!("不明な引数です: {other}\n\n{USAGE}")),
        }
        index += 2;
    }

    Ok(Command::Journalize(JournalizeArgs {
        rules: rules.ok_or("--rules を指定してください（仕訳化ルールの YAML）")?,
        fiscal_year,
        source,
    }))
}

/// 一度に見る明細の上限。
const JOURNALIZE_LIMIT: u32 = 200;

/// ルールが当たった明細を、いくつまで並べて見せるか。
const JOURNALIZE_PREVIEW: usize = 10;

/// ルールが無い摘要を、いくつまで並べて見せるか。
///
/// 多い順に出す。**次にどのルールを書けば一番効くか**が分かるようにするため。
const UNMATCHED_TO_SHOW: usize = 15;

/// 取り込んだ明細にルールを当てて、仕訳の案を見せる。
///
/// # まだ記帳しない
///
/// 帳簿は追記のみで、入れた仕訳は消せない（訂正は逆仕訳）。ルールが正しいか
/// 分からないうちに自動で記帳すると、逆仕訳の山を作ることになる。**まずは
/// 当たり方を見せる**ことに徹する。
async fn run_journalize(args: JournalizeArgs) -> Result<Vec<PathBuf>, String> {
    use kaikei_app::journalize::{self, MatchTarget};
    use kaikei_app::ports::ImportedTxQuery;
    use kaikei_app::view::ImportedTxQuerySpec;

    let yaml = std::fs::read_to_string(&args.rules).map_err(|error| {
        format!(
            "ルールを読めませんでした: {}（{error}）",
            args.rules.display()
        )
    })?;
    let rules = rules::load_rules(&yaml)?;
    let active = rules.iter().filter(|rule| rule.active).count();
    println!("ルール: {}件（うち有効 {}件）", rules.len(), active);

    let currency_code = env_var("KAIKEI_BOOK_CURRENCY")?;
    let currency = kaikei_app::currency::currency_from_code(&currency_code)
        .map_err(|error| format!("KAIKEI_BOOK_CURRENCY が不正です: {error}"))?;

    let database_url = env_var("APP_DATABASE_URL")?;
    let pool = connect_app(&database_url)
        .await
        .map_err(|error| format!("PostgreSQL に接続できませんでした: {error}"))?;

    let (date_from, date_to) = match args.fiscal_year {
        Some(year) => {
            let fiscal_year = FiscalYear::calendar_year(year);
            (Some(fiscal_year.start()), Some(fiscal_year.end()))
        }
        None => (None, None),
    };

    let query = PgImportedTxQuery::new(pool.clone());
    let pending = query
        .list_imported(
            &ImportedTxQuerySpec {
                source: args.source.clone(),
                status: Some("pending".to_string()),
                date_from,
                date_to,
            },
            JOURNALIZE_LIMIT,
        )
        .await
        .map_err(|error| format!("未処理の明細を読めませんでした: {error}"))?;

    // **0件の意味を取り違えさせない。** 全部片付いたのか、そもそも取り込んで
    // いないのかで、次にやることが正反対になる。
    let counts = query
        .import_status_counts(args.source.as_deref())
        .await
        .map_err(|error| format!("取込の状況を読めませんでした: {error}"))?;
    println!(
        "取込: 全{}件（未処理 {} / 仕訳済み {} / 無視 {}）",
        counts.total(),
        counts.pending,
        counts.journalized,
        counts.ignored
    );
    if counts.total() == 0 {
        println!("※ まだ1件も取り込んでいません。kaikei import で取り込んでください");
        return Ok(Vec::new());
    }
    if pending.is_empty() {
        println!("※ 未処理の明細はありません");
        return Ok(Vec::new());
    }

    // 科目名を出すために勘定科目表を読む。コードだけだと、500 と 501 を
    // 取り違えたルールに気づけない。
    let store = PgStore::new(pool);
    let chart = with_tx_err(&store, |tx| Box::pin(async move { tx.load_chart().await }))
        .await
        .map_err(|error: kaikei_app::error::RepoError| {
            format!("勘定科目表を読めませんでした: {error}")
        })?;

    let mut matched = Vec::new();
    let mut unmatched: Vec<&kaikei_app::view::ImportedTxView> = Vec::new();
    for row in &pending {
        let target = MatchTarget {
            source: &row.source,
            occurred_on: row.occurred_on,
            amount_minor: row.amount_minor,
            direction: if row.is_money_in {
                kaikei_app::ports::ImportDirection::In
            } else {
                kaikei_app::ports::ImportDirection::Out
            },
            raw_description: &row.raw_description,
        };
        match journalize::first_matching(&rules, &target) {
            Some(rule) => {
                let built = journalize::build_entry(rule, &target, currency)
                    .map_err(|error| format!("ルール {} で仕訳を作れません: {error}", rule.id))?;
                matched.push((row, built));
            }
            None => unmatched.push(row),
        }
    }

    println!(
        "\n未処理 {}件のうち、ルールが当たったのは {}件（当たらなかったのは {}件）",
        pending.len(),
        matched.len(),
        unmatched.len()
    );

    for (row, built) in matched.iter().take(JOURNALIZE_PREVIEW) {
        println!(
            "\n  {}  {}",
            row.occurred_on.to_iso_string(),
            row.raw_description
        );
        for line in &built.entry.lines {
            let name = chart
                .get(line.account())
                .map(|def| def.name.as_str())
                // **知らない科目を黙って通さない。** 勘定科目表に無いコードを
                // 書いたルールは、記帳の段で必ず落ちる。ここで見えるようにする。
                .unwrap_or("※この科目は勘定科目表にありません");
            println!(
                "    {} {} {}  {:>12}",
                match line.side() {
                    kaikei_core::Side::Debit => "借",
                    kaikei_core::Side::Credit => "貸",
                },
                line.account().as_str(),
                name,
                group_digits(i64::try_from(line.amount().minor()).unwrap_or(i64::MAX))
            );
        }
        println!("    ルール: {}", built.rule_id);
    }
    if matched.len() > JOURNALIZE_PREVIEW {
        println!("\n  ...（ほか {}件）", matched.len() - JOURNALIZE_PREVIEW);
    }

    if !unmatched.is_empty() {
        println!("\nルールが無い明細（多い順）:");
        for (description, count, total) in summarize_unmatched(&unmatched) {
            println!(
                "  {count:>3}件  {:>12}円  {description}",
                group_digits(total)
            );
        }
    }

    println!("\n※ 見せただけで、まだ記帳していません");
    Ok(Vec::new())
}

/// ルールが無い明細を、摘要ごとにまとめる。
///
/// **多い順に返す。** 次にどのルールを書けば一番効くかが分かるようにする。
/// 同数のときは摘要の順で決める（並びが実行のたびに変わらないように）。
fn summarize_unmatched(rows: &[&kaikei_app::view::ImportedTxView]) -> Vec<(String, usize, i64)> {
    let mut groups: std::collections::BTreeMap<String, (usize, i64)> =
        std::collections::BTreeMap::new();
    for row in rows {
        let entry = groups.entry(row.raw_description.clone()).or_insert((0, 0));
        entry.0 += 1;
        entry.1 = entry.1.saturating_add(row.amount_minor);
    }
    let mut summary: Vec<(String, usize, i64)> = groups
        .into_iter()
        .map(|(description, (count, total))| (description, count, total))
        .collect();
    summary.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    summary.truncate(UNMATCHED_TO_SHOW);
    summary
}

/// 仕訳IDを解釈する。
fn parse_entry_id(text: &str) -> Result<kaikei_core::EntryId, String> {
    let uuid = uuid::Uuid::parse_str(text)
        .map_err(|error| format!("--entry は UUID で指定してください（{text}: {error}）"))?;
    Ok(kaikei_core::EntryId::new(uuid.as_u128()))
}

/// 現在時刻（UNIX ナノ秒）。
fn now_unix_nanos() -> Result<i128, String> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as i128)
        .map_err(|error| format!("現在時刻を取れませんでした: {error}"))
}

/// 証憑の中身が保存時から変わっていないかを確かめる。
///
/// `docs/06-documents.md` §6。**この検査が「改変されていないことを証明できる」
/// という価値の実体**である。
///
/// # 保存先が無いときに「検証済み」と言わない
///
/// `KAIKEI_BLOB_ROOT` が未設定なら、証憑の検証は**行っていない**ことを画面に
/// 出して素通りする。黙って通すと、一度も検証していない帳簿が「不整合は
/// 見つかりませんでした」と表示されることになる。
///
/// 帳簿に証憑が1件も登録されていなければ、設定が無くても何も言わない
/// （証憑を使っていない人に設定を求めない）。
async fn verify_documents(query: &PgDocumentQuery) -> Result<Vec<String>, String> {
    use kaikei_app::ports::DocumentQueryPort;
    use kaikei_blob::BlobStore;

    let hashes = query
        .all_blob_hashes()
        .await
        .map_err(|error| format!("証憑の一覧を読めませんでした: {error}"))?;

    if hashes.is_empty() {
        return Ok(Vec::new());
    }

    let Ok(root) = std::env::var("KAIKEI_BLOB_ROOT") else {
        println!(
            concat!(
                "注意: 証憑が {} 件ありますが、KAIKEI_BLOB_ROOT が未設定のため",
                "中身の検証は行っていません"
            ),
            hashes.len()
        );
        return Ok(Vec::new());
    };

    let store = kaikei_blob::LocalBlobStore::new(root);
    let mut findings = Vec::new();
    let mut verified = 0usize;
    for hex in &hashes {
        let hash = match kaikei_core::BlobHash::parse_hex(hex) {
            Ok(hash) => hash,
            Err(error) => {
                findings.push(format!("証憑のハッシュが不正です: {hex}（{error}）"));
                continue;
            }
        };
        match store.verify(&hash).await {
            // **中身が変わっている。** 最も知りたいのがこれ。
            Ok(false) => findings.push(format!(
                "証憑の中身が保存時から変わっています: {hex}。帳簿の記録と一致しません"
            )),
            Ok(true) => verified += 1,
            // 「無い」と「変わっている」を分けて言う。
            Err(kaikei_blob::BlobError::NotFound { hash }) => {
                findings.push(format!("証憑のファイルが保存先に見つかりません: {hash}"))
            }
            Err(error) => findings.push(format!("証憑を検証できませんでした: {error}")),
        }
    }
    println!("検証した証憑: {verified} 件（全 {} 件）", hashes.len());
    Ok(findings)
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
    yayoi: bool,
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
    // 証憑のエクスポートに使う。**クロージャへ移す前に取っておく。**
    let pool_for_documents = PgDocumentQuery::new(pool.clone());

    // 仕訳と勘定科目表を1つのトランザクションで読む。**間に記帳が入ると、
    // 仕訳日記帳と財務諸表が別の帳簿を映す。**
    let schema = catalog.schema().clone();
    let (entries, chart, statements, cumulative, opening_balance_sheet) =
        with_tx_err(&store, move |tx| {
            let schema = schema.clone();
            Box::pin(async move {
                let chart = tx.load_chart().await?;
                let entries = tx.list_entries_in_period(from, to).await?;
                let policy = JpStatementPolicy::new(chart.clone());
                // 損益計算書は**会計年度の期間**。その期間の損益そのものである。
                let statements =
                    statements::execute(tx, &policy, &schema, StatementsInput { from, to }).await?;
                // 貸借対照表は**帳簿の最初からの累計**。ある時点の残高であって
                // 期間の増減ではないので、期首残高（前期末の仕訳）を含めるには
                // 会計年度より前まで遡る必要がある。この非対称は会計の性質で
                // あって実装の都合ではない（`usecase::statements` のモジュール
                // doc「貸借対照表には期首残高が要る」）。
                let cumulative = statements::execute(
                    tx,
                    &policy,
                    &schema,
                    StatementsInput {
                        from: book_beginning(),
                        to,
                    },
                )
                .await?;
                // 決算書の貸借対照表は期首列も要る。期首＝会計年度の開始日の
                // **前日**までの累計（期首残高は前期末の日付で記帳する）。
                let opening_balance_sheet = statements::execute(
                    tx,
                    &policy,
                    &schema,
                    StatementsInput {
                        from: book_beginning(),
                        to: day_before(from),
                    },
                )
                .await?;
                Ok((
                    entries,
                    chart,
                    statements,
                    cumulative,
                    opening_balance_sheet,
                ))
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

    // 貸借対照表が何を集計したものかを出力に載せる。**会計年度の増減では
    // なく残高である**ことが読む人に伝わらないと、期首残高の入れ忘れにも
    // 気づけない。
    let notes = balance_sheet_notes(&cumulative, to);

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
        &kaikei_report::statement::to_csv(&cumulative.balance_sheet),
        &kaikei_report::statement::to_html(
            &cumulative.balance_sheet,
            &format!("{} 現在", to.to_iso_string()),
            &notes,
        ),
    )?);
    written.extend(write_pair(
        &out_dir,
        "income_statement",
        &kaikei_report::statement::to_csv(&statements.income_statement),
        &kaikei_report::statement::to_html(&statements.income_statement, &period_label, &[]),
    )?);

    let (blue_return_paths, blue_return_fields) = write_blue_return(
        &out_dir,
        &statements.income_statement,
        &period_label,
        deduction,
    )?;
    written.extend(blue_return_paths);

    written.extend(write_blue_return_balance_sheet(
        &out_dir,
        &opening_balance_sheet.balance_sheet,
        &cumulative.balance_sheet,
        to,
        &blue_return_fields,
    )?);

    // 全件 JSON。**この出力はこのソフトが消えてもデータが残るためのもの**
    // なので、既定で必ず出す（docs/03-database.md §8）。
    let export_path = out_dir.join("export.json");
    std::fs::write(
        &export_path,
        kaikei_report::export::to_json(&entries, &chart),
    )
    .map_err(|error| format!("書き出せませんでした: {}（{error}）", export_path.display()))?;
    written.push(export_path);

    if yayoi {
        written.extend(write_yayoi(&out_dir, &entries, &chart)?);
    }

    written.extend(write_document_export(&out_dir, &pool_for_documents, from, to).await?);

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
) -> Result<
    (
        Vec<PathBuf>,
        std::collections::BTreeMap<u32, kaikei_core::Money>,
    ),
    String,
> {
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

    // 貸借対照表が転記する欄をそのまま渡す。**再計算しない**——同じ数字を
    // 2回計算すると、片方だけ直したときに損益計算書と貸借対照表がずれる
    // （様式の書き方が「必ず一致します」と言っている箇所である）。
    let fields_by_no = filled
        .fields
        .iter()
        .map(|field| (field.no, field.amount))
        .collect();

    Ok((written, fields_by_no))
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

/// 貸借対照表を集計するときの期間の下限。
///
/// 貸借対照表は**ある時点の残高**なので、帳簿にある仕訳をすべて含める必要が
/// ある。期首残高は前期末の日付で記帳するため、会計年度の開始日から集計すると
/// 落ちる（`write_reports` のコメント）。
///
/// 個別の帳簿の最初の仕訳日を引く経路を足す代わりに、**どんな帳簿より前**の
/// 日付を下限に使う。範囲を広く取っても、含まれる仕訳は変わらない。
fn book_beginning() -> AccountingDate {
    AccountingDate::new(1900, 1, 1).expect("1900-01-01 は常に有効な日付である")
}

/// 弥生インポート形式の CSV を書き出す。
///
/// **税込経理を前提にしている。** 同梱の税区分の写像は税込（「込」）の名称
/// なので、税抜経理の帳簿に使うと税額の扱いがずれる。`KAIKEI_TAX_MODE` を
/// 確かめ、一致しなければ書き出さない——黙って出すと、消費税の扱いが違う
/// データが税理士に渡る。
///
/// 変換できなかった仕訳は別の CSV（UTF-8）に理由付きで書き出す。**弥生に
/// 渡すファイルに混ぜない**——取り込む側が読むファイルではない。
fn write_yayoi(
    out_dir: &Path,
    entries: &[kaikei_core::JournalEntry],
    chart: &kaikei_core::ChartOfAccounts,
) -> Result<Vec<PathBuf>, String> {
    let map = kaikei_jp::yayoi::load_embedded(kaikei_jp_data::YAYOI_TAX_CATEGORIES)
        .map_err(|error| format!("弥生の税区分の写像を読めませんでした: {error}"))?;

    let tax_mode = env_var("KAIKEI_TAX_MODE")?;
    if tax_mode != map.tax_mode() {
        return Err(format!(
            "弥生向けの出力は{}（KAIKEI_TAX_MODE={}）の帳簿を前提にしていますが、\
             この帳簿は {} です。税額の扱いがずれるため書き出しません",
            map.taxation_method(),
            map.tax_mode(),
            tax_mode
        ));
    }

    let tax_map = build_tax_map(&map);

    let conversion = kaikei_report::yayoi::convert(entries, chart, &tax_map);
    let (bytes, had_encoding_errors) = kaikei_report::yayoi::to_shift_jis_csv(&conversion.rows);

    let mut written = Vec::new();
    let csv_path = out_dir.join("yayoi_journal.csv");
    std::fs::write(&csv_path, &bytes)
        .map_err(|error| format!("書き出せませんでした: {}（{error}）", csv_path.display()))?;
    written.push(csv_path);

    // 変換できなかった仕訳。**0 件でも見出しだけのファイルを書く**
    // （消すと「無かった」のか「出し忘れた」のかが読めない）。
    let skipped_path = out_dir.join("yayoi_skipped.csv");
    let mut skipped_csv = String::from("\u{feff}仕訳番号,取引日,摘要,変換できなかった理由\r\n");
    for item in &conversion.skipped {
        skipped_csv.push_str(&format!(
            "{},{},\"{}\",\"{}\"\r\n",
            item.entry_no,
            item.date,
            item.description.replace('"', "\"\""),
            item.reason.replace('"', "\"\"")
        ));
    }
    std::fs::write(&skipped_path, skipped_csv).map_err(|error| {
        format!(
            "書き出せませんでした: {}（{error}）",
            skipped_path.display()
        )
    })?;
    written.push(skipped_path);

    println!(
        "弥生形式: {} 行を書き出しました（Shift-JIS）",
        conversion.rows.len()
    );

    // **写像が未確認であることを必ず伝える。** ある区分を別の区分として
    // 出力することは、その取引の税務上の扱いを変える。
    if !map.all_verified() {
        eprintln!(
            "注意: 弥生の税区分の写像は実機で確認していません（未確認 {} 件）。\
             取り込む前に税理士に確認してください",
            map.unverified_count()
        );
    }
    if !conversion.skipped.is_empty() {
        eprintln!(
            "注意: 弥生形式に変換できなかった仕訳が {} 件あります。\
             yayoi_skipped.csv を確認してください",
            conversion.skipped.len()
        );
    }
    if !conversion.truncated_descriptions.is_empty() {
        eprintln!(
            "注意: 摘要が弥生の上限（半角64桁）を超える仕訳が {} 件あります。\
             インポート時に切り捨てられます",
            conversion.truncated_descriptions.len()
        );
    }
    if had_encoding_errors {
        eprintln!(
            "注意: Shift-JIS で表せない文字があり、置換されました（{} 件の仕訳）。\
             該当する仕訳を直してから出し直してください:",
            conversion.unmappable_characters.len()
        );
        // **どの仕訳かを言う。** 化けたことだけ知らせても直しようがない。
        for (entry_no, chars) in conversion.unmappable_characters.iter().take(20) {
            eprintln!("  仕訳 {entry_no}: {chars}");
        }
        if conversion.unmappable_characters.len() > 20 {
            eprintln!(
                "  （ほか {} 件）",
                conversion.unmappable_characters.len() - 20
            );
        }
    }
    if conversion.exceeds_online_row_limit() {
        eprintln!(
            "注意: {} 行あります。弥生会計 オンラインは {} 行を超えるファイルを\
             取り込めません（デスクトップ版には制限の記載がありません）。\
             期間を分けて出し直してください",
            conversion.rows.len(),
            kaikei_report::yayoi::ONLINE_MAX_ROWS
        );
    }
    if bytes.len() > kaikei_report::yayoi::ONLINE_MAX_BYTES {
        eprintln!(
            "注意: {} バイトあります。弥生会計 オンラインは 1.0MB を超える\
             ファイルを取り込めません",
            bytes.len()
        );
    }

    Ok(written)
}

/// 税区分の写像を、出力側が使う素の対応表にする。
fn build_tax_map(
    map: &kaikei_jp::yayoi::YayoiTaxMap,
) -> std::collections::BTreeMap<String, String> {
    // `kaikei-report` は `kaikei-jp` を知らない（層を保つ）ので、
    // ここで素の文字列の対応表に落とす。
    let mut out = std::collections::BTreeMap::new();
    for code in [
        "SALES_10",
        "SALES_8_REDUCED",
        "SALES_EXPORT",
        "PURCHASE_10_QUALIFIED",
        "PURCHASE_8_REDUCED_QUALIFIED",
        "PURCHASE_10_NON_QUALIFIED",
        "PURCHASE_8_REDUCED_NON_QUALIFIED",
        "TAX_FREE",
        "OUT_OF_SCOPE",
        "NOT_APPLICABLE",
    ] {
        if let Some(mapping) = map.get(code) {
            out.insert(code.to_string(), mapping.yayoi.clone());
        }
    }
    out
}

/// 会計年度の開始日の前日。決算書の貸借対照表の期首列に使う。
///
/// 暦年のみ対応（`KAIKEI_FISCAL_YEAR_RULE` が `calendar_year` であることを
/// 呼び出し前に確かめている）なので、開始日は必ず 1/1 であり、前日は
/// 前年の 12/31 になる。
fn day_before(fiscal_year_start: AccountingDate) -> AccountingDate {
    AccountingDate::new(fiscal_year_start.year() - 1, 12, 31)
        .expect("前年の 12/31 は常に有効な日付である")
}

/// 青色申告決算書（貸借対照表）のデータを書き出す。
///
/// `opening` は期首時点、`closing` は期末時点の貸借対照表。
/// `blue_return_fields` は損益計算書の欄で、青色申告特別控除前の所得金額
/// （㊸）の転記に使う。
fn write_blue_return_balance_sheet(
    out_dir: &Path,
    opening: &kaikei_app::policy::Statement,
    closing: &kaikei_app::policy::Statement,
    to: AccountingDate,
    blue_return_fields: &std::collections::BTreeMap<u32, kaikei_core::Money>,
) -> Result<Vec<PathBuf>, String> {
    let form =
        kaikei_jp::blue_return_bs::load_embedded(kaikei_jp_data::STATEMENT_BLUE_RETURN_GENERAL_BS)
            .map_err(|error| {
                format!("決算書（貸借対照表）の当てはめ表を読めませんでした: {error}")
            })?;

    let filled = kaikei_jp::blue_return_bs::fill(&form, opening, closing, blue_return_fields)
        .map_err(|error| format!("決算書（貸借対照表）の金額を計算できませんでした: {error}"))?;

    let sections: Vec<kaikei_report::blue_return_bs::BsSection> = filled
        .sections
        .iter()
        .map(|section| kaikei_report::blue_return_bs::BsSection {
            title: section.title.clone(),
            rows: section
                .rows
                .iter()
                .map(|row| kaikei_report::blue_return_bs::BsRow {
                    label: row.label.clone(),
                    opening: row.opening,
                    closing: row.closing,
                })
                .collect(),
        })
        .collect();

    let imbalance = filled.imbalance();
    let title = format!("{}（{}）", filled.form, filled.part);
    let period = format!("{} 現在", to.to_iso_string());

    let written = write_pair(
        out_dir,
        "blue_return_balance_sheet",
        &kaikei_report::blue_return_bs::to_csv(&sections),
        &kaikei_report::blue_return_bs::to_html(&title, &period, &sections, imbalance, &[]),
    )?;

    // **貸借が合わないことを画面でも伝える。** 出力ファイルを開かない人が
    // そのまま提出するのを防ぐ。
    match imbalance {
        Some(diff) if !diff.is_zero() => eprintln!(
            "注意: 決算書の貸借対照表で、資産合計と負債・資本合計が {} 円\
             ずれています。決算書としては一致している必要があります",
            kaikei_app::amount::money_to_plain_string(&diff)
        ),
        Some(_) => {}
        None => eprintln!("注意: 決算書の貸借対照表で、貸借が一致するかを確認できませんでした"),
    }

    if !filled.not_on_form.is_empty() {
        eprintln!(
            "注意: 決算書の貸借対照表のどの行にも載らなかった科目が {} 件あります",
            filled.not_on_form.len()
        );
    }

    // **決算振替を記帳した後に決算書を出していないか。** 様式は元入金が
    // 期首と期末で同額であることを前提にしている（所得金額を別の行に書く
    // ため）。動いていたら、決算書は振替前の帳簿から作り直す必要がある。
    for (label, book_opening, book_closing) in &filled.same_column_mismatches {
        eprintln!(
            "注意: 決算書は「{label}」が期首と期末で同額であることを前提にしていますが、             帳簿では期首 {} / 期末 {} と動いています。             決算振替を記帳した後に決算書を出していないか確認してください             （決算書は振替前の帳簿から作ります）",
            kaikei_app::amount::money_to_plain_string(book_opening),
            kaikei_app::amount::money_to_plain_string(book_closing)
        );
    }

    Ok(written)
}

/// 貸借対照表が何を集計したものかの注記。
///
/// **「会計年度の増減」ではなく「その日現在の残高」であることを明示する。**
/// 決算書に転記する人がこれを取り違えると、期首残高が二重に入る。
///
/// あわせて、帳簿の最初の仕訳が集計の終了日と同じ年度にしか無い場合は、
/// 期首残高が帳簿に入っていない可能性を伝える。**期首残高の入れ忘れは黙って
/// 進むと決算まで気づけない**が、実際に要るかは事業の状況によるので
/// 「疑わしい」までしか言わない。
fn balance_sheet_notes(output: &statements::StatementsOutput, to: AccountingDate) -> Vec<String> {
    if output.entry_count == 0 {
        return Vec::new();
    }

    let mut notes = vec![format!(
        "この貸借対照表は帳簿の最初から {} までを集計した残高です\
         （会計年度中の増減ではありません）。",
        to.to_iso_string()
    )];

    if let Some(first) = output.first_entry_date {
        if first.year() == to.year() {
            notes.push(format!(
                "帳簿で最も古い仕訳は {} で、この会計年度の中にあります。\
                 開業初年度であれば問題ありませんが、そうでなければ\
                 前期から繰り越した残高（期首残高）が帳簿に入っていない\
                 可能性があります。期首残高は前期末の日付で記帳してください。",
                first.to_iso_string()
            ));
        }
    }
    notes
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
                yayoi,
            } => {
                assert_eq!(fiscal_year, 2026);
                assert_eq!(out_dir, PathBuf::from("./out"));
                assert_eq!(
                    deduction, DEFAULT_BLUE_RETURN_DEDUCTION,
                    "--deduction 省略時は既定値"
                );
                assert!(!yayoi, "--yayoi 省略時は弥生向けを出さない");
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

    // --yayoi を指定すると弥生向けも出す。
    #[test]
    fn the_yayoi_output_is_opt_in() {
        let command = parse_args(&args(&[
            "report", "--year", "2026", "--out", "./out", "--yayoi",
        ]))
        .unwrap();
        match command {
            Command::Report { yayoi, .. } => assert!(yayoi),
            other => panic!("report として解釈されるはず: {other:?}"),
        }
    }

    // verify は書き出さないので --yayoi を黙って無視しない。
    #[test]
    fn verify_rejects_yayoi_instead_of_ignoring_it() {
        let err = parse_args(&args(&["verify", "--year", "2026", "--yayoi"]))
            .expect_err("verify では拒否されるはず");
        assert!(err.contains("--yayoi"), "{err}");
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

    fn date(year: i32, month: u8, day: u8) -> AccountingDate {
        AccountingDate::new(year, month, day).unwrap()
    }

    fn output(entry_count: usize, first: Option<AccountingDate>) -> statements::StatementsOutput {
        use kaikei_app::policy::Statement;
        use kaikei_core::{Currency, Money};
        let empty = || Statement {
            title: String::new(),
            sections: Vec::new(),
            total: Money::from_minor(0, Currency::JPY),
        };
        statements::StatementsOutput {
            balance_sheet: empty(),
            income_statement: empty(),
            entry_count,
            first_entry_date: first,
        }
    }

    // 貸借対照表が「残高」であることを必ず伝える。
    //
    // **これが無いと、会計年度の増減と読み違えて期首残高が二重に入る。**
    #[test]
    fn the_balance_sheet_note_says_it_is_a_balance_not_a_period_movement() {
        let notes = balance_sheet_notes(&output(10, Some(date(2025, 12, 31))), date(2026, 12, 31));

        assert!(!notes.is_empty(), "注記を出すこと");
        assert!(notes[0].contains("残高"), "{notes:?}");
        assert!(notes[0].contains("増減ではありません"), "{notes:?}");
        assert!(notes[0].contains("2026-12-31"), "{notes:?}");
    }

    // 帳簿の最初の仕訳が集計年度の中にしか無ければ、期首残高の入れ忘れを疑う。
    #[test]
    fn a_book_that_starts_inside_the_fiscal_year_is_flagged_as_possibly_missing_its_opening() {
        let notes = balance_sheet_notes(&output(10, Some(date(2026, 3, 1))), date(2026, 12, 31));

        assert!(
            notes.iter().any(|note| note.contains("期首残高")),
            "期首残高の入れ忘れを疑う注記を出すこと: {notes:?}"
        );
    }

    // 前年以前の仕訳があれば、入れ忘れの疑いは出さない（期首残高が入っている）。
    #[test]
    fn a_book_that_starts_before_the_fiscal_year_is_not_flagged() {
        let notes = balance_sheet_notes(&output(10, Some(date(2025, 12, 31))), date(2026, 12, 31));

        assert!(
            !notes.iter().any(|note| note.contains("入っていない")),
            "期首残高が入っているのに疑いを出さないこと: {notes:?}"
        );
    }

    // 仕訳が0件なら注記は出さない（件数の警告が別に出る。二重に言わない）。
    #[test]
    fn an_empty_book_gets_no_balance_sheet_note() {
        assert!(balance_sheet_notes(&output(0, None), date(2026, 12, 31)).is_empty());
    }

    // attach は必須の指定を既定値で埋めない。
    //
    // **取引年月日・種別・授受の経路は後から復元できない。** 既定値で埋めると、
    // 誤った値の証憑が黙って帳簿に入る。
    #[test]
    fn attach_requires_what_cannot_be_recovered_later() {
        for (missing, args_without) in [
            (
                "--file",
                vec![
                    "attach",
                    "--date",
                    "2026-06-15",
                    "--type",
                    "invoice",
                    "--via",
                    "email",
                ],
            ),
            (
                "--date",
                vec![
                    "attach", "--file", "x.pdf", "--type", "invoice", "--via", "email",
                ],
            ),
            (
                "--type",
                vec![
                    "attach",
                    "--file",
                    "x.pdf",
                    "--date",
                    "2026-06-15",
                    "--via",
                    "email",
                ],
            ),
            (
                "--via",
                vec![
                    "attach",
                    "--file",
                    "x.pdf",
                    "--date",
                    "2026-06-15",
                    "--type",
                    "invoice",
                ],
            ),
        ] {
            let err =
                parse_args(&args(&args_without)).expect_err("{missing} が無ければ拒否されるはず");
            assert!(err.contains(missing), "{missing} を求めること: {err}");
        }
    }

    // 金額と取引先は任意（契約書のように金額の無い証憑がある）。
    #[test]
    fn attach_allows_a_document_without_an_amount() {
        let command = parse_args(&args(&[
            "attach",
            "--file",
            "契約書.pdf",
            "--date",
            "2026-04-01",
            "--type",
            "contract",
            "--via",
            "manual",
        ]))
        .unwrap();
        match command {
            Command::Attach(attach) => {
                assert_eq!(attach.amount_minor, None, "0 で埋めないこと");
                assert_eq!(attach.counterparty, None);
                assert_eq!(attach.doc_type, "contract");
            }
            other => panic!("attach として解釈されるはず: {other:?}"),
        }
    }

    // 拡張子から MIME が決まらなければ、推測せずに指定を求める。
    #[test]
    fn an_unknown_extension_does_not_get_a_guessed_mime_type() {
        assert_eq!(
            mime_from_extension(Path::new("請求書.pdf")),
            Some("application/pdf")
        );
        assert_eq!(
            mime_from_extension(Path::new("領収書.PDF")),
            Some("application/pdf")
        );
        assert_eq!(mime_from_extension(Path::new("なぞ.xyz")), None);
        assert_eq!(mime_from_extension(Path::new("拡張子なし")), None);
    }

    // 集計の下限は、どんな帳簿の最初の仕訳よりも前であること。
    #[test]
    fn the_book_beginning_precedes_any_plausible_first_entry() {
        assert!(book_beginning() < date(1970, 1, 1));
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

    // ─── import ─────────────────────────────────────────

    fn profile(id: &str) -> kaikei_import::profile::CsvProfile {
        let yaml = format!(
            "id: {id}\nname: テスト\nkind: bank\n\
             date:\n  column: 0\n  format: \"%Y/%m/%d\"\n\
             amount:\n  mode: separate_columns\n  debit_column: 1\n  credit_column: 2\n\
             description:\n  columns: [3]\n"
        );
        kaikei_import::profile::CsvProfile::load_all(&yaml)
            .unwrap()
            .pop()
            .unwrap()
    }

    /// **本命。** 既定では保存しない。
    ///
    /// 取り込んだ明細は消せない（DELETE を与えていない）。列の対応を
    /// 間違えたまま保存すると、桁の狂った明細が残り続ける。
    #[test]
    fn import_does_not_write_unless_commit_is_given() {
        let command = parse_import(&args(&["--profile", "p.yaml", "--file", "m.csv"])).unwrap();
        match command {
            Command::Import(args) => assert!(!args.commit, "既定は下見であること"),
            other => panic!("import が返らない: {other:?}"),
        }

        let command = parse_import(&args(&[
            "--profile",
            "p.yaml",
            "--file",
            "m.csv",
            "--commit",
        ]))
        .unwrap();
        match command {
            Command::Import(args) => assert!(args.commit),
            other => panic!("import が返らない: {other:?}"),
        }
    }

    /// `--commit` の次の引数を値として食べない。
    ///
    /// 食べると `--commit --file x` が黙って通り、`--file` を指定したつもりで
    /// 「--file を指定してください」と言われる（あるいは通ってしまう）。
    #[test]
    fn commit_does_not_swallow_the_next_argument() {
        let command = parse_import(&args(&[
            "--profile",
            "p.yaml",
            "--commit",
            "--file",
            "m.csv",
        ]))
        .unwrap();
        match command {
            Command::Import(args) => {
                assert!(args.commit);
                assert_eq!(args.file, PathBuf::from("m.csv"));
            }
            other => panic!("import が返らない: {other:?}"),
        }
    }

    #[test]
    fn import_needs_a_profile_and_a_file() {
        let error = parse_import(&args(&["--file", "m.csv"])).unwrap_err();
        assert!(error.contains("--profile"), "{error}");

        let error = parse_import(&args(&["--profile", "p.yaml"])).unwrap_err();
        assert!(error.contains("--file"), "{error}");
    }

    /// 打ち間違いを黙って無視しない。
    #[test]
    fn an_unknown_import_argument_is_rejected() {
        let error = parse_import(&args(&[
            "--profile",
            "p.yaml",
            "--file",
            "m.csv",
            "--fiel",
            "x",
        ]))
        .unwrap_err();
        assert!(error.contains("--fiel"), "{error}");
    }

    /// **本命。** プロファイルが複数あるのに指定が無ければ止める。
    ///
    /// 勝手に先頭を使うと、別の銀行の列の対応で読んで桁が狂う。
    #[test]
    fn several_profiles_without_a_choice_stops_instead_of_guessing() {
        let error = choose_profile(vec![profile("mizuho"), profile("mufg")], None).unwrap_err();

        assert!(error.contains("--profile-id"), "{error}");
        // どれが選べるかを出す（利用者が次に何をすればよいか分かるように）。
        assert!(error.contains("mizuho"), "{error}");
        assert!(error.contains("mufg"), "{error}");
    }

    #[test]
    fn a_single_profile_needs_no_choice() {
        let chosen = choose_profile(vec![profile("mizuho")], None).unwrap();
        assert_eq!(chosen.id, "mizuho");
    }

    #[test]
    fn a_named_profile_is_picked_out_of_several() {
        let chosen =
            choose_profile(vec![profile("mizuho"), profile("mufg")], Some("mufg")).unwrap();
        assert_eq!(chosen.id, "mufg");
    }

    #[test]
    fn an_unknown_profile_id_lists_what_is_available() {
        let error = choose_profile(vec![profile("mizuho")], Some("rakuten")).unwrap_err();
        assert!(error.contains("rakuten"), "{error}");
        assert!(error.contains("mizuho"), "{error}");
    }

    #[test]
    fn digits_are_grouped_in_threes() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1_000), "1,000");
        assert_eq!(group_digits(550_000), "550,000");
        assert_eq!(group_digits(1_234_567), "1,234,567");
        assert_eq!(group_digits(-1_000), "-1,000");
    }

    #[test]
    fn a_chrono_date_becomes_an_accounting_date() {
        let date = to_accounting_date(chrono::NaiveDate::from_ymd_opt(2026, 6, 15).unwrap())
            .expect("普通の日付");
        assert_eq!(date, AccountingDate::new(2026, 6, 15).unwrap());
    }

    // 使い方に import の要点が載っている。
    //
    // 「既定では保存しない」は、読まないと事故る側の情報である。
    #[test]
    fn the_usage_explains_that_import_is_a_preview_by_default() {
        assert!(USAGE.contains("--commit"), "{USAGE}");
        assert!(USAGE.contains("既定は下見"), "{USAGE}");
        assert!(USAGE.contains("--profile"), "{USAGE}");
    }

    // ─── journalize ─────────────────────────────────────

    #[test]
    fn journalize_needs_rules() {
        let error = parse_journalize(&args(&[])).unwrap_err();
        assert!(error.contains("--rules"), "{error}");
    }

    #[test]
    fn journalize_takes_a_year_and_a_source() {
        let command = parse_journalize(&args(&[
            "--rules", "r.yaml", "--year", "2026", "--source", "mizuho",
        ]))
        .unwrap();
        match command {
            Command::Journalize(args) => {
                assert_eq!(args.rules, PathBuf::from("r.yaml"));
                assert_eq!(args.fiscal_year, Some(2026));
                assert_eq!(args.source.as_deref(), Some("mizuho"));
            }
            other => panic!("journalize が返らない: {other:?}"),
        }
    }

    #[test]
    fn an_unknown_journalize_argument_is_rejected() {
        let error = parse_journalize(&args(&["--rules", "r.yaml", "--yaer", "2026"])).unwrap_err();
        assert!(error.contains("--yaer"), "{error}");
    }

    fn unmatched(description: &str, amount: i64) -> kaikei_app::view::ImportedTxView {
        kaikei_app::view::ImportedTxView {
            id: format!("id-{description}-{amount}"),
            source: "mizuho".to_string(),
            occurred_on: AccountingDate::new(2026, 6, 15).unwrap(),
            amount_minor: amount,
            is_money_in: false,
            raw_description: description.to_string(),
            balance_after: None,
            status: "pending".to_string(),
            entry_id: None,
            ignore_reason: None,
        }
    }

    /// **本命。** ルールが無い明細は多い順に出る。
    ///
    /// 次にどのルールを書けば一番効くかが分かるようにするため。
    #[test]
    fn unmatched_lines_come_out_most_frequent_first() {
        let rows = [
            unmatched("ｾﾌﾞﾝ", 500),
            unmatched("ｽｰﾊﾟｰ", 3_000),
            unmatched("ｾﾌﾞﾝ", 700),
            unmatched("ｾﾌﾞﾝ", 300),
            unmatched("ﾔﾏﾄﾞ", 100),
            unmatched("ｽｰﾊﾟｰ", 2_000),
        ];
        let borrowed: Vec<&kaikei_app::view::ImportedTxView> = rows.iter().collect();

        let summary = summarize_unmatched(&borrowed);

        assert_eq!(summary[0].0, "ｾﾌﾞﾝ");
        assert_eq!(summary[0].1, 3, "件数");
        assert_eq!(summary[0].2, 1_500, "金額の合計");
        assert_eq!(summary[1].0, "ｽｰﾊﾟｰ");
        assert_eq!(summary[2].0, "ﾔﾏﾄﾞ");
    }

    /// 同数なら摘要の順で決まる。
    ///
    /// 決めておかないと、実行のたびに並びが変わって差分が読めない。
    #[test]
    fn a_tie_among_unmatched_lines_is_broken_by_description() {
        let rows = [unmatched("ｾﾞﾌﾞﾗ", 100), unmatched("ｱﾙﾌｧ", 100)];
        let borrowed: Vec<&kaikei_app::view::ImportedTxView> = rows.iter().collect();

        let summary = summarize_unmatched(&borrowed);

        assert_eq!(summary[0].0, "ｱﾙﾌｧ");
        assert_eq!(summary[1].0, "ｾﾞﾌﾞﾗ");
    }

    // 使い方に、記帳しないことが書いてある。
    //
    // 「当たったのが見えた＝記帳された」と取り違えると、帳簿に何も入って
    // いないまま確定申告を迎える。
    #[test]
    fn the_usage_says_journalize_does_not_record_yet() {
        assert!(USAGE.contains("journalize"), "{USAGE}");
        assert!(USAGE.contains("まだ記帳しません"), "{USAGE}");
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
