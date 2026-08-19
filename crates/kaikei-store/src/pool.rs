//! [`PgStore`]（[`kaikei_app::ports::Store`] の PostgreSQL 実装）と、
//! アプリ実行用ロール／マイグレーション実行用ロールでの接続確立ヘルパ。

use crate::error::from_sqlx_error;
use kaikei_app::error::RepoError;
use sqlx::postgres::PgPoolOptions;

/// 接続プール。**呼び出し側が sqlx に直接依存しなくて済むよう再輸出する。**
pub use sqlx::postgres::PgPool;
use std::time::Duration;

/// アプリ実行用プールの最大接続数。
///
/// アプリ経路は axum のハンドラ等から並行に呼ばれることを前提とするため、
/// 複数の同時接続を許容する。
const APP_POOL_MAX_CONNECTIONS: u32 = 10;

/// アプリ実行用プールが接続を確保できるまで待つ上限（`sqlx` の
/// `acquire_timeout`）の既定値。
///
/// `sqlx` の既定値と同じ 30 秒。**接続できない相手に対してもこの時間だけ
/// 待つ**（接続拒否でも即座には諦めず、満了までリトライする）ので、
/// 到達しない接続先を使うテストは1件で30秒かかる。短い値を渡せる入口が
/// [`connect_app_with`] である。
pub const APP_DEFAULT_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(30);

/// マイグレーション実行用プールの最大接続数。
///
/// `bin/kaikei-migrate.rs` はマイグレーション適用のためだけに単発で使う接続
/// であり、並行アクセスは想定しない。
const MIGRATOR_POOL_MAX_CONNECTIONS: u32 = 1;

/// アプリ実行用ロール（`kaikei_app`）で PostgreSQL に接続するプールを作る。
///
/// `docs/03-database.md` §1 のとおり、帳簿本体（`journal_entries`/
/// `journal_lines`）への `UPDATE`/`DELETE`/`TRUNCATE` 権限を持たないロールでの
/// 接続を想定する（append-only を DB 権限で強制する。`CLAUDE.md` §2）。
///
/// # Errors
///
/// 接続に失敗した場合は `sqlx::Error` を返す。
pub async fn connect_app(database_url: &str) -> Result<PgPool, sqlx::Error> {
    connect_app_with(database_url, APP_DEFAULT_ACQUIRE_TIMEOUT).await
}

/// [`connect_app`] と同じプールを、接続確保の待ち時間を指定して作る。
///
/// # なぜ待ち時間を指定できる入口が要るのか
///
/// 既定の 30 秒は「一時的に混んでいる DB を待つ」ための値であり、
/// **到達しない接続先に対しても同じだけ待つ**。「設定が揃っていても DB に
/// 繋げなければ起動しない」ことを確かめるテストは意図的に到達しない
/// 接続先を使うので、既定値のままだとテスト1件で 30 秒かかる
/// （`cargo test --workspace` は必須チェックの `quality` ジョブと開発者の
/// ローカル実行の両方に乗るため、そこに 30 秒を置くと全員が毎回払う）。
///
/// **本番の待ち時間を短くして解決しない。** 短くすると、混んでいるだけの
/// DB に対して起動が失敗するようになる。待ち時間は呼び出し側の事情なので、
/// 呼び出し側に渡させる。
///
/// # Errors
///
/// 接続に失敗した場合は `sqlx::Error` を返す。
pub async fn connect_app_with(
    database_url: &str,
    acquire_timeout: Duration,
) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(APP_POOL_MAX_CONNECTIONS)
        .acquire_timeout(acquire_timeout)
        .connect(database_url)
        .await
}

/// マイグレーション実行用ロール（`kaikei_migrator`）で PostgreSQL に接続する
/// プールを作る。
///
/// `bin/kaikei-migrate.rs` から使う。テーブル/スキーマ所有者としての接続で
/// あり、通常のアプリ経路（[`connect_app`]）とは別ロールになる。所有者は
/// REVOKE をバイパスできるため（phase1計画 R5）、この接続は原則としてマイグレーション
/// 適用時のみ使うこと。
///
/// # Errors
///
/// 接続に失敗した場合は `sqlx::Error` を返す。
pub async fn connect_migrator(database_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(MIGRATOR_POOL_MAX_CONNECTIONS)
        .connect(database_url)
        .await
}

/// append-only を DB 権限で守る対象のテーブル。
///
/// `0003_journal.sql` の `REVOKE` の対象と**同じ集合**であること。
/// 片方だけを検査すると、もう片方に誤って権限を与えた環境を見逃す。
pub const JOURNAL_TABLES: [&str; 2] = ["journal_entries", "journal_lines"];

/// 帳簿本体に対して接続ロールが持っていてはならない権限。
///
/// `0003_journal.sql` の `REVOKE` と同じ3つ。`TRUNCATE` を落とすと、
/// 「1行ずつは消せないがテーブルごとなら空にできる」ロールが検査を素通りする。
pub const FORBIDDEN_JOURNAL_PRIVILEGES: [&str; 3] = ["UPDATE", "DELETE", "TRUNCATE"];

/// 接続中のロールが帳簿本体に対して持っている、**持っていてはならない権限**。
///
/// [`inspect_journal_privileges`] が返す。**判断（起動を中止するか）は
/// 呼び出し側（合成ルート）が行う**——「起動時に何を致命的とするか」は
/// presentation 層の方針であり、永続化層が決めることではない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalPrivileges {
    /// 接続中のロール名（`current_user`）。
    pub role: String,

    /// 検出された「持っていてはならない権限」の一覧。
    ///
    /// 要素は `(テーブル名, 権限名)`。[`JOURNAL_TABLES`] ×
    /// [`FORBIDDEN_JOURNAL_PRIVILEGES`] の全組み合わせのうち、実際に
    /// 保持しているものだけが入る（append-only が効いていれば空）。
    ///
    /// 真偽値のフィールドを組み合わせの数だけ並べないのは、対象が増える
    /// たびに呼び出し側の `is_append_only` 相当の式を書き足す必要が生じ、
    /// **足し忘れが検査の穴になる**ため。
    pub granted: Vec<(String, String)>,
}

impl JournalPrivileges {
    /// append-only が DB 権限の層でも効いている（禁止権限を1つも持って
    /// いない）か。
    pub fn is_append_only(&self) -> bool {
        self.granted.is_empty()
    }

    /// 保持している禁止権限の表示（`"journal_lines の UPDATE"` を並べたもの）。
    ///
    /// 起動を中止する側がそのままメッセージに埋め込める形にする
    /// （`CLAUDE.md` §11。「どのテーブルのどの権限が余計なのか」が
    /// 分からないと、どの `GRANT` を取り消せばよいか判断できない）。
    pub fn describe_granted(&self) -> String {
        if self.granted.is_empty() {
            return "（なし）".to_string();
        }
        self.granted
            .iter()
            .map(|(table, privilege)| format!("{table} の {privilege}"))
            .collect::<Vec<_>>()
            .join("、")
    }
}

/// 接続中のロールが帳簿本体（[`JOURNAL_TABLES`]）に対して持っている
/// [`FORBIDDEN_JOURNAL_PRIVILEGES`] を調べる。
///
/// # なぜ起動時にこれを見るのか
///
/// `docs/07-mcp-server.md` §8 のとおり、`kaikei-mcp` は認証機構を持たない
/// （stdio でソケットを開かない）ため、**実効的な権限境界は DB ロールだけ**に
/// なる。接続先を `APP_DATABASE_URL`（`kaikei_app`）ではなく
/// `MIGRATOR_DATABASE_URL`（`kaikei_migrator` = テーブル所有者）に
/// 取り違えると、`0003_journal.sql` の `REVOKE` を所有者権限でバイパスし、
/// append-only の防御4層（`docs/07-mcp-server.md` §1）のうち DB 権限の層が
/// 丸ごと消える。**環境変数を1つ間違えるだけで起きる。**
///
/// ロール名の文字列比較（`current_user = 'kaikei_migrator'`）ではなく
/// `has_table_privilege` を見るのは、**守りたい性質そのもの**を検査するため。
/// ロールの名前を変えた環境や、`kaikei_app` に誤って `GRANT` してしまった
/// 環境でも検出できる。
///
/// # 対象は2テーブル × 3権限
///
/// `journal_entries` だけを見ると、`journal_lines` に権限を与えた環境
/// （明細だけを書き換えれば貸借も金額も変えられる）を見逃す。`TRUNCATE`
/// を見ないと、テーブルごと空にできるロールを見逃す。組み合わせは
/// 定数から導出しており、SQL 側に手で書き写さない。
///
/// # Errors
///
/// 対象テーブルが存在しない場合（マイグレーション未適用）を含め、
/// 問い合わせに失敗したら [`RepoError`]。
pub async fn inspect_journal_privileges(pool: &PgPool) -> Result<JournalPrivileges, RepoError> {
    let tables: Vec<String> = JOURNAL_TABLES.iter().map(|t| (*t).to_string()).collect();
    let privileges: Vec<String> = FORBIDDEN_JOURNAL_PRIVILEGES
        .iter()
        .map(|p| (*p).to_string())
        .collect();

    // 組み合わせを SQL 側で展開する（列を手で並べると、定数を増やしたときに
    // SQL だけが古いまま残る）。`has_table_privilege` は現在の接続ロールに
    // ついて判定する2引数版。
    let rows: Vec<(String, String, String, bool)> = sqlx::query_as(
        "SELECT current_user::text, t.table_name, p.privilege, \
                has_table_privilege(t.table_name, p.privilege) \
         FROM UNNEST($1::text[]) AS t(table_name) \
         CROSS JOIN UNNEST($2::text[]) AS p(privilege)",
    )
    .bind(&tables)
    .bind(&privileges)
    .fetch_all(pool)
    .await
    .map_err(from_sqlx_error)?;

    let role = rows
        .first()
        .map(|(role, _, _, _)| role.clone())
        .ok_or_else(|| RepoError::Corrupt {
            reason: "接続ロールの権限を問い合わせた結果が0行でした\
                     （検査対象の一覧が空になっています）"
                .to_string(),
        })?;
    let granted = rows
        .into_iter()
        .filter(|(_, _, _, held)| *held)
        .map(|(_, table, privilege, _)| (table, privilege))
        .collect();

    Ok(JournalPrivileges { role, granted })
}

/// [`kaikei_app::ports::Store`] の PostgreSQL 実装。
///
/// `Arc<PgStore>` として axum の `State` 等に積む設計を想定する
/// （`kaikei_app::ports` モジュール doc、`DECISIONS.md` D-029 を参照）。
#[derive(Debug, Clone)]
pub struct PgStore {
    pool: PgPool,
}

impl PgStore {
    /// 接続済みの `PgPool`（[`connect_app`] 等で作成したもの）から store を作る。
    pub fn new(pool: PgPool) -> Self {
        PgStore { pool }
    }

    /// 内部のプールへの参照を返す。`crate::store` が `begin` の実装に使う。
    pub(crate) fn pool(&self) -> &PgPool {
        &self.pool
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 検査対象の集合が `0003_journal.sql` の `REVOKE` と揃っている。
    ///
    /// 定数を減らすと、実際に `GRANT` して検出を確かめる pg-tests
    /// （`tests/privileges.rs`）が「見ていない対象」を検査しなくなる。
    /// マイグレーションを一次情報として突き合わせる（`DECISIONS.md` D-047
    /// 「手で維持する一覧は腐る」）。
    #[test]
    fn the_inspected_set_matches_the_revoke_in_the_migration() {
        let migration = include_str!("../migrations/0003_journal.sql");
        for table in JOURNAL_TABLES {
            assert!(
                migration.contains(table),
                "{table} が 0003_journal.sql に現れません"
            );
        }
        for privilege in FORBIDDEN_JOURNAL_PRIVILEGES {
            assert!(
                migration.contains(privilege),
                "{privilege} が 0003_journal.sql に現れません"
            );
        }
    }

    /// 禁止権限を1つでも保持していれば append-only ではない、と判定する。
    #[test]
    fn holding_any_forbidden_privilege_is_not_append_only() {
        let clean = JournalPrivileges {
            role: "kaikei_app".to_string(),
            granted: Vec::new(),
        };
        assert!(clean.is_append_only());
        assert_eq!(clean.describe_granted(), "（なし）");

        let leaky = JournalPrivileges {
            role: "kaikei_app".to_string(),
            granted: vec![("journal_lines".to_string(), "UPDATE".to_string())],
        };
        assert!(!leaky.is_append_only());
        assert_eq!(leaky.describe_granted(), "journal_lines の UPDATE");
    }
}
