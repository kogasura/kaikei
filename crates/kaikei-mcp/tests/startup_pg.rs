//! 実 PostgreSQL に対する合成ルートの検証（`pg-tests`）。
//!
//! `PROGRESS.md` Phase 2 の教訓2「**構築は通るが記帳できない**」への
//! 直接の回答。合成ルートは「起動できたつもり」を最も作りやすい場所なので、
//! 本物の DB に対して最後まで通す。
//!
//! # このテストは使い捨てDBを作らない
//!
//! `kaikei-mcp` は `sqlx` に依存しない（`docs/07-mcp-server.md` §10 MC-30 の
//! 許可リスト）ため、`#[sqlx::test]` が使えない。`APP_DATABASE_URL` が指す
//! DB をそのまま使う。
//!
//! **書き込むのは勘定科目マスタだけ**（追加のみ・冪等。`DECISIONS.md` D-081）で、
//! 仕訳は1件も書かない。同じ投入は本番の起動でも毎回行われるので、開発機の
//! 帳簿に対して実行しても副作用は「同梱テンプレートの科目が入る」ことだけである。
//!
//! 「投入した後に**記帳が通る**」ことは `crates/kaikei-e2e`（使い捨てDBを
//! 作れる側）が確認する。ここで見るのは合成ルートそのものの成立である。

#![cfg(feature = "pg-tests")]

use kaikei_app::ports::ChartRepo;
use kaikei_app::tx::with_tx;
use kaikei_app::usecase::import_chart;
use kaikei_mcp::config::{ServerConfig, ENV_APP_DATABASE_URL};
use kaikei_mcp::server::KaikeiServer;
use kaikei_mcp::startup;
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

/// テスト用の設定。接続文字列だけ環境（`.env`）から取る。
///
/// `ServerConfig::from_lookup` を使うのは、プロセスの環境変数を書き換えずに
/// 済ませるため（`std::env::set_var` はテスト間で干渉する）。
fn config() -> ServerConfig {
    let url = std::env::var(ENV_APP_DATABASE_URL).unwrap_or_else(|_| {
        panic!(
            "環境変数 {ENV_APP_DATABASE_URL} が未設定です。\
             pg-tests は実 PostgreSQL に接続します。\
             .env.example を参照して設定してください。"
        )
    });
    let vars: HashMap<&str, String> = HashMap::from([
        (ENV_APP_DATABASE_URL, url),
        ("KAIKEI_BOOK_CURRENCY", "JPY".to_string()),
        ("KAIKEI_FISCAL_YEAR_RULE", "calendar_year".to_string()),
        ("KAIKEI_TAX_MODE", "exclusive".to_string()),
        ("KAIKEI_ROUNDING", "floor".to_string()),
        ("KAIKEI_ROUNDING_UNIT", "line".to_string()),
        ("KAIKEI_IS_TAXABLE_BUSINESS", "true".to_string()),
        ("KAIKEI_SIMPLIFIED_TAXATION", "false".to_string()),
        ("KAIKEI_CLOSING_ACCOUNT_CAPITAL", "400".to_string()),
        ("KAIKEI_CLOSING_ACCOUNT_OWNER_DRAWINGS", "410".to_string()),
        (
            "KAIKEI_CLOSING_ACCOUNT_OWNER_CONTRIBUTIONS",
            "420".to_string(),
        ),
        ("KAIKEI_CLOSING_TAX_CATEGORY", "NOT_APPLICABLE".to_string()),
    ]);
    ServerConfig::from_lookup(&|name| vars.get(name).cloned())
        .expect("テスト用の設定は揃っているはず")
}

/// 設定が揃っていれば合成ルートが最後まで通り、勘定科目が DB に入っている。
///
/// そのうえで**もう一度組み立てても1行も追加されない**ことまで見る
/// （サーバの再起動に相当。`DECISIONS.md` D-081）。
#[tokio::test]
async fn assembling_twice_succeeds_and_leaves_the_chart_unchanged() {
    let config = config();

    let first = startup::assemble(&config)
        .await
        .unwrap_or_else(|e| panic!("合成ルートが失敗しました: {e}"));

    // 同梱テンプレートの全科目が DB から読めること（記帳の前提）。
    let template_count = first.runtime.composition.chart.iter().count();
    assert!(template_count > 0);
    let stored = with_tx(first.runtime.store.as_ref(), |tx| {
        Box::pin(async move { Ok(tx.load_chart().await?) })
    })
    .await
    .expect("勘定科目表を読めること");
    for def in first.runtime.composition.chart.iter() {
        assert!(
            stored.get(&def.code).is_some(),
            "テンプレートの科目 {} が DB に入っていない",
            def.code.as_str()
        );
    }

    // 診断は「起動時に stderr へ出す行」であって、応答でも stdout でもない。
    assert!(
        first
            .diagnostics
            .iter()
            .any(|d| d.contains("勘定科目マスタ")),
        "投入結果が診断に出ること: {:?}",
        first.diagnostics
    );

    // 2回目（再起動相当）。追加は起きない。
    let second = startup::assemble(&config)
        .await
        .unwrap_or_else(|e| panic!("2回目の合成が失敗しました: {e}"));
    let reimport = with_tx(second.runtime.store.as_ref(), |tx| {
        let chart = second.runtime.composition.chart.clone();
        Box::pin(async move { import_chart::execute(tx, &chart).await })
    })
    .await
    .expect("再投入が失敗しないこと");
    assert_eq!(
        reimport.inserted_rows, 0,
        "2回目以降の起動で科目が追加されてはいけない"
    );

    // 合成した依存がサーバーに渡ること（PR-F 以降のツールはここから取る）。
    let server = KaikeiServer::with_runtime(Arc::clone(&first.runtime));
    assert!(server.runtime().is_some());
}

/// 接続ロールを `kaikei_migrator`（テーブル所有者）にすると起動を拒否する。
///
/// `docs/07-mcp-server.md` §8: 環境変数を1つ取り違えるだけで append-only の
/// DB 権限による防御が丸ごと消える。**その取り違えを起動時に検出できる**
/// ことを、実際に所有者ロールで繋いで確かめる。
#[tokio::test]
async fn assembling_with_the_owner_role_is_refused() {
    let Ok(migrator_url) = std::env::var("MIGRATOR_DATABASE_URL") else {
        panic!(
            "環境変数 MIGRATOR_DATABASE_URL が未設定です。\
             この検査は所有者ロールでの接続を実際に試します。\
             .env.example を参照して設定してください。"
        );
    };

    let mut config = config();
    config.app_database_url = migrator_url;

    let Err(err) = startup::assemble(&config).await else {
        panic!("所有者ロールでの起動は拒否されるはず");
    };
    let text = err.to_string();
    assert!(text.contains("UPDATE"), "{text}");
    assert!(text.contains("kaikei_app"), "{text}");
    assert!(text.contains("APP_DATABASE_URL"), "{text}");
}

/// **起動後の stdout に JSON-RPC 以外が出ない**（`docs/07-mcp-server.md` §4）。
///
/// バイナリを子プロセスとして起動し、`initialize` を1件送って、stdout に
/// 出た行が**すべて JSON-RPC のメッセージ**であることを確認する。
/// 診断（勘定科目マスタの投入結果など）は stderr に出ているはず。
#[tokio::test]
async fn the_running_server_writes_only_json_rpc_to_stdout() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let url = std::env::var(ENV_APP_DATABASE_URL).expect("APP_DATABASE_URL");

    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_kaikei-mcp"))
        .env_clear()
        .env(ENV_APP_DATABASE_URL, url)
        .env("KAIKEI_BOOK_CURRENCY", "JPY")
        .env("KAIKEI_FISCAL_YEAR_RULE", "calendar_year")
        .env("KAIKEI_TAX_MODE", "exclusive")
        .env("KAIKEI_ROUNDING", "floor")
        .env("KAIKEI_ROUNDING_UNIT", "line")
        .env("KAIKEI_IS_TAXABLE_BUSINESS", "true")
        .env("KAIKEI_SIMPLIFIED_TAXATION", "false")
        .env("KAIKEI_CLOSING_ACCOUNT_CAPITAL", "400")
        .env("KAIKEI_CLOSING_ACCOUNT_OWNER_DRAWINGS", "410")
        .env("KAIKEI_CLOSING_ACCOUNT_OWNER_CONTRIBUTIONS", "420")
        .env("KAIKEI_CLOSING_TAX_CATEGORY", "NOT_APPLICABLE")
        // Windows ではシステム DLL の解決に SystemRoot が要る。
        .env(
            "SystemRoot",
            std::env::var("SystemRoot").unwrap_or_default(),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("kaikei-mcp を起動できること");

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");

    let request = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":"#,
        r#"{"protocolVersion":"2024-11-05","capabilities":{},"#,
        r#""clientInfo":{"name":"kaikei-mcp-test","version":"0"}}}"#,
        "\n"
    );
    stdin
        .write_all(request.as_bytes())
        .await
        .expect("initialize を書き込めること");
    stdin.flush().await.expect("flush");

    let mut reader = BufReader::new(stdout).lines();
    let line = tokio::time::timeout(Duration::from_secs(30), reader.next_line())
        .await
        .expect("30秒以内に stdout へ応答が出ること")
        .expect("stdout を読めること")
        .expect("応答が1行はあること");

    let _ = child.kill().await;

    // 1行目が JSON-RPC のメッセージであること（ここに診断の1行でも
    // 混ざっていれば JSON として解釈できず落ちる）。
    let value: serde_json::Value = serde_json::from_str(&line)
        .unwrap_or_else(|e| panic!("stdout の1行目が JSON-RPC ではありません（{e}）: {line}"));
    assert_eq!(
        value.get("jsonrpc").and_then(|v| v.as_str()),
        Some("2.0"),
        "stdout の1行目: {line}"
    );
    assert_eq!(value.get("id").and_then(|v| v.as_i64()), Some(1));
}
