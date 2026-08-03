//! 2ロールハーネス（pg-tests 共通ヘルパ）。
//!
//! `crates/kaikei-store/tests/common/mod.rs` とほぼ同じ内容だが、コピーで
//! ある（別crateに切り出して共有していない）。理由は crate をまたぐため:
//! `tests/` 配下の統合テストヘルパは各crateのプライベートなコンパイル単位
//! であり、Rust の可視性の仕組み上、`kaikei-store` の `tests/common` を
//! `kaikei-e2e` から `use` する経路が無い（`kaikei-store` の公開APIとして
//! 再エクスポートすることも考えられるが、テストのためだけに本体の公開APIを
//! 汚すのは筋が悪い）。両者が乖離しても実害は小さい小さなヘルパなので、
//! 複製のコストより「共有機構を新設するコスト」の方が高いと判断した。
//!
//! `#[sqlx::test]` は `fn(PgPoolOptions, PgConnectOptions)` という形の
//! テスト関数を受け付ける。この crate のマイグレーションは
//! `crates/kaikei-store/migrations`（`#[sqlx::test(migrations = "...")]`で
//! 相対パス指定）を使う（`kaikei-e2e` 自身はテーブルを定義しない）。

#![allow(dead_code)]

/// 実バイナリを stdio で起動して `tools/call` を送るハーネス
/// （`tests/mcp_stdio_server.rs` と `tests/mcp_walkthrough.rs` が共有する）。
pub mod mcp_stdio;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::PgPool;

/// `KAIKEI_APP_PASSWORD` を環境変数から読む。未設定なら明示的に panic する。
///
/// `#[ignore]` でテストを無言スキップする設計は採らない
/// （ローカルで実行されず「通った」と錯覚することを防ぐため）。
fn app_password() -> String {
    std::env::var("KAIKEI_APP_PASSWORD").unwrap_or_else(|_| {
        panic!(
            "環境変数 KAIKEI_APP_PASSWORD が未設定です。pg-tests は kaikei_app ロールへの\
             張り直しを検証するため必須です。.env.example を参照して設定してください。"
        )
    })
}

/// migrator ロールと kaikei_app ロール、両方の接続プール。
pub struct Roles {
    /// テーブル/スキーマ所有者（マイグレーション実行用ロール）。
    pub migrator: PgPool,
    /// アプリ実行用ロール。journal_entries/journal_lines への
    /// UPDATE/DELETE/TRUNCATE 権限を持たない（append-only の強制）。
    pub app: PgPool,
}

/// `#[sqlx::test]` から渡された migrator 用の接続情報を使って、
/// 同一テストDBに対する migrator / kaikei_app 両ロールのプールを作る。
pub async fn roles(pool_opts: PgPoolOptions, migrator_opts: PgConnectOptions) -> Roles {
    let migrator = pool_opts
        .clone()
        .connect_with(migrator_opts.clone())
        .await
        .expect("kaikei_migrator ロールでの接続に失敗しました");

    let app = app_pool(migrator_opts).await;

    Roles { migrator, app }
}

/// `#[sqlx::test]` から渡された migrator 用の接続情報を使い、同一テストDBに対する
/// **kaikei_app ロールのプールだけ**を張る。
pub async fn app_pool(migrator_opts: PgConnectOptions) -> PgPool {
    let app_opts = migrator_opts
        .username("kaikei_app")
        .password(&app_password());
    PgPoolOptions::new()
        .max_connections(2)
        .connect_with(app_opts)
        .await
        .expect(
            "kaikei_app ロールでの接続に失敗しました。\
             docker/postgres/init/01-roles.sql が適用されているか確認してください。",
        )
}
