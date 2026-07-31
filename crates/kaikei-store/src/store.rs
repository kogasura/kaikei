//! [`kaikei_app::ports::Store`] の実装本体（[`PgTx`]）。
//!
//! # トランザクションのライフタイム（phase1計画 G1 / `DECISIONS.md` D-021）
//!
//! [`PgTx`] はライフタイム `'c` を持つ。[`kaikei_app::ports::Store::Tx`] に
//! 割り当てるのは `PgTx<'static>`（`sqlx::PgPool::begin()` が
//! `Transaction<'static, Postgres>` を返すため、GAT を使わずにこの制約を
//! 満たせる。sqlx 0.8.6 で実測確認済み）。`journal` / `chart` / `period` /
//! `numbering` の各 repo trait 実装はすべて `impl ... for PgTx<'_>` と
//! 汎用のライフタイムに対して行う（`journal/mod.rs` 等を参照）。これにより
//! 将来 SAVEPOINT（`&'t mut Transaction<'c, DB>` に対する `Acquire<'t>`
//! 実装を経由したネスト）を表現できる余地を残す。
//!
//! # commit 忘れへの防御（phase1計画 G5 / R10）
//!
//! `store.begin()` して commit を書き忘れると、エラーも警告も出ずに何も
//! 保存されない（会計データでは致命的）。store 層が担う緩和:
//! - [`PgTx`] に `#[must_use]`
//! - [`Drop`] で `committed` フラグを見て `tracing::warn!`
//! - `kaikei_app::tx::with_tx` を唯一の推奨入口とする
//!   （[`kaikei_app::ports::Store::begin`] の doc を参照）

use crate::error::from_sqlx_error;
use crate::pool::PgStore;
use async_trait::async_trait;
use kaikei_app::error::RepoError;
use kaikei_app::ports::{Store, TxScope};
use sqlx::{PgConnection, Postgres, Transaction};

/// [`kaikei_app::ports::Store::Tx`] の PostgreSQL 実装。
///
/// `tx` を `Option` で保持するのは、[`Drop`] を実装した型からはフィールドを
/// 直接ムーブできない（E0509）ため。`commit`/`rollback`（`self` を値で取る）は
/// `Option::take` で中身を取り出してから `sqlx::Transaction::commit`/
/// `rollback` に渡す。`take` した後は `tx` が `None` になるため、
/// [`Drop`] は「`tx` がまだ `Some` のまま破棄された＝commit/rollback を
/// 呼び忘れた」ことをこの1フィールドだけで判定できる。
#[must_use = "PgTx を保持したまま破棄すると commit/rollback されず変更が失われます。\
              kaikei_app::tx::with_tx を使ってください"]
pub struct PgTx<'c> {
    tx: Option<Transaction<'c, Postgres>>,
}

impl<'c> PgTx<'c> {
    fn new(tx: Transaction<'c, Postgres>) -> Self {
        PgTx { tx: Some(tx) }
    }

    /// 各 repo 実装（`journal/mod.rs` 等）がクエリ発行に使う接続を返す。
    ///
    /// # Panics
    ///
    /// commit/rollback 後に呼び出すと panic する。`TxScope::commit`/
    /// `rollback` は `self` を値で消費するため、正しく使う限りこの状態には
    /// 到達しない（呼び出し側のバグでのみ起こりうる）。
    pub(crate) fn conn(&mut self) -> &mut PgConnection {
        self.tx
            .as_mut()
            .expect("PgTx: commit/rollback 済みのトランザクションは再利用できません（バグ）")
    }
}

#[async_trait]
impl Store for PgStore {
    type Tx = PgTx<'static>;

    async fn begin(&self) -> Result<Self::Tx, RepoError> {
        let tx = self.pool().begin().await.map_err(from_sqlx_error)?;
        Ok(PgTx::new(tx))
    }
}

#[async_trait]
impl<'c> TxScope for PgTx<'c> {
    async fn commit(mut self) -> Result<(), RepoError> {
        let tx = self
            .tx
            .take()
            .expect("PgTx::commit: 構築直後は必ず Some（二重commitは型で防がれる）");
        tx.commit().await.map_err(from_sqlx_error)
    }

    async fn rollback(mut self) -> Result<(), RepoError> {
        let tx = self
            .tx
            .take()
            .expect("PgTx::rollback: 構築直後は必ず Some（二重rollbackは型で防がれる）");
        tx.rollback().await.map_err(from_sqlx_error)
    }
}

impl Drop for PgTx<'_> {
    fn drop(&mut self) {
        if self.tx.is_some() {
            tracing::warn!(
                "PgTx が commit/rollback されずに破棄されました。この中で行った変更は\
                 保存されません（会計データでは致命的です）。kaikei_app::tx::with_tx を\
                 使ってください。"
            );
        }
    }
}
