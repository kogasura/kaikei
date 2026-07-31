//! 記帳時刻の取得。実時刻（`SystemTime::now()` 相当）を呼んでよいのは
//! `kaikei-app` の中でもここだけ（`CLAUDE.md` §7）。

use kaikei_core::{Clock, Timestamp};
use std::time::{SystemTime, UNIX_EPOCH};

/// 実時刻を返す `Clock` 実装。
///
/// `journal_entries.recorded_at` は `TIMESTAMPTZ`（マイクロ秒精度）で保存される
/// ため、生成した時点でマイクロ秒に丸めておく（`DECISIONS.md` D-030）。丸めずに
/// ナノ秒のまま生成すると、保存して読み戻した値がナノ秒未満の端数だけ元の値と
/// 食い違い、往復同値性のテスト（save → find）が必ず失敗する。
///
/// `Timestamp` → `AccountingDate` の変換関数はここに置かない。タイムゾーンの
/// 決定が必要であり、「今日」が何日かの判断は presentation 層の責務
/// （`CLAUDE.md` §7）。
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        let elapsed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("システム時刻が1970-01-01以降であること");
        let nanos = elapsed.as_nanos() as i128;
        Timestamp::from_unix_nanos(nanos / 1_000 * 1_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_clock_now_is_rounded_to_microseconds() {
        let ts = SystemClock.now();
        assert_eq!(
            ts.as_unix_nanos() % 1_000,
            0,
            "SystemClock はマイクロ秒未満の端数を切り捨てて返すべき"
        );
    }

    #[test]
    fn system_clock_now_is_after_unix_epoch() {
        let ts = SystemClock.now();
        assert!(ts.as_unix_nanos() > 0);
    }
}
