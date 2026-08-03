//! `kaikei-mcp` の起動。**設定を読む → 組み立てる → stdio で待ち受ける**
//! の3手だけを行う（`docs/07-mcp-server.md` §4 / §7）。
//!
//! # ★stdout は JSON-RPC 専用チャネル★
//!
//! stdio トランスポートでは stdout が MCP のフレーミングそのものである。
//! `println!` が1行でも混ざるとプロトコルが壊れ、接続ごと落ちる。
//! **このバイナリは stdout に一切書かない。** 診断・エラーは全て
//! `eprintln!`（stderr）に出す。
//!
//! この不変条件は2つの経路で機械的に見張っている:
//!
//! | 検査 | 置き場 |
//! |---|---|
//! | ソースに `println!` / `print!` / `io::stdout` が現れないこと | `tests/stdout_is_json_rpc_only.rs` |
//! | 起動に失敗したとき stdout が1バイトも出ないこと | `tests/startup_config.rs`（実際にこのバイナリを起動する） |
//! | 起動できたとき stdout が JSON-RPC の行だけであること | `tests/startup_pg.rs`（`pg-tests`） |
//!
//! ログライブラリ（`tracing_subscriber`）は入れていない。購読者を登録
//! しない限り `tracing` のイベントはどこにも出力されないため、
//! `kaikei-store` の `tracing::warn!`（`PgTx` の commit 忘れ警告）が
//! stdout を汚すことはない。**将来購読者を入れる場合は writer を
//! stderr に固定すること**（既定は stdout であり、入れた瞬間に壊れる）。
//!
//! # 起動時に落とす
//!
//! 設定・マスタ・接続ロール・勘定科目マスタの投入まで、全て起動時に
//! 検証してから待受に入る。失敗したら stderr に理由を書いて終了コード 1
//! で終わる。ツール応答に到達させない（`docs/07-mcp-server.md` §7）。

use kaikei_mcp::config::ServerConfig;
// `serve_stdio` は `dispatch` にある。stdio トランスポートを名指しするには
// `rmcp` を書く必要があり、それが許されるのは `dispatch.rs` と `error.rs`
// だけであるため（`crates/kaikei-mcp/src/dispatch.rs` のモジュール doc）。
use kaikei_mcp::dispatch::serve_stdio;
use kaikei_mcp::server::KaikeiServer;
use kaikei_mcp::startup::{self, StartupError};
use std::process::ExitCode;
use std::sync::Arc;

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // stderr。stdout には絶対に書かない。
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), StartupError> {
    let config = ServerConfig::from_env()?;
    let startup = startup::assemble(&config).await?;

    for line in &startup.diagnostics {
        eprintln!("[kaikei-mcp] {line}");
    }
    eprintln!("[kaikei-mcp] stdio で待ち受けます（stdout は JSON-RPC 専用）");

    let server = KaikeiServer::with_runtime(Arc::clone(&startup.runtime));
    serve_stdio(server).await.map_err(|source| {
        StartupError::new(format!("MCP サーバーの待受が異常終了しました: {source}"))
    })
}
