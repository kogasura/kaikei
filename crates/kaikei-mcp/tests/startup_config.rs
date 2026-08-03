//! MC-24（`docs/07-mcp-server.md` §10）: **事業者設定を与えずに起動すると、
//! 既定値にフォールバックせず起動が失敗し、不足項目を名指しするメッセージが
//! 出る**（§7 / `DECISIONS.md` D-057 / D-082）。
//!
//! # なぜ実際にバイナリを起動するのか
//!
//! `config.rs` の単体テストは「文字列 → 値」の解釈を見ているが、
//! **その検証が起動経路に本当に繋がっているか**は見ていない。`main.rs` が
//! `ServerConfig::from_env()` の `Err` を握り潰したり、既定値で先に進んだり
//! しても、単体テストは緑のまま通る。ここでは `cargo` がビルドした
//! バイナリそのものを子プロセスとして起動して、
//!
//! - 終了コードが 0 でないこと
//! - stderr に不足項目の名前と次の手が出ること
//! - **stdout が1バイトも出ないこと**（stdio トランスポートでは stdout が
//!   JSON-RPC 専用チャネル。`docs/07-mcp-server.md` §4）
//!
//! を確認する。DB は要らない（設定の検証は接続より前に終わる）。
//!
//! `env!("CARGO_BIN_EXE_kaikei-mcp")` は cargo が統合テストに渡す、
//! ビルド済みバイナリの絶対パス。

use kaikei_mcp::config::{ServerConfig, REQUIRED_ENV_VARS};
use kaikei_mcp::startup;
use std::collections::HashMap;
use std::process::{Command, Output};
use std::time::Duration;

/// 全項目が揃った環境（接続先はローカルの実在しないポートでよい。
/// このファイルのテストはいずれも DB 接続まで到達しない）。
fn complete_env() -> HashMap<&'static str, &'static str> {
    HashMap::from([
        (
            "APP_DATABASE_URL",
            "postgres://kaikei_app:dummy@127.0.0.1:5432/kaikei",
        ),
        ("KAIKEI_BOOK_CURRENCY", "JPY"),
        ("KAIKEI_FISCAL_YEAR_RULE", "calendar_year"),
        ("KAIKEI_TAX_MODE", "exclusive"),
        ("KAIKEI_ROUNDING", "floor"),
        ("KAIKEI_ROUNDING_UNIT", "line"),
        ("KAIKEI_IS_TAXABLE_BUSINESS", "true"),
        ("KAIKEI_SIMPLIFIED_TAXATION", "false"),
        ("KAIKEI_CLOSING_ACCOUNT_CAPITAL", "400"),
        ("KAIKEI_CLOSING_ACCOUNT_OWNER_DRAWINGS", "410"),
        ("KAIKEI_CLOSING_ACCOUNT_OWNER_CONTRIBUTIONS", "420"),
        ("KAIKEI_CLOSING_TAX_CATEGORY", "NOT_APPLICABLE"),
    ])
}

/// 指定した環境変数だけを持つ状態でサーバーを起動し、終了を待つ。
///
/// `env_clear` で親プロセスの環境を捨てる。開発機では `.env` を
/// シェルに流し込んでからテストを走らせるため（README）、捨てないと
/// 「外したはずの変数が親から漏れて起動してしまう」テストになる。
fn run_with(env: &HashMap<&str, &str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_kaikei-mcp"));
    command.env_clear();
    for (key, value) in env {
        command.env(key, value);
    }
    // Windows ではシステム DLL の解決に SystemRoot が要る（env_clear 後に
    // 明示的に戻さないとプロセスが起動できない）。
    if let Ok(system_root) = std::env::var("SystemRoot") {
        command.env("SystemRoot", system_root);
    }
    command
        .output()
        .expect("kaikei-mcp のバイナリを起動できること")
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

/// MC-24: どの1項目を外しても起動に失敗し、その項目を名指しする。
#[test]
fn missing_business_settings_abort_the_startup_naming_the_item() {
    for missing in REQUIRED_ENV_VARS {
        let env: HashMap<&str, &str> = complete_env()
            .into_iter()
            .filter(|(key, _)| key != missing)
            .collect();

        let output = run_with(&env);
        let stderr = stderr_of(&output);

        assert!(
            !output.status.success(),
            "{missing} を外したのに起動に成功した（既定値にフォールバックしている）"
        );
        assert!(
            stderr.contains(missing),
            "{missing} を外したのに、その項目名が stderr に出ていない:\n{stderr}"
        );
        assert!(
            stderr.contains("既定値では起動しません"),
            "{missing}: 既定値に落ちないことが読み取れる文言であること:\n{stderr}"
        );
        assert!(
            output.stdout.is_empty(),
            "{missing}: 起動失敗時に stdout へ出力があった（stdout は JSON-RPC 専用）: {:?}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

/// 何も設定していない場合、不足項目が**全部**1回の出力で分かる。
///
/// 1件ずつ潰させると 12 回起動し直すことになる（`CLAUDE.md` §11）。
#[test]
fn an_empty_environment_reports_every_missing_item_at_once() {
    let output = run_with(&HashMap::new());
    let stderr = stderr_of(&output);

    assert!(!output.status.success());
    for name in REQUIRED_ENV_VARS {
        assert!(
            stderr.contains(name),
            "{name} が stderr に出ていない:\n{stderr}"
        );
    }
    assert!(output.stdout.is_empty(), "stdout は空であること");
    // 次の手（どこに書くか）が示されていること。
    assert!(stderr.contains(".env.example"), "{stderr}");
    assert!(stderr.contains("README"), "{stderr}");
}

/// 値が不正な場合も、既定値に落とさず起動を止める。
#[test]
fn an_invalid_value_aborts_the_startup_instead_of_falling_back() {
    let mut env = complete_env();
    env.insert("KAIKEI_IS_TAXABLE_BUSINESS", "1");
    let output = run_with(&env);
    let stderr = stderr_of(&output);

    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("KAIKEI_IS_TAXABLE_BUSINESS"), "{stderr}");
    assert!(stderr.contains("true"), "{stderr}");
    assert!(output.stdout.is_empty());
}

/// 税区分マスタに無い消費税区分コードは、**空でないだけでは通さない**。
///
/// 12個の必須設定のうち `KAIKEI_CLOSING_TAX_CATEGORY` だけが語彙の検証を
/// 受けておらず、存在しないコードでもサーバが正常に起動していた
/// （`KAIKEI_TAX_MODE=zeinuki` は「有効な値: exclusive, inclusive」で
/// 起動を中止するのと対照的だった）。Phase 3 には `close_period` が
/// 無いので実害は出ないが、決算振替を実装した Phase で
/// 「起動は通るのに決算だけが落ちる」形になる（`docs/07-mcp-server.md` §7）。
///
/// DB には到達しない（この検証は接続より前に終わる）。
#[test]
fn an_unknown_closing_tax_category_aborts_the_startup_listing_the_valid_codes() {
    let mut env = complete_env();
    env.insert("KAIKEI_CLOSING_TAX_CATEGORY", "NOPE");
    let output = run_with(&env);
    let stderr = stderr_of(&output);

    assert!(
        !output.status.success(),
        "税区分マスタに無いコードで起動に成功した:\n{stderr}"
    );
    assert!(stderr.contains("KAIKEI_CLOSING_TAX_CATEGORY"), "{stderr}");
    assert!(stderr.contains("NOPE"), "{stderr}");
    assert!(
        stderr.contains("有効な値") && stderr.contains("NOT_APPLICABLE"),
        "有効な値の一覧が示されること（他の項目と揃える。CLAUDE.md §11）:\n{stderr}"
    );
    assert!(output.stdout.is_empty(), "stdout は空であること");
}

/// 決算科目のコードが勘定科目表に無い場合、**どの環境変数を直せばよいか**が
/// 分かる。
///
/// `ComposeError` の文言そのものは言い換えない（`docs/07-mcp-server.md` §7）が、
/// あの文言は「正しい科目コードを `JpSoleProprietorClosingPolicy::new` に
/// 指定してください」という**利用者が触れない Rust の構築関数名**を次の手として
/// 提示する。DB 接続の失敗が `APP_DATABASE_URL` を添えているのと同じ形で、
/// 環境変数名と現在の値を後ろに足す（`CLAUDE.md` §11）。
#[test]
fn a_closing_account_that_is_missing_from_the_chart_names_the_environment_variable() {
    let mut env = complete_env();
    env.insert("KAIKEI_CLOSING_ACCOUNT_CAPITAL", "999");
    let output = run_with(&env);
    let stderr = stderr_of(&output);

    assert!(!output.status.success(), "{stderr}");
    // ComposeError の本文（言い換えていない）。
    assert!(stderr.contains("勘定科目表に見つかりません"), "{stderr}");
    // 足した部分: どの環境変数のどの値が原因か。
    assert!(
        stderr.contains("KAIKEI_CLOSING_ACCOUNT_CAPITAL") && stderr.contains("999"),
        "直すべき環境変数と現在の値が分かること:\n{stderr}"
    );
    assert!(
        stderr.contains("KAIKEI_CLOSING_ACCOUNT_OWNER_DRAWINGS")
            && stderr.contains("KAIKEI_CLOSING_ACCOUNT_OWNER_CONTRIBUTIONS"),
        "3科目のどれを直せばよいか対応が付くこと:\n{stderr}"
    );
    assert!(output.stdout.is_empty(), "stdout は空であること");
}

/// 設定が揃っていても DB へ繋げなければ起動しない（ツール応答に到達させない。
/// `docs/07-mcp-server.md` §7）。接続文字列そのものは出さない（§8）。
///
/// # ここだけバイナリを起動しない
///
/// 到達しない接続先に対して、`sqlx` のプールは接続確保の待ち時間
/// （既定 30 秒）が満了するまでリトライする。バイナリを起動して終了を待つと
/// **このテスト1件で 30 秒**かかり、しかもこのファイルは `pg-tests` ゲートの
/// 外にあるので、必須チェックの `quality` ジョブと開発者のローカル実行の
/// 両方がその 30 秒を毎回払うことになる（実測: 該当スイート 30.31s、
/// 他スイートは最大 0.43s）。
///
/// 待ち時間を短くして合成ルート（`startup::assemble`）を直接呼ぶ。
/// **本番の待ち時間は縮めない**——混んでいるだけの DB に対して起動が
/// 失敗するようになるため（`kaikei_store::pool::connect_app_with` の doc）。
///
/// `main.rs` がこの `Err` を握り潰さないことは、このファイルの他の3件
/// （実際にバイナリを起動して終了コードと stdout を見る）が押さえている。
#[tokio::test]
async fn a_complete_configuration_still_aborts_when_the_database_is_unreachable() {
    let mut vars = complete_env();
    // 到達しないポートを指す（PostgreSQL の既定ポートを避ける）。
    vars.insert(
        "APP_DATABASE_URL",
        "postgres://kaikei_app:dummy@127.0.0.1:1/kaikei",
    );

    let mut config = ServerConfig::from_lookup(&|name| vars.get(name).map(|v| (*v).to_string()))
        .expect("設定は揃っていること");
    config.connect_timeout = Duration::from_millis(500);

    let Err(error) = startup::assemble(&config).await else {
        panic!("DB へ接続できないのに起動が成功した");
    };
    let text = error.to_string();

    assert!(
        text.contains("APP_DATABASE_URL"),
        "どの環境変数を見ればよいかが分かること:\n{text}"
    );
    assert!(
        !text.contains("dummy"),
        "接続文字列（パスワードを含む）を出さないこと:\n{text}"
    );
}
