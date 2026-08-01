//! テスト専用の共通ヘルパ（`#[cfg(test)]` 配下でのみコンパイルされる）。
//!
//! ここに置くのは**どのモジュールから見ても同じもの**だけにする。
//! 税区分マスタの fixture や科目コードの生成のような**ドメイン固有の
//! ヘルパは各モジュールの `#[cfg(test)] mod tests` に置いたまま**にする
//! （そちらは「そのテストが何を前提にしているか」を読む手がかりであり、
//! 共通化すると却って読みにくくなる）。

use kaikei_core::{
    AccountingDate, ChartOfAccounts, EntryId, EntryNumber, FiscalYear, JournalEntry, JournalLine,
    NewEntry, PeriodGuard, PeriodStatus, TagSchema,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// 常に Open を返す `PeriodGuard`。締め状態を検証しないテストに使う
/// （`kaikei-app/src/test_support.rs` の同名の fake と同じ役割）。
pub(crate) struct AllOpen;

impl PeriodGuard for AllOpen {
    fn status(&self, _date: AccountingDate) -> PeriodStatus {
        PeriodStatus::Open
    }
}

/// `TrialBalance` を組み立てるための `JournalEntry` を、`JournalEntry::new`
/// （不変条件の全検証あり）を経由して作る。
///
/// `closing`/`statement` のテストが `TrialBalance::from_entries` に渡す
/// フィクスチャを組み立てる際に共通で使う。`kaikei-jp` から `rehydrate`
/// （無検証の復元専用API）を呼ぶことは CI（`architecture.yml`）が禁じているため、
/// テストであっても正規の生成経路（`JournalEntry::new`）を通す。
///
/// `id` / `entry_no` は呼び出し側が指定する（同一テスト内で複数の仕訳を
/// 作る場合に重複しないよう呼び出し側が管理すること）。
#[allow(clippy::too_many_arguments)]
pub(crate) fn new_entry(
    id: u128,
    entry_no: u32,
    fy: &FiscalYear,
    chart: &ChartOfAccounts,
    schema: &TagSchema,
    date: AccountingDate,
    description: &str,
    lines: Vec<JournalLine>,
) -> JournalEntry {
    JournalEntry::new(
        NewEntry {
            id: EntryId::new(id),
            entry_no: EntryNumber::new(entry_no),
            entry_date: date,
            description: description.to_string(),
            lines,
            document_refs: Vec::new(),
        },
        fy,
        chart,
        schema,
        &AllOpen,
        &kaikei_core::FixedClock(kaikei_core::Timestamp::from_unix_nanos(0)),
    )
    .expect("テスト用仕訳の構築に失敗しました（テストの前提が壊れています）")
}

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
