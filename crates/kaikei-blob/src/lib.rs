//! 証憑ファイルの保存（Content-Addressed Storage）。
//!
//! `docs/06-documents.md`。内容の SHA-256 で管理する。
//!
//! # 削除する経路を用意しない
//!
//! [`BlobStore`] に `delete` を**定義しない**。帳簿書類には保存義務（7年、
//! 欠損金の繰越があれば10年）があるので、消す手段をコードに置かない。
//!
//! # 何を保証するか
//!
//! - **同じ内容は1つ。** 内容が同じなら同じハッシュになり、2回入れても
//!   ファイルは1つ
//! - **改変が見つかる。** [`BlobStore::verify`] が保存内容のハッシュを
//!   計算し直して照合する
//! - **書きかけが残らない。** 一時領域に書いてから移動する（同じファイル
//!   システム上での移動なので原子的）
//!
//! # 帳簿とは結び付けない
//!
//! この層はファイルの保存と取り出しに徹する。「どの仕訳の証憑か」は
//! `documents` テーブル（`kaikei-store`）の仕事である。

use async_trait::async_trait;
use kaikei_core::BlobHash;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// 証憑の保存で起きる失敗。
#[derive(Debug, Error)]
pub enum BlobError {
    /// 求められた証憑が保存されていない。
    #[error("証憑が見つかりません: {hash}")]
    NotFound {
        /// 探したハッシュ（16進表記）。
        hash: String,
    },
    /// 保存先の読み書きに失敗した。
    #[error("証憑の保存先を読み書きできませんでした: {path}（{source}）")]
    Io {
        /// 対象のパス。
        path: String,
        /// 元の失敗。
        #[source]
        source: std::io::Error,
    },
}

/// 証憑ファイルの保存先。
#[async_trait]
pub trait BlobStore: Send + Sync {
    /// 保存して内容のハッシュを返す。
    ///
    /// **既に同じ内容が保存されていれば何もせず、同じハッシュを返す。**
    async fn put(&self, bytes: &[u8]) -> Result<BlobHash, BlobError>;

    /// 取り出す。
    async fn get(&self, hash: &BlobHash) -> Result<Vec<u8>, BlobError>;

    /// 保存されているか。
    async fn exists(&self, hash: &BlobHash) -> Result<bool, BlobError>;

    /// 保存内容のハッシュを計算し直して照合する。
    ///
    /// **false は「中身が変わっている」ことを意味する。** 保存されていない
    /// 場合は [`BlobError::NotFound`] を返す（「無い」と「変わっている」を
    /// 混ぜない）。
    async fn verify(&self, hash: &BlobHash) -> Result<bool, BlobError>;

    // delete は定義しない（モジュール doc を参照）。
}

/// ローカルのファイルシステムに保存する [`BlobStore`]。
#[derive(Debug, Clone)]
pub struct LocalBlobStore {
    root: PathBuf,
}

/// 書きかけを置く場所。
const TEMP_DIR: &str = "tmp";

impl LocalBlobStore {
    /// 保存先の根を指定して作る。
    ///
    /// ディレクトリは [`Self::prepare`] で作る。**構築だけで副作用を持たせない**
    /// ——設定を読んだだけでディスクに書くと、検査目的で作ったときに驚く。
    pub fn new(root: impl Into<PathBuf>) -> Self {
        LocalBlobStore { root: root.into() }
    }

    /// 保存先のディレクトリを用意する。
    pub async fn prepare(&self) -> Result<(), BlobError> {
        self.create_dir(&self.root.join(TEMP_DIR)).await
    }

    fn path_of(&self, hash: &BlobHash) -> PathBuf {
        self.root.join(hash.to_path())
    }

    async fn create_dir(&self, path: &Path) -> Result<(), BlobError> {
        tokio::fs::create_dir_all(path)
            .await
            .map_err(|source| BlobError::Io {
                path: path.display().to_string(),
                source,
            })
    }
}

#[async_trait]
impl BlobStore for LocalBlobStore {
    async fn put(&self, bytes: &[u8]) -> Result<BlobHash, BlobError> {
        let hash = hash_of(bytes);
        let destination = self.path_of(&hash);

        // 既にあれば何もしない。**上書きしない**——同じ内容なのだから書き直す
        // 理由が無く、書けば書きかけの窓が生まれる。
        if tokio::fs::try_exists(&destination)
            .await
            .map_err(|source| BlobError::Io {
                path: destination.display().to_string(),
                source,
            })?
        {
            return Ok(hash);
        }

        let temp_dir = self.root.join(TEMP_DIR);
        self.create_dir(&temp_dir).await?;
        if let Some(parent) = destination.parent() {
            self.create_dir(parent).await?;
        }

        // **一時領域に書いてから移動する。** 直接書くと、途中で落ちたときに
        // 「ハッシュの名前を持つ、中身が欠けたファイル」が残る。それは
        // 検証を通らないが、存在するせいで put が「保存済み」と誤判定する。
        let temp_path = temp_dir.join(hash.to_hex());
        tokio::fs::write(&temp_path, bytes)
            .await
            .map_err(|source| BlobError::Io {
                path: temp_path.display().to_string(),
                source,
            })?;
        tokio::fs::rename(&temp_path, &destination)
            .await
            .map_err(|source| BlobError::Io {
                path: destination.display().to_string(),
                source,
            })?;
        Ok(hash)
    }

    async fn get(&self, hash: &BlobHash) -> Result<Vec<u8>, BlobError> {
        let path = self.path_of(hash);
        match tokio::fs::read(&path).await {
            Ok(bytes) => Ok(bytes),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                Err(BlobError::NotFound {
                    hash: hash.to_hex(),
                })
            }
            Err(source) => Err(BlobError::Io {
                path: path.display().to_string(),
                source,
            }),
        }
    }

    async fn exists(&self, hash: &BlobHash) -> Result<bool, BlobError> {
        let path = self.path_of(hash);
        tokio::fs::try_exists(&path)
            .await
            .map_err(|source| BlobError::Io {
                path: path.display().to_string(),
                source,
            })
    }

    async fn verify(&self, hash: &BlobHash) -> Result<bool, BlobError> {
        let bytes = self.get(hash).await?;
        Ok(&hash_of(&bytes) == hash)
    }
}

/// 内容の SHA-256。
pub fn hash_of(bytes: &[u8]) -> BlobHash {
    let digest = Sha256::digest(bytes);
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    BlobHash::from_bytes(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// テスト用の使い捨てディレクトリ。
    ///
    /// `tempfile` を足さずに済ませる（依存を増やさない）。落ちたときに中身を
    /// 見られるよう、あえて自動削除はしない——一時領域なので OS が片付ける。
    fn scratch(label: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "kaikei-blob-{}-{}-{}",
            label,
            std::process::id(),
            unique
        ));
        let _ = std::fs::remove_dir_all(&path);
        path
    }

    // BS-1: 保存すると取り出せる。
    #[tokio::test]
    async fn what_is_put_can_be_read_back() {
        let store = LocalBlobStore::new(scratch("roundtrip"));
        store.prepare().await.unwrap();
        let content = "領収書のPDFのつもり".as_bytes();

        let hash = store.put(content).await.unwrap();

        assert_eq!(store.get(&hash).await.unwrap(), content);
        assert!(store.exists(&hash).await.unwrap());
    }

    // BS-2: **本命。** 同じ内容を2回入れてもファイルは1つ。
    #[tokio::test]
    async fn the_same_content_is_stored_once() {
        let root = scratch("dedupe");
        let store = LocalBlobStore::new(&root);
        store.prepare().await.unwrap();
        let content = b"same bytes";

        let first = store.put(content).await.unwrap();
        let second = store.put(content).await.unwrap();

        assert_eq!(first, second);
        // 実際に1つしか無いことを数える（tmp は除く）。
        let stored = count_stored(&root);
        assert_eq!(stored, 1, "同じ内容が2つ保存されている");
    }

    // BS-3: 内容が違えば別のハッシュになる。
    #[tokio::test]
    async fn different_content_gets_a_different_hash() {
        let store = LocalBlobStore::new(scratch("distinct"));
        store.prepare().await.unwrap();

        let a = store.put(b"invoice A").await.unwrap();
        let b = store.put(b"invoice B").await.unwrap();

        assert_ne!(a, b);
        assert_eq!(store.get(&a).await.unwrap(), b"invoice A");
        assert_eq!(store.get(&b).await.unwrap(), b"invoice B");
    }

    // BS-4: **本命。** 中身が書き換えられたら検証が false を返す。
    //
    //       真実性の担保がここに掛かっている。
    #[tokio::test]
    async fn tampering_with_the_stored_file_is_detected() {
        let root = scratch("tamper");
        let store = LocalBlobStore::new(&root);
        store.prepare().await.unwrap();
        let hash = store.put(b"original receipt").await.unwrap();
        assert!(store.verify(&hash).await.unwrap(), "書き換える前は通ること");

        // 保存されたファイルを直接書き換える。
        std::fs::write(root.join(hash.to_path()), b"tampered receipt!").unwrap();

        assert!(
            !store.verify(&hash).await.unwrap(),
            "書き換えを検出できていない"
        );
    }

    // BS-5: 保存されていないものは「無い」と言う。
    //
    //       「無い」と「変わっている」を混ぜない。
    #[tokio::test]
    async fn a_missing_blob_is_reported_as_missing_not_as_tampered() {
        let store = LocalBlobStore::new(scratch("missing"));
        store.prepare().await.unwrap();
        let hash = hash_of(b"never stored");

        assert!(!store.exists(&hash).await.unwrap());
        assert!(matches!(
            store.get(&hash).await,
            Err(BlobError::NotFound { .. })
        ));
        assert!(matches!(
            store.verify(&hash).await,
            Err(BlobError::NotFound { .. })
        ));
    }

    // BS-6: 保存先は先頭2文字で枝分かれする（1つの階層に集めない）。
    #[tokio::test]
    async fn blobs_are_sharded_into_subdirectories() {
        let root = scratch("shard");
        let store = LocalBlobStore::new(&root);
        store.prepare().await.unwrap();

        let hash = store.put(b"anything").await.unwrap();

        let hex = hash.to_hex();
        assert!(root.join(&hex[0..2]).join(&hex).exists(), "{hex}");
    }

    // BS-7: 書き終わったら一時領域に何も残らない。
    #[tokio::test]
    async fn nothing_is_left_in_the_temporary_area() {
        let root = scratch("tmp-clean");
        let store = LocalBlobStore::new(&root);
        store.prepare().await.unwrap();

        store.put(b"one").await.unwrap();
        store.put(b"two").await.unwrap();

        let left: Vec<_> = std::fs::read_dir(root.join(TEMP_DIR))
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        assert!(left.is_empty(), "書きかけが残っている: {} 件", left.len());
    }

    // BS-8: 空のファイルも保存できる（0バイトの証憑を特別扱いしない）。
    #[tokio::test]
    async fn an_empty_blob_is_stored_like_any_other() {
        let store = LocalBlobStore::new(scratch("empty"));
        store.prepare().await.unwrap();

        let hash = store.put(b"").await.unwrap();

        assert!(store.exists(&hash).await.unwrap());
        assert_eq!(store.get(&hash).await.unwrap(), b"");
        assert!(store.verify(&hash).await.unwrap());
    }

    // BS-9: ハッシュは SHA-256 の既知の値と一致する。
    //
    //       自前の実装と突き合わせず、外から確かめられる値で固定する。
    #[test]
    fn the_hash_matches_the_known_sha256_of_the_empty_input() {
        // 空文字列の SHA-256（広く知られた値）。
        assert_eq!(
            hash_of(b"").to_hex(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        // "abc" の SHA-256。
        assert_eq!(
            hash_of(b"abc").to_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    fn count_stored(root: &Path) -> usize {
        let mut total = 0;
        let Ok(entries) = std::fs::read_dir(root) else {
            return 0;
        };
        for entry in entries.filter_map(Result::ok) {
            if entry.file_name() == TEMP_DIR {
                continue;
            }
            if entry.path().is_dir() {
                total += std::fs::read_dir(entry.path())
                    .map(|inner| inner.filter_map(Result::ok).count())
                    .unwrap_or(0);
            }
        }
        total
    }
}
