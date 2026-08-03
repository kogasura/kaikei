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

use kaikei_mcp::config::REQUIRED_ENV_VARS;
use std::collections::HashMap;
use std::process::{Command, Output};

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

/// 設定が揃っていても DB へ繋げなければ起動しない（ツール応答に到達させない。
/// `docs/07-mcp-server.md` §7）。接続文字列そのものは出さない（§8）。
#[test]
fn a_complete_configuration_still_aborts_when_the_database_is_unreachable() {
    let mut env = complete_env();
    // 到達しないポートを指す（PostgreSQL の既定ポートを避ける）。
    env.insert(
        "APP_DATABASE_URL",
        "postgres://kaikei_app:dummy@127.0.0.1:1/kaikei",
    );
    let output = run_with(&env);
    let stderr = stderr_of(&output);

    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("APP_DATABASE_URL"),
        "どの環境変数を見ればよいかが分かること:\n{stderr}"
    );
    assert!(
        !stderr.contains("dummy"),
        "接続文字列（パスワードを含む）を出さないこと:\n{stderr}"
    );
    assert!(output.stdout.is_empty(), "stdout は空であること");
}
