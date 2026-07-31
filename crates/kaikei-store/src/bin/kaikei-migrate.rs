//! マイグレーション実行バイナリ。
//!
//! `MIGRATOR_DATABASE_URL`（`kaikei_migrator` ロールでの接続。`.env.example`
//! 参照）に接続し、`crates/kaikei-store/migrations/` を適用する。
//!
//! ```text
//! MIGRATOR_DATABASE_URL=postgres://kaikei_migrator:...@localhost:5432/kaikei \
//!     cargo run -p kaikei-store --bin kaikei-migrate
//! ```

use kaikei_store::pool::connect_migrator;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let database_url = std::env::var("MIGRATOR_DATABASE_URL").map_err(|_| {
        "環境変数 MIGRATOR_DATABASE_URL が未設定です（.env.example を参照してください）"
    })?;

    let pool = connect_migrator(&database_url).await?;

    sqlx::migrate!("./migrations").run(&pool).await?;

    println!("マイグレーションを適用しました");
    Ok(())
}
