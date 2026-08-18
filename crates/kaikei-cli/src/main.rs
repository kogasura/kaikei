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
use kaikei_app::ports::{ChartRepo, FixedAssetRepo, JournalRepo};
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
    kaikei counterparty import --file <取引先.csv> [--commit]
    kaikei counterparty verify --code <取引先> [--registration-no <T+13桁>]
                               [--qualified true|false] [--on <YYYY-MM-DD>] [--commit]
    kaikei fixedasset add --name <名前> --account <科目> --acquired <YYYY-MM-DD>
                          --cost <円> --method <方法> [--life <年>] [--commit]
    kaikei fixedasset list [--year <西暦>]
    kaikei fixedasset dispose --id <UUID> --on <YYYY-MM-DD> [--commit]
    kaikei depreciation --year <西暦>
    kaikei household --year <西暦> --account <科目> --ratio <事業割合> [--amount <円>]
    kaikei consumptiontax --year <西暦>

report は帳簿をファイルに書き出します。
verify は帳簿の整合性を検査します（書き出しません）。
import は銀行・カードの明細 CSV を取り込みます。
journalize は取り込んだ明細にルールを当てて、仕訳の案を見せます。
counterparty import は取引先マスタを CSV から投入します（既存は上書きしません）。
counterparty verify は既存の取引先に適格請求書発行事業者の登録番号と
    確認結果を記録します。**名前とコードは変えません。**
fixedasset add は固定資産を台帳に入れます（既定は下見）。
fixedasset list は台帳の中身を並べます（--year でその年度の償却費も出ます）。
fixedasset dispose は資産を除却します（既定は下見。行は消しません）。
depreciation は固定資産台帳から減価償却費を出します（記帳はしません）。
household は決算時の家事按分の振替仕訳を出します（記帳はしません）。
consumptiontax は消費税の申告に向けた集計を出します（申告書ではありません）。
    科目の一部だけが按分対象なら --amount でその額を指定します
    （例: 通信費のうち携帯代だけ。指定した額は計上額を超えられません）。

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

    consumption_tax.csv には消費税の申告に向けた集計を出します（原則課税・
    税込経理の帳簿のみ）。**申告書の金額ではありません。** 反映していない
    ものは consumption_tax_notes.txt に書き出します。

    invoices_to_collect.csv には、適格請求書を揃えるべき取引を並べます
    （取引先が記録されていない課税仕入れのうち、税込1万円以上のもの）。
    日付・金額・摘要・科目が入っているので、そのまま作業リストに使えます。

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

    あわせて、決算書を書き出したときと同じ指摘も出します:
      ・固定資産があるのに減価償却費が1円も計上されていない
      ・資産がマイナス残高、負債がプラス残高になっている
      ・毎月ほぼ同じ日の同額の支出が、月によって違う科目に入っている
      ・同じ費用科目の中で消費税の区分が割れている（控除額が変わります）
      ・売上があるのに売掛金・買掛金・前受金・前払金がどれも0
    いずれも貸借は一致したままなので、決算書を見ても分かりません。
    指摘があっても検査は失敗しません（誤りと決まったわけではないため）。

attach の引数:
    --file <パス>        取り込むファイル（必須）
    --date <YYYY-MM-DD>  取引年月日（検索要件の1つ）
                         --entry を指定すれば省略できます（仕訳の取引日を使う）
    --type <種別>        invoice / receipt / contract / other（必須）
    --via <経路>         email / download / scan / manual（必須）
    --amount <円>        取引金額（検索要件。無い証憑もあるので任意）
    --counterparty <名>  取引先（検索要件）
    --match-amount <円>  この金額の仕訳を探して紐付けます（--entry の代わり）
                         領収書から読める金額で引けます。同じ額の仕訳が
                         複数あれば、候補を並べて止まります
    --match-year <西暦>  金額で探す年（省略時は --date の年）
    --entry <UUID>       紐付ける仕訳のID
                         指定すると、取引年月日・取引金額・取引先を
                         その仕訳から埋めます（明示した値の方が優先）
    --entry-no <番号>    仕訳番号で紐付けます（--entry の代わり）
                         invoices_to_collect.csv が出すのはこの番号です。
                         年は --match-year か --date から決めます
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

counterparty import の引数:
    --file <パス>        取引先の CSV（必須）
    --commit             実際に投入します

    列は code,name,invoice_registration_no,is_qualified です。見出し行が
    要ります。code と name 以外は省略できます。

    **is_qualified の空欄は「まだ確認していない」という意味です。**
    false（非適格だと確認した）とは別に扱われ、false のときだけ記帳が
    拒まれます。外部システムから写すときは、登録番号を確認していない
    取引先を false にしないでください。

    **既定は下見です。** --commit が無ければ読んで見せるだけです。
    既にあるコードは上書きしません（違いがあれば知らせます）。

fixedasset add の引数:
    --name <名前>        決算書に出す資産の名前（必須）
    --account <科目>     帳簿上の科目コード（必須。例: 210）
    --acquired <日付>    取得年月日 YYYY-MM-DD（必須）
    --cost <円>          取得価額（必須）
    --method <方法>      straight-line=定額法 / lump-sum=一括償却資産（3年均等）
                         / immediate=少額減価償却資産（全額即時）（必須）
    --life <年>          耐用年数。**定額法のときだけ指定します**
    --ratio <割合>       事業専用割合（0より大きく1以下。省略時は100%）
    --note <文>          備考
    --commit             実際に台帳へ入れます

    **耐用年数と償却方法は指定するものです。** 資産名から推定しません。
    同じ資産でも扱いを選べることがあり、初年度の償却費が何倍も変わります。

    **既定は下見です。** --commit が無ければ償却の予定表を見せるだけです。
    台帳は後から直せますが、記帳した仕訳は追記型なので戻せません。

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
        Command::Counterparty(args) => runtime.block_on(run_counterparty_import(args)),
        Command::CounterpartyVerify {
            code,
            registration_no,
            is_qualified,
            verified_on,
            commit,
        } => runtime.block_on(run_counterparty_verify(
            code,
            registration_no,
            is_qualified,
            verified_on,
            commit,
        )),
        Command::Depreciation { fiscal_year } => runtime.block_on(run_depreciation(fiscal_year)),
        Command::Household {
            fiscal_year,
            account,
            ratio,
            amount,
        } => runtime.block_on(run_household(fiscal_year, &account, &ratio, amount)),
        Command::ConsumptionTax { fiscal_year } => {
            runtime.block_on(run_consumption_tax(fiscal_year))
        }
        Command::FixedAsset(args) => runtime.block_on(run_fixed_asset_add(args)),
        Command::FixedAssetList { fiscal_year } => {
            runtime.block_on(run_fixed_asset_list(fiscal_year))
        }
        Command::FixedAssetDispose { id, on, commit } => {
            runtime.block_on(run_fixed_asset_dispose(id, on, commit))
        }
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
    /// 取引先マスタを CSV から投入する。
    Counterparty(CounterpartyArgs),
    /// 既存の取引先に適格請求書発行事業者の情報を記録する。
    CounterpartyVerify {
        code: String,
        registration_no: Option<String>,
        is_qualified: Option<bool>,
        verified_on: Option<AccountingDate>,
        commit: bool,
    },
    /// 固定資産台帳から、その年度の減価償却費を出す。
    Depreciation { fiscal_year: i32 },
    /// 消費税の申告に向けた集計を出す。
    ConsumptionTax { fiscal_year: i32 },
    /// 決算時の家事按分の振替仕訳を出す。
    Household {
        fiscal_year: i32,
        /// 按分対象の科目コード（地代家賃など）。
        account: String,
        /// 事業割合（0〜1の小数）。
        ratio: String,
        /// 按分する額。省略すると科目の年間計上額の全額。
        /// **科目の一部だけが按分対象のときに使う。**
        amount: Option<i64>,
    },
    /// 固定資産を台帳に入れる。
    FixedAsset(FixedAssetArgs),
    /// 固定資産台帳の中身を並べる。
    FixedAssetList { fiscal_year: Option<i32> },
    /// 固定資産を除却する。
    FixedAssetDispose {
        id: String,
        on: AccountingDate,
        commit: bool,
    },
}

/// `kaikei counterparty import` の引数。
#[derive(Debug)]
struct CounterpartyArgs {
    /// 取り込む CSV。
    file: PathBuf,
    /// 実際に書き込むか。**既定は下見。**
    commit: bool,
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
    ///
    /// `--entry` を指定した場合は省略できる（仕訳の取引日を使う）。
    doc_date: Option<AccountingDate>,
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
    /// 仕訳IDの代わりに、この金額の仕訳を探す。
    ///
    /// **領収書から読める値で引けるようにする。** 仕訳IDを人が探すのが、
    /// 証憑を登録するときのいちばんの手間である。
    match_amount: Option<i64>,
    /// 金額で探すときの年（省略時は取引年月日の年）。
    match_year: Option<i32>,
    /// 仕訳IDの代わりに、この**仕訳番号**の仕訳を探す。
    ///
    /// `invoices_to_collect.csv` が出すのは仕訳番号である。UUID しか
    /// 受けないと、32件を紐付けるのに毎回引き直すことになる。
    match_entry_no: Option<u32>,
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
    if subcommand == "counterparty" {
        return parse_counterparty(&args[1..]);
    }
    if subcommand == "depreciation" {
        return parse_depreciation(&args[1..]);
    }
    if subcommand == "household" {
        return parse_household(&args[1..]);
    }
    if subcommand == "consumptiontax" {
        return parse_consumption_tax(&args[1..]);
    }
    if subcommand == "fixedasset" {
        return parse_fixed_asset(&args[1..]);
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

    // **証憑がどれだけ付いているかを数字で出す。**
    // 1件も登録されていないことは帳簿を見ても分からない。数字が見えないと、
    // 登録が進んでいるかどうかも分からない。
    print_document_coverage(&documents, fiscal_year, output.entry_count).await?;

    // **検査するコマンドが、決算書の出力より検査が緩いのはおかしい。**
    // `report` が出しているのと同じ指摘を、同じ関数を呼んで出す
    // （文言が2箇所に分かれると、片方だけ直したときに食い違う）。
    warn_from_statements(&store, fiscal_year).await?;

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

        // **電子取引とスキャナ保存は別の場所に置く。** 制度が違うものを
        // 混ぜると、提示のときにどれがどちらか分からない
        // （`kaikei_report::documents` のモジュール doc）。
        let folder = dir.join(&entry.folder);
        std::fs::create_dir_all(&folder)
            .map_err(|error| format!("作れませんでした: {}（{error}）", folder.display()))?;
        let path = folder.join(&entry.file_name);
        std::fs::write(&path, &bytes)
            .map_err(|error| format!("書き出せませんでした: {}（{error}）", path.display()))?;
        // checksums にも置き場所を入れる。同じ名前が別のフォルダにあるとき、
        // どちらのハッシュか分からなくなる。
        checksums.push_str(&format!(
            "{}  {}/{}\n",
            entry.document.blob_hash, entry.folder, entry.file_name
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
    let mut match_amount: Option<i64> = None;
    let mut match_year: Option<i32> = None;
    let mut match_entry_no: Option<u32> = None;
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
            "--entry-no" => {
                match_entry_no = Some(take()?.parse::<u32>().map_err(|_| {
                    "--entry-no は仕訳番号（正の整数）で指定してください".to_string()
                })?)
            }
            "--match-amount" => {
                let text = take()?;
                match_amount = Some(
                    kaikei_app::amount::strip_thousands_separators(&text)
                        .parse::<i64>()
                        .map_err(|_| format!("--match-amount は数字で指定してください: {text}"))?,
                )
            }
            "--match-year" => {
                let text = take()?;
                match_year = Some(
                    text.parse::<i32>()
                        .map_err(|_| format!("--match-year は西暦で指定してください: {text}"))?,
                )
            }
            "--note" => note = Some(take()?),
            other => return Err(format!("不明な引数です: {other}")),
        }
    }

    // **`--entry` があれば検索要件は仕訳から採れる。** 1件ごとに5つの引数を
    // 打たせると、証憑の登録が現実的でなくなる（実際に1件も登録されていない）。
    // **両方は受けない。** どちらを使ったのかが分からないまま登録されると、
    // 意図しない仕訳に紐付いても気づけない。
    let ways = [
        entry_id.is_some(),
        match_amount.is_some(),
        match_entry_no.is_some(),
    ]
    .iter()
    .filter(|used| **used)
    .count();
    if ways > 1 {
        return Err("--entry / --match-amount / --entry-no は同時に指定できません".to_string());
    }
    if doc_date.is_none() && ways == 0 {
        return Err("--date を指定してください（例: --date 2026-06-15）。\
             --entry / --entry-no / --match-amount のいずれかで仕訳を指定すれば、\
             その仕訳の取引日を使います"
            .to_string());
    }
    Ok(Command::Attach(AttachArgs {
        file: file.ok_or("--file を指定してください")?,
        doc_date,
        amount_minor,
        counterparty,
        doc_type: doc_type
            .ok_or("--type を指定してください（invoice / receipt / contract / other）")?,
        received_via: received_via
            .ok_or("--via を指定してください（email / download / scan / manual）")?,
        mime_type,
        entry_id,
        match_amount,
        match_year,
        match_entry_no,
        note,
    }))
}

/// 決算振替が記帳済みに見えるなら知らせる。
///
/// # なぜ順序が効くのか
///
/// 決算振替は収益・費用をゼロにして元入金へ振り替える。損益計算書は会計年度の
/// 期間で集計するので、**同じ年度に記帳された決算振替もその期間に入る**。
/// 結果、決算書の所得が 0 になる。
///
/// 実際に帳簿の複製で通し稽古したところ、決算振替を記帳した後の決算書は
/// 売上 0・所得 0・所得金額 −650,000（控除だけが残る）になった。貸借対照表の
/// 元入金も期首欄が期末の値になり、期首の貸借が合わなくなった。
///
/// **決算書を作ってから決算振替を記帳する。** この順序をここで思い出せる
/// ようにする。
///
/// # 断定しない
///
/// 収益も費用も 0 の年度は、決算振替済みとは限らない（開業前など）。
/// 「そう見える」までにとどめる。
fn warn_if_the_year_looks_closed(
    income_statement: &kaikei_app::policy::Statement,
    entry_count: usize,
) {
    if !year_looks_closed(income_statement, entry_count) {
        return;
    }

    eprintln!("注意: この年度には仕訳が {entry_count} 件ありますが、収益も費用も残っていません。");
    eprintln!("  決算振替を記帳した後の帳簿に見えます。");
    eprintln!(
        "  決算振替は収益・費用をゼロにするので、その後に作った決算書は所得が 0 になります。"
    );
    eprintln!("  決算書は決算振替を記帳する前に作ってください。");
}

/// 証憑の検索要件が欠けていれば知らせる。
///
/// # 何が問題か
///
/// 電子取引データは**取引年月日・取引金額・取引先**の3項目で検索できる必要が
/// ある（`docs/06-documents.md` §4）。取引先が空だと、その1つが欠ける。
///
/// 実際に帳簿の複製で稽古したところ、`--match-amount` で紐付けた証憑の取引先が
/// どちらも空になった。**この帳簿には取引先タグが1件も無い**（1,395明細中0件）
/// ので、仕訳から埋められない。
///
/// # 止めない
///
/// ファイルを保存しないより、取引先が空でも保存した方がよい。**保存した後で
/// 取引先を足すことはできない**（証憑は追記のみ）ので、そこは知らせる。
fn warn_if_the_search_fields_are_incomplete(counterparty: &Option<String>, amount: Option<i64>) {
    let missing_counterparty = counterparty
        .as_deref()
        .map(|text| text.trim().is_empty())
        .unwrap_or(true);
    if !missing_counterparty {
        return;
    }

    eprintln!("注意: 取引先が空のまま登録します。");
    eprintln!("  電子取引データは取引年月日・取引金額・取引先で検索できる必要があり、");
    eprintln!("  取引先が空だとその1つが欠けます。");
    if amount.is_none() {
        eprintln!("  取引金額も空です（契約書など金額の無い証憑なら、それで構いません）。");
    }
    eprintln!("  --counterparty で指定できます。証憑は後から書き換えられません。");
}

/// 前年の事業主貸・事業主借が期首に残っていれば知らせる。
///
/// # なぜ要るのか
///
/// 個人事業主では、事業主貸・事業主借は**翌期首に元入金へ振り替えて0に戻す**
/// （`docs/04-jp-tax.md` §9）。振り替えないと、翌年度の帳簿が前年の残高を
/// 抱えたまま始まり、**年を追うごとに膨らむ**。
///
/// 帳簿の複製で2027年度を開く稽古をしたところ、事業主貸 10,013,438 円・
/// 事業主借 1,012,434 円が持ち越されたまま2027年が始まった。貸借は一致して
/// いるので、決算書を見ても気づけない。
///
/// # 振替仕訳は作らない
///
/// 当年度末と翌年期首のどちらで振り替えるか、振替仕訳を起こすか期首残高と
/// して設定するかが決まっていない（`DECISIONS.md` D-065）。**判断を先取り
/// しない代わりに、持ち越されていることを知らせる。**
/// 期首振替を**年内に**記帳していないかを知らせる。
///
/// # 何が起きるか
///
/// 期首振替（事業主貸・事業主借 → 元入金）は**翌年1月1日**に記帳する
/// （D-102）。年内（12月31日など）に入れると、**決算書の貸借対照表から
/// 事業主貸・事業主借が消える。**
///
/// 青色申告決算書の様式にはこの2欄があり、期末残高をそのまま書く。
/// 0 で提出することになる。
///
/// **実際に試したら再現した。** 実帳簿の複製で12月31日に期首振替を入れたところ、
/// 事業主貸 9,923,381円 と事業主借 1,012,434円 が決算書から消え、
/// `verify` は終了コード0のままだった。
///
/// # 判定
///
/// **期中に動きがあったのに期末が0**なら疑う。動きが無ければ0が正しい
/// （その年に事業主貸を1度も使っていない、ということはありうる）。
///
/// 動きには**期首振替そのものも数える**。最初は「原因だから除く」と考えて
/// 外していたが、**それだと見逃す場面がある**——期首振替は前年からの繰越を
/// 振り替えるので、その年に事業主貸を1度も使っていなくても振替は起きる。
/// 除くと「何も動いていない」ことになり、黙って通る（E2E で確かめた）。
///
/// 正しく翌年1月1日に記帳された期首振替は、そもそもこの年度の仕訳に入って
/// こない（日付で外れる）。**この年度に居る期首振替は、それ自体が誤りである。**
///
/// # 断定しない
///
/// 期中に立てた事業主貸を、年内に別の理由で相殺することはありうる。
/// 「確かめてください」と言うにとどめる。
fn warn_if_the_opening_transfer_was_posted_too_early(
    entries: &[kaikei_core::JournalEntry],
    balance_sheet: &kaikei_app::policy::Statement,
) -> Result<(), String> {
    let owner = kaikei_jp::chart::load_owner_accounts(kaikei_jp_data::CHART_SOLE_PROPRIETOR)
        .map_err(|error| format!("科目表の owner_accounts を読めませんでした: {error}"))?;
    let Some(owner) = owner else {
        return Ok(());
    };

    for (code, name) in [
        (&owner.drawings, "事業主貸"),
        (&owner.contributions, "事業主借"),
    ] {
        // 期末が0でなければ、消えていない。
        if !amount_of(balance_sheet, code).is_zero() {
            continue;
        }
        let moved = entries
            .iter()
            .any(|entry| entry.lines().iter().any(|line| line.account() == code));
        if !moved {
            continue;
        }
        eprintln!("注意: {name} は期中に動いているのに、期末残高が 0 です。");
        eprintln!("  期首振替（事業主貸・事業主借 → 元入金）を年内に記帳していないか");
        eprintln!("  確かめてください。**翌年1月1日**に記帳するものです。");
        eprintln!("  年内に入れると、決算書の貸借対照表からこの欄が消えます。");
    }
    Ok(())
}

fn warn_if_owner_accounts_carried_over(
    opening_balance_sheet: &kaikei_app::policy::Statement,
    fiscal_year: i32,
) -> Result<(), String> {
    let owner = kaikei_jp::chart::load_owner_accounts(kaikei_jp_data::CHART_SOLE_PROPRIETOR)
        .map_err(|error| format!("科目表の owner_accounts を読めませんでした: {error}"))?;
    let Some(owner) = owner else {
        return Ok(());
    };

    let drawings = amount_of(opening_balance_sheet, &owner.drawings);
    let contributions = amount_of(opening_balance_sheet, &owner.contributions);
    if drawings.is_zero() && contributions.is_zero() {
        return Ok(());
    }

    eprintln!("注意: 前年の事業主貸・事業主借が {fiscal_year} 年の期首に残っています:");
    // **残高の大きさで出す。** 事業主貸・事業主借は純資産に分類されるので、
    // 財務諸表では貸方を正とした符号が付く。事業主貸は借方に立つのが自然
    // なので負の数として出てしまい、「マイナスの事業主貸」と読めてしまう。
    // 決算書の貸借対照表と同じ見え方（大きさ）に揃える。
    if !drawings.is_zero() {
        eprintln!(
            "  {} 事業主貸 {}",
            owner.drawings.as_str(),
            group_digits(i64::try_from(drawings.minor().abs()).unwrap_or(i64::MAX))
        );
    }
    if !contributions.is_zero() {
        eprintln!(
            "  {} 事業主借 {}",
            owner.contributions.as_str(),
            group_digits(i64::try_from(contributions.minor().abs()).unwrap_or(i64::MAX))
        );
    }
    eprintln!("  これらは翌期首に元入金へ振り替えて0に戻すものです。");
    eprintln!("  振り替えないと、年を追うごとに残高が膨らみます。");
    eprintln!("  振替の時期と方式は税理士に確認してから決めてください（このソフトは振替仕訳を作りません）。");
    Ok(())
}

/// 適格請求書が要る税区分の明細を、取引先タグの有無で数える。
///
/// # なぜすり抜けるのか
///
/// `JpTaxPolicy` は、適格請求書が要る税区分（`requires_qualified_invoice`）に
/// **取引先タグが付いていれば**、その取引先が適格請求書発行事業者かを見る。
/// 取引先タグが無ければ、見るものが無いので何も言わない。
///
/// # なぜ日付でマスタを選ばないのか
///
/// `requires_qualified_invoice` は施行日で変わらない（適格請求書が要る区分は
/// 要るまま）。日付でマスタを選ぶと、**期首仕訳のように会計期間の外に日付を
/// 持つ仕訳で「該当マスタなし」になり、検査そのものが黙って飛ぶ**。
/// 数え落としが警告なしで起きるので、コードを知っているマスタで判定する。
///
/// # 断定しない
///
/// 仕入税額控除が認められるかは、証憑の保存状況や相手方の登録状況で決まる。
/// ここで出すのは**帳簿の事実**（件数）だけである（`CLAUDE.md` §10）。
fn count_qualified_without_counterparty(
    entries: &[kaikei_core::JournalEntry],
    rule_sets: &kaikei_jp::tax::TaxRuleSets,
) -> (usize, usize) {
    let mut debit = 0;
    let mut credit = 0;
    for line in entries.iter().flat_map(|entry| entry.lines().iter()) {
        if !line_needs_a_counterparty(line.tags(), rule_sets) {
            continue;
        }
        if line.side() == kaikei_core::Side::Debit {
            debit += 1;
        } else {
            credit += 1;
        }
    }
    (debit, credit)
}

/// 取引先が無い課税仕入れの取引を、少額特例の金額の境目で分ける。
#[derive(Debug, Default, PartialEq, Eq)]
struct SplitBySmallAmount {
    /// 1万円未満・期間内の取引数（少額特例の対象になりうる）。
    small: usize,
    /// それ以外の取引数（適格請求書の保存が要る）。
    large: usize,
    /// `large` の合計額。
    large_total_minor: i128,
}

/// **取引ごとに**数える（明細ごとではない）。
///
/// 少額特例の1万円未満は「一回の取引の税込金額」で見る（国税庁「少額特例に
/// おける1万円未満の判定単位」）。**明細で分けると数が変わる**——1件の
/// 取引を複数の明細に分けている帳簿では、どれも1万円未満に見えてしまう。
///
/// 取引の額は借方の合計を使う。**貸方の合計でも同じ数になる**——仕訳は
/// 貸借が一致しているからである。借方にするのは、費用の側から見た「取引額」
/// として読みやすいというだけで、計算上の意味は無い。
/// （借方を貸方に変える変異を入れても、どのテストも落ちない。当然である。）
fn split_by_small_amount(
    entries: &[kaikei_core::JournalEntry],
    rule_sets: &kaikei_jp::tax::TaxRuleSets,
) -> SplitBySmallAmount {
    let mut split = SplitBySmallAmount::default();
    for entry in entries {
        let (needs, total) = needs_an_invoice(entry, rule_sets);
        // **借方の明細だけを数える。** 貸方に立つ課税仕入れは返還（返金・
        // 値引き）で、要るのは適格返還請求書である。`invoices_to_collect`
        // と同じ条件にしないと、件数と一覧の行数が食い違う。
        if !entry.lines().iter().any(|line| {
            line.side() == kaikei_core::Side::Debit
                && line_needs_a_counterparty(line.tags(), rule_sets)
        }) {
            continue;
        }
        if needs {
            split.large += 1;
            split.large_total_minor += total;
        } else {
            split.small += 1;
        }
    }
    split
}

/// その取引に適格請求書の保存が要るか、と取引額。
///
/// **数える側（`split_by_small_amount`）と並べる側（`invoices_to_collect`）が
/// 同じ判定を使う。** 二重に書くと、件数と一覧の行数が食い違ったときに
/// どちらが正しいのか分からなくなる。
///
/// 戻り値の `bool` は「取引先が無い課税仕入れであること」を前提にしていない
/// ——呼び出し側がそちらを先に確かめる。ここは金額と日付だけを見る。
fn needs_an_invoice(
    entry: &kaikei_core::JournalEntry,
    _rule_sets: &kaikei_jp::tax::TaxRuleSets,
) -> (bool, i128) {
    let total: i128 = entry
        .lines()
        .iter()
        .filter(|line| line.side() == kaikei_core::Side::Debit)
        .map(|line| line.amount().minor())
        .sum();
    let money = kaikei_core::Money::from_minor(total, kaikei_core::Currency::JPY);
    let small = kaikei_jp::invoice::is_within_the_small_amount_special(&money, entry.entry_date());
    (!small, total)
}

/// 明細1行の判定。**表示から切り離してある**（`accounts_on_the_wrong_side`
/// と同じ理由。何を拾うかをテストで固定できないと、呼び出しごと消えても
/// 気づけない）。
fn line_needs_a_counterparty(
    tags: &kaikei_core::TagSet,
    rule_sets: &kaikei_jp::tax::TaxRuleSets,
) -> bool {
    let Ok(tax_key) = kaikei_core::TagKey::parse("tax_category") else {
        return false;
    };
    let Ok(counterparty_key) = kaikei_core::TagKey::parse("counterparty") else {
        return false;
    };
    let Some(kaikei_core::TagValue::Code(code)) = tags.get(&tax_key) else {
        return false;
    };
    // 知らないコードは黙って見送る。ここは税区分の妥当性を検査する場所では
    // ない（それは `JpTaxPolicy` が記帳時にやっている）。
    let requires_qualified_invoice = rule_sets
        .iter()
        .find_map(|table| table.category(code).ok())
        .is_some_and(|category| category.requires_qualified_invoice);

    requires_qualified_invoice && tags.get(&counterparty_key).is_none()
}

/// その明細の税区分が、適格請求書の保存を要求しているか。
///
/// **取引先の有無は見ない**（`line_needs_a_counterparty` はそこも見る）。
/// 取引先が付いている明細について、相手の適格性まで確かめたいときに使う。
fn line_requires_a_qualified_invoice(
    tags: &kaikei_core::TagSet,
    rule_sets: &kaikei_jp::tax::TaxRuleSets,
) -> bool {
    let Ok(tax_key) = kaikei_core::TagKey::parse("tax_category") else {
        return false;
    };
    let Some(kaikei_core::TagValue::Code(code)) = tags.get(&tax_key) else {
        return false;
    };
    rule_sets
        .iter()
        .find_map(|table| table.category(code).ok())
        .is_some_and(|category| category.requires_qualified_invoice)
}

/// 適格請求書を揃えるべき取引を並べる。
///
/// **`split_by_small_amount` と同じ条件で選ぶ。** 数えた件数と一覧の行数が
/// 食い違うと、どちらが正しいのか分からなくなる。数える側と並べる側で条件を
/// 二重に書かないよう、判定そのもの（`needs_an_invoice`）を共有する。
///
/// 並び順は**金額の大きい順**。先頭しか読まれないことがあるので、並び順が
/// 「何を見せるか」になる（`check_suspected_duplicates` と同じ理由）。
fn invoices_to_collect(
    entries: &[kaikei_core::JournalEntry],
    rule_sets: &kaikei_jp::tax::TaxRuleSets,
    chart: &kaikei_core::ChartOfAccounts,
) -> Vec<kaikei_report::invoices_to_collect::InvoiceToCollect> {
    let mut rows: Vec<_> = entries
        .iter()
        .filter_map(|entry| {
            let (needs, total) = needs_an_invoice(entry, rule_sets);
            if !needs {
                return None;
            }
            // **借方の明細だけを載せる。** 貸方に立つ課税仕入れは「仕入れ」
            // ではなく**返還**（返金・値引き）である。返還に要るのは適格請求書
            // ではなく**適格返還請求書**で、必要な書類が違う。同じ一覧に混ぜると
            // 「請求書を探しても見つからない」ことになる。
            //
            // 実帳簿では 603 件中5件がこれ（ドメイン代の返金 60,831円）。
            // 返還の側の検査は、必要になったら別に作ること。
            let line = entry.lines().iter().find(|line| {
                line.side() == kaikei_core::Side::Debit
                    && line_needs_a_counterparty(line.tags(), rule_sets)
            })?;
            let account = line.account().as_str().to_string();
            let account_name = chart
                .get(line.account())
                .map(|def| def.name.clone())
                .unwrap_or_else(|| account.clone());
            Some(kaikei_report::invoices_to_collect::InvoiceToCollect {
                date: entry.entry_date().to_iso_string(),
                entry_no: i64::from(entry.entry_no().as_u32()),
                amount_minor: total,
                description: entry.description().to_string(),
                account,
                account_name,
            })
        })
        .collect();
    rows.sort_by(|a, b| {
        b.amount_minor
            .cmp(&a.amount_minor)
            .then_with(|| a.entry_no.cmp(&b.entry_no))
    });
    rows
}

/// 一覧のうち、**請求書・領収書が既に紐付いている**仕訳の番号を返す。
///
/// # なぜ種別を見るのか
///
/// 契約書（`contract`）が付いていても、その取引の請求書があることには
/// ならない。**揃えるべきものが揃ったか**を見たいので、`invoice` と
/// `receipt` だけを数える。
///
/// # 取引先までは求めない
///
/// 証憑に取引先が入っていなくても、**請求書そのものは揃っている**。
/// 取引先が空なら `attach` が別に警告する（検索要件の話であって、
/// 「集める」作業とは段が違う）。
///
/// # 一覧に載っている分だけ引く
///
/// `documents_of_entry` は仕訳1件ごとの問い合わせである。600件全部に
/// 投げると重いので、**一覧に残っている数十件だけ**を見る。
async fn entries_with_an_invoice_document(
    documents: &PgDocumentQuery,
    entries: &[kaikei_core::JournalEntry],
    to_collect: &[kaikei_report::invoices_to_collect::InvoiceToCollect],
) -> Result<std::collections::BTreeSet<i64>, String> {
    use kaikei_app::ports::DocumentQueryPort;
    use std::collections::BTreeSet;

    let wanted: BTreeSet<i64> = to_collect.iter().map(|row| row.entry_no).collect();
    let mut done = BTreeSet::new();
    for entry in entries {
        let entry_no = i64::from(entry.entry_no().as_u32());
        if !wanted.contains(&entry_no) {
            continue;
        }
        let attached = documents
            .documents_of_entry(entry.id())
            .await
            .map_err(|error| format!("証憑を読めませんでした: {error}"))?;
        if attached
            .iter()
            .any(|doc| doc.doc_type == "invoice" || doc.doc_type == "receipt")
        {
            done.insert(entry_no);
        }
    }
    Ok(done)
}

/// 取引先は付いているが、その取引先の**登録番号が分からない**明細を数える。
///
/// # 取引先が付いていれば済む話ではない
///
/// `warn_if_qualified_invoice_lacks_a_counterparty` は取引先タグの有無しか
/// 見ていない。**タグが付いていても、その相手が適格請求書発行事業者かどうかを
/// 確かめていなければ、仕入税額控除の根拠にならない。**
///
/// 実帳簿（2026-08-17）では、取引先マスタ31件すべてが未確認だった
/// （`invoice_registration_no` も `is_qualified_invoice_issuer` も空）。
/// freee 側の取引先34件も登録番号が全件未入力である。
///
/// # 「未確認」と「非適格」は違う
///
/// `is_qualified_invoice_issuer` が `false` なら「非適格だと確認した」で、
/// 経過措置の対象として処理できる。`None` は**まだ調べていない**であり、
/// どちらの扱いもできない。この2つを混ぜない（`list_partners` と同じ立場）。
fn count_counterparties_without_a_registration_number(
    entries: &[kaikei_core::JournalEntry],
    rule_sets: &kaikei_jp::tax::TaxRuleSets,
    counterparties: &kaikei_app::policy::CounterpartyIndex,
) -> (usize, std::collections::BTreeSet<String>) {
    let mut lines = 0;
    let mut names = std::collections::BTreeSet::new();
    for line in entries.iter().flat_map(|entry| entry.lines().iter()) {
        if line.side() != kaikei_core::Side::Debit {
            continue;
        }
        if let Some(name) = unverified_counterparty_name(line.tags(), rule_sets, counterparties) {
            lines += 1;
            names.insert(name);
        }
    }
    (lines, names)
}

/// 明細1行の判定。**表示から切り離してある**（`line_needs_a_counterparty`
/// と同じ理由。何を拾うかをテストで固定できないと、呼び出しごと消えても
/// 気づけない）。
///
/// 適格請求書が要る税区分で、取引先が付いていて、その取引先の適格性が
/// **未確認**なら、その取引先名を返す。
fn unverified_counterparty_name(
    tags: &kaikei_core::TagSet,
    rule_sets: &kaikei_jp::tax::TaxRuleSets,
    counterparties: &kaikei_app::policy::CounterpartyIndex,
) -> Option<String> {
    if !line_requires_a_qualified_invoice(tags, rule_sets) {
        return None;
    }
    let key = kaikei_core::TagKey::parse("counterparty").ok()?;
    let kaikei_core::TagValue::Code(code) = tags.get(&key)? else {
        return None;
    };
    let party = counterparties.get(code)?;
    // **未確認だけを数える。** 非適格と確認済みなら経過措置で処理できる。
    if party.is_qualified_invoice_issuer.is_none() && party.invoice_registration_no.is_none() {
        Some(party.name.clone())
    } else {
        None
    }
}

/// 非適格と**確認済み**の取引先なのに、適格請求書を前提とした税区分に
/// なっている明細を挙げる。
///
/// # 控除しすぎになる
///
/// 相手が適格請求書発行事業者でないなら、その課税仕入れは経過措置の対象で
/// あって全額控除できない（2026年9月まで80%、10月以降70%）。適格の区分の
/// ままだと**全額控除しているのと同じ**になる。
///
/// 実帳簿で言えば、外注工賃 385,000円 の相手は個人で、適格かどうかを
/// 確かめていない。**非適格と分かった場合、税額にして約7,000円の差**が出る。
///
/// # 「未確認」は対象にしない
///
/// `is_qualified_invoice_issuer` が `None` の相手は別の指摘で挙げている
/// （D-122）。ここで挙げると、**確認が進んでいない帳簿では全部が二重に
/// 出る**。ここは「調べた結果、非適格だと分かっている」相手だけを見る。
///
/// # 断定しない
///
/// 適格の区分を使うこと自体が誤りとは限らない——**確認より前に記帳した分**は
/// 当然そうなる。「区分を見直してください」と言うにとどめる。
fn lines_with_a_known_non_qualified_counterparty(
    entries: &[kaikei_core::JournalEntry],
    rule_sets: &kaikei_jp::tax::TaxRuleSets,
    counterparties: &kaikei_app::policy::CounterpartyIndex,
) -> (usize, i128, std::collections::BTreeSet<String>) {
    let Ok(key) = kaikei_core::TagKey::parse("counterparty") else {
        return (0, 0, std::collections::BTreeSet::new());
    };
    let mut count = 0usize;
    let mut total = 0i128;
    let mut names = std::collections::BTreeSet::new();
    for line in entries.iter().flat_map(|entry| entry.lines().iter()) {
        if line.side() != kaikei_core::Side::Debit {
            continue;
        }
        if !line_requires_a_qualified_invoice(line.tags(), rule_sets) {
            continue;
        }
        let Some(kaikei_core::TagValue::Code(code)) = line.tags().get(&key) else {
            continue;
        };
        let Some(party) = counterparties.get(code) else {
            continue;
        };
        // **確認済みで「非適格」の相手だけ。** 未確認（None）は別の指摘。
        if party.is_qualified_invoice_issuer == Some(false) {
            count += 1;
            total += line.amount().minor();
            names.insert(party.name.clone());
        }
    }
    (count, total, names)
}

/// 上の件数を知らせる。
fn warn_if_qualified_invoice_lacks_a_counterparty(
    entries: &[kaikei_core::JournalEntry],
    counterparties: &kaikei_app::policy::CounterpartyIndex,
) -> Result<(), String> {
    let rule_sets = kaikei_jp::tax::TaxRuleSets::from_embedded()
        .map_err(|error| format!("同梱の消費税区分マスタを読めませんでした: {error}"))?;

    // **非適格と確認済みの相手に、適格の税区分が付いていないか。**
    // 控除しすぎになる（経過措置の対象なので全額は控除できない）。
    let (non_qualified, amount, party_names) =
        lines_with_a_known_non_qualified_counterparty(entries, &rule_sets, counterparties);
    if non_qualified > 0 {
        eprintln!();
        eprintln!(
            "注意: 非適格と確認済みの取引先に、適格請求書を前提とした税区分が付いた明細が {non_qualified} 件あります（{} 円）。",
            kaikei_core::Money::from_minor(amount, kaikei_core::Currency::JPY).to_display_string()
        );
        eprintln!(
            "    {}",
            party_names.iter().cloned().collect::<Vec<_>>().join(" / ")
        );
        eprintln!("  経過措置の対象なので全額は控除できません（2026年9月まで80%、10月以降70%）。");
        eprintln!("  税区分を非適格のものに見直してください。");
        eprintln!("  ※ 確認より前に記帳した分は当然こうなります。誤りとは限りません。");
    }

    // **件数だけでは動けない。** 603件と言われても手の付けようがないが、
    // 少額特例（税込1万円未満は適格請求書の保存が不要）で分ければ、
    // 実際に請求書を揃える必要がある取引はずっと少ないことがある。
    let split = split_by_small_amount(entries, &rule_sets);
    if split.small + split.large > 0 {
        eprintln!();
        // **分母を必ず言う。** 上の件数（明細）と内訳（取引）は数え方が違ううえ、
        // 内訳は借方だけを見ている。分母を出さないと「足しても合わない」と
        // 読まれる。
        eprintln!(
            "  借方の {} 取引を、税込1万円で分けると:",
            split.small + split.large
        );
        eprintln!(
            "    1万円未満 {} 件（少額特例の対象になりうる）",
            split.small
        );
        eprintln!(
            "    1万円以上 {} 件・{} 円（適格請求書の保存が要ります）",
            split.large,
            kaikei_core::Money::from_minor(split.large_total_minor, kaikei_core::Currency::JPY)
                .to_display_string()
        );
        eprintln!("  ※ 少額特例には基準期間の課税売上高1億円以下などの要件があり、");
        eprintln!("    このソフトでは判定していません（前々年の帳簿が無いことがあるため）。");
        eprintln!("    免除されるのは適格請求書の保存だけで、帳簿の記載事項は免除されません。");
        eprintln!("    令和11年9月30日で終わる特例です（28年改正法附則53の2）。");
    }

    // **3つの指摘は互いに独立している。**
    //
    // 以前はここで `debit + credit == 0` なら早期 return していた。すると
    // 取引先が全部付いている帳簿では、下の「登録番号が分かりません」が
    // **永久に出なかった**。取引先が付いていることと、その相手の登録状況を
    // 確かめてあることは別の話である。
    let (debit, credit) = count_qualified_without_counterparty(entries, &rule_sets);
    if debit + credit > 0 {
        eprintln!(
            "注意: 課税仕入れの明細 {} 件に取引先が記録されていません。",
            debit + credit
        );
        eprintln!("  誰との取引なのかを、帳簿から辿れません。");
        eprintln!("  記帳するときに counterparty タグを付けると記録されます。");
        // **借方と貸方を分けて言う。** 貸方に立つものは「仕入れ」ではなく返還
        // （返金・値引き）か、家事按分のような**内部の振替**である。後者には
        // そもそも相手方が存在しないので、「請求書を揃えてください」は的外れに
        // なる。実際、家事按分を記帳したら3件増えた。
        if credit > 0 {
            eprintln!(
                "  うち {credit} 件は貸方に立っています（返金・値引き、または家事按分などの内部の振替）。"
            );
            eprintln!("    返金・値引きに要るのは適格請求書ではなく適格返還請求書です。");
            eprintln!("    内部の振替には相手方が無いので、取引先を付けようがありません。");
        }
        eprintln!(
            "  仕入税額控除が認められるかどうかは、証憑の保存状況と相手方の登録状況で決まります。"
        );
    }

    // **取引先が付いていれば済む話ではない。** 相手が適格請求書発行事業者か
    // どうかを確かめていなければ、控除の根拠にならない。
    let (unverified, names) =
        count_counterparties_without_a_registration_number(entries, &rule_sets, counterparties);
    if unverified > 0 {
        eprintln!();
        eprintln!(
            "  取引先が付いている明細のうち {unverified} 件は、相手の登録番号が分かりません。"
        );
        eprintln!(
            "    {}",
            names.iter().cloned().collect::<Vec<_>>().join(" / ")
        );
        eprintln!("    「未確認」であって「非適格」ではありません。非適格と確認できれば");
        eprintln!("    経過措置で処理できますが、未確認のままではどちらの扱いもできません。");
    }
    Ok(())
}

/// 収益も費用も残っていない年度か（＝決算振替済みに見えるか）。
///
/// **表示から切り離してある。** 何を拾うかをテストで固定できないと、
/// 「落ちないこと」しか確かめられない。
fn year_looks_closed(income_statement: &kaikei_app::policy::Statement, entry_count: usize) -> bool {
    if entry_count == 0 {
        // **空の帳簿を「決算振替済み」と言わない。** 収益も費用も0なのは
        // 当たり前で、指摘しても何の手がかりにもならない。
        return false;
    }
    // 収益も費用も1件残らずゼロなら、ゼロ化された後の姿である。
    income_statement
        .sections
        .iter()
        .flat_map(|section| section.lines.iter())
        .all(|line| line.amount.is_zero())
}

/// 全件エクスポートが帳簿の試算表と一致するかを確かめる。
///
/// # なぜ要るのか
///
/// `export.json` は**このソフトが無くなっても帳簿が残る**ための出口である
/// （`docs/03-database.md` §8）。明細が欠けていても、必要になったときに初めて
/// 気づく——**そのときにはもう元の帳簿が無い。**
fn warn_if_export_does_not_match_the_book(
    json: &str,
    trial_balance: &kaikei_app::view::TrialBalanceView,
) {
    let totals = match kaikei_report::export::sum_by_account(json) {
        Ok(totals) => totals,
        Err(reason) => {
            // **読めないことを「一致した」にしない。**
            eprintln!("注意: 全件エクスポートを読み直せませんでした: {reason}");
            return;
        }
    };

    let mut mismatched: Vec<String> = Vec::new();
    for row in trial_balance.rows() {
        let code = row.account.as_str();
        let (debit, credit) = totals.get(code).copied().unwrap_or((0, 0));
        if debit != row.debit_total.minor() || credit != row.credit_total.minor() {
            mismatched.push(format!(
                "  {code}: 帳簿 借{} 貸{} / export.json 借{debit} 貸{credit}",
                row.debit_total.minor(),
                row.credit_total.minor()
            ));
        }
    }

    if mismatched.is_empty() {
        return;
    }
    eprintln!(
        "注意: 全件エクスポートが帳簿と一致しません（{} 科目）:",
        mismatched.len()
    );
    for line in &mismatched {
        eprintln!("{line}");
    }
    eprintln!("  このファイルからは帳簿を復元できません。");
}

/// 弥生向けの出力が帳簿の試算表と一致するかを確かめる。
///
/// # なぜ要るのか
///
/// 弥生CSV は**税理士に渡す形**である。取り込んだ数字が決算書と違っていても、
/// 渡した側も受け取った側も気づけない。列のずれ・金額の取り違え・行の脱落は、
/// **書き出したものを数え直して初めて分かる。**
///
/// # 何を比べるか
///
/// 科目ごとの借方合計・貸方合計を比べる。残高だけを比べると、借方と貸方に
/// 同じ額が余分に立っていても気づけない。
///
/// 決算振替のように帳簿に無い行を弥生側だけに足すことはしていないので、
/// 一致するのが正しい。
fn warn_if_yayoi_does_not_match_the_book(
    rows: &[Vec<String>],
    chart: &kaikei_core::ChartOfAccounts,
    trial_balance: &kaikei_app::view::TrialBalanceView,
) {
    let from_csv = kaikei_report::yayoi::sum_by_account(rows);

    let mut mismatched: Vec<String> = Vec::new();
    for row in trial_balance.rows() {
        let Some(def) = chart.get(&row.account) else {
            continue;
        };
        let (debit, credit) = from_csv.get(&def.name).copied().unwrap_or((0, 0));
        if debit != row.debit_total.minor() || credit != row.credit_total.minor() {
            mismatched.push(format!(
                "  {} {}: 帳簿 借{} 貸{} / 弥生CSV 借{debit} 貸{credit}",
                row.account.as_str(),
                def.name,
                row.debit_total.minor(),
                row.credit_total.minor()
            ));
        }
    }

    if mismatched.is_empty() {
        return;
    }
    eprintln!(
        "注意: 弥生向けの出力が帳簿と一致しません（{} 科目）:",
        mismatched.len()
    );
    for line in &mismatched {
        eprintln!("{line}");
    }
    eprintln!("  このファイルを税理士に渡すと、決算書と違う数字が取り込まれます。");
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

    let store_pg = PgStore::new(pool.clone());

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

    let entry = match (&args.entry_id, args.match_amount, args.match_entry_no) {
        (Some(text), _, _) => Some(parse_entry_id(text)?),
        // 金額から引く。**候補が1つに絞れなければ止める。**
        (None, Some(amount), _) => Some(find_entry_by_amount(&store_pg, amount, &args).await?),
        // 仕訳番号から引く。`invoices_to_collect.csv` が出すのはこれである。
        (None, None, Some(entry_no)) => {
            Some(find_entry_by_number(&store_pg, entry_no, &args).await?)
        }
        (None, None, None) => None,
    };

    // **検索要件を仕訳から埋める。** 指定があればそちらを優先する
    // （証憑の日付・金額が仕訳と違うことはある）。
    let from_entry = match entry {
        Some(entry_id) => find_entry_facts(&store_pg, entry_id).await?,
        None => None,
    };
    let doc_date = match (args.doc_date, &from_entry) {
        (Some(date), _) => date,
        (None, Some(facts)) => facts.date,
        (None, None) => {
            return Err("--date か --entry のどちらかが要ります".to_string());
        }
    };
    let amount_minor = args
        .amount_minor
        .or_else(|| from_entry.as_ref().and_then(|facts| facts.amount));
    let counterparty = args.counterparty.clone().or_else(|| {
        from_entry
            .as_ref()
            .and_then(|facts| facts.counterparty.clone())
    });

    // **検索要件の3項目が揃っているか。** 取引先が空だと、取引先での検索が
    // できない（`docs/06-documents.md` §4）。止めはしない——ファイルを
    // 保存しないより、取引先が空でも保存した方がよい。
    warn_if_the_search_fields_are_incomplete(&counterparty, amount_minor);

    let document = NewDocument {
        id: uuid::Uuid::now_v7().to_string(),
        blob_hash: hash.to_hex(),
        original_name,
        mime_type,
        byte_size: bytes.len() as i64,
        doc_date,
        amount_minor,
        counterparty,
        doc_type: args.doc_type,
        received_via: args.received_via,
        received_at: kaikei_core::Timestamp::from_unix_nanos(now_unix_nanos()?),
        note: args.note,
    };
    let document_id = document.id.clone();

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
/// `kaikei counterparty import --file <CSV> [--commit]` を解釈する。
/// `kaikei depreciation --year <西暦>` を解釈する。
/// `kaikei fixedasset add` の引数。
#[derive(Debug)]
struct FixedAssetArgs {
    name: String,
    account: kaikei_core::AccountCode,
    acquired_on: AccountingDate,
    /// 取得価額（円）。
    cost: i64,
    /// 1=定額法 / 2=一括償却 / 3=少額特例。
    method: i16,
    useful_life_years: Option<i16>,
    /// 事業専用割合（10進文字列）。`None` は100%。
    business_ratio: Option<String>,
    note: Option<String>,
    /// 実際に台帳へ入れるか。**既定は下見。**
    commit: bool,
}

/// `kaikei fixedasset add ...` を解釈する。
/// `kaikei fixedasset list [--year <西暦>]` を解釈する。
/// `kaikei fixedasset dispose --id <UUID> --on <YYYY-MM-DD> [--commit]` を解釈する。
fn parse_fixed_asset_dispose(args: &[String]) -> Result<Command, String> {
    let mut id = None;
    let mut on = None;
    let mut commit = false;
    let mut index = 0;
    while index < args.len() {
        let key = args[index].as_str();
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
            "--id" => id = Some(value),
            "--on" => {
                on =
                    Some(AccountingDate::parse(&value).map_err(|error| {
                        format!("--on は YYYY-MM-DD で指定してください: {error}")
                    })?)
            }
            other => return Err(format!("不明な引数です: {other}\n\n{USAGE}")),
        }
        index += 2;
    }
    Ok(Command::FixedAssetDispose {
        id: id.ok_or("--id を指定してください（kaikei fixedasset list で確認できます）")?,
        on: on.ok_or("--on を指定してください（除却した日。例: --on 2026-06-30）")?,
        commit,
    })
}

/// 固定資産を除却する。
///
/// # 行は消さない
///
/// 台帳から資産を外す唯一の手段が `disposed_on` を埋めることである
/// （`DECISIONS.md` D-104）。消せると、過去の年度の償却費がどの資産のもの
/// だったか辿れなくなる。
///
/// # 未償却残高を必ず見せる
///
/// 除却すると、その資産は台帳の計算対象から外れる。**帳簿にはまだ残高が
/// 残っている**ので、除却損の記帳が要る。いくら残っているかを示さないと、
/// 記帳しないまま `verify` の指摘だけが増える。
///
/// **除却損の額や科目は決めない。** 売却なら売却損益になり、金額も相手科目も
/// 事情で変わる（`CLAUDE.md` §10）。
async fn run_fixed_asset_dispose(
    id: String,
    on: AccountingDate,
    commit: bool,
) -> Result<Vec<PathBuf>, String> {
    use kaikei_app::ports::FixedAssetRepo;

    let database_url = env_var("APP_DATABASE_URL")?;
    let pool = connect_app(&database_url)
        .await
        .map_err(|error| format!("PostgreSQL に接続できませんでした: {error}"))?;
    let store = PgStore::new(pool);

    let rows = with_tx_err(&store, |tx| {
        Box::pin(async move { tx.list_fixed_assets().await })
    })
    .await
    .map_err(|error: kaikei_app::error::RepoError| {
        format!("固定資産台帳を読めませんでした: {error}")
    })?;

    let Some(asset) = rows.iter().find(|row| row.id == id) else {
        return Err(format!(
            "その ID の資産が台帳にありません: {id}。\
             kaikei fixedasset list で確認してください"
        ));
    };
    if let Some(disposed) = asset.disposed_on {
        return Err(format!(
            "{} は既に {} に除却済みです。除却日を動かすと過去の決算書の数字が\
             変わるので、この操作では直せません",
            asset.name,
            disposed.to_iso_string()
        ));
    }
    if on < asset.acquired_on {
        return Err(format!(
            "除却日が取得日より前です（取得 {} / 除却 {}）",
            asset.acquired_on.to_iso_string(),
            on.to_iso_string()
        ));
    }

    // 除却する直前の未償却残高。**除却する年の前年末の簿価**である
    // （除却した年は計算対象から外すため）。
    let input = to_fixed_asset(asset)?;
    let schedule = kaikei_jp::depreciation::schedule(&input)
        .map_err(|error| format!("{}: 償却額を計算できませんでした: {error}", asset.name))?;
    let book_value = schedule
        .years
        .iter()
        .rfind(|y| y.year < on.year())
        .map(|y| y.book_value)
        .unwrap_or(asset.acquisition_cost);

    println!("{}  [{}]", asset.name, method_label(asset.method));
    println!("  科目      {}", asset.account.as_str());
    println!(
        "  取得      {}  {} 円",
        asset.acquired_on.to_iso_string(),
        asset.acquisition_cost.to_display_string()
    );
    println!("  除却      {}", on.to_iso_string());
    println!(
        "  除却直前の未償却残高  {} 円",
        book_value.to_display_string()
    );
    println!();
    println!("この額が帳簿に残ります。除却損などの記帳は別に必要です。");
    println!(
        "  借方 <除却損などの科目> / 貸方 {}",
        asset.account.as_str()
    );
    println!("金額と科目は事情で変わる（売却なら売却損益）ので、こちらでは決めません。");
    println!();
    println!(
        "除却した年（{} 年）以降、この資産は台帳の計算対象から外れます。",
        on.year()
    );
    println!("除却した年の償却をどう扱うかは申告上の判断なので、計算しません。");

    if !commit {
        println!();
        println!("下見です。まだ除却していません。");
        println!("この内容でよければ --commit を付けて実行してください。");
        return Ok(Vec::new());
    }

    let updated = with_tx_err(&store, move |tx| {
        let id = id.clone();
        Box::pin(async move { tx.dispose_fixed_asset(&id, on).await })
    })
    .await
    .map_err(|error: kaikei_app::error::RepoError| format!("除却できませんでした: {error}"))?;

    println!();
    if updated == 0 {
        return Err("除却できませんでした（既に除却済みか、行が見つかりません）".to_string());
    }
    println!("除却しました。");
    Ok(Vec::new())
}

fn parse_fixed_asset_list(args: &[String]) -> Result<Command, String> {
    let mut fiscal_year = None;
    let mut index = 0;
    while index < args.len() {
        let key = args[index].as_str();
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("{key} の値がありません"))?;
        match key {
            "--year" => {
                fiscal_year = Some(value.parse::<i32>().map_err(|_| {
                    format!("--year は西暦の数字で指定してください（受け取った値: {value}）")
                })?)
            }
            other => return Err(format!("不明な引数です: {other}\n\n{USAGE}")),
        }
        index += 2;
    }
    Ok(Command::FixedAssetList { fiscal_year })
}

/// 固定資産台帳の中身を並べる。
///
/// # なぜ要るのか
///
/// `fixedasset add` で入れた後、**台帳を見る手段が SQL しか無かった**。
/// 入力を取り違えていても気づけない。台帳から行は消せない（`DECISIONS.md`
/// D-104）ので、入っているものを確かめられることがなおさら要る。
///
/// # ID を必ず出す
///
/// 台帳の行を後から直すには ID が要る。**見えないと直せない。**
async fn run_fixed_asset_list(fiscal_year: Option<i32>) -> Result<Vec<PathBuf>, String> {
    use kaikei_app::ports::FixedAssetRepo;

    let database_url = env_var("APP_DATABASE_URL")?;
    let pool = connect_app(&database_url)
        .await
        .map_err(|error| format!("PostgreSQL に接続できませんでした: {error}"))?;
    let store = PgStore::new(pool);
    let rows = with_tx_err(&store, |tx| {
        Box::pin(async move { tx.list_fixed_assets().await })
    })
    .await
    .map_err(|error: kaikei_app::error::RepoError| {
        format!("固定資産台帳を読めませんでした: {error}")
    })?;

    if rows.is_empty() {
        println!("固定資産台帳に登録がありません。");
        println!("入れるには kaikei fixedasset add を使います。");
        return Ok(Vec::new());
    }

    println!("固定資産台帳 {} 件", rows.len());
    for row in &rows {
        println!();
        println!("  {}  [{}]", row.name, method_label(row.method));
        println!("    ID        {}", row.id);
        println!(
            "    科目      {}   取得 {}   取得価額 {} 円",
            row.account.as_str(),
            row.acquired_on.to_iso_string(),
            row.acquisition_cost.to_display_string(),
        );
        if let Some(life) = row.useful_life_years {
            println!("    耐用年数  {life} 年");
        }
        if let Some(ratio) = &row.business_ratio {
            println!("    事業割合  {ratio}");
        }
        if let Some(disposed) = row.disposed_on {
            println!("    除却      {}", disposed.to_iso_string());
        }
        if let Some(note) = &row.note {
            println!("    備考      {note}");
        }
        // 年を指定されていれば、その年度の償却費と期末簿価も出す。
        // **指定が無ければ出さない**——どの年の数字なのかが読めなくなる。
        if let Some(year) = fiscal_year {
            let asset = to_fixed_asset(row)?;
            let schedule = kaikei_jp::depreciation::schedule(&asset)
                .map_err(|error| format!("{}: 償却額を計算できませんでした: {error}", row.name))?;
            match schedule.year(year) {
                Some(y) => println!(
                    "    {year} 年   償却費 {} 円（{}か月）  期末簿価 {} 円",
                    y.amount.to_display_string(),
                    y.months,
                    y.book_value.to_display_string(),
                ),
                None => println!("    {year} 年   償却なし（取得前か、償却し終わっています）"),
            }
        }
    }

    if fiscal_year.is_none() {
        println!();
        println!("年度の償却費を見るには --year を付けます。");
    }
    Ok(Vec::new())
}

fn parse_fixed_asset(args: &[String]) -> Result<Command, String> {
    let Some(action) = args.first() else {
        return Err(format!(
            "fixedasset の後に add か list を指定してください\n\n{USAGE}"
        ));
    };
    if action == "list" {
        return parse_fixed_asset_list(&args[1..]);
    }
    if action == "dispose" {
        return parse_fixed_asset_dispose(&args[1..]);
    }
    if action != "add" {
        return Err(format!(
            "fixedasset のサブコマンドは add と list です（受け取った値: {action}）\n\n{USAGE}"
        ));
    }

    let mut name = None;
    let mut account = None;
    let mut acquired_on = None;
    let mut cost = None;
    let mut method = None;
    let mut useful_life_years = None;
    let mut business_ratio = None;
    let mut note = None;
    let mut commit = false;

    let rest = &args[1..];
    let mut index = 0;
    while index < rest.len() {
        let key = rest[index].as_str();
        // 値を取る引数と取らない引数を混ぜない。
        if key == "--commit" {
            commit = true;
            index += 1;
            continue;
        }
        let value = rest
            .get(index + 1)
            .ok_or_else(|| format!("{key} の値がありません"))?
            .clone();
        match key {
            "--name" => name = Some(value),
            "--account" => {
                account =
                    Some(kaikei_core::AccountCode::parse(&value).map_err(|error| {
                        format!("--account が科目コードとして読めません: {error}")
                    })?)
            }
            "--acquired" => {
                acquired_on = Some(AccountingDate::parse(&value).map_err(|error| {
                    format!("--acquired は YYYY-MM-DD で指定してください: {error}")
                })?)
            }
            "--cost" => {
                cost = Some(
                    kaikei_app::amount::strip_thousands_separators(&value)
                        .parse::<i64>()
                        .map_err(|_| format!("--cost は数字で指定してください: {value}"))?,
                )
            }
            "--method" => {
                method = Some(match value.as_str() {
                    "straight-line" => 1i16,
                    "lump-sum" => 2,
                    "immediate" => 3,
                    other => {
                        return Err(format!(
                            "--method は straight-line / lump-sum / immediate のいずれかです\
                             （受け取った値: {other}）。\
                             straight-line=定額法、lump-sum=一括償却資産（3年均等）、\
                             immediate=少額減価償却資産（全額即時）"
                        ));
                    }
                })
            }
            "--life" => {
                useful_life_years = Some(
                    value
                        .parse::<i16>()
                        .map_err(|_| format!("--life は年数の数字で指定してください: {value}"))?,
                )
            }
            "--ratio" => business_ratio = Some(value),
            "--note" => note = Some(value),
            other => return Err(format!("不明な引数です: {other}\n\n{USAGE}")),
        }
        index += 2;
    }

    let method =
        method.ok_or("--method を指定してください（straight-line / lump-sum / immediate）")?;
    // **定額法かどうかで耐用年数の要否が変わる。** DB の CHECK でも守って
    // いるが、ここで止めた方が何を直せばよいか分かる。
    if method == 1 && useful_life_years.is_none() {
        return Err("定額法には --life（耐用年数）が要ります。\
             耐用年数は申告上の判断なので、このソフトは推定しません"
            .to_string());
    }
    if method != 1 && useful_life_years.is_some() {
        return Err(
            "一括償却（lump-sum）と少額特例（immediate）は耐用年数を使いません。\
             --life を外してください。\
             付けたままだと、効いていると思ったまま進むことになります"
                .to_string(),
        );
    }

    Ok(Command::FixedAsset(FixedAssetArgs {
        name: name.ok_or("--name を指定してください（決算書に出す資産の名前）")?,
        account: account.ok_or("--account を指定してください（例: --account 210）")?,
        acquired_on: acquired_on
            .ok_or("--acquired を指定してください（例: --acquired 2025-07-24）")?,
        cost: cost.ok_or("--cost を指定してください（取得価額。円）")?,
        method,
        useful_life_years,
        business_ratio,
        note,
        commit,
    }))
}

/// 固定資産を台帳に入れる。
///
/// # 既定では入れない
///
/// **入れる前に償却の予定表を見せる。** 耐用年数や償却方法を取り違えると
/// 何年にもわたって誤った償却費が出る。台帳は `UPDATE` できるので直せるが、
/// 既に記帳した仕訳は追記型なので戻せない（`DECISIONS.md` D-104）。
async fn run_fixed_asset_add(args: FixedAssetArgs) -> Result<Vec<PathBuf>, String> {
    use kaikei_app::ports::{FixedAssetRepo, FixedAssetRow};

    let row = FixedAssetRow {
        id: uuid::Uuid::now_v7().to_string(),
        name: args.name.clone(),
        account: args.account.clone(),
        acquired_on: args.acquired_on,
        acquisition_cost: kaikei_core::Money::from_minor(
            i128::from(args.cost),
            kaikei_core::Currency::JPY,
        ),
        method: args.method,
        useful_life_years: args.useful_life_years,
        business_ratio: args.business_ratio.clone(),
        disposed_on: None,
        note: args.note.clone(),
    };

    // **予定表を先に出す。** 入力を取り違えていれば、ここで気づける。
    let asset = to_fixed_asset(&row)?;
    let schedule = kaikei_jp::depreciation::schedule(&asset)
        .map_err(|error| format!("償却額を計算できませんでした: {error}"))?;

    println!(
        "{}  {}  取得 {}  {} 円  [{}]",
        row.name,
        row.account.as_str(),
        row.acquired_on.to_iso_string(),
        row.acquisition_cost.to_display_string(),
        method_label(row.method),
    );
    if let Some(life) = row.useful_life_years {
        println!("  耐用年数 {life} 年");
    }
    if let Some(ratio) = &row.business_ratio {
        println!("  事業専用割合 {ratio}");
    }
    println!("  償却の予定:");
    for year in &schedule.years {
        println!(
            "    {} 年: {:>12} 円（{}か月）  期末簿価 {} 円",
            year.year,
            year.amount.to_display_string(),
            year.months,
            year.book_value.to_display_string(),
        );
    }
    let total = schedule
        .total()
        .map_err(|error| format!("償却費の合計を出せませんでした: {error}"))?;
    println!("    合計 {} 円", total.to_display_string());

    // **取得価額と償却方法が合っているかを見る。** 選べない方法を選んでも
    // 帳簿は何も言わない。50万円に少額特例を当てれば初年度に50万円が経費に
    // なり、貸借は一致したまま所得だけが減る。`verify` でも拾えない
    // （台帳の方法で計算した結果と帳簿は一致してしまう）。
    // **経理方式は任意で読む。** 帯の検査（所令138/139・措法28の2）に経理方式は
    // 要らない。設定が無いことを理由に本体の検査まで止めるのは筋が悪いので、
    // 読めなければ `None` を渡し、「確かめられなかった」と言わせる。
    let concerns = kaikei_jp::depreciation::cost_concerns(
        &row.acquisition_cost,
        asset.method,
        optional_tax_mode(),
    );
    if !concerns.is_empty() {
        println!();
        println!("  確かめてください:");
        for concern in &concerns {
            println!("    ・{}", concern.message);
            println!("      （{}）", concern.basis);
        }
    }

    if !args.commit {
        println!();
        println!("下見です。まだ台帳に入れていません。");
        println!("この予定でよければ --commit を付けて実行してください。");
        return Ok(Vec::new());
    }

    let database_url = env_var("APP_DATABASE_URL")?;
    let pool = connect_app(&database_url)
        .await
        .map_err(|error| format!("PostgreSQL に接続できませんでした: {error}"))?;
    let store = PgStore::new(pool);
    let inserted = with_tx_err(&store, move |tx| {
        let row = row.clone();
        Box::pin(async move { tx.insert_fixed_assets(&[row]).await })
    })
    .await
    .map_err(|error: kaikei_app::error::RepoError| {
        format!("固定資産台帳に入れられませんでした: {error}")
    })?;

    println!();
    println!("台帳に {inserted} 件入れました。");
    println!("償却費を出すには kaikei depreciation --year <西暦> を使います。");
    Ok(Vec::new())
}

fn parse_depreciation(args: &[String]) -> Result<Command, String> {
    let mut fiscal_year = None;
    let mut index = 0;
    while index < args.len() {
        let key = args[index].as_str();
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("{key} の値がありません"))?;
        match key {
            "--year" => {
                fiscal_year = Some(value.parse::<i32>().map_err(|_| {
                    format!("--year は西暦の数字で指定してください（受け取った値: {value}）")
                })?)
            }
            other => return Err(format!("不明な引数です: {other}\n\n{USAGE}")),
        }
        index += 2;
    }
    Ok(Command::Depreciation {
        fiscal_year: fiscal_year.ok_or("--year を指定してください（例: --year 2026）")?,
    })
}

/// 帳簿の税制設定。
///
/// # 環境変数から読み、既定値を持たない
///
/// 丸め方（`KAIKEI_ROUNDING`）と経理方式（`KAIKEI_TAX_MODE`）は MCP と同じ
/// 環境変数から読む。**帳簿ごとに違う値を黙って決め打ちしない**
/// （`docs/04-jp-tax.md`）。
///
/// **決め打ちにしていて誤りだった。** 最初これを家事按分専用に書いたとき、
/// 按分の金額は丸め方だけで決まるからと `tax_mode` に `Exclusive` を
/// 置いていた。その後、固定資産の取得価額の判定（10万/20万/30万円の帯）に
/// 使ったところ、**税込経理の帳簿を税抜として扱ってしまった**。実帳簿は
/// 税込経理で、108,000円 が税抜なら 98,181円 で帯が変わる——まさに
/// 気づきたかった場面で黙る形になっていた。
///
/// `rounding_unit` / `is_taxable_business` / `simplified_taxation` は
/// **まだどの計算にも効いていない**。効かせるときに読むこと。
/// 経理方式。**読めなければ `None`。**
///
/// 取得価額の帯の検査（`cost_concerns`）は経理方式が無くても動くので、
/// この設定が無いことを理由に検査そのものを止めない。読めなかったことは
/// 指摘の文面に出る（`kaikei_jp::depreciation::cost_concerns`）。
///
/// 値が壊れている場合も `None` にする。**ここで止めると、設定を直すまで
/// 資産を台帳に入れられなくなる**——それはこの検査が背負う責任ではない。
fn optional_tax_mode() -> Option<kaikei_jp::tax::TaxMode> {
    let code = std::env::var("KAIKEI_TAX_MODE").ok()?;
    kaikei_jp::tax::TaxMode::from_code(&code).ok()
}

fn jp_settings() -> Result<kaikei_jp::tax::JpSettings, String> {
    let code = env_var("KAIKEI_ROUNDING")?;
    let rounding = kaikei_jp::tax::round_mode_from_code(&code)
        .map_err(|error| format!("KAIKEI_ROUNDING を読めません: {error}"))?;
    let mode = env_var("KAIKEI_TAX_MODE")?;
    let tax_mode = kaikei_jp::tax::TaxMode::from_code(&mode)
        .map_err(|error| format!("KAIKEI_TAX_MODE を読めません: {error}"))?;
    Ok(kaikei_jp::tax::JpSettings {
        rounding,
        tax_mode,
        // 以下はまだどの計算にも効いていない（上の doc を参照）。
        rounding_unit: kaikei_jp::tax::RoundingUnit::Line,
        is_taxable_business: true,
        simplified_taxation: false,
    })
}

fn parse_consumption_tax(args: &[String]) -> Result<Command, String> {
    let mut fiscal_year = None;
    let mut index = 0;
    while index < args.len() {
        let key = args[index].as_str();
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("{key} の値がありません"))?;
        match key {
            "--year" => {
                fiscal_year = Some(value.parse::<i32>().map_err(|_| {
                    format!("--year は西暦の数字で指定してください（受け取った値: {value}）")
                })?)
            }
            other => {
                return Err(format!(
                    "不明な引数です: {other}

{USAGE}"
                ))
            }
        }
        index += 2;
    }
    Ok(Command::ConsumptionTax {
        fiscal_year: fiscal_year.ok_or("--year を指定してください（例: --year 2026）")?,
    })
}

/// 消費税の申告に向けた集計を出す。
///
/// # 申告書ではない
///
/// 出すのは集計値だけである。**家事按分・適格の確認・端数処理の規定・
/// 課税売上割合は反映していない**（`kaikei_jp::consumption_tax` の doc）。
/// 何をどの欄に書くかは申告上の判断であり、このソフトは決めない。
///
/// # 税込経理でなければ止める
///
/// 税額は税込金額から割り戻す。税抜経理の帳簿でこれをやると**税額を二重に
/// 数える**。黙って誤った数字を出すより止める。
/// 仕訳の明細を、消費税の集計に渡す形へ翻訳する。
///
/// **`kaikei-jp` は帳簿の読み方を知らない。** タグの取り出しはこの端が持つ。
fn tagged_lines_for_consumption_tax(
    entries: &[kaikei_core::JournalEntry],
) -> Result<Vec<kaikei_jp::consumption_tax::TaggedLine>, String> {
    let tax_key = kaikei_core::TagKey::parse("tax_category")
        .map_err(|error| format!("タグキーを読めません: {error}"))?;
    Ok(entries
        .iter()
        .flat_map(|entry| entry.lines().iter())
        .map(|line| kaikei_jp::consumption_tax::TaggedLine {
            tax_category: match line.tags().get(&tax_key) {
                Some(kaikei_core::TagValue::Code(code)) => Some(code.clone()),
                _ => None,
            },
            side: line.side(),
            amount: *line.amount(),
        })
        .collect())
}

async fn run_consumption_tax(fiscal_year: i32) -> Result<Vec<PathBuf>, String> {
    let settings = jp_settings()?;
    if settings.tax_mode != kaikei_jp::tax::TaxMode::Exclusive {
        // 税込経理。これが前提。
    } else {
        return Err(
            "この集計は税込経理（KAIKEI_TAX_MODE=inclusive）の帳簿を前提にしています。
             税抜経理では仮払・仮受消費税の明細から集計する必要があり、まだ作っていません。"
                .to_string(),
        );
    }
    if !settings.is_taxable_business {
        println!(
            "免税事業者の設定です（KAIKEI_IS_TAXABLE_BUSINESS=false）。消費税の申告はありません。"
        );
        return Ok(Vec::new());
    }
    if settings.simplified_taxation {
        return Err("簡易課税の設定です。この集計は原則課税を前提にしています。
             簡易課税では事業区分ごとのみなし仕入率が要り、まだ作っていません。"
            .to_string());
    }

    let database_url = env_var("APP_DATABASE_URL")?;
    let pool = connect_app(&database_url)
        .await
        .map_err(|error| format!("PostgreSQL に接続できませんでした: {error}"))?;
    let store = PgStore::new(pool);

    let year = FiscalYear::calendar_year(fiscal_year);
    let (from, to) = (year.start(), year.end());
    let entries = with_tx_err(&store, move |tx| {
        Box::pin(async move { tx.list_entries_in_period(from, to).await })
    })
    .await
    .map_err(|error: kaikei_app::error::RepoError| format!("帳簿を読めませんでした: {error}"))?;

    let rule_sets = kaikei_jp::tax::TaxRuleSets::from_embedded()
        .map_err(|error| format!("同梱の消費税区分マスタを読めませんでした: {error}"))?;
    let table = rule_sets
        .iter()
        .next()
        .ok_or("同梱の消費税区分マスタが空です")?;

    // **`report` と同じ関数で翻訳する。** 2箇所に書くと、片方だけ直したときに
    // 画面とファイルで数字が食い違う。
    let lines = tagged_lines_for_consumption_tax(&entries)?;

    let summary = kaikei_jp::consumption_tax::summarize(&lines, table)
        .map_err(|error| format!("集計できませんでした: {error}"))?;

    println!("{fiscal_year} 年の消費税の集計（原則課税・税込経理）");
    println!();
    for category in &summary.categories {
        let tax = match &category.tax {
            Some(tax) => format!("うち消費税相当額 {} 円", tax.to_display_string()),
            None => "税額の計算対象外".to_string(),
        };
        // **全角の桁揃えは当てにしない。** Rust の `{:<28}` はバイト数でも
        // 文字数でもなく char 数で数えるので、全角混じりでは揃わない。
        // 揃えようとして崩れるより、区切り記号で読ませる。
        println!(
            "  {} … {} 円（{tax}）",
            category.label,
            category.amount.to_display_string()
        );
    }
    println!();
    println!(
        "  課税売上（税込）  {} 円 / 消費税相当額 {} 円",
        summary.taxable_sales().to_display_string(),
        summary.tax_on_sales().to_display_string()
    );
    println!(
        "  課税仕入（税込）  {} 円 / 消費税相当額 {} 円",
        summary.taxable_purchases().to_display_string(),
        summary.tax_on_purchases().to_display_string()
    );

    // **集計が不完全なら、必ず言う。** 数字だけ見せると揃っていると読める。
    if summary.lines_without_a_category > 0 {
        println!();
        println!(
            "注意: 税区分が付いていない明細が {} 件あります。",
            summary.lines_without_a_category
        );
        println!("  口座や事業主貸のように税区分を持たない明細も含まれます。");
        println!("  課税取引なのに付いていないものがあれば、その分は集計から抜けています。");
    }
    if summary.lines_with_an_unknown_category > 0 {
        eprintln!();
        eprintln!(
            "注意: 同梱の税区分マスタに無いコードの明細が {} 件あります。",
            summary.lines_with_an_unknown_category
        );
        eprintln!("  この分は集計に入っていません。");
    }

    println!();
    println!("**これは申告書の金額ではありません。**");
    println!("  家事按分・適格請求書発行事業者かどうかの確認・経過措置の控除割合・");
    println!("  端数処理の規定・課税売上割合による按分は、いずれも反映していません。");
    println!("  申告に使う前に税理士に確認してください。");
    Ok(Vec::new())
}

fn parse_household(args: &[String]) -> Result<Command, String> {
    let mut fiscal_year = None;
    let mut account = None;
    let mut ratio = None;
    let mut amount = None;
    let mut index = 0;
    while index < args.len() {
        let key = args[index].as_str();
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("{key} の値がありません"))?;
        match key {
            "--year" => {
                fiscal_year = Some(value.parse::<i32>().map_err(|_| {
                    format!("--year は西暦の数字で指定してください（受け取った値: {value}）")
                })?)
            }
            "--account" => account = Some(value.clone()),
            "--ratio" => ratio = Some(value.clone()),
            "--amount" => {
                amount = Some(
                    kaikei_app::amount::strip_thousands_separators(value)
                        .parse::<i64>()
                        .map_err(|_| format!("--amount は数字で指定してください: {value}"))?,
                )
            }
            other => return Err(format!("不明な引数です: {other}\n\n{USAGE}")),
        }
        index += 2;
    }
    Ok(Command::Household {
        fiscal_year: fiscal_year.ok_or("--year を指定してください（例: --year 2026）")?,
        account: account.ok_or("--account を指定してください（例: --account 615）")?,
        ratio: ratio.ok_or("--ratio を指定してください（例: --ratio 0.3）")?,
        amount,
    })
}

/// 決算時の家事按分の振替仕訳を出す。
///
/// # 記帳はしない
///
/// **事業割合が妥当かはこのソフトには分からない。** 自宅の面積のうち仕事に
/// 使っている割合や、使用時間の記録を持っていないからである。出すだけにして、
/// 記帳するかどうかは人が決める（`depreciation` と同じ立場）。
///
/// # なぜ要るのか
///
/// 家事按分を忘れると**所得が過少になる**。減価償却の忘れ（所得が過大）と
/// 向きが逆で、こちらは税務上のほうが不利になる。しかも `verify` では
/// 拾えない——どの科目が按分対象かはソフトには分からず、按分していない帳簿と
/// 事業専用の帳簿は見分けがつかないからである。**人が年に一度打つ**しかない。
///
/// 実際 WeBanana.SP の2026年の帳簿は、自宅の家賃と電気代を全額そのまま
/// 経費に計上したまま「按分は確定申告時」と摘要に書いて先送りしていた。
async fn run_household(
    fiscal_year: i32,
    account: &str,
    ratio: &str,
    amount: Option<i64>,
) -> Result<Vec<PathBuf>, String> {
    let expense_account = kaikei_core::AccountCode::parse(account)
        .map_err(|error| format!("科目コードを読めません: {account}（{error}）"))?;
    let business_ratio = kaikei_core::Ratio::parse_fraction(ratio)
        .map_err(|error| format!("事業割合を読めません: {ratio}（{error}）"))?;

    let database_url = env_var("APP_DATABASE_URL")?;
    let pool = connect_app(&database_url)
        .await
        .map_err(|error| format!("PostgreSQL に接続できませんでした: {error}"))?;
    let store = PgStore::new(pool);

    let catalog = TagCatalog::from_embedded(kaikei_jp_data::TAGS)
        .map_err(|error| format!("同梱のタグ定義を読めませんでした: {error}"))?;
    let schema = catalog.schema().clone();
    let year = FiscalYear::calendar_year(fiscal_year);
    let (from, to) = (year.start(), year.end());

    let wanted = expense_account.clone();
    let (income_statement, is_in_the_chart) = with_tx_err(&store, move |tx| {
        let schema = schema.clone();
        let wanted = wanted.clone();
        Box::pin(async move {
            let chart = tx.load_chart().await?;
            // **科目表にあるかを先に見る。** 無いコードを打ったときに
            // 「計上がありません」とだけ出ると、打ち間違いなのか本当に
            // 0円なのか分からない。
            let is_in_the_chart = chart.get(&wanted).is_some();
            let policy = JpStatementPolicy::new(chart);
            let statements = statements::execute(
                tx,
                &policy,
                &schema,
                StatementsInput {
                    from,
                    to,
                    // **決算振替を外す。** 外さないと、決算振替を記帳した後に
                    // 打ったとき経費が0に見えて「按分するものが無い」と出る。
                    exclude_closing_on: Some(year.end()),
                    only_opening_on: None,
                },
            )
            .await?;
            Ok::<_, kaikei_app::error::AppError>((statements.income_statement, is_in_the_chart))
        })
    })
    .await
    .map_err(|error: kaikei_app::error::AppError| format!("帳簿を読めませんでした: {error}"))?;

    let total_minor = income_statement
        .sections
        .iter()
        .flat_map(|section| section.lines.iter())
        .find(|line| line.account == expense_account)
        .map(|line| line.amount.minor())
        .unwrap_or(0);

    let account_name = income_statement
        .sections
        .iter()
        .flat_map(|section| section.lines.iter())
        .find(|line| line.account == expense_account)
        .map(|line| line.label.clone())
        .unwrap_or_else(|| expense_account.as_str().to_string());

    if !is_in_the_chart {
        return Err(format!(
            "{} という科目は勘定科目表にありません。
kaikei report を出すと科目の一覧が見られます。",
            expense_account.as_str()
        ));
    }

    if total_minor <= 0 {
        println!(
            "{} には {fiscal_year} 年の計上がありません（残高 {total_minor} 円）。",
            expense_account.as_str()
        );
        println!("年度を確かめてください。按分するものがありません。");
        return Ok(Vec::new());
    }

    // 按分する額。`--amount` が無ければ科目の年間計上額の全額。
    //
    // **計上額を超える額は受け付けない。** 帳簿に無い額を按分すると、経費が
    // マイナスになる仕訳ができあがる。貸借は一致するので決算書を見ても
    // 気づけない。
    let subject_minor = match amount {
        Some(given) => {
            if given <= 0 {
                return Err(format!("--amount は正の値で指定してください: {given}"));
            }
            let given = i128::from(given);
            if given > total_minor {
                return Err(format!(
                    "--amount が {} の計上額を超えています（指定 {given} 円 / 計上額 {total_minor} 円）。\n\
                    帳簿に無い額は按分できません。",
                    expense_account.as_str()
                ));
            }
            given
        }
        None => total_minor,
    };

    println!("{fiscal_year} 年の家事按分（{account_name}・事業割合 {ratio}）");
    println!();

    let total = kaikei_core::Money::from_minor(subject_minor, kaikei_core::Currency::JPY);
    let settings = jp_settings()?;
    let lines = kaikei_jp::household_split::year_end_household_split(
        kaikei_jp::household_split::YearEndHouseholdSplitInput {
            total,
            business_ratio,
            expense_account: expense_account.clone(),
            owner_account: kaikei_core::AccountCode::parse("410")
                .expect("\"410\"（事業主貸）は勘定科目表にある固定のコード"),
            tax_category: None,
        },
        &settings,
    )
    .map_err(|error| format!("按分を計算できませんでした: {error}"))?;

    // **両方を出す。** 一部だけを按分したとき、どちらの額なのかが
    // 分からないと、後から見て正しいか確かめられない。
    println!(
        "  計上額 {} 円",
        kaikei_core::Money::from_minor(total_minor, kaikei_core::Currency::JPY).to_display_string()
    );
    if subject_minor != total_minor {
        println!(
            "  うち按分対象 {} 円（--amount で指定）",
            total.to_display_string()
        );
        println!(
            "  残り {} 円は事業専用として按分しません",
            kaikei_core::Money::from_minor(total_minor - subject_minor, kaikei_core::Currency::JPY)
                .to_display_string()
        );
    }
    if lines.is_empty() {
        println!();
        println!("事業割合が100%なので、振り替えるものはありません。");
        return Ok(Vec::new());
    }

    println!();
    println!(
        "  {} 決算整理（家事按分・{account_name}）",
        year.end().to_iso_string()
    );
    for line in &lines {
        let side = if line.side() == kaikei_core::Side::Debit {
            "借"
        } else {
            "貸"
        };
        println!(
            "    {side} {:<8} {} 円",
            line.account().as_str(),
            line.amount().to_display_string()
        );
    }

    println!();
    println!("記帳はしていません。仕訳にするには post_journal_entry を使います。");
    println!();
    println!("**事業割合が妥当かは、このソフトでは判定していません。**");
    println!("面積や使用時間など、割合の根拠になる記録を残してください。");
    Ok(Vec::new())
}

/// 台帳の1件を、償却計算の入力に翻訳する。
///
/// # なぜ端で翻訳するのか
///
/// `kaikei-app` は `kaikei-jp` に依存できない（CI が禁じている）。台帳は
/// 償却方法を数値で持っており、`DepreciationMethod` への写像はこの端が持つ。
fn to_fixed_asset(
    row: &kaikei_app::ports::FixedAssetRow,
) -> Result<kaikei_jp::depreciation::FixedAsset, String> {
    use kaikei_jp::depreciation::DepreciationMethod;

    let method = match row.method {
        1 => {
            let life = row
                .useful_life_years
                .ok_or_else(|| format!("{}: 定額法なのに耐用年数がありません", row.name))?;
            DepreciationMethod::StraightLine {
                useful_life_years: u8::try_from(life)
                    .map_err(|_| format!("{}: 耐用年数が範囲外です: {life}", row.name))?,
            }
        }
        2 => DepreciationMethod::LumpSumOverThreeYears,
        3 => DepreciationMethod::ImmediateExpense,
        other => {
            return Err(format!(
                "{}: 知らない償却方法です: {other}（1=定額法 / 2=一括償却 / 3=少額特例）",
                row.name
            ));
        }
    };

    let business_ratio = match &row.business_ratio {
        Some(text) => Some(
            kaikei_core::Ratio::parse_fraction(text)
                .map_err(|error| format!("{}: 事業専用割合を読めません: {error}", row.name))?,
        ),
        None => None,
    };

    Ok(kaikei_jp::depreciation::FixedAsset {
        name: row.name.clone(),
        acquired_on: row.acquired_on,
        acquisition_cost: row.acquisition_cost,
        method,
        business_ratio,
    })
}

/// 固定資産台帳から、その年度の減価償却費を出す。
///
/// # 記帳はしない
///
/// 出すだけである。**どの扱いを選ぶかは申告上の判断**であり、台帳に入れる値
/// （耐用年数・償却方法）を決めるのは人である（`DECISIONS.md` D-103）。
/// 記帳するかどうかも人が決める。
async fn run_depreciation(fiscal_year: i32) -> Result<Vec<PathBuf>, String> {
    use kaikei_app::ports::FixedAssetRepo;

    let database_url = env_var("APP_DATABASE_URL")?;
    let pool = connect_app(&database_url)
        .await
        .map_err(|error| format!("PostgreSQL に接続できませんでした: {error}"))?;
    let store = PgStore::new(pool);

    let rows = with_tx_err(&store, |tx| {
        Box::pin(async move { tx.list_fixed_assets().await })
    })
    .await
    .map_err(|error: kaikei_app::error::RepoError| {
        format!("固定資産台帳を読めませんでした: {error}")
    })?;

    if rows.is_empty() {
        println!("固定資産台帳に登録がありません。");
        println!("減価償却費を出すには、取得日・取得価額・償却方法・耐用年数を台帳に入れます。");
        return Ok(Vec::new());
    }

    println!("{fiscal_year} 年の減価償却費");
    println!();

    let mut total: i128 = 0;
    let mut counted = 0usize;
    for row in &rows {
        if is_outside_the_ledger_for(row, fiscal_year) {
            continue;
        }
        let asset = to_fixed_asset(row)?;
        let schedule = kaikei_jp::depreciation::schedule(&asset)
            .map_err(|error| format!("{}: 償却額を計算できませんでした: {error}", row.name))?;
        let Some(year) = schedule.year(fiscal_year) else {
            continue;
        };
        total += year.amount.minor();
        counted += 1;
        println!(
            "  {}  {} 円（{}か月）  期末簿価 {} 円  [{}]",
            row.name,
            year.amount.to_display_string(),
            year.months,
            year.book_value.to_display_string(),
            method_label(row.method),
        );
    }

    println!();
    if counted == 0 {
        println!("この年度に償却する資産はありません（取得前か、償却し終わっています）。");
        return Ok(Vec::new());
    }
    println!(
        "合計 {} 円（{counted} 件）",
        kaikei_core::Money::from_minor(total, kaikei_core::Currency::JPY).to_display_string()
    );
    println!();
    println!("記帳はしていません。仕訳にするには post_journal_entry を使います。");
    println!("  借方 減価償却費 / 貸方 それぞれの資産の科目");
    Ok(Vec::new())
}

/// 償却方法の表示名。
fn method_label(method: i16) -> &'static str {
    match method {
        1 => "定額法",
        2 => "一括償却",
        3 => "少額特例",
        _ => "不明",
    }
}

/// 既存の取引先に、適格請求書発行事業者の登録番号と確認結果を記録する。
///
/// # なぜ要るのか
///
/// 相手が適格請求書発行事業者かどうかは**後から分かる**情報である。取引を
/// 記帳した時点では未確認で、あとで先方に伺って埋める。`counterparty import`
/// は既存を上書きしないので、この経路が無いと**一度作った取引先には
/// 二度と登録番号を入れられない**（実帳簿の31件がその状態だった）。
///
/// # 名前は変えない
///
/// 変えるのは登録番号・適格の確認結果・確認日だけ。**名前を変えられると、
/// 過去の仕訳が指している相手が静かに別物になる。**
///
/// # 既定は下見
///
/// 何がどう変わるかを見せてから、`--commit` で書き込む。
async fn run_counterparty_verify(
    code: String,
    registration_no: Option<String>,
    is_qualified: Option<bool>,
    verified_on: Option<AccountingDate>,
    commit: bool,
) -> Result<Vec<PathBuf>, String> {
    use kaikei_app::ports::{ChartRepo, CounterpartyWriteRepo};

    // 登録番号の形を先に見る。**書き込んでから気づいても遅い**
    // （帳簿ではないので直せるが、誤った番号は「確認済み」に見える）。
    if let Some(number) = &registration_no {
        kaikei_jp::invoice::InvoiceRegistrationNo::parse(number)
            .map_err(|error| format!("登録番号の形が正しくありません: {error}"))?;
    }

    // **確認日は省略できる。** 省略時は今日（JST）。日付を打たせると、
    // 「いつ確認したか」を記録する目的なのに打ち間違いが入りうる。
    let on = match verified_on {
        Some(date) => date,
        None => {
            use chrono::Datelike;
            let now = chrono::Local::now().date_naive();
            AccountingDate::new(now.year(), now.month() as u8, now.day() as u8)
                .map_err(|error| format!("今日の日付を作れませんでした: {error}"))?
        }
    };

    let database_url = env_var("APP_DATABASE_URL")?;
    let pool = connect_app(&database_url)
        .await
        .map_err(|error| format!("PostgreSQL に接続できませんでした: {error}"))?;
    let store = PgStore::new(pool);

    let counterparties = with_tx_err(&store, |tx| {
        Box::pin(async move { tx.load_counterparties().await })
    })
    .await
    .map_err(|error: kaikei_app::error::RepoError| {
        format!("取引先マスタを読めませんでした: {error}")
    })?;

    let before = counterparties.get(&code).ok_or_else(|| {
        format!("取引先 {code} が見つかりません。kaikei counterparty import で先に登録してください")
    })?;

    println!("{}（{}）", before.name, before.code);
    println!(
        "  登録番号  {} → {}",
        before
            .invoice_registration_no
            .clone()
            .unwrap_or_else(|| "（なし）".to_string()),
        registration_no
            .clone()
            .unwrap_or_else(|| "（変えません）".to_string())
    );
    println!(
        "  適格      {} → {}",
        match before.is_qualified_invoice_issuer {
            Some(true) => "適格",
            Some(false) => "非適格",
            None => "（未確認）",
        },
        match is_qualified {
            Some(true) => "適格".to_string(),
            Some(false) => "非適格".to_string(),
            None => "（変えません）".to_string(),
        }
    );
    println!("  確認日    {}", on.to_iso_string());

    if !commit {
        println!();
        println!("下見です。まだ書き込んでいません。");
        println!("この内容でよければ --commit を付けて実行してください。");
        return Ok(Vec::new());
    }

    // **省略した項目は既存の値を残す。** `None` をそのまま渡すと消える。
    let reg = registration_no.or_else(|| before.invoice_registration_no.clone());
    let qualified = is_qualified.or(before.is_qualified_invoice_issuer);

    let updated = with_tx_err(&store, move |tx| {
        let code = code.clone();
        let reg = reg.clone();
        Box::pin(async move {
            tx.set_counterparty_invoice_status(&code, reg.as_deref(), qualified, on)
                .await
        })
    })
    .await
    .map_err(|error: kaikei_app::error::RepoError| format!("記録できませんでした: {error}"))?;

    println!();
    println!("{updated} 件を更新しました。");
    Ok(Vec::new())
}

/// `kaikei counterparty verify` の引数を解析する。
///
/// # なぜ `import` と分けるのか
///
/// **一括投入と1件ずつの確認は別の操作である。** 前者は「まとめて取り込む」
/// で既存を上書きしない。後者は「調べた結果を記録する」で、上書きするのが
/// 目的である。同じコマンドにすると、取り込みのつもりが上書きになる。
fn parse_counterparty_verify(args: &[String]) -> Result<Command, String> {
    let mut code = None;
    let mut registration_no = None;
    let mut is_qualified = None;
    let mut verified_on = None;
    let mut commit = false;
    let mut index = 0;
    while index < args.len() {
        let key = args[index].as_str();
        if key == "--commit" {
            commit = true;
            index += 1;
            continue;
        }
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("{key} の値がありません"))?;
        match key {
            "--code" => code = Some(value.clone()),
            "--registration-no" => registration_no = Some(value.clone()),
            "--qualified" => {
                is_qualified = Some(match value.as_str() {
                    "true" => true,
                    "false" => false,
                    other => {
                        return Err(format!(
                            "--qualified は true か false で指定してください（受け取った値: {other}）。「未確認」を表したいなら、この引数を付けないでください。"
                        ))
                    }
                })
            }
            "--on" => {
                verified_on = Some(
                    AccountingDate::parse(value)
                        .map_err(|error| format!("--on は YYYY-MM-DD で指定してください: {error}"))?,
                )
            }
            other => return Err(format!("不明な引数です: {other}")),
        }
        index += 2;
    }

    // **何も変えない呼び出しを黙って通さない。** 打ち間違いで
    // `--registration-no` を落とすと、確認日だけが更新されて
    // 「確認したのに何も分からなかった」状態になる。
    if registration_no.is_none() && is_qualified.is_none() {
        return Err(
            "--registration-no か --qualified のどちらかを指定してください。どちらも無いと、確認日だけが変わって中身は変わりません。"
                .to_string(),
        );
    }

    Ok(Command::CounterpartyVerify {
        code: code.ok_or("--code を指定してください（例: --code jdf）")?,
        registration_no,
        is_qualified,
        verified_on,
        commit,
    })
}

fn parse_counterparty(args: &[String]) -> Result<Command, String> {
    let Some(action) = args.first() else {
        return Err(format!(
            "counterparty の後に import を指定してください\n\n{USAGE}"
        ));
    };
    if action == "verify" {
        return parse_counterparty_verify(&args[1..]);
    }
    if action != "import" {
        return Err(format!(
            "counterparty のサブコマンドは import と verify です（受け取った値: {action}）\n\n{USAGE}"
        ));
    }

    let mut file = None;
    let mut commit = false;
    let rest = &args[1..];
    let mut index = 0;
    while index < rest.len() {
        let key = rest[index].as_str();
        // 値を取る引数と取らない引数を混ぜない（`parse_import` と同じ理由）。
        if key == "--commit" {
            commit = true;
            index += 1;
            continue;
        }
        let value = rest
            .get(index + 1)
            .ok_or_else(|| format!("{key} の値がありません"))?
            .clone();
        match key {
            "--file" => file = Some(PathBuf::from(value)),
            other => return Err(format!("不明な引数です: {other}\n\n{USAGE}")),
        }
        index += 2;
    }

    Ok(Command::Counterparty(CounterpartyArgs {
        file: file.ok_or("--file を指定してください（取引先の CSV）")?,
        commit,
    }))
}

/// 取引先 CSV の1行。
#[derive(Debug)]
struct CounterpartyRow {
    counterparty: kaikei_app::Counterparty,
    /// 何行目か（1始まり。見出しを除く）。エラーの位置を示すため。
    line_no: usize,
}

/// 取引先 CSV を読む。
///
/// 列は `code,name,invoice_registration_no,is_qualified`。見出し行が要る
/// （列の順番を覚えさせない）。`code` と `name` 以外は省略できる。
///
/// # 未確認と非適格を区別する
///
/// `is_qualified` が**空欄なら未確認**（`None`）、`true`/`false` ならその値。
/// この区別は `JpTaxPolicy` が記帳を拒むかどうかを決めている
/// （`Some(false)` のときだけ拒む）。外部システムの「誰も入力していないので
/// false」をそのまま `false` として持ち込むと、確認していない取引先を
/// 「非適格だと確認済み」に仕立ててしまう。**実際に freee の取引先 34 件は
/// 全件が `qualified_invoice_issuer: false` かつ登録番号 `null` だった。**
fn parse_counterparty_csv(text: &str) -> Result<Vec<CounterpartyRow>, String> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(false)
        .from_reader(text.as_bytes());

    let headers = reader
        .headers()
        .map_err(|error| format!("CSV の見出し行を読めませんでした: {error}"))?
        .clone();
    let column = |name: &str| -> Option<usize> {
        headers
            .iter()
            // BOM 付きで保存された CSV では最初の列名に BOM が残る。
            .position(|h| h.trim_start_matches('\u{feff}').trim() == name)
    };
    let code_at = column("code").ok_or("CSV に code 列がありません")?;
    let name_at = column("name").ok_or("CSV に name 列がありません")?;
    let reg_at = column("invoice_registration_no");
    let qualified_at = column("is_qualified");

    let mut rows = Vec::new();
    for (index, record) in reader.records().enumerate() {
        let line_no = index + 1;
        let record =
            record.map_err(|error| format!("{line_no} 行目を読めませんでした: {error}"))?;
        let cell = |at: Option<usize>| -> Option<String> {
            at.and_then(|at| record.get(at))
                .map(|text| text.trim().to_string())
        };
        let code = record.get(code_at).unwrap_or("").trim().to_string();
        let name = record.get(name_at).unwrap_or("").trim().to_string();
        if code.is_empty() {
            return Err(format!("{line_no} 行目: code が空です"));
        }
        if name.is_empty() {
            return Err(format!("{line_no} 行目: name が空です（{code}）"));
        }

        let invoice_registration_no = cell(reg_at).filter(|text| !text.is_empty());
        let is_qualified_invoice_issuer = match cell(qualified_at).unwrap_or_default().as_str() {
            "" => None,
            "true" => Some(true),
            "false" => Some(false),
            other => {
                return Err(format!(
                    "{line_no} 行目: is_qualified は true / false / 空欄のいずれかです（受け取った値: {other}）。空欄は「まだ確認していない」という意味で、false（非適格だと確認した）とは別に扱われます"
                ));
            }
        };

        rows.push(CounterpartyRow {
            counterparty: kaikei_app::Counterparty {
                code,
                name,
                invoice_registration_no,
                is_qualified_invoice_issuer,
            },
            line_no,
        });
    }

    Ok(rows)
}

/// 下見で並べて見せる取引先の件数。
const COUNTERPARTY_PREVIEW_ROWS: usize = 15;

/// 取引先マスタを CSV から投入する。
///
/// # 既定では保存しない
///
/// `kaikei import` と同じ（`--commit` が無ければ読んで見せるだけ）。
/// 取引先コードは仕訳のタグに入る値であり、**一度タグを付けた後にコードを
/// 変えると、過去の仕訳が指す先が消える**（`journal_lines` は追記型なので
/// タグを直せない）。入れる前に目で確かめられるようにする。
async fn run_counterparty_import(args: CounterpartyArgs) -> Result<Vec<PathBuf>, String> {
    let text = std::fs::read_to_string(&args.file)
        .map_err(|error| format!("{} を読めませんでした: {error}", args.file.display()))?;
    let rows = parse_counterparty_csv(&text)?;
    if rows.is_empty() {
        println!("取り込む取引先がありません（{}）", args.file.display());
        return Ok(Vec::new());
    }

    println!(
        "{} 件を読み取りました（{}）",
        rows.len(),
        args.file.display()
    );
    for row in rows.iter().take(COUNTERPARTY_PREVIEW_ROWS) {
        let qualified = match row.counterparty.is_qualified_invoice_issuer {
            None => "未確認",
            Some(true) => "適格",
            Some(false) => "非適格",
        };
        println!(
            "  {:>3}  {}  {}  登録番号={}  適格={}",
            row.line_no,
            row.counterparty.code,
            row.counterparty.name,
            row.counterparty
                .invoice_registration_no
                .as_deref()
                .unwrap_or("(なし)"),
            qualified,
        );
    }
    if rows.len() > COUNTERPARTY_PREVIEW_ROWS {
        println!("  ... 他 {} 件", rows.len() - COUNTERPARTY_PREVIEW_ROWS);
    }

    if !args.commit {
        println!();
        println!("下見です。まだ書き込んでいません。");
        println!("この内容でよければ --commit を付けて実行してください。");
        return Ok(Vec::new());
    }

    let list: Vec<kaikei_app::Counterparty> =
        rows.into_iter().map(|row| row.counterparty).collect();
    let database_url = env_var("APP_DATABASE_URL")?;
    let pool = connect_app(&database_url)
        .await
        .map_err(|error| format!("PostgreSQL に接続できませんでした: {error}"))?;
    let store = PgStore::new(pool);
    let output = with_tx_err(&store, move |tx| {
        let list = list.clone();
        Box::pin(
            async move { kaikei_app::usecase::import_counterparties::execute(tx, &list).await },
        )
    })
    .await
    .map_err(|error: kaikei_app::error::AppError| {
        format!("取引先マスタを投入できませんでした: {error}")
    })?;

    println!("{}", output.summary());
    for difference in &output.kept_existing {
        eprintln!("注意: {}", difference.describe());
    }
    Ok(Vec::new())
}

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

/// 固定資産があるのに減価償却費が計上されていなければ知らせる。
///
/// # なぜ黙って出さないか
///
/// **減価償却の計上漏れは決算書を見ても分からない。** 貸借は一致したままで、
/// 所得だけが過大になる。翌年に気づいても、その年分は申告済みである。
///
/// # 何を言い、何を言わないか
///
/// 言うのは「固定資産があるのに減価償却費が0である」という**帳簿の事実**
/// だけである。いくら償却すべきかは言わない——このソフトは固定資産台帳を
/// 持たず、取得年月日も耐用年数も事業専用割合も知らない（`CLAUDE.md` §10。
/// 税務判断を断定しない）。
///
/// 科目表に `depreciation` 節が無ければ何もしない（利用者が自分の科目表に
/// 差し替えた場合）。
/// 債権・債務の科目名。**勘定科目表の名前と一致していなければ検査が効かない。**
///
/// 科目コードではなく名前で見ているのは、この検査が「一般的な帳簿の形」を
/// 見ているからである。コードの体系は帳簿ごとに違いうるが、この4つの名前は
/// 青色申告決算書の様式にある。
///
/// **名前が変わると黙って効かなくなる。** 同梱の勘定科目表にこの名前が実在
/// することをテストで固定してある（`the_chart_still_has_the_names_this_check_needs`）。
const RECEIVABLE_AND_PAYABLE_NAMES: [&str; 4] = ["売掛金", "買掛金", "前受金", "前払金"];

/// 売上があるのに、**債権・債務の科目が1件も動いていない**ことを知らせる。
///
/// # 入出金ベースの記帳は決算で足りない
///
/// 個人事業主の原則は発生主義である（所得税法36条1項「その年において
/// **収入すべき**金額」）。入金した日に売上を立てる形のままだと、
/// **12月に役務提供が終わって1月に入金される分がその年の所得に入らない。**
///
/// 現金主義の特例（所法67条）は前々年の事業所得等が300万円以下という要件が
/// あり、多くの場合は使えない。
///
/// # 「使っていない」と「要らない」は違う
///
/// 売掛金が0件でも、**その年のうちに全部入金されていれば正しい**。
/// 前受金・前払金も同じで、無いのが正常な年はある。**だから断定しない。**
///
/// # 決算書を見ても分からない
///
/// 貸借は一致したままで、売掛金の行が 0 と表示されるだけである。
/// 「立て忘れ」と「立てる必要が無かった」は、決算書の上では同じに見える。
///
/// # 売上が無い年は言わない
///
/// 開業前や休業中に「売掛金がありません」と言っても手がかりにならない。
fn warn_if_receivables_are_never_used(
    income_statement: &kaikei_app::policy::Statement,
    balance_sheet: &kaikei_app::policy::Statement,
    chart: &kaikei_core::ChartOfAccounts,
) -> Result<(), String> {
    if !receivables_are_never_used(income_statement, balance_sheet, chart) {
        return Ok(());
    }

    eprintln!("注意: 売上がありますが、売掛金・買掛金・前受金・前払金がどれも0です。");
    eprintln!("  入金した日に売上を立てる記帳（入出金ベース）になっていないか確かめてください。");
    eprintln!("  個人事業主の原則は発生主義（所得税法36条1項）で、");
    eprintln!("  12月に役務提供が終わって1月に入金される分は、その年の所得に入ります。");
    eprintln!("  その年のうちに全部入金・支払済みであれば、0のままで正しいです。");
    Ok(())
}

/// 判定だけ。**表示から切り離してある**（`year_looks_closed` と同じ理由。
/// 何を拾うかをテストで固定できないと、「落ちないこと」しか確かめられない）。
fn receivables_are_never_used(
    income_statement: &kaikei_app::policy::Statement,
    balance_sheet: &kaikei_app::policy::Statement,
    chart: &kaikei_core::ChartOfAccounts,
) -> bool {
    // 収益が1円も無ければ何も言わない。
    let has_revenue = income_statement
        .sections
        .iter()
        .flat_map(|section| section.lines.iter())
        .any(|line| {
            !line.amount.is_zero()
                && chart
                    .get(&line.account)
                    .is_some_and(|def| def.account_type == kaikei_core::AccountType::Revenue)
        });
    if !has_revenue {
        return false;
    }

    // 債権・債務の科目が1つでも動いていれば何も言わない。
    !RECEIVABLE_AND_PAYABLE_NAMES.iter().any(|name| {
        balance_sheet
            .sections
            .iter()
            .flat_map(|section| section.lines.iter())
            .any(|line| line.label.contains(name) && !line.amount.is_zero())
    })
}

fn warn_if_depreciation_is_missing(
    income_statement: &kaikei_app::policy::Statement,
    balance_sheet: &kaikei_app::policy::Statement,
) -> Result<(), String> {
    let hint = kaikei_jp::chart::load_depreciation_hint(kaikei_jp_data::CHART_SOLE_PROPRIETOR)
        .map_err(|error| format!("科目表の depreciation 節を読めませんでした: {error}"))?;
    let Some(hint) = hint else {
        return Ok(());
    };

    let expense = amount_of(income_statement, &hint.expense_account);
    if !expense.is_zero() {
        return Ok(());
    }

    let assets: Vec<(&kaikei_core::AccountCode, kaikei_core::Money)> = hint
        .depreciable_accounts
        .iter()
        .map(|code| (code, amount_of(balance_sheet, code)))
        .filter(|(_, amount)| !amount.is_zero())
        .collect();
    if assets.is_empty() {
        return Ok(());
    }

    eprintln!(
        "注意: 減価償却の対象になりうる資産がありますが、減価償却費（{}）が\
         1円も計上されていません。",
        hint.expense_account.as_str()
    );
    for (code, amount) in &assets {
        eprintln!("  {} {}", code.as_str(), amount.to_display_string());
    }
    eprintln!(
        "  計上漏れであれば、所得がその分だけ過大になります（貸借は一致した\
         ままなので決算書を見ても分かりません）。"
    );
    eprintln!(
        "  いくら償却するかはこのソフトでは決められません（取得年月日・\
         耐用年数・事業専用割合を持っていません）。"
    );
    Ok(())
}

/// 資産がマイナス残高、負債がプラス残高になっていれば知らせる。
///
/// # なぜ黙って出さないか
///
/// **貸借は一致したままなので決算書を見ても気づけない。** 実際に weBanana.SP で、
/// 償却の相手科目を取り違えたために工具器具備品が -118,800 円になっていた誤りが
/// 4年間気づかれずに残った。預金がマイナスのまま貸借対照表に載るのも同じ形である。
///
/// # 評価勘定は指摘しない
///
/// 減価償却累計額は資産に分類されるが**貸方に立つのが正しい**。これを指摘すると
/// 正しい帳簿で毎回警告が出て、本当の異常が埋もれる。どれが評価勘定かは科目表が
/// 持つ（`contra_accounts`）。
///
/// 事業主貸・事業主借は純資産なので対象外——どちらの向きにも立ちうる。
fn warn_if_a_balance_sits_on_the_wrong_side(
    balance_sheet: &kaikei_app::policy::Statement,
    chart: &kaikei_core::ChartOfAccounts,
) -> Result<(), String> {
    let contra = kaikei_jp::chart::load_contra_accounts(kaikei_jp_data::CHART_SOLE_PROPRIETOR)
        .map_err(|error| format!("科目表の contra_accounts を読めませんでした: {error}"))?;

    let wrong = accounts_on_the_wrong_side(balance_sheet, chart, &contra);
    if wrong.is_empty() {
        return Ok(());
    }

    eprintln!(
        "注意: 貸借が自然な向きと逆になっている科目が {} 件あります:",
        wrong.len()
    );
    for (code, name, amount) in &wrong {
        eprintln!("  {code} {name} {}", amount.to_display_string());
    }
    eprintln!(
        "  資産がマイナス、負債がプラスの残高は普通ではありません\
         （貸借は一致したままなので決算書を見ても分かりません）。"
    );
    eprintln!(
        "  記帳先の取り違え・期首残高の誤り・仕訳の抜けのいずれかを疑ってください。\
         評価勘定（減価償却累計額など）はこの指摘の対象外です。"
    );
    Ok(())
}

/// 証憑が付いている仕訳の割合を出す。
///
/// # 断定しない
///
/// 出すのは**数字だけ**である。保存義務を満たしているかどうかは、事業者の
/// 状況（猶予措置の適用可否など）で変わるので、このソフトでは判断しない
/// （`CLAUDE.md` §10）。0件のときに何を確かめればよいかは添える。
async fn print_document_coverage(
    documents: &PgDocumentQuery,
    fiscal_year: i32,
    entry_count: usize,
) -> Result<(), String> {
    use kaikei_app::ports::DocumentQueryPort;

    let year = FiscalYear::calendar_year(fiscal_year);
    let with_documents = documents
        .entries_with_documents(year.start(), year.end())
        .await
        .map_err(|error| format!("証憑の紐付けを数えられませんでした: {error}"))?;

    println!("証憑が付いている仕訳: {with_documents} / {entry_count} 件");

    if with_documents == 0 && entry_count > 0 {
        // **何をすればよいかを添える**（`CLAUDE.md` §11）。数字だけでは、
        // 登録の仕方が分からないまま放置される。
        println!("  この年度の仕訳には証憑が1件も紐付いていません。");
        // **1行を1文にする。** Rust の行継続（`\` + 改行）は、次の行の
        // 字下げをそのまま文字列に含める。整形のための空白が本文に混ざる。
        println!("  メールやダウンロードで受け取った請求書・領収書は、");
        println!("  電子データのまま保存することが求められる場合があります");
        println!("  （適用の可否は事業者の状況によります）。");
        println!("  登録するには:");
        println!(
            "    kaikei attach --file <ファイル> --type receipt --via download --entry <仕訳ID>"
        );
    }
    Ok(())
}

/// 決算書を作らずに、決算書と同じ指摘だけを出す。
///
/// # なぜ `verify` からも呼ぶのか
///
/// **検査するコマンドが、書き出すコマンドより検査が緩いのはおかしい。**
/// 「帳簿は大丈夫か」を確かめたい人は `verify` を打つのであって、
/// `report` を打って警告を読むわけではない。
///
/// 指摘の中身は `report` と同じ関数を呼ぶ。文言や判定が2箇所に分かれると、
/// 片方だけ直したときに食い違う。
async fn warn_from_statements(store: &PgStore, fiscal_year: i32) -> Result<(), String> {
    let catalog = TagCatalog::from_embedded(kaikei_jp_data::TAGS)
        .map_err(|error| format!("同梱のタグ定義を読めませんでした: {error}"))?;
    let schema = catalog.schema().clone();
    let year = FiscalYear::calendar_year(fiscal_year);
    let (from, to) = (year.start(), year.end());

    let (chart, income_statement, balance_sheet, entries, fixed_assets, counterparties) =
        with_tx_err(store, move |tx| {
            let schema = schema.clone();
            Box::pin(async move {
                let chart = tx.load_chart().await?;
                // **取引先マスタも読む。** 取引先タグが付いていても、その相手の
                // 登録番号を確かめていなければ控除の根拠にならない。
                let counterparties = tx.load_counterparties().await?;
                let policy = JpStatementPolicy::new(chart.clone());
                // 損益計算書は会計年度の期間、貸借対照表は帳簿の最初からの累計
                // （`write_reports` と同じ非対称。会計の性質であって都合ではない）。
                let statements = statements::execute(
                    tx,
                    &policy,
                    &schema,
                    StatementsInput {
                        from,
                        to,
                        // **決算書は決算振替を外して出す。** 外さないと、
                        // 決算振替を記帳した瞬間に売上0・所得0になる。
                        exclude_closing_on: Some(year.end()),
                        only_opening_on: None,
                    },
                )
                .await?;
                let cumulative = statements::execute(
                    tx,
                    &policy,
                    &schema,
                    StatementsInput {
                        from: book_beginning(),
                        to,
                        exclude_closing_on: Some(year.end()),
                        only_opening_on: None,
                    },
                )
                .await?;
                // **仕訳そのものも読む。** 税区分と取引先はタグなので、
                // 財務諸表からは見えない。
                let entries = tx.list_entries_in_period(from, to).await?;
                // 固定資産台帳。**verify でも見る**——report を出したときにしか
                // 気づけないと、年末まで食い違いが放置される。
                let fixed_assets = tx.list_fixed_assets().await?;
                Ok::<_, kaikei_app::error::AppError>((
                    chart,
                    statements.income_statement,
                    cumulative.balance_sheet,
                    entries,
                    fixed_assets,
                    counterparties,
                ))
            })
        })
        .await
        .map_err(|error: kaikei_app::error::AppError| {
            format!("財務諸表を組み立てられませんでした: {error}")
        })?;

    warn_if_depreciation_is_missing(&income_statement, &balance_sheet)?;
    warn_if_receivables_are_never_used(&income_statement, &balance_sheet, &chart)?;
    warn_if_a_balance_sits_on_the_wrong_side(&balance_sheet, &chart)?;
    warn_if_qualified_invoice_lacks_a_counterparty(&entries, &counterparties)?;
    warn_if_the_fixed_asset_ledger_does_not_match_the_book(
        &fixed_assets,
        fiscal_year,
        &balance_sheet,
    )?;
    Ok(())
}

/// その年度に台帳の計算対象から外すか。
///
/// # 除却した年**以降**を外す
///
/// 除却した年の償却をどう扱うか（月割で計上するか、除却損に含めるか）は
/// 申告上の判断であり、帳簿からは決まらない（`CLAUDE.md` §10）。
/// **決めないので計算しない。** 除却した時点の未償却残高は
/// `fixedasset dispose` が示すので、除却損の記帳は人が決める。
///
/// 取得前も外す。数えると、来年買う予定のものが今年の帳簿と食い違って見える。
fn is_outside_the_ledger_for(asset: &kaikei_app::ports::FixedAssetRow, fiscal_year: i32) -> bool {
    if asset.acquired_on.year() > fiscal_year {
        return true;
    }
    match asset.disposed_on {
        Some(disposed) => disposed.year() <= fiscal_year,
        None => false,
    }
}

/// 固定資産台帳の期末簿価と、帳簿の残高が科目ごとに合っているか。
///
/// **表示から切り離してある**（`accounts_on_the_wrong_side` と同じ理由。
/// 何を拾うかをテストで固定できないと、呼び出しごと消えても気づけない）。
///
/// 返すのは `(科目コード, 台帳の期末簿価, 帳簿の残高)` の一覧。
fn fixed_asset_accounts_that_do_not_match(
    ledger: &[(kaikei_core::AccountCode, i128)],
    balance_sheet: &kaikei_app::policy::Statement,
) -> Vec<(String, i128, i128)> {
    let mut mismatches = Vec::new();
    for (account, book_value) in ledger {
        let in_book = balance_sheet
            .sections
            .iter()
            .flat_map(|section| section.lines.iter())
            .find(|line| &line.account == account)
            .map(|line| line.amount.minor())
            .unwrap_or(0);
        if *book_value != in_book {
            mismatches.push((account.as_str().to_string(), *book_value, in_book));
        }
    }
    mismatches
}

/// 台帳の期末簿価を科目ごとにまとめる。
///
/// 除却した年より後の資産は数えない。
fn fixed_asset_book_values(
    assets: &[kaikei_app::ports::FixedAssetRow],
    fiscal_year: i32,
) -> Result<Vec<(kaikei_core::AccountCode, i128)>, String> {
    use std::collections::BTreeMap;

    let mut by_account: BTreeMap<kaikei_core::AccountCode, i128> = BTreeMap::new();
    for asset in assets {
        if is_outside_the_ledger_for(asset, fiscal_year) {
            continue;
        }
        let input = to_fixed_asset(asset)?;
        let schedule = kaikei_jp::depreciation::schedule(&input)
            .map_err(|error| format!("{}: 償却額を計算できませんでした: {error}", asset.name))?;
        // その年度末の簿価。償却が終わっていれば最後の年の簿価、
        // 取得前なら取得価額（まだ償却していない）。
        let book_value = match schedule.year(fiscal_year) {
            Some(year) => year.book_value.minor(),
            None => {
                // 取得前は `is_outside_the_ledger_for` が外している。
                // ここに来るのは償却し終わった資産で、最後の年の簿価が続く。
                schedule
                    .years
                    .last()
                    .map(|y| y.book_value.minor())
                    .unwrap_or(asset.acquisition_cost.minor())
            }
        };
        *by_account.entry(asset.account.clone()).or_insert(0) += book_value;
    }
    Ok(by_account.into_iter().collect())
}

/// 上の食い違いを知らせる。
fn warn_if_the_fixed_asset_ledger_does_not_match_the_book(
    assets: &[kaikei_app::ports::FixedAssetRow],
    fiscal_year: i32,
    balance_sheet: &kaikei_app::policy::Statement,
) -> Result<(), String> {
    if assets.is_empty() {
        return Ok(());
    }
    let ledger = fixed_asset_book_values(assets, fiscal_year)?;
    let mismatches = fixed_asset_accounts_that_do_not_match(&ledger, balance_sheet);
    if mismatches.is_empty() {
        return Ok(());
    }

    eprintln!(
        "注意: 固定資産台帳の期末簿価と帳簿の残高が違う科目が {} 件あります:",
        mismatches.len()
    );
    for (account, book_value, in_book) in &mismatches {
        eprintln!(
            "  {account}  台帳 {book_value} 円 / 帳簿 {in_book} 円（差 {}）",
            in_book - book_value
        );
    }
    eprintln!(
        "  償却の記帳漏れ、記帳先の科目の取り違え、台帳の入力誤りのいずれかを疑ってください。"
    );
    eprintln!("  貸借は一致したままなので、決算書を見ても分かりません。");
    Ok(())
}

/// 貸借が自然な向きと逆になっている科目を挙げる。
///
/// **表示から切り離してある。** 何を拾うかをテストで固定できないと、
/// 「落ちないこと」しか確かめられない。
fn accounts_on_the_wrong_side(
    balance_sheet: &kaikei_app::policy::Statement,
    chart: &kaikei_core::ChartOfAccounts,
    contra: &[kaikei_core::AccountCode],
) -> Vec<(String, String, kaikei_core::Money)> {
    use kaikei_core::AccountType;

    let mut wrong = Vec::new();
    for line in balance_sheet
        .sections
        .iter()
        .flat_map(|section| section.lines.iter())
    {
        if line.amount.is_zero() || contra.contains(&line.account) {
            continue;
        }
        let Some(def) = chart.get(&line.account) else {
            continue;
        };
        // 資産は借方、負債は貸方が自然な向き。財務諸表の金額は、その科目の
        // 自然な向きを正として出ている。純資産は対象外——事業主貸・事業主借は
        // どちらの向きにも立ちうる。
        let natural = matches!(
            def.account_type,
            AccountType::Asset | AccountType::Liability
        );
        if natural && line.amount.is_negative() {
            wrong.push((
                line.account.as_str().to_string(),
                def.name.clone(),
                line.amount,
            ));
        }
    }
    wrong
}

/// 財務諸表から1科目の金額を取る。**無ければ0。**
fn amount_of(
    statement: &kaikei_app::policy::Statement,
    account: &kaikei_core::AccountCode,
) -> kaikei_core::Money {
    statement
        .sections
        .iter()
        .flat_map(|section| section.lines.iter())
        .find(|line| &line.account == account)
        .map(|line| line.amount)
        .unwrap_or_else(|| kaikei_core::Money::from_minor(0, statement.total.currency()))
}

/// 金額で引いたときに並べる候補の上限。
///
/// 全部並べると、毎月同額のサブスクリプションで画面が流れる。
const MAX_ENTRY_CANDIDATES: usize = 10;

/// 金額から仕訳を1つ引く。
///
/// # なぜ金額で引くのか
///
/// **仕訳IDを人が探すのが、証憑を登録するときのいちばんの手間である。**
/// 領収書を見れば金額は分かるので、それで引けるようにする。
///
/// # 1つに絞れなければ止める
///
/// 同じ額の取引は普通にある（毎月同額のサブスクリプションなど）。**勝手に
/// 1つ選ぶと、意図しない仕訳に証憑が付く**——しかも紐付けは追記のみなので
/// 消せない。候補を並べて止め、`--entry` で選んでもらう。
/// 仕訳番号から仕訳を引く。
///
/// # なぜ要るのか
///
/// `invoices_to_collect.csv`（適格請求書を揃えるべき取引の一覧）が出すのは
/// **仕訳番号**である。`--entry` が UUID しか受けないと、一覧の行ごとに
/// 帳簿を引き直して UUID を調べることになる。32件でそれをやらせない。
///
/// # 金額で引くのとの違い
///
/// 仕訳番号は**年度の中で一意**なので、候補が複数になることがない。
/// 金額で引く方（[`find_entry_by_amount`]）は同じ額の仕訳が複数あると止まる。
async fn find_entry_by_number(
    store: &PgStore,
    entry_no: u32,
    args: &AttachArgs,
) -> Result<kaikei_core::EntryId, String> {
    use kaikei_app::ports::JournalRepo;

    // 探す年は、指定 → 取引年月日 の順（`find_entry_by_amount` と同じ）。
    let year = match (args.match_year, args.doc_date) {
        (Some(year), _) => year,
        (None, Some(date)) => date.year(),
        (None, None) => {
            return Err(
                "--entry-no で探す年が決まりません。--match-year か --date を指定してください"
                    .to_string(),
            )
        }
    };
    let fiscal_year = FiscalYear::calendar_year(year);
    let (from, to) = (fiscal_year.start(), fiscal_year.end());

    let entries = with_tx_err(store, move |tx| {
        Box::pin(async move { tx.list_entries_in_period(from, to).await })
    })
    .await
    .map_err(|error: kaikei_app::error::RepoError| format!("仕訳を読めませんでした: {error}"))?;

    entries
        .iter()
        .find(|entry| entry.entry_no().as_u32() == entry_no)
        .map(|entry| entry.id())
        .ok_or_else(|| {
            format!("{year}年に仕訳番号 {entry_no} の仕訳がありません。年と番号を確かめてください")
        })
}

async fn find_entry_by_amount(
    store: &PgStore,
    amount: i64,
    args: &AttachArgs,
) -> Result<kaikei_core::EntryId, String> {
    use kaikei_app::ports::JournalRepo;

    // 探す年は、指定 → 取引年月日 の順。どちらも無ければ決められない。
    let year =
        match (args.match_year, args.doc_date) {
            (Some(year), _) => year,
            (None, Some(date)) => date.year(),
            (None, None) => return Err(
                "--match-amount で探す年が決まりません。--match-year か --date を指定してください"
                    .to_string(),
            ),
        };
    let fiscal_year = FiscalYear::calendar_year(year);
    let (from, to) = (fiscal_year.start(), fiscal_year.end());

    let entries = with_tx_err(store, move |tx| {
        Box::pin(async move { tx.list_entries_in_period(from, to).await })
    })
    .await
    .map_err(|error: kaikei_app::error::RepoError| format!("仕訳を読めませんでした: {error}"))?;

    let target = i128::from(amount);
    let matched: Vec<&kaikei_core::JournalEntry> = entries
        .iter()
        // 赤伝は候補にしない。証憑を付ける先ではない。
        .filter(|entry| entry.reverses().is_none())
        .filter(|entry| entry.debit_total().minor() == target)
        .collect();

    match matched.len() {
        1 => Ok(matched[0].id()),
        0 => Err(format!(
            "{year}年に借方合計が {amount} 円の仕訳がありません。金額と年を確かめるか、--entry で仕訳を指定してください"
        )),
        _ => {
            // **候補を並べる。** どれを選べばよいか分からないまま止めない。
            // 行継続（`\` + 改行）は次の行の字下げを文字列に含めるので、
            // つなぎ目は1行に収める。
            let mut message = format!(
                "{year}年に借方合計が {amount} 円の仕訳が {} 件あります。--entry でどれかを指定してください:",
                matched.len()
            );
            for entry in matched.iter().take(MAX_ENTRY_CANDIDATES) {
                message.push_str(&format!(
                    "
  {} {} {}",
                    kaikei_app::id::entry_id_to_uuid_string(entry.id()),
                    entry.entry_date().to_iso_string(),
                    entry.description()
                ));
            }
            if matched.len() > MAX_ENTRY_CANDIDATES {
                message.push_str(&format!(
                    "
  （ほか {} 件）",
                    matched.len() - MAX_ENTRY_CANDIDATES
                ));
            }
            Err(message)
        }
    }
}

/// 証憑の検索要件を、紐付ける仕訳から採ったもの。
struct EntryFacts {
    date: AccountingDate,
    amount: Option<i64>,
    counterparty: Option<String>,
}

/// 仕訳から、証憑の検索要件に使える値を引く。
///
/// # なぜ仕訳から採るのか
///
/// 証憑1件ごとに取引年月日・取引金額・取引先を打たせると、登録が現実的で
/// なくなる。**実際にこの帳簿には証憑が1件も登録されていない。**
/// 紐付ける仕訳が決まっているなら、3項目はそこにある。
///
/// # 見つからない仕訳を黙って通さない
///
/// 指定した仕訳が無ければエラーにする。**紐付けに失敗した証憑は、帳簿から
/// 辿れないまま保存される**——それは登録しないのとほとんど変わらない。
async fn find_entry_facts(
    store: &PgStore,
    entry_id: kaikei_core::EntryId,
) -> Result<Option<EntryFacts>, String> {
    use kaikei_app::ports::JournalRepo;

    let found = with_tx_err(store, move |tx| {
        Box::pin(async move { tx.find_entry(entry_id).await })
    })
    .await
    .map_err(|error: kaikei_app::error::RepoError| format!("仕訳を読めませんでした: {error}"))?;

    let Some(entry) = found else {
        return Err(format!(
            "--entry で指定した仕訳が見つかりません（{}）",
            kaikei_app::id::entry_id_to_uuid_string(entry_id)
        ));
    };

    // 金額は借方合計。1つの仕訳に複数の明細があっても、証憑の金額は
    // 取引全体の額である。
    let amount = i64::try_from(entry.debit_total().minor()).ok();
    // 取引先は明細のタグから採る。**複数あって食い違う場合は採らない**
    // ——どちらが証憑の取引先かを決められない。
    let key = kaikei_core::TagKey::parse("counterparty").ok();
    let mut counterparties: Vec<String> = Vec::new();
    if let Some(key) = &key {
        for line in entry.lines() {
            if let Some(value) = line.tags().get(key) {
                let text = kaikei_jp::tags::tag_value_to_string(value);
                if !counterparties.contains(&text) {
                    counterparties.push(text);
                }
            }
        }
    }
    let counterparty = match counterparties.len() {
        1 => counterparties.into_iter().next(),
        _ => None,
    };

    Ok(Some(EntryFacts {
        date: entry.entry_date(),
        amount,
        counterparty,
    }))
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
    let (
        entries,
        chart,
        statements,
        cumulative,
        opening_balance_sheet,
        fixed_assets,
        counterparties,
    ) = with_tx_err(&store, move |tx| {
        let schema = schema.clone();
        Box::pin(async move {
            let chart = tx.load_chart().await?;
            let entries = tx.list_entries_in_period(from, to).await?;
            let policy = JpStatementPolicy::new(chart.clone());
            // 損益計算書は**会計年度の期間**。その期間の損益そのものである。
            let statements = statements::execute(
                tx,
                &policy,
                &schema,
                StatementsInput {
                    from,
                    to,
                    // **決算書は決算振替を外して出す。** 外さないと、
                    // 決算振替を記帳した瞬間に売上0・所得0になる。
                    exclude_closing_on: Some(to),
                    only_opening_on: None,
                },
            )
            .await?;
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
                    exclude_closing_on: Some(to),
                    only_opening_on: None,
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
                    // **期首振替（1/1）まで含める。** 期首の姿とは
                    // 「期首振替を済ませた後、その年の商売が始まる前」
                    // である。前日で切ると事業主貸・事業主借が期首に
                    // 残り、期首の貸借が合わなくなる。
                    to: from,
                    // 期首の列では決算振替を外さない。前年度の決算振替
                    // （前年12/31の所得→元入金）は期首残高の一部である。
                    // 外すと元入金が過少になり、前年度の収益・費用が
                    // 期首の貸借対照表に漏れる。
                    exclude_closing_on: None,
                    // ただし 1/1 の普通の取引は入れない（期首の姿では
                    // なく、その年の商売である）。
                    only_opening_on: Some(from),
                },
            )
            .await?;
            // 固定資産台帳。決算書の「減価償却費の計算」欄に出す。
            let fixed_assets = tx.list_fixed_assets().await?;
            // 取引先マスタ。登録番号が確かめられているかを見る。
            let counterparties = tx.load_counterparties().await?;
            Ok((
                entries,
                chart,
                statements,
                cumulative,
                opening_balance_sheet,
                fixed_assets,
                counterparties,
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

    // **減価償却の計上漏れを指摘する。** 貸借は一致したままで所得だけが
    // 過大になる誤りで、決算書を見ても分からない。
    warn_if_depreciation_is_missing(&statements.income_statement, &cumulative.balance_sheet)?;

    // **貸借が自然な向きと逆の科目を指摘する。** 貸借は一致したままなので
    // 決算書を見ても気づけない。
    warn_if_a_balance_sits_on_the_wrong_side(&cumulative.balance_sheet, &chart)?;

    // **決算振替を記帳した後では決算書が作れない。** 収益・費用がゼロ化
    // されているので、所得が 0 の決算書ができる。
    warn_if_the_year_looks_closed(&statements.income_statement, entries.len());

    // **適格請求書ありの税区分に取引先が付いているか。** 取引先が無いと、
    // 適格請求書発行事業者かどうかの検証がすり抜ける。
    warn_if_qualified_invoice_lacks_a_counterparty(&entries, &counterparties)?;

    // **前年の事業主貸・事業主借が持ち越されていないか。** 翌期首に元入金へ
    // 振り替えないと、年を追うごとに膨らむ。
    warn_if_owner_accounts_carried_over(&opening_balance_sheet.balance_sheet, fiscal_year_label)?;

    // **期首振替を年内に入れていないか。** 入れると決算書から事業主貸・
    // 事業主借が消える（D-102）。上の検査は「忘れた」を見るが、こちらは
    // 「早すぎた」を見る。
    warn_if_the_opening_transfer_was_posted_too_early(&entries, &cumulative.balance_sheet)?;

    written.extend(write_blue_return_balance_sheet(
        &out_dir,
        &opening_balance_sheet.balance_sheet,
        &cumulative.balance_sheet,
        to,
        &blue_return_fields,
    )?);

    written.extend(write_blue_return_depreciation(
        &out_dir,
        &fixed_assets,
        fiscal_year_label,
        &period_label,
        &statements.income_statement,
    )?);

    // 台帳の期末簿価と帳簿の残高。**verify と同じ指摘を report でも出す**
    // （どちらか一方でしか出ないと、見ている方によって気づけない）。
    warn_if_the_fixed_asset_ledger_does_not_match_the_book(
        &fixed_assets,
        fiscal_year_label,
        &cumulative.balance_sheet,
    )?;

    // 適格請求書を揃えるべき取引の一覧。
    //
    // **件数だけでは進まない。** verify が「1万円以上は 33 件」と言っても、
    // どの取引なのかが分からなければ請求書を探しに行けない。日付・金額・
    // 摘要・科目があれば、通帳やメールから元の取引を辿れる。
    // 0 件でも見出しだけのファイルを書く（`blue_return_not_on_form.csv` と同じ）。
    let rule_sets = kaikei_jp::tax::TaxRuleSets::from_embedded()
        .map_err(|error| format!("同梱の消費税区分マスタを読めませんでした: {error}"))?;
    let mut to_collect = invoices_to_collect(&entries, &rule_sets, &chart);
    // **済んだ分は落とす。** 減らない一覧は作業リストとして使えない。
    // 32件を順に片付けても件数が変わらなければ、どこまで進んだか分からない。
    let done = entries_with_an_invoice_document(&pool_for_documents, &entries, &to_collect).await?;
    let before = to_collect.len();
    to_collect.retain(|row| !done.contains(&row.entry_no));
    if before != to_collect.len() {
        println!(
            "  うち {} 件は証憑が登録済みなので一覧から外しました",
            before - to_collect.len()
        );
    }
    let collect_path = out_dir.join("invoices_to_collect.csv");
    std::fs::write(
        &collect_path,
        kaikei_report::invoices_to_collect::to_csv(&to_collect),
    )
    .map_err(|error| {
        format!(
            "書き出せませんでした: {}（{error}）",
            collect_path.display()
        )
    })?;
    written.push(collect_path);
    if !to_collect.is_empty() {
        println!(
            "適格請求書を揃えるべき取引が {} 件あります（invoices_to_collect.csv）",
            to_collect.len()
        );
    }

    // 消費税の集計。**税理士へ渡す一式に入れる。**
    //
    // CLI（`kaikei consumptiontax`）でも見られるが、**画面で読む人と
    // ファイルを受け取る人は別である。** 確定申告には消費税の申告も含まれる
    // ので、決算書と一緒に出す。
    //
    // 前提（原則課税・税込経理）に合わない帳簿では出さない。**黙って誤った
    // 数字を渡すより、無い方がよい。**
    //
    // **設定は任意で読む。** ここで `jp_settings()?` を呼ぶと、
    // `KAIKEI_ROUNDING` / `KAIKEI_TAX_MODE` が無い環境で **`report` 全体が
    // 落ちる**。実際 CI の E2E が10件落ちた（手元では `.env` を読んでいたので
    // 気づかなかった）。D-113 と同じ形——**設定が無いことを理由に、本体の
    // 出力まで止めない。**
    //
    // 消費税の集計に丸め方は要らない（税込から割り戻すだけ）ので、
    // 経理方式だけを見る。
    if optional_tax_mode() == Some(kaikei_jp::tax::TaxMode::Inclusive) {
        let lines = tagged_lines_for_consumption_tax(&entries)?;
        let table = rule_sets
            .iter()
            .next()
            .ok_or("同梱の消費税区分マスタが空です")?;
        let summary = kaikei_jp::consumption_tax::summarize(&lines, table)
            .map_err(|error| format!("消費税を集計できませんでした: {error}"))?;

        let path = out_dir.join("consumption_tax.csv");
        std::fs::write(&path, kaikei_report::consumption_tax::to_csv(&summary))
            .map_err(|error| format!("書き出せませんでした: {}（{error}）", path.display()))?;
        written.push(path);

        // 注記は別ファイルにする（1つの CSV に表と注記を混ぜると表計算で
        // 読めなくなる。`blue_return_not_on_form.csv` と同じ理由）。
        let notes_path = out_dir.join("consumption_tax_notes.txt");
        std::fs::write(
            &notes_path,
            kaikei_report::consumption_tax::notes_to_text(&summary),
        )
        .map_err(|error| format!("書き出せませんでした: {}（{error}）", notes_path.display()))?;
        written.push(notes_path);

        println!(
            "消費税の集計: 課税売上 {} 円 / 課税仕入 {} 円（consumption_tax.csv）",
            summary.taxable_sales().to_display_string(),
            summary.taxable_purchases().to_display_string()
        );
    }

    // 全件 JSON。**この出力はこのソフトが消えてもデータが残るためのもの**
    // なので、既定で必ず出す（docs/03-database.md §8）。
    let export_path = out_dir.join("export.json");
    let export_json = kaikei_report::export::to_json(&entries, &chart);

    // ★書き出したものを読み直して、帳簿と突き合わせる★
    //
    // このファイルは**このソフトが無くなっても帳簿が残る**ための出口である。
    // 中身が欠けていても、必要になったときに初めて気づく——そのときにはもう
    // 元の帳簿が無い。
    warn_if_export_does_not_match_the_book(&export_json, &trial_balance);

    std::fs::write(&export_path, &export_json)
        .map_err(|error| format!("書き出せませんでした: {}（{error}）", export_path.display()))?;
    written.push(export_path);

    if yayoi {
        written.extend(write_yayoi(&out_dir, &entries, &chart, &trial_balance)?);
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
/// この帳簿で実際に使われている税区分のコードを集める。
///
/// **表に載っているだけの区分と区別する。** 未確認の写像を知らせるとき、
/// 使っていない区分まで数えると「8件」のような数字になり、どれを確かめれば
/// よいのか分からなくなる。
fn tax_categories_used(
    entries: &[kaikei_core::JournalEntry],
) -> std::collections::BTreeSet<String> {
    let mut used = std::collections::BTreeSet::new();
    let Ok(key) = kaikei_core::TagKey::parse("tax_category") else {
        return used;
    };
    for line in entries.iter().flat_map(|entry| entry.lines().iter()) {
        if let Some(kaikei_core::TagValue::Code(code)) = line.tags().get(&key) {
            used.insert(code.clone());
        }
    }
    used
}

fn write_yayoi(
    out_dir: &Path,
    entries: &[kaikei_core::JournalEntry],
    chart: &kaikei_core::ChartOfAccounts,
    trial_balance: &kaikei_app::view::TrialBalanceView,
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

    // ★書き出したものを読み直して、帳簿と突き合わせる★
    //
    // **税理士が取り込んだ数字が決算書と違えば、そこで気づけない。**
    // 列のずれ・金額の取り違え・行の脱落は、書き出したものを数えないと
    // 分からない。
    warn_if_yayoi_does_not_match_the_book(&conversion.rows, chart, trial_balance);

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
        // **この帳簿が実際に使っている区分に絞る。** 「未確認 8 件」とだけ
        // 言われても、そのうちどれが自分に効くのか分からない。確認を求める
        // 相手は税理士なので、**確かめる対象を絞れないと依頼そのものが
        // 重くなる。**
        let used = tax_categories_used(entries);
        let unverified = map.unverified_among(&used);
        if unverified.is_empty() {
            eprintln!(
                "注意: 弥生の税区分の写像には未確認のものが {} 件ありますが、この帳簿では使っていません",
                map.unverified_count()
            );
        } else {
            eprintln!(
                "注意: 弥生の税区分の写像を実機で確認していません。この帳簿が使っている {} 件が未確認です:",
                unverified.len()
            );
            for (kaikei_code, yayoi_label) in &unverified {
                eprintln!("  {kaikei_code} → {yayoi_label}");
            }
            eprintln!("  取り込む前に税理士に確認してください。");
            eprintln!(
                "  （表は全 {} 件が未確認ですが、残りはこの帳簿で使っていません）",
                map.unverified_count()
            );
        }
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
) -> std::collections::BTreeMap<String, kaikei_report::yayoi::YayoiCategory> {
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
            // 売上側と仕入側を両方渡す。片方だけにすると、非課税の仕入が
            // 「非課売上」として出力される。
            out.insert(
                code.to_string(),
                kaikei_report::yayoi::YayoiCategory {
                    sales: mapping.yayoi.clone(),
                    purchase: mapping.yayoi_purchase.clone(),
                },
            );
        }
    }
    out
}

/// 青色申告決算書の「減価償却費の計算」欄を書き出す。
///
/// # 損益計算書の減価償却費と突き合わせる
///
/// 台帳から出した合計と、帳簿に記帳されている減価償却費が食い違えば知らせる。
/// **どちらが正しいかは決めない**——台帳の入力が違うのか、記帳が漏れているのか、
/// 記帳し過ぎているのかは帳簿からは決まらない。事実だけを出す。
fn write_blue_return_depreciation(
    out_dir: &Path,
    assets: &[kaikei_app::ports::FixedAssetRow],
    fiscal_year: i32,
    period_label: &str,
    income_statement: &kaikei_app::policy::Statement,
) -> Result<Vec<PathBuf>, String> {
    use kaikei_report::blue_return_depreciation::DepreciationRow;

    let mut rows = Vec::new();
    let mut total: i128 = 0;
    for asset in assets {
        if is_outside_the_ledger_for(asset, fiscal_year) {
            continue;
        }
        let input = to_fixed_asset(asset)?;
        let schedule = kaikei_jp::depreciation::schedule(&input)
            .map_err(|error| format!("{}: 償却額を計算できませんでした: {error}", asset.name))?;
        let Some(year) = schedule.year(fiscal_year) else {
            continue;
        };
        total += year.amount.minor();
        rows.push(DepreciationRow {
            name: asset.name.clone(),
            acquired: format!(
                "{:04}-{:02}",
                asset.acquired_on.year(),
                asset.acquired_on.month()
            ),
            acquisition_cost: asset.acquisition_cost,
            // 定額法の「償却の基礎になる金額」は取得価額である
            // （旧定額法の 90% ではない。2007-04-01 以降取得）。
            base_amount: asset.acquisition_cost,
            method: method_label(asset.method).to_string(),
            useful_life_years: asset.useful_life_years,
            rate: schedule
                .rate_per_mille
                .map(|per_mille| format!("{}.{:03}", per_mille / 1000, per_mille % 1000)),
            period: format!("{}/12", year.months),
            before_ratio: year.before_ratio,
            business_ratio: match &asset.business_ratio {
                Some(text) => text.to_string(),
                None => "100%".to_string(),
            },
            amount: year.amount,
            book_value: year.book_value,
            note: asset.note.clone().unwrap_or_default(),
        });
    }

    let total = kaikei_core::Money::from_minor(total, kaikei_core::Currency::JPY);
    let written = write_pair(
        out_dir,
        "blue_return_depreciation",
        &kaikei_report::blue_return_depreciation::to_csv(&rows),
        &kaikei_report::blue_return_depreciation::to_html(period_label, &rows, &total),
    )?;

    warn_if_depreciation_does_not_match_the_book(&total, income_statement, rows.len())?;
    Ok(written)
}

/// 台帳から出した償却費と、帳簿の減価償却費が合っているか。
///
/// **食い違いを黙らない。** 決算書には台帳から出した額を書くので、帳簿の
/// 減価償却費と違っていると、決算書と損益計算書で数字が合わなくなる。
fn warn_if_depreciation_does_not_match_the_book(
    from_ledger: &kaikei_core::Money,
    income_statement: &kaikei_app::policy::Statement,
    asset_count: usize,
) -> Result<(), String> {
    let in_book = amount_of_expense(income_statement, "減価償却費");
    if from_ledger.minor() == in_book {
        return Ok(());
    }
    // どちらも0なら言うことはない（台帳が空で、記帳も無い）。
    if from_ledger.minor() == 0 && in_book == 0 {
        return Ok(());
    }
    eprintln!("注意: 固定資産台帳から出した償却費と、帳簿の減価償却費が違います。",);
    eprintln!(
        "  台帳（{asset_count} 件）: {} 円",
        from_ledger.to_display_string()
    );
    eprintln!("  帳簿の減価償却費: {in_book} 円");
    eprintln!("  決算書には台帳の額を書くので、このままだと損益計算書と食い違います。");
    eprintln!("  台帳の入力・記帳の漏れ・記帳し過ぎのいずれかを確かめてください。");
    Ok(())
}

/// 損益計算書から、名前で費用の金額を引く。無ければ0。
fn amount_of_expense(statement: &kaikei_app::policy::Statement, name: &str) -> i128 {
    statement
        .sections
        .iter()
        .flat_map(|section| section.lines.iter())
        .find(|line| line.label == name)
        .map(|line| line.amount.minor())
        .unwrap_or(0)
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
        // 1行を1文にする。行継続（`\` + 改行）は次の行の字下げを
        // 文字列に含めるので、整形のための空白が本文に混ざる。
        eprintln!("注意: 決算書は「{label}」が期首と期末で同額であることを前提にしていますが、");
        eprintln!(
            "  帳簿では期首 {} / 期末 {} と動いています。",
            kaikei_app::amount::money_to_plain_string(book_opening),
            kaikei_app::amount::money_to_plain_string(book_closing)
        );
        eprintln!("  決算振替を記帳した後に決算書を出していないか確認してください");
        eprintln!("  （決算書は振替前の帳簿から作ります）。");
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

    /// 税区分・取引先のタグを持つ明細のタグ集合を作る。
    fn tags_of(tax_category: Option<&str>, counterparty: Option<&str>) -> kaikei_core::TagSet {
        let mut tags = kaikei_core::TagSet::new();
        if let Some(code) = tax_category {
            tags.insert(
                kaikei_core::TagKey::parse("tax_category").unwrap(),
                kaikei_core::TagValue::Code(code.to_string()),
            );
        }
        if let Some(code) = counterparty {
            tags.insert(
                kaikei_core::TagKey::parse("counterparty").unwrap(),
                kaikei_core::TagValue::Code(code.to_string()),
            );
        }
        tags
    }

    // 空欄は「未確認」であって「非適格」ではない。**この区別が消えると、
    // 誰も確認していない取引先が「非適格だと確認済み」になる。**
    #[test]
    fn a_blank_is_qualified_means_unverified_not_false() {
        let rows = parse_counterparty_csv(
            "code,name,invoice_registration_no,is_qualified
             anthropic,Anthropic,,
             bitech,ビーテック,T1234567890123,true
             kojin,個人商店,,false
",
        )
        .unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows[0].counterparty.is_qualified_invoice_issuer, None,
            "空欄は未確認"
        );
        assert_eq!(rows[1].counterparty.is_qualified_invoice_issuer, Some(true));
        assert_eq!(
            rows[2].counterparty.is_qualified_invoice_issuer,
            Some(false)
        );
        assert_eq!(
            rows[1].counterparty.invoice_registration_no.as_deref(),
            Some("T1234567890123")
        );
        assert_eq!(rows[0].counterparty.invoice_registration_no, None);
    }

    // 列の順番を変えても読める（見出しで引く）。
    #[test]
    fn columns_are_found_by_header_not_by_position() {
        let rows = parse_counterparty_csv(
            "name,is_qualified,code
Anthropic,true,anthropic
",
        )
        .unwrap();
        assert_eq!(rows[0].counterparty.code, "anthropic");
        assert_eq!(rows[0].counterparty.name, "Anthropic");
        assert_eq!(rows[0].counterparty.is_qualified_invoice_issuer, Some(true));
    }

    // 社名にカンマが入っていても壊れない（split(',') では読めない）。
    #[test]
    fn a_comma_inside_a_quoted_name_is_not_a_separator() {
        let rows = parse_counterparty_csv(
            "code,name
abc,\"株式会社A, B事業部\"
",
        )
        .unwrap();
        assert_eq!(rows[0].counterparty.name, "株式会社A, B事業部");
    }

    // 知らない値を黙って未確認に丸めない。
    #[test]
    fn an_unreadable_is_qualified_is_an_error() {
        let error = parse_counterparty_csv(
            "code,name,is_qualified
abc,A,yes
",
        )
        .unwrap_err();
        assert!(error.contains("1 行目"), "{error}");
        assert!(error.contains("yes"), "受け取った値を見せること: {error}");
    }

    // code / name が無いCSVは受け取らない。
    #[test]
    fn code_and_name_are_required() {
        let error = parse_counterparty_csv(
            "name,is_qualified
A,true
",
        )
        .unwrap_err();
        assert!(error.contains("code"), "{error}");
        let error = parse_counterparty_csv(
            "code,name
,A
",
        )
        .unwrap_err();
        assert!(error.contains("code が空"), "{error}");
        let error = parse_counterparty_csv(
            "code,name
abc,
",
        )
        .unwrap_err();
        assert!(error.contains("name が空"), "{error}");
    }

    // 既定は下見。--commit を付けたときだけ書き込む。
    #[test]
    fn counterparty_import_does_not_write_without_commit() {
        let command = parse_args(&args(&["counterparty", "import", "--file", "./cp.csv"])).unwrap();
        match command {
            Command::Counterparty(args) => {
                assert_eq!(args.file, PathBuf::from("./cp.csv"));
                assert!(!args.commit, "--commit が無ければ書き込まない");
            }
            other => panic!("counterparty として解釈されるはず: {other:?}"),
        }

        let command = parse_args(&args(&[
            "counterparty",
            "import",
            "--file",
            "./cp.csv",
            "--commit",
        ]))
        .unwrap();
        match command {
            Command::Counterparty(args) => assert!(args.commit),
            other => panic!("counterparty として解釈されるはず: {other:?}"),
        }
    }

    fn fixed_asset_args(list: &[&str]) -> Vec<String> {
        let mut v = vec!["fixedasset".to_string(), "add".to_string()];
        v.extend(list.iter().map(|s| s.to_string()));
        v
    }

    const MINIMAL: &[&str] = &[
        "--name",
        "パソコン",
        "--account",
        "210",
        "--acquired",
        "2025-07-24",
        "--cost",
        "280717",
    ];

    fn with(extra: &[&str]) -> Vec<String> {
        let mut v: Vec<&str> = MINIMAL.to_vec();
        v.extend_from_slice(extra);
        fixed_asset_args(&v)
    }

    // **本命。** 定額法には耐用年数が要る。
    //
    // 無いまま台帳に入れると、償却額の計算時に落ちる。入口で止める。
    #[test]
    fn straight_line_without_a_life_is_rejected() {
        let error = parse_args(&with(&["--method", "straight-line"])).unwrap_err();
        assert!(error.contains("--life"), "{error}");
        assert!(
            error.contains("推定しません"),
            "なぜ指定が要るかを言うこと: {error}"
        );
    }

    // **本命。** 一括償却・少額特例に耐用年数を付けさせない。
    //
    // 無視されるだけで済ませると、効いていると思ったまま進むことになる。
    #[test]
    fn other_methods_reject_a_life() {
        for method in ["lump-sum", "immediate"] {
            let error = parse_args(&with(&["--method", method, "--life", "3"])).unwrap_err();
            assert!(error.contains("--life を外して"), "{method}: {error}");
        }
    }

    #[test]
    fn the_three_methods_map_to_their_codes() {
        for (text, code) in [("straight-line", 1i16), ("lump-sum", 2), ("immediate", 3)] {
            let extra: Vec<&str> = if code == 1 {
                vec!["--method", text, "--life", "4"]
            } else {
                vec!["--method", text]
            };
            match parse_args(&with(&extra)).unwrap() {
                Command::FixedAsset(args) => assert_eq!(args.method, code, "{text}"),
                other => panic!("fixedasset として解釈されるはず: {other:?}"),
            }
        }
    }

    // 知らない償却方法は黙って通さない。
    #[test]
    fn an_unknown_method_is_rejected() {
        let error = parse_args(&with(&["--method", "declining"])).unwrap_err();
        assert!(
            error.contains("declining"),
            "受け取った値を見せること: {error}"
        );
        assert!(
            error.contains("straight-line"),
            "選べる値を挙げること: {error}"
        );
    }

    // 既定は下見。--commit を付けたときだけ入れる。
    #[test]
    fn fixed_asset_add_does_not_write_without_commit() {
        match parse_args(&with(&["--method", "lump-sum"])).unwrap() {
            Command::FixedAsset(args) => assert!(!args.commit),
            other => panic!("{other:?}"),
        }
        match parse_args(&with(&["--method", "lump-sum", "--commit"])).unwrap() {
            Command::FixedAsset(args) => assert!(args.commit),
            other => panic!("{other:?}"),
        }
    }

    // 必須の引数が無ければ止まる。
    #[test]
    fn the_required_arguments_are_checked() {
        let error = parse_args(&fixed_asset_args(&["--method", "lump-sum"])).unwrap_err();
        assert!(error.contains("--name"), "{error}");

        let error = parse_args(&fixed_asset_args(&[
            "--name",
            "x",
            "--account",
            "210",
            "--acquired",
            "2025-07-24",
        ]))
        .unwrap_err();
        assert!(error.contains("--method"), "{error}");
    }

    // 取得価額は3桁区切りでも読める。
    #[test]
    fn the_cost_accepts_thousands_separators() {
        let mut v: Vec<&str> = vec![
            "--name",
            "x",
            "--account",
            "210",
            "--acquired",
            "2025-07-24",
            "--cost",
            "280,717",
            "--method",
            "lump-sum",
        ];
        v.dedup();
        match parse_args(&fixed_asset_args(&v)).unwrap() {
            Command::FixedAsset(args) => assert_eq!(args.cost, 280_717),
            other => panic!("{other:?}"),
        }
    }

    // サブコマンドを指定しなければ、何を指定すればよいか言う。
    #[test]
    fn the_subcommand_is_required() {
        let error = parse_args(&["fixedasset".to_string()]).unwrap_err();
        assert!(error.contains("add か list"), "{error}");
    }

    // ---- fixedasset list ----

    #[test]
    fn fixed_asset_list_takes_an_optional_year() {
        match parse_args(&["fixedasset".to_string(), "list".to_string()]).unwrap() {
            Command::FixedAssetList { fiscal_year } => assert_eq!(fiscal_year, None),
            other => panic!("{other:?}"),
        }
        match parse_args(&[
            "fixedasset".to_string(),
            "list".to_string(),
            "--year".to_string(),
            "2026".to_string(),
        ])
        .unwrap()
        {
            Command::FixedAssetList { fiscal_year } => assert_eq!(fiscal_year, Some(2026)),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn fixed_asset_list_rejects_unknown_arguments() {
        let error = parse_args(&[
            "fixedasset".to_string(),
            "list".to_string(),
            "--name".to_string(),
            "x".to_string(),
        ])
        .unwrap_err();
        assert!(error.contains("--name"), "{error}");
    }

    // add と list 以外は受けない。
    #[test]
    fn fixed_asset_rejects_other_subcommands() {
        let error = parse_args(&["fixedasset".to_string(), "remove".to_string()]).unwrap_err();
        assert!(error.contains("add と list"), "{error}");
        assert!(
            error.contains("remove"),
            "受け取った値を見せること: {error}"
        );
    }

    // ---- fixedasset dispose ----

    #[test]
    fn dispose_needs_an_id_and_a_date() {
        let error = parse_args(&["fixedasset".to_string(), "dispose".to_string()]).unwrap_err();
        assert!(error.contains("--id"), "{error}");

        let error = parse_args(&[
            "fixedasset".to_string(),
            "dispose".to_string(),
            "--id".to_string(),
            "x".to_string(),
        ])
        .unwrap_err();
        assert!(error.contains("--on"), "{error}");
    }

    // 既定は下見。--commit を付けたときだけ除却する。
    #[test]
    fn dispose_does_not_write_without_commit() {
        let base = [
            "fixedasset".to_string(),
            "dispose".to_string(),
            "--id".to_string(),
            "abc".to_string(),
            "--on".to_string(),
            "2026-06-30".to_string(),
        ];
        match parse_args(&base).unwrap() {
            Command::FixedAssetDispose { id, on, commit } => {
                assert_eq!(id, "abc");
                assert_eq!(on, AccountingDate::new(2026, 6, 30).unwrap());
                assert!(!commit);
            }
            other => panic!("{other:?}"),
        }
        let mut with_commit = base.to_vec();
        with_commit.push("--commit".to_string());
        match parse_args(&with_commit).unwrap() {
            Command::FixedAssetDispose { commit, .. } => assert!(commit),
            other => panic!("{other:?}"),
        }
    }

    // ---- 除却した年以降を計算対象から外す ----

    // **本命。** 除却した年**そのもの**から外す。
    //
    // 除却した年の償却をどう扱うか（月割で計上するか、除却損に含めるか）は
    // 申告上の判断なので、このソフトは計算しない。
    #[test]
    fn the_year_of_disposal_is_already_outside_the_ledger() {
        let mut asset = fa("220", (2025, 3, 10), 108_000, 1, Some(2));
        asset.disposed_on = Some(AccountingDate::new(2026, 6, 30).unwrap());

        assert!(
            !is_outside_the_ledger_for(&asset, 2025),
            "除却前の年は対象に入る"
        );
        assert!(
            is_outside_the_ledger_for(&asset, 2026),
            "除却した年から外れる"
        );
        assert!(is_outside_the_ledger_for(&asset, 2027), "その後も外れる");
    }

    #[test]
    fn an_asset_not_yet_acquired_is_outside_the_ledger() {
        let asset = fa("210", (2027, 1, 1), 100_000, 1, Some(2));
        assert!(is_outside_the_ledger_for(&asset, 2026));
        assert!(!is_outside_the_ledger_for(&asset, 2027));
    }

    #[test]
    fn an_asset_without_a_disposal_date_stays_in_the_ledger() {
        let asset = fa("210", (2025, 1, 1), 100_000, 1, Some(2));
        assert!(!is_outside_the_ledger_for(&asset, 2030));
    }

    // ---- 固定資産台帳と帳簿の突き合わせ ----

    fn fa(
        account: &str,
        acquired: (i32, u8, u8),
        cost: i128,
        method: i16,
        life: Option<i16>,
    ) -> kaikei_app::ports::FixedAssetRow {
        kaikei_app::ports::FixedAssetRow {
            id: format!("{account}-{}", acquired.0),
            name: format!("資産{account}"),
            account: kaikei_core::AccountCode::parse(account).unwrap(),
            acquired_on: AccountingDate::new(acquired.0, acquired.1, acquired.2).unwrap(),
            acquisition_cost: kaikei_core::Money::from_minor(cost, kaikei_core::Currency::JPY),
            method,
            useful_life_years: life,
            business_ratio: None,
            disposed_on: None,
            note: None,
        }
    }

    fn balance_sheet_with(lines: &[(&str, i128)]) -> kaikei_app::policy::Statement {
        kaikei_app::policy::Statement {
            title: "貸借対照表".to_string(),
            sections: vec![kaikei_app::policy::StatementSection {
                title: "資産".to_string(),
                lines: lines
                    .iter()
                    .map(|(code, amount)| kaikei_app::policy::StatementLine {
                        account: kaikei_core::AccountCode::parse(code).unwrap(),
                        label: (*code).to_string(),
                        amount: kaikei_core::Money::from_minor(*amount, kaikei_core::Currency::JPY),
                    })
                    .collect(),
                subtotal: kaikei_core::Money::from_minor(0, kaikei_core::Currency::JPY),
            }],
            total: kaikei_core::Money::from_minor(0, kaikei_core::Currency::JPY),
        }
    }

    // **本命。** 償却済みの資産が帳簿に取得価額のまま残っていれば拾う。
    //
    // 実帳簿がこれ。2022年取得の pc（一括償却）は2024年で償却し終えている
    // のに、償却の相手科目を取り違えていたため機械装置が 118,800 のまま残った。
    #[test]
    fn a_fully_depreciated_asset_still_on_the_books_is_caught() {
        let assets = [fa("205", (2022, 8, 5), 118_800, 2, None)];
        let ledger = fixed_asset_book_values(&assets, 2026).unwrap();
        assert_eq!(
            ledger,
            vec![(kaikei_core::AccountCode::parse("205").unwrap(), 0)]
        );

        let found = fixed_asset_accounts_that_do_not_match(
            &ledger,
            &balance_sheet_with(&[("205", 118_800)]),
        );
        assert_eq!(found, vec![("205".to_string(), 0, 118_800)]);
    }

    // **本命。** 償却の記帳漏れを拾う。
    #[test]
    fn an_unposted_depreciation_is_caught() {
        // 2025年取得・定額法2年。2026年末の簿価は 9,000。
        let assets = [fa("220", (2025, 3, 10), 108_000, 1, Some(2))];
        let ledger = fixed_asset_book_values(&assets, 2026).unwrap();
        assert_eq!(ledger[0].1, 9_000);

        // 帳簿は取得価額のまま（1円も償却していない）。
        let found = fixed_asset_accounts_that_do_not_match(
            &ledger,
            &balance_sheet_with(&[("220", 108_000)]),
        );
        assert_eq!(found, vec![("220".to_string(), 9_000, 108_000)]);
    }

    // 合っていれば何も出ない。
    #[test]
    fn a_matching_ledger_reports_nothing() {
        let assets = [fa("220", (2025, 3, 10), 108_000, 1, Some(2))];
        let ledger = fixed_asset_book_values(&assets, 2026).unwrap();
        assert!(fixed_asset_accounts_that_do_not_match(
            &ledger,
            &balance_sheet_with(&[("220", 9_000)])
        )
        .is_empty());
    }

    // 同じ科目の資産は合算する。
    #[test]
    fn assets_on_the_same_account_are_summed() {
        let assets = [
            fa("210", (2025, 1, 1), 100_000, 1, Some(2)),
            fa("210", (2025, 1, 1), 40_000, 1, Some(2)),
        ];
        let ledger = fixed_asset_book_values(&assets, 2025).unwrap();
        assert_eq!(ledger.len(), 1);
        // 100,000 × 0.5 = 50,000 残 50,000 ／ 40,000 × 0.5 = 20,000 残 20,000
        assert_eq!(ledger[0].1, 70_000);
    }

    // **本命。** まだ取得していない資産は数えない。
    //
    // 数えると、来年買う予定のものが今年の帳簿と食い違って見える。
    #[test]
    fn an_asset_acquired_later_is_not_counted() {
        let assets = [fa("210", (2027, 1, 1), 100_000, 1, Some(2))];
        assert!(fixed_asset_book_values(&assets, 2026).unwrap().is_empty());
    }

    // 除却した年より後は数えない。
    #[test]
    fn a_disposed_asset_is_not_counted() {
        let mut asset = fa("210", (2025, 1, 1), 100_000, 1, Some(2));
        asset.disposed_on = Some(AccountingDate::new(2025, 12, 31).unwrap());
        assert!(fixed_asset_book_values(&[asset], 2026).unwrap().is_empty());
    }

    // 帳簿に科目が現れない場合は残高0として比べる。
    #[test]
    fn an_account_missing_from_the_book_counts_as_zero() {
        let assets = [fa("210", (2025, 1, 1), 100_000, 1, Some(2))];
        let ledger = fixed_asset_book_values(&assets, 2025).unwrap();
        let found = fixed_asset_accounts_that_do_not_match(&ledger, &balance_sheet_with(&[]));
        assert_eq!(found, vec![("210".to_string(), 50_000, 0)]);
    }

    fn embedded_tax_rule_sets() -> kaikei_jp::tax::TaxRuleSets {
        kaikei_jp::tax::TaxRuleSets::from_embedded().unwrap()
    }

    // 適格請求書が要る税区分なのに取引先が無い明細を拾う。
    #[test]
    fn a_qualified_purchase_without_a_counterparty_is_picked_up() {
        let rule_sets = embedded_tax_rule_sets();
        assert!(line_needs_a_counterparty(
            &tags_of(Some("PURCHASE_10_QUALIFIED"), None),
            &rule_sets
        ));
    }

    // 取引先が付いていれば拾わない（`JpTaxPolicy` が検証できる状態にある）。
    #[test]
    fn a_qualified_purchase_with_a_counterparty_is_not_picked_up() {
        let rule_sets = embedded_tax_rule_sets();
        assert!(!line_needs_a_counterparty(
            &tags_of(Some("PURCHASE_10_QUALIFIED"), Some("ANTHROPIC")),
            &rule_sets
        ));
    }

    // 適格請求書が要らない税区分は、取引先が無くても拾わない。
    // **これが効かないと 771 行の税区分なし明細まで数えてしまう。**
    #[test]
    fn a_non_qualified_purchase_is_not_picked_up() {
        let rule_sets = embedded_tax_rule_sets();
        assert!(
            !line_needs_a_counterparty(
                &tags_of(Some("PURCHASE_10_NON_QUALIFIED"), None),
                &rule_sets
            ),
            "非適格の仕入は適格請求書を前提にしていない"
        );
        assert!(!line_needs_a_counterparty(
            &tags_of(Some("OUT_OF_SCOPE"), None),
            &rule_sets
        ));
        assert!(
            !line_needs_a_counterparty(&tags_of(None, None), &rule_sets),
            "税区分が無い明細は対象外"
        );
    }

    // ---- 取引先の登録番号が確かめられているか ----

    fn party(
        code: &str,
        name: &str,
        reg: Option<&str>,
        qualified: Option<bool>,
    ) -> kaikei_app::policy::Counterparty {
        kaikei_app::policy::Counterparty {
            code: code.to_string(),
            name: name.to_string(),
            invoice_registration_no: reg.map(str::to_string),
            is_qualified_invoice_issuer: qualified,
        }
    }

    fn index(list: Vec<kaikei_app::policy::Counterparty>) -> kaikei_app::policy::CounterpartyIndex {
        kaikei_app::policy::CounterpartyIndex::new(list)
    }

    // **本命。** 適格請求書が要る税区分は、取引先の有無に関わらず true。
    #[test]
    fn the_tax_category_alone_decides_whether_an_invoice_is_required() {
        let rule_sets = embedded_tax_rule_sets();
        assert!(line_requires_a_qualified_invoice(
            &tags_of(Some("PURCHASE_10_QUALIFIED"), None),
            &rule_sets
        ));
        assert!(
            line_requires_a_qualified_invoice(
                &tags_of(Some("PURCHASE_10_QUALIFIED"), Some("apple")),
                &rule_sets
            ),
            "取引先の有無で変わらない"
        );
        assert!(!line_requires_a_qualified_invoice(
            &tags_of(Some("OUT_OF_SCOPE"), None),
            &rule_sets
        ));
    }

    // **本命。** 「未確認」と「非適格」を混ぜない。
    //
    // 非適格と確認できていれば経過措置で処理できる。未確認のままでは
    // どちらの扱いもできない——だから未確認だけを拾う。
    #[test]
    fn only_an_unverified_counterparty_is_picked_up() {
        let rule_sets = embedded_tax_rule_sets();
        let parties = index(vec![
            party("unknown", "未確認の相手", None, None),
            party("not_qualified", "非適格と確認済み", None, Some(false)),
            party(
                "qualified",
                "適格と確認済み",
                Some("T1234567890123"),
                Some(true),
            ),
        ]);

        assert_eq!(
            unverified_counterparty_name(
                &tags_of(Some("PURCHASE_10_QUALIFIED"), Some("unknown")),
                &rule_sets,
                &parties
            ),
            Some("未確認の相手".to_string())
        );
        assert_eq!(
            unverified_counterparty_name(
                &tags_of(Some("PURCHASE_10_QUALIFIED"), Some("not_qualified")),
                &rule_sets,
                &parties
            ),
            None,
            "非適格と確認済みなら経過措置で処理できる"
        );
        assert_eq!(
            unverified_counterparty_name(
                &tags_of(Some("PURCHASE_10_QUALIFIED"), Some("qualified")),
                &rule_sets,
                &parties
            ),
            None
        );
    }

    // 取引先マスタに無いコードは拾わない（ここは整合性を見る場所ではない）。
    #[test]
    fn a_counterparty_missing_from_the_master_is_skipped() {
        let rule_sets = embedded_tax_rule_sets();
        assert_eq!(
            unverified_counterparty_name(
                &tags_of(Some("PURCHASE_10_QUALIFIED"), Some("no_such_code")),
                &rule_sets,
                &index(vec![])
            ),
            None
        );
    }

    // 適格請求書が要らない税区分は拾わない。
    #[test]
    fn a_line_that_needs_no_invoice_is_not_picked_up() {
        let rule_sets = embedded_tax_rule_sets();
        let parties = index(vec![party("apple", "Apple", None, None)]);
        assert_eq!(
            unverified_counterparty_name(
                &tags_of(Some("OUT_OF_SCOPE"), Some("apple")),
                &rule_sets,
                &parties
            ),
            None
        );
    }

    // 取引先が付いていない明細は拾わない（そちらは別の指摘が出る）。
    #[test]
    fn a_line_without_a_counterparty_is_not_picked_up_here() {
        let rule_sets = embedded_tax_rule_sets();
        assert_eq!(
            unverified_counterparty_name(
                &tags_of(Some("PURCHASE_10_QUALIFIED"), None),
                &rule_sets,
                &index(vec![party("apple", "Apple", None, None)])
            ),
            None
        );
    }

    // 知らないコードで落ちたり数え上げたりしない。
    #[test]
    fn an_unknown_tax_category_is_not_picked_up() {
        let rule_sets = embedded_tax_rule_sets();
        assert!(!line_needs_a_counterparty(
            &tags_of(Some("NO_SUCH_CATEGORY"), None),
            &rule_sets
        ));
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

    // ─── 減価償却の計上漏れ ─────────────────────────────

    fn statement(title: &str, lines: Vec<(&str, i128)>) -> kaikei_app::policy::Statement {
        use kaikei_core::{Currency, Money};
        let lines: Vec<kaikei_app::policy::StatementLine> = lines
            .into_iter()
            .map(|(code, amount)| kaikei_app::policy::StatementLine {
                account: kaikei_core::AccountCode::parse(code).unwrap(),
                label: code.to_string(),
                amount: Money::from_minor(amount, Currency::JPY),
            })
            .collect();
        kaikei_app::policy::Statement {
            title: title.to_string(),
            sections: vec![kaikei_app::policy::StatementSection {
                title: "区分".to_string(),
                lines,
                subtotal: Money::from_minor(0, Currency::JPY),
            }],
            total: Money::from_minor(0, Currency::JPY),
        }
    }

    /// 財務諸表に無い科目は 0 として扱う。
    ///
    /// 無いことを「読めない」にすると、科目を1つも使っていない年度で
    /// 出力そのものが止まる。
    #[test]
    fn an_account_absent_from_the_statement_counts_as_zero() {
        let empty = statement("損益計算書", vec![]);
        let code = kaikei_core::AccountCode::parse("610").unwrap();
        assert!(amount_of(&empty, &code).is_zero());
    }

    #[test]
    fn an_account_present_in_the_statement_is_found() {
        let pl = statement("損益計算書", vec![("610", 50_000)]);
        let code = kaikei_core::AccountCode::parse("610").unwrap();
        assert_eq!(amount_of(&pl, &code).minor(), 50_000);
    }

    /// **本命。** 減価償却費が計上されていれば指摘しない。
    ///
    /// 正しい帳簿で毎年出る指摘は、当たり前になって本当の漏れを覆い隠す。
    #[test]
    fn a_book_with_depreciation_is_not_warned_about() {
        let pl = statement("損益計算書", vec![("610", 50_000)]);
        let bs = statement("貸借対照表", vec![("210", 161_917)]);

        // 指摘は stderr に出るので、ここで見るのは「落ちないこと」と
        // 「呼び出しが成功すること」である。中身の判定は下の2つが持つ。
        assert!(warn_if_depreciation_is_missing(&pl, &bs).is_ok());
    }

    /// **本命。** 対象になる資産が無ければ指摘しない。
    #[test]
    fn a_book_without_depreciable_assets_is_not_warned_about() {
        let pl = statement("損益計算書", vec![]);
        // 現金しかない帳簿。
        let bs = statement("貸借対照表", vec![("100", 552_542)]);

        assert!(warn_if_depreciation_is_missing(&pl, &bs).is_ok());
    }

    /// 指摘の対象になる帳簿でも、出力そのものは止めない。
    ///
    /// **止めると決算書が出せなくなる。** 償却額が決まるまで帳簿を見られない
    /// のでは、判断のしようがない。
    #[test]
    fn a_book_that_should_be_warned_about_still_produces_output() {
        let pl = statement("損益計算書", vec![]);
        let bs = statement("貸借対照表", vec![("210", 161_917)]);

        assert!(
            warn_if_depreciation_is_missing(&pl, &bs).is_ok(),
            "指摘しても出力は続けること"
        );
    }

    // ─── 貸借が逆向きの科目 ─────────────────────────────

    fn chart_for_sides() -> kaikei_core::ChartOfAccounts {
        use kaikei_core::{AccountDef, AccountType};
        let def = |code: &str, name: &str, account_type: AccountType| AccountDef {
            code: kaikei_core::AccountCode::parse(code).unwrap(),
            name: name.to_string(),
            account_type,
            parent: None,
            postable: true,
        };
        kaikei_core::ChartOfAccounts::new(vec![
            def("110", "普通預金", AccountType::Asset),
            def("210", "工具器具備品", AccountType::Asset),
            def("240", "減価償却累計額", AccountType::Asset),
            def("325", "未払金", AccountType::Liability),
            def("410", "事業主貸", AccountType::Equity),
        ])
        .unwrap()
    }

    /// **本命。** 資産のマイナス残高を見つける。
    ///
    /// 実際に weBanana.SP で、償却の相手科目を取り違えたために工具器具備品が
    /// -118,800 円になっていた誤りが4年間気づかれずに残った。貸借は一致した
    /// ままなので決算書を見ても分からない。
    #[test]
    fn an_asset_with_a_negative_balance_is_reported() {
        let bs = statement("貸借対照表", vec![("210", -118_800)]);

        let wrong = accounts_on_the_wrong_side(&bs, &chart_for_sides(), &[]);

        assert_eq!(wrong.len(), 1, "{wrong:?}");
        assert_eq!(wrong[0].0, "210");
        assert_eq!(wrong[0].1, "工具器具備品");
    }

    /// **本命。** 評価勘定は指摘しない。
    ///
    /// 減価償却累計額は資産に分類されるが貸方に立つのが正しい。指摘すると
    /// 正しい帳簿で毎回警告が出て、本当の異常が埋もれる。
    #[test]
    fn a_contra_account_is_not_reported() {
        let bs = statement("貸借対照表", vec![("240", -50_000)]);
        let contra = vec![kaikei_core::AccountCode::parse("240").unwrap()];

        let wrong = accounts_on_the_wrong_side(&bs, &chart_for_sides(), &contra);

        assert!(wrong.is_empty(), "{wrong:?}");
    }

    /// 同梱の科目表が、評価勘定だけを例外にしている。
    #[test]
    fn the_bundled_chart_excepts_only_the_contra_accounts() {
        let contra =
            kaikei_jp::chart::load_contra_accounts(kaikei_jp_data::CHART_SOLE_PROPRIETOR).unwrap();
        let codes: Vec<&str> = contra.iter().map(|c| c.as_str()).collect();

        assert!(
            codes.contains(&"240"),
            "減価償却累計額が例外に無い: {codes:?}"
        );
        // 預金や備品を例外にしてしまうと、本当の異常を拾えなくなる。
        assert!(!codes.contains(&"110"), "{codes:?}");
        assert!(!codes.contains(&"210"), "{codes:?}");
    }

    /// 純資産は対象外。
    ///
    /// 事業主貸・事業主借はどちらの向きにも立ちうる。
    #[test]
    fn an_equity_account_is_out_of_scope() {
        let bs = statement("貸借対照表", vec![("410", -500_000)]);

        let wrong = accounts_on_the_wrong_side(&bs, &chart_for_sides(), &[]);

        assert!(wrong.is_empty(), "{wrong:?}");
    }

    /// 自然な向きの帳簿では何も挙がらない。
    #[test]
    fn a_normal_balance_sheet_is_quiet() {
        let bs = statement("貸借対照表", vec![("110", 500_000), ("325", 200_000)]);

        let wrong = accounts_on_the_wrong_side(&bs, &chart_for_sides(), &[]);

        assert!(wrong.is_empty(), "{wrong:?}");
    }

    /// **本命。** 検査するコマンドが、書き出すコマンドより検査が緩くない。
    ///
    /// 「帳簿は大丈夫か」を確かめたい人は verify を打つのであって、
    /// report を打って警告を読むわけではない。
    #[test]
    fn the_usage_says_verify_makes_the_same_checks_as_report() {
        // 使い方の verify の節に、report と同じ指摘が載っている。
        let verify_section = USAGE
            .split("verify が見るもの:")
            .nth(1)
            .expect("verify の節があること");
        assert!(verify_section.contains("減価償却費"), "{verify_section}");
        assert!(verify_section.contains("マイナス残高"), "{verify_section}");
    }

    /// 指摘が出ても検査は失敗しない。
    ///
    /// 失敗させると、償却額が決まるまで verify が通らなくなる。
    #[test]
    fn the_usage_says_a_warning_does_not_fail_the_check() {
        let verify_section = USAGE.split("verify が見るもの:").nth(1).unwrap();
        assert!(verify_section.contains("失敗しません"), "{verify_section}");
    }

    // ─── attach の引数 ──────────────────────────────────

    /// **本命。** `--entry` があれば `--date` を要求しない。
    ///
    /// 1件ごとに5つの引数を打たせると、証憑の登録が現実的でなくなる。
    /// 実際にこの帳簿には証憑が1件も登録されていない。
    #[test]
    fn attach_does_not_need_a_date_when_an_entry_is_given() {
        let command = parse_attach(&args(&[
            "--file",
            "x.pdf",
            "--type",
            "receipt",
            "--via",
            "download",
            "--entry",
            "11111111-1111-1111-1111-111111111111",
        ]))
        .expect("--date なしで通ること");

        match command {
            Command::Attach(attach) => assert_eq!(attach.doc_date, None),
            other => panic!("attach が返らない: {other:?}"),
        }
    }

    /// `--entry` が無ければ `--date` は要る。
    ///
    /// **どちらも無いまま通すと、取引年月日の無い証憑ができる**——
    /// 検索要件の1つが欠ける。
    #[test]
    fn attach_still_needs_a_date_without_an_entry() {
        let error = parse_attach(&args(&[
            "--file", "x.pdf", "--type", "receipt", "--via", "download",
        ]))
        .expect_err("拒否されること");

        assert!(error.contains("--date"), "{error}");
        // 次の手を示す（`--entry` でも済むことを知らせる）。
        assert!(error.contains("--entry"), "{error}");
    }

    /// 明示した日付は仕訳より優先される。
    ///
    /// 証憑の日付が仕訳と違うことはある（請求書の日付と計上日など）。
    #[test]
    fn an_explicit_date_is_kept_even_with_an_entry() {
        let command = parse_attach(&args(&[
            "--file",
            "x.pdf",
            "--type",
            "receipt",
            "--via",
            "download",
            "--entry",
            "11111111-1111-1111-1111-111111111111",
            "--date",
            "2026-06-01",
        ]))
        .unwrap();

        match command {
            Command::Attach(attach) => assert_eq!(
                attach.doc_date,
                Some(AccountingDate::new(2026, 6, 1).unwrap())
            ),
            other => panic!("attach が返らない: {other:?}"),
        }
    }

    /// 使い方に、仕訳から埋まることが書いてある。
    #[test]
    fn the_usage_says_the_entry_fills_the_search_fields() {
        let attach_section = USAGE.split("attach の引数:").nth(1).expect("attach の節");
        assert!(
            attach_section.contains("その仕訳から埋めます"),
            "{attach_section}"
        );
    }

    // ─── 決算振替の後に決算書を作っていないか ──────────

    /// **本命。** 収益も費用も残っていない年度を指摘する。
    ///
    /// 帳簿の複製で通し稽古したところ、決算振替を記帳した後の決算書は
    /// 売上0・所得0・所得金額 −650,000（控除だけが残る）になった。
    #[test]
    fn a_year_with_no_revenue_and_no_expense_is_reported() {
        let pl = statement("損益計算書", vec![("500", 0), ("609", 0)]);

        assert!(year_looks_closed(&pl, 695));
    }

    /// 仕訳が無い年度では指摘しない。
    ///
    /// **空の帳簿を「決算振替済み」と言わない。** 収益も費用も0なのは
    /// 当たり前で、指摘しても何の手がかりにもならない。
    #[test]
    fn an_empty_year_is_not_reported() {
        let pl = statement("損益計算書", vec![]);

        assert!(!year_looks_closed(&pl, 0));
    }

    /// 収益か費用が残っていれば指摘しない。
    ///
    /// 正しい帳簿で毎回出る指摘は、当たり前になって本当の異常を覆い隠す。
    #[test]
    fn a_year_with_activity_is_not_reported() {
        let pl = statement("損益計算書", vec![("500", 11_520_080), ("609", 0)]);

        assert!(!year_looks_closed(&pl, 695));
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

    // ---- kaikei household（決算時の家事按分） ----

    fn household_args(rest: &[&str]) -> Vec<String> {
        let mut v = vec!["household".to_string()];
        v.extend(rest.iter().map(|s| s.to_string()));
        v
    }

    #[test]
    fn household_takes_the_year_account_and_ratio() {
        match parse_args(&household_args(&[
            "--year",
            "2026",
            "--account",
            "615",
            "--ratio",
            "0.3",
        ]))
        .unwrap()
        {
            Command::Household {
                fiscal_year,
                account,
                ratio,
                ..
            } => {
                assert_eq!(fiscal_year, 2026);
                assert_eq!(account, "615");
                assert_eq!(ratio, "0.3");
            }
            other => panic!("{other:?}"),
        }
    }

    // **本命。** 3つとも必須。どれが足りないかを言う。
    //
    // 事業割合を省いたまま通すと、既定値（何%？）を勝手に決めることになる。
    // 按分率は帳簿ごとに違い、決め打ちできる値ではない。
    #[test]
    fn household_requires_all_three_arguments() {
        for (given, missing) in [
            (vec!["--account", "615", "--ratio", "0.3"], "--year"),
            (vec!["--year", "2026", "--ratio", "0.3"], "--account"),
            (vec!["--year", "2026", "--account", "615"], "--ratio"),
        ] {
            let error = parse_args(&household_args(&given)).unwrap_err();
            assert!(error.contains(missing), "{missing} を要求すること: {error}");
        }
    }

    // **本命。** 科目の一部だけを按分できる。
    //
    // 実帳簿の通信費 476,631円 のうち、按分対象は携帯の 105,991円 だけで、
    // 残りはドメイン・サーバー・AI で事業専用である。科目まるごとしか
    // 按分できないと、この科目は手計算に落ちる。
    #[test]
    fn household_takes_an_amount_for_part_of_the_account() {
        match parse_args(&household_args(&[
            "--year",
            "2026",
            "--account",
            "604",
            "--ratio",
            "0.3",
            "--amount",
            "105991",
        ]))
        .unwrap()
        {
            Command::Household { amount, .. } => assert_eq!(amount, Some(105_991)),
            other => panic!("{other:?}"),
        }
    }

    // 額は3桁区切りでも読める（`fixedasset add --cost` と同じ）。
    #[test]
    fn the_household_amount_accepts_thousands_separators() {
        match parse_args(&household_args(&[
            "--year",
            "2026",
            "--account",
            "604",
            "--ratio",
            "0.3",
            "--amount",
            "105,991",
        ]))
        .unwrap()
        {
            Command::Household { amount, .. } => assert_eq!(amount, Some(105_991)),
            other => panic!("{other:?}"),
        }
    }

    // 省略すれば科目の全額。
    #[test]
    fn household_without_an_amount_means_the_whole_account() {
        match parse_args(&household_args(&[
            "--year",
            "2026",
            "--account",
            "615",
            "--ratio",
            "0.3",
        ]))
        .unwrap()
        {
            Command::Household { amount, .. } => assert_eq!(amount, None),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn household_rejects_an_amount_that_is_not_a_number() {
        let error = parse_args(&household_args(&[
            "--year",
            "2026",
            "--account",
            "604",
            "--ratio",
            "0.3",
            "--amount",
            "いくらか",
        ]))
        .unwrap_err();
        assert!(error.contains("--amount"), "{error}");
    }

    #[test]
    fn household_rejects_an_unknown_argument() {
        let error = parse_args(&household_args(&[
            "--year",
            "2026",
            "--account",
            "615",
            "--ratio",
            "0.3",
            "--commit",
        ]))
        .unwrap_err();
        assert!(error.contains("--commit"), "{error}");
    }

    // **本命。** 記帳する手段を持たない。
    //
    // 事業割合が妥当かはソフトには分からないので、出すだけにしてある
    // （`depreciation` と同じ立場）。`--commit` を足したくなったら、
    // まず「割合の妥当性を誰が保証するのか」を決めること。
    #[test]
    fn household_has_no_way_to_post_the_entry() {
        let command = parse_args(&household_args(&[
            "--year",
            "2026",
            "--account",
            "615",
            "--ratio",
            "0.3",
        ]))
        .unwrap();
        assert!(
            matches!(command, Command::Household { .. }),
            "記帳を伴う型に変わっていないこと"
        );
    }

    // ---- 入出金ベースの記帳（warn_if_receivables_are_never_used） ----

    /// ラベル付きの財務諸表を作る（`statement` はコードをラベルにしてしまう）。
    fn statement_with_labels(
        title: &str,
        lines: Vec<(&str, &str, i128)>,
    ) -> kaikei_app::policy::Statement {
        use kaikei_core::{Currency, Money};
        kaikei_app::policy::Statement {
            title: title.to_string(),
            sections: vec![kaikei_app::policy::StatementSection {
                title: "区分".to_string(),
                lines: lines
                    .into_iter()
                    .map(|(code, label, amount)| kaikei_app::policy::StatementLine {
                        account: kaikei_core::AccountCode::parse(code).unwrap(),
                        label: label.to_string(),
                        amount: Money::from_minor(amount, Currency::JPY),
                    })
                    .collect(),
                subtotal: Money::from_minor(0, Currency::JPY),
            }],
            total: Money::from_minor(0, Currency::JPY),
        }
    }

    fn chart_for_receivables() -> kaikei_core::ChartOfAccounts {
        use kaikei_core::{AccountCode, AccountDef, AccountType, ChartOfAccounts};
        ChartOfAccounts::new(vec![
            AccountDef {
                code: AccountCode::parse("500").unwrap(),
                name: "売上高".to_string(),
                account_type: AccountType::Revenue,
                parent: None,
                postable: true,
            },
            AccountDef {
                code: AccountCode::parse("135").unwrap(),
                name: "売掛金".to_string(),
                account_type: AccountType::Asset,
                parent: None,
                postable: true,
            },
        ])
        .unwrap()
    }

    /// **本命。** 売上があるのに債権・債務が全部0なら知らせる。
    ///
    /// 実帳簿がこれ（売上11件すべてが「普通預金／売上高」で、売掛金の明細は0件）。
    /// **決算書を見ても分からない**——売掛金の行が 0 と表示されるだけである。
    #[test]
    fn revenue_without_any_receivable_is_reported() {
        let pl = statement_with_labels("損益計算書", vec![("500", "売上高", 1_000_000)]);
        let bs = statement_with_labels("貸借対照表", vec![("135", "売掛金", 0)]);

        // 出力そのものは stderr なので、ここでは落ちないことと Ok を見る。
        assert!(warn_if_receivables_are_never_used(&pl, &bs, &chart_for_receivables()).is_ok());
        assert!(
            receivables_are_never_used(&pl, &bs, &chart_for_receivables()),
            "指摘すべき状態と判定すること"
        );
    }

    /// 売掛金が動いていれば言わない。
    #[test]
    fn a_used_receivable_is_quiet() {
        let pl = statement_with_labels("損益計算書", vec![("500", "売上高", 1_000_000)]);
        let bs = statement_with_labels("貸借対照表", vec![("135", "売掛金", 220_000)]);
        assert!(!receivables_are_never_used(
            &pl,
            &bs,
            &chart_for_receivables()
        ));
    }

    /// **本命。** 売上が無い年は言わない。
    ///
    /// 開業前や休業中に「売掛金がありません」と言っても手がかりにならない。
    #[test]
    fn a_year_without_revenue_is_quiet() {
        let pl = statement_with_labels("損益計算書", vec![("500", "売上高", 0)]);
        let bs = statement_with_labels("貸借対照表", vec![("135", "売掛金", 0)]);
        assert!(!receivables_are_never_used(
            &pl,
            &bs,
            &chart_for_receivables()
        ));
    }

    /// 買掛金だけでも動いていれば言わない（4つのどれか1つで足りる）。
    #[test]
    fn any_one_of_the_four_is_enough() {
        let pl = statement_with_labels("損益計算書", vec![("500", "売上高", 1_000_000)]);
        let bs = statement_with_labels("貸借対照表", vec![("310", "買掛金", 50_000)]);
        assert!(!receivables_are_never_used(
            &pl,
            &bs,
            &chart_for_receivables()
        ));
    }

    /// **本命。** 同梱の勘定科目表に、この検査が頼っている名前が実在する。
    ///
    /// **名前で見ているので、科目名が変わると黙って効かなくなる。**
    /// このテストが落ちたら、検査の名前も直すこと。
    #[test]
    fn the_chart_still_has_the_names_this_check_needs() {
        let chart = kaikei_jp::chart::load_embedded(kaikei_jp_data::CHART_SOLE_PROPRIETOR)
            .expect("同梱の勘定科目表が読めること");
        for name in RECEIVABLE_AND_PAYABLE_NAMES {
            assert!(
                chart.iter().any(|def| def.name == name),
                "勘定科目表に「{name}」がありません。検査が効かなくなります"
            );
        }
    }

    // ---- kaikei consumptiontax ----

    #[test]
    fn consumption_tax_takes_a_year() {
        match parse_args(&[
            "consumptiontax".to_string(),
            "--year".to_string(),
            "2026".to_string(),
        ])
        .unwrap()
        {
            Command::ConsumptionTax { fiscal_year } => assert_eq!(fiscal_year, 2026),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn consumption_tax_requires_a_year() {
        let error = parse_args(&["consumptiontax".to_string()]).unwrap_err();
        assert!(error.contains("--year"), "{error}");
    }

    #[test]
    fn consumption_tax_rejects_an_unknown_argument() {
        let error = parse_args(&[
            "consumptiontax".to_string(),
            "--year".to_string(),
            "2026".to_string(),
            "--commit".to_string(),
        ])
        .unwrap_err();
        assert!(error.contains("--commit"), "{error}");
    }

    /// **本命。** 記帳する手段を持たない。
    ///
    /// 集計を出すだけで、仕訳は作らない（`depreciation` / `household` と
    /// 同じ立場）。`--commit` を足したくなったら、まず「申告書の金額を
    /// 誰が保証するのか」を決めること。
    #[test]
    fn consumption_tax_has_no_way_to_post_anything() {
        let command = parse_args(&[
            "consumptiontax".to_string(),
            "--year".to_string(),
            "2026".to_string(),
        ])
        .unwrap();
        assert!(matches!(command, Command::ConsumptionTax { .. }));
    }
    // ---- kaikei counterparty verify ----

    fn verify_args(rest: &[&str]) -> Vec<String> {
        let mut v = vec!["counterparty".to_string(), "verify".to_string()];
        v.extend(rest.iter().map(|s| s.to_string()));
        v
    }

    #[test]
    fn counterparty_verify_takes_a_registration_number() {
        match parse_args(&verify_args(&[
            "--code",
            "jdf",
            "--registration-no",
            "T7123456789012",
        ]))
        .unwrap()
        {
            Command::CounterpartyVerify {
                code,
                registration_no,
                is_qualified,
                commit,
                ..
            } => {
                assert_eq!(code, "jdf");
                assert_eq!(registration_no.as_deref(), Some("T7123456789012"));
                assert_eq!(is_qualified, None, "指定しなければ変えない");
                assert!(!commit, "既定は下見");
            }
            other => panic!("{other:?}"),
        }
    }

    // **本命。** 「非適格と確認した」を記録できる。
    //
    // 非適格と分かっていれば経過措置で処理できる。**「未確認」とは別物**
    // なので、false を記録する手段が要る（D-122）。
    #[test]
    fn counterparty_verify_can_record_a_non_qualified_issuer() {
        match parse_args(&verify_args(&["--code", "povo", "--qualified", "false"])).unwrap() {
            Command::CounterpartyVerify { is_qualified, .. } => {
                assert_eq!(is_qualified, Some(false));
            }
            other => panic!("{other:?}"),
        }
    }

    // **本命。** 何も変えない呼び出しを断る。
    //
    // 打ち間違いで `--registration-no` を落とすと、確認日だけが更新されて
    // 「確認したのに何も分からなかった」状態になる。
    #[test]
    fn counterparty_verify_refuses_a_call_that_changes_nothing() {
        let error = parse_args(&verify_args(&["--code", "jdf"])).unwrap_err();
        assert!(error.contains("--registration-no"), "{error}");
        assert!(error.contains("--qualified"), "{error}");
    }

    // **本命。** 「未確認」を true/false で表さない。
    #[test]
    fn counterparty_verify_rejects_a_qualified_value_that_is_not_a_boolean() {
        let error =
            parse_args(&verify_args(&["--code", "jdf", "--qualified", "maybe"])).unwrap_err();
        assert!(error.contains("true"), "{error}");
        assert!(
            error.contains("未確認"),
            "付けなければ未確認だと言うこと: {error}"
        );
    }

    #[test]
    fn counterparty_verify_requires_a_code() {
        let error = parse_args(&verify_args(&["--qualified", "true"])).unwrap_err();
        assert!(error.contains("--code"), "{error}");
    }

    #[test]
    fn counterparty_verify_takes_a_verification_date() {
        match parse_args(&verify_args(&[
            "--code",
            "jdf",
            "--qualified",
            "true",
            "--on",
            "2026-08-18",
        ]))
        .unwrap()
        {
            Command::CounterpartyVerify { verified_on, .. } => {
                assert_eq!(verified_on.unwrap().to_iso_string(), "2026-08-18");
            }
            other => panic!("{other:?}"),
        }
    }

    // import は verify を巻き込まない（別の操作である）。
    #[test]
    fn counterparty_import_is_still_its_own_subcommand() {
        let error = parse_args(&["counterparty".to_string(), "unknown".to_string()]).unwrap_err();
        assert!(error.contains("import"), "{error}");
        assert!(error.contains("verify"), "{error}");
    }
    // ---- 非適格と確認済みの相手に適格の税区分（lines_with_a_known_non_qualified_counterparty） ----

    /// タグ付きの明細を1本だけ持つ仕訳を作る代わりに、判定を明細のタグで見る。
    /// **`JournalEntry` を組み立てるには chart / schema / guard / clock が要る**
    /// ので、CLI のテストはタグ集合を組み立てる形に揃えている。
    ///
    /// ここでは集計まで見たいので、判定の中身（どの相手を拾うか）を
    /// `unverified_counterparty_name` と同じ粒度で確かめる。
    #[test]
    fn a_confirmed_non_qualified_issuer_is_distinguished_from_an_unverified_one() {
        let rule_sets = embedded_tax_rule_sets();
        let parties = index(vec![
            party("unknown", "未確認の相手", None, None),
            party("not_qualified", "非適格と確認済み", None, Some(false)),
            party(
                "qualified",
                "適格と確認済み",
                Some("T7123456789012"),
                Some(true),
            ),
        ]);

        // 未確認だけが「登録番号が分からない」に挙がる。
        assert_eq!(
            unverified_counterparty_name(
                &tags_of(Some("PURCHASE_10_QUALIFIED"), Some("not_qualified")),
                &rule_sets,
                &parties
            ),
            None,
            "**非適格と確認済みは「未確認」ではない**"
        );
        assert_eq!(
            unverified_counterparty_name(
                &tags_of(Some("PURCHASE_10_QUALIFIED"), Some("unknown")),
                &rule_sets,
                &parties
            ),
            Some("未確認の相手".to_string())
        );
    }

    /// **本命。** 非適格に対応する税区分が存在する。
    ///
    /// 「非適格のものに見直してください」と言う以上、見直す先が無いと
    /// 案内にならない。
    #[test]
    fn a_non_qualified_tax_category_exists_to_switch_to() {
        let rule_sets = embedded_tax_rule_sets();
        for code in [
            "PURCHASE_10_NON_QUALIFIED",
            "PURCHASE_8_REDUCED_NON_QUALIFIED",
        ] {
            let found = rule_sets.iter().any(|table| table.category(code).is_ok());
            assert!(found, "{code} が税区分マスタにありません");
        }
    }

    /// 非適格の区分は適格請求書を要求しない（要求すると指摘が止まらなくなる）。
    #[test]
    fn the_non_qualified_category_does_not_require_an_invoice() {
        let rule_sets = embedded_tax_rule_sets();
        assert!(
            !line_requires_a_qualified_invoice(
                &tags_of(Some("PURCHASE_10_NON_QUALIFIED"), Some("x")),
                &rule_sets
            ),
            "見直した先でまた指摘されては直しようがない"
        );
    }
}
