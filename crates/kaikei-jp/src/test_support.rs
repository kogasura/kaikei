//! テスト専用の共通ヘルパ（`#[cfg(test)]` 配下でのみコンパイルされる）。
//!
//! ここに置くのは**どのモジュールから見ても同じもの**だけにする。
//! 税区分マスタの fixture や科目コードの生成のような**ドメイン固有の
//! ヘルパは各モジュールの `#[cfg(test)] mod tests` に置いたまま**にする
//! （そちらは「そのテストが何を前提にしているか」を読む手がかりであり、
//! 共通化すると却って読みにくくなる）。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// スコープを抜けたら必ず消える一時ファイル。
///
/// 素朴に「書く → 使う → `remove_file`」と並べると、途中の `unwrap()` が
/// panic した時点で削除に到達せず、一時ディレクトリにゴミが残る。
pub(crate) struct TempFile(PathBuf);

impl TempFile {
    pub(crate) fn with_contents(contents: &str) -> Self {
        // プロセスIDだけだと、同一プロセス内で並列に走る別テストと衝突しうる。
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("kaikei_jp_test_{}_{n}.yaml", std::process::id()));
        std::fs::write(&path, contents).expect("一時ファイルへの書き込みに失敗");
        TempFile(path)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
