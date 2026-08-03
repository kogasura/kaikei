//! `with_tx` — トランザクションの開始・確定・破棄を1箇所に閉じる唯一の推奨入口。
//!
//! [`crate::ports::Store::begin`] を直接呼んで commit を書き忘れると、
//! エラーも警告も出ずに何も保存されない（会計データでは致命的）。この
//! ヘルパに閉じることで、commit 漏れが構造的に起きにくくなる。

use crate::error::{AppError, RepoError};
use crate::ports::{Store, TxScope};
use std::future::Future;
use std::pin::Pin;

/// `with_tx` に渡すクロージャが返す future を trait object 化するためのエイリアス。
///
/// クロージャは `&mut S::Tx` を借用するため、戻り値の future はその借用の
/// ライフタイムを持つ（`BoxFut<'a, _>`）。
pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// トランザクションを開始し、`f` を実行し、成功したら commit・失敗したら
/// rollback する。
///
/// # クロージャに渡せるもの（必ず読むこと）
///
/// `f` は HRTB（`for<'a>`）で全称量化されているため、**`'static` でない
/// 借用を一切キャプチャできない**。呼び出し側が依存（`&dyn TaxPolicy` や
/// `&TagSchema` 等）を借用のまま `move` クロージャに persist させようとすると、
/// `` `'1` must outlive `'static` `` や E0597 のようなエラーになる（`match` の
/// スクルティニーが引き起こす下記の E0505 よりも、実際にはこちらの方が
/// 高頻度で踏む罠である）。
///
/// 依存はすべて**所有値**にしてから `move` クロージャに入れること:
/// - `Arc<dyn TaxPolicy>` / `Arc<dyn AppClock>` / `Arc<dyn IdGenerator>` の
///   ように `Arc` で保持し、クロージャに渡す直前に `Arc::clone` する
/// - `TagSchema`（`kaikei-jp-data` 相当。合成ルートが起動時に読み込む可変
///   データ）も同様に `Arc<TagSchema>` として持ち回る。`context::BookSettings`
///   に含めない設計（`context.rs` の doc を参照）と対になる規律であり、
///   合成ルート側がこの形で保持する
/// - `&[TagKey]` のようなスライス引数は `.to_vec()` で所有値化する
///
/// # トランザクションのネストと外部 I/O
///
/// `f` の中で `store.begin()` や別のトランザクションを開始しないこと
/// （このトランザクションと二重にネストする必要が生じるユースケースは
/// 現時点で想定していない）。また `f` の中では DB 以外の I/O
/// （証憑ファイルの書き込み・外部API・LLM呼び出し等）を行わないこと。
/// トランザクション（例: 採番カウンタの行ロック）を握ったまま外部応答を
/// 待つ状態（idle-in-transaction）を防ぐため。
///
/// # エラー型は [`AppError`]
///
/// ユースケース専用のエラー型（[`crate::usecase::post_entry::PostEntryFailure`]
/// のように、失敗経路でも `PolicyNote` を運ぶもの）を返すクロージャには
/// [`with_tx_err`] を使う。両者は同じ実装を通る。
///
/// **この関数を総称化しなかった**のは、`Ok(..)` しか書かないクロージャ
/// （読み取りだけのトランザクション）で `E` が推論できなくなり、
/// 呼び出し側すべてに turbofish か型注釈が要るようになるため
/// （実測で `kaikei-store` / `kaikei-e2e` の pg-tests が8箇所壊れた）。
/// **最も多い書き方が最も短く書ける**ようにしてある。
pub async fn with_tx<S, T, F>(store: &S, f: F) -> Result<T, AppError>
where
    S: Store,
    T: Send,
    F: for<'a> FnOnce(&'a mut S::Tx) -> BoxFut<'a, Result<T, AppError>> + Send,
{
    with_tx_err(store, f).await
}

/// [`with_tx`] の、**エラー型について総称**な版。
///
/// ユースケースが `AppError` 以外の失敗値を返す場合に使う。現状の唯一の
/// 利用者は [`crate::usecase::post_entry::execute`] /
/// [`crate::usecase::post_entry::preview`]（失敗経路でも `PolicyNote` を運ぶ
/// [`crate::usecase::post_entry::PostEntryFailure`] を返す）。
///
/// `E` に要求するのは `From<RepoError>` だけ（`begin` / `commit` の失敗を
/// そのエラー型で表現できる必要があるため）。
///
/// トランザクションの扱い（commit / rollback の条件、クロージャに渡せるもの、
/// ネスト・外部 I/O の禁止）は [`with_tx`] と完全に同じ——**同じ関数本体**である。
/// 分けているのは型推論の都合だけで、規律を二重に持っているわけではない。
///
/// ```ignore
/// // ユースケースの戻り値の型が E を決めるので、注釈は要らない。
/// let out = with_tx_err(&store, |tx| {
///     Box::pin(async move { post_entry::execute(tx, /* .. */).await })
/// })
/// .await?;
/// ```
pub async fn with_tx_err<S, T, E, F>(store: &S, f: F) -> Result<T, E>
where
    S: Store,
    T: Send,
    E: From<RepoError>,
    F: for<'a> FnOnce(&'a mut S::Tx) -> BoxFut<'a, Result<T, E>> + Send,
{
    let mut tx = store.begin().await?;
    // ★ ここで `match f(&mut tx).await { .. }` と書くと E0505 になる。
    //   match のスクルティニー（`f(&mut tx).await` の一時値）が match 式
    //   全体のスコープで生存し続け、`Ok`/`Err` 分岐内で行う `tx.commit()`
    //   （`self` を消費する = `tx` を move する）と `&mut tx` の借用が
    //   衝突するため。必ず `let` で受けて `&mut tx` への借用をここで
    //   終わらせてから分岐する。
    let result = f(&mut tx).await;
    match result {
        Ok(value) => {
            tx.commit().await?;
            Ok(value)
        }
        Err(err) => {
            // rollback 自体の失敗は元のエラーを覆い隠さない（呼び出し側には
            // 元の失敗理由を返す）。rollback 失敗はログに残す価値があるが、
            // ログ基盤の導入は本 PR の対象外なので握りつぶす。
            let _ = tx.rollback().await;
            Err(err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{ChartRepo, NumberingRepo};
    use crate::testing::InMemoryStore;
    use kaikei_core::{AccountCode, AccountDef, AccountType, ChartOfAccounts};

    fn sample_chart() -> ChartOfAccounts {
        ChartOfAccounts::new(vec![AccountDef {
            code: AccountCode::parse("100").unwrap(),
            name: "現金".to_string(),
            account_type: AccountType::Asset,
            parent: None,
            postable: true,
        }])
        .unwrap()
    }

    // with_tx が実際にコンパイルでき、成功時に commit されることを確認する
    // （E0505 を踏んでいないことの証明でもある）。
    #[tokio::test]
    async fn with_tx_commits_on_success() {
        let store = InMemoryStore::with_chart(sample_chart());

        let result: Result<usize, AppError> = with_tx(&store, |tx| {
            Box::pin(async move {
                let chart = tx.load_chart().await?;
                Ok(chart.iter().count())
            })
        })
        .await;

        assert_eq!(result.unwrap(), 1);
    }

    // 失敗時は rollback され、コミット経路と異なることを確認する
    // （インメモリ fake は commit されたものだけを採番の永続化に反映するため、
    // rollback 側の効果が無いことを裏から確認する）。
    #[tokio::test]
    async fn with_tx_rolls_back_on_failure() {
        let store = InMemoryStore::new();

        let result: Result<(), AppError> = with_tx(&store, |tx| {
            Box::pin(async move {
                let _ = tx.next_entry_no(2026).await?;
                Err(AppError::Rejected {
                    reason: "テスト用の意図的な失敗".to_string(),
                })
            })
        })
        .await;

        assert!(result.is_err());
        // ロールバックしたので、採番カウンタは進んでいないはず。
        let next: Result<_, AppError> = with_tx(&store, |tx| {
            Box::pin(async move {
                let no = tx.next_entry_no(2026).await?;
                Ok(no)
            })
        })
        .await;
        assert_eq!(next.unwrap().as_u32(), 1);
    }
}
