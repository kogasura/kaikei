//! 2ロールハーネス（pg-tests 共通ヘルパ）。
//!
//! `#[sqlx::test]` は `fn(PgPoolOptions, PgConnectOptions)` という形の
//! テスト関数を受け付ける（sqlx-core-0.8.6/src/testing/mod.rs:124 で実測確認済み）。
//! この形で受け取った `PgConnectOptions` は、`#[sqlx::test]` が新規作成し
//! マイグレーション適用済みのテスト用データベースを指す接続情報で、
//! 接続ロールは `DATABASE_URL`（migrator の URL を想定。README 参照）に従う。
//!
//! [`roles`] はこの migrator 用の接続情報から、同じテスト用データベースに対して
//! kaikei_app ロールでも張り直したプールを追加で作る。これにより、同一の
//! テスト用データベースに対して「所有者（migrator）」と
//! 「権限を制限したアプリロール（app）」の両方から権限・トリガの挙動を検証できる。
//!
//! `tests/*.rs` は Cargo により各ファイルが独立したテストバイナリとしてコンパイル
//! されるため、このモジュールの関数のうち一部はテストバイナリによって使わない
//! （例: `migrations.rs` は `sqlstate`/`insert_balanced_entry` を使わない）。
//! これは共有テストヘルパの一般的な形であり、バイナリごとに未使用の関数がある
//! こと自体はコードの欠陥ではないため、`dead_code` 警告を抑止する。

#![allow(dead_code)]

use kaikei_core::{AccountingDate, PeriodGuard, PeriodStatus};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::PgPool;

/// 常に `Open` を返す `PeriodGuard`（テスト用）。
///
/// `round_trip.rs` と `trial_balance_differential.rs` の双方が同一の定義を
/// 個別に持っていた重複を解消するため、ここに1つだけ定義する
/// （`kaikei_app::period_guard` には依存しない。`kaikei-store` は app 層の
/// テスト用 fake ではなく core 型のみを使う）。
pub struct AllOpen;

impl PeriodGuard for AllOpen {
    fn status(&self, _date: AccountingDate) -> PeriodStatus {
        PeriodStatus::Open
    }
}

/// `KAIKEI_APP_PASSWORD` を環境変数から読む。未設定なら明示的に panic する。
///
/// `#[ignore]` でテストを無言スキップする設計は採らない
/// （ローカルで実行されず「通った」と錯覚することを防ぐため。phase1計画 §6-2）。
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
    /// R5: 所有者は REVOKE をバイパスできるため、このロールに対する防御は
    /// トリガのみが最後の砦になる。
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
///
/// [`roles`] は migrator と app の2本を張るが、「アプリロールで接続し直して
/// 読めるか」を見たいだけのテスト（`e2e_usecase.rs` の E2E-01）では migrator 側が
/// 使われないまま接続を消費する。そのようなケース向けの軽量版。
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

/// `sqlx::Error` から SQLSTATE（5桁のエラーコード）を取り出す。
/// DB エラーでない場合（接続断等）は `None`。
pub fn sqlstate(err: &sqlx::Error) -> Option<String> {
    err.as_database_error()
        .and_then(|e| e.code())
        .map(|c| c.into_owned())
}

/// 貸借が一致した最小限の仕訳（明細2行）を1トランザクションで INSERT する。
///
/// 権限・トリガ系のテストが「まず正常な1件が存在する状態」を作るための共通
/// ヘルパ（勘定科目コードは `'100'`/`'500'` の固定値。`journal_lines.account_code`
/// に FK は無いため `accounts` への事前 INSERT は不要。docs/03-database.md §2）。
pub async fn insert_balanced_entry(
    pool: &PgPool,
    id: uuid::Uuid,
    fiscal_year: i32,
    entry_no: i32,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;

    sqlx::query(
        "INSERT INTO journal_entries \
         (id, fiscal_year, entry_no, entry_date, description, recorded_at) \
         VALUES ($1, $2, $3, '2026-04-01', 'テスト仕訳', now())",
    )
    .bind(id)
    .bind(fiscal_year)
    .bind(entry_no)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO journal_lines \
         (entry_id, line_no, account_code, side, amount_minor, currency, currency_minor_unit) \
         VALUES \
         ($1, 1, '100', 1, 1000, 'JPY', 0), \
         ($1, 2, '500', 2, 1000, 'JPY', 0)",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await
}
