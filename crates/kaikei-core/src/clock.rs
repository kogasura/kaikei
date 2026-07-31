//! 記帳時刻の取得。
//!
//! `chrono` は core の依存に含めない（`CLAUDE.md` §7）。UTC の Unix 時刻
//! （ナノ秒）を自前の `Timestamp` で表現し、現在時刻の取得は `Clock` trait
//! 経由で注入する。core / policy 内で実時刻を直接取得しない。

/// UTC の Unix 時刻（ナノ秒）。
///
/// 1970-01-01T00:00:00Z からの経過ナノ秒。1970年より前は負値で表す。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(i128);

impl Timestamp {
    /// Unix 時刻（ナノ秒）から構築する。
    pub fn from_unix_nanos(nanos: i128) -> Self {
        Timestamp(nanos)
    }

    /// Unix 時刻（ナノ秒）を返す。
    pub fn as_unix_nanos(&self) -> i128 {
        self.0
    }
}

/// 記帳時刻の取得を抽象化する trait。
///
/// core / policy 内で `SystemTime::now()` 等の実時刻取得を直接呼ばない。
/// 実時刻を返す実装は上位層（`kaikei-app` 等）が提供する。
pub trait Clock {
    /// 現在時刻を返す。
    fn now(&self) -> Timestamp;
}

/// テスト用の固定時刻 `Clock`。常に構築時に渡した時刻を返す。
pub struct FixedClock(pub Timestamp);

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_from_unix_nanos_round_trips() {
        let ts = Timestamp::from_unix_nanos(1_700_000_000_000_000_000);
        assert_eq!(ts.as_unix_nanos(), 1_700_000_000_000_000_000);
    }

    #[test]
    fn timestamp_ordering_reflects_chronological_order() {
        let earlier = Timestamp::from_unix_nanos(100);
        let later = Timestamp::from_unix_nanos(200);
        assert!(earlier < later);
        assert!(later > earlier);
        assert_eq!(earlier, Timestamp::from_unix_nanos(100));
    }

    #[test]
    fn timestamp_supports_negative_values_before_epoch() {
        let ts = Timestamp::from_unix_nanos(-1_000_000_000);
        assert_eq!(ts.as_unix_nanos(), -1_000_000_000);
        assert!(ts < Timestamp::from_unix_nanos(0));
    }

    #[test]
    fn fixed_clock_always_returns_the_same_timestamp() {
        let ts = Timestamp::from_unix_nanos(42);
        let clock = FixedClock(ts);
        assert_eq!(clock.now(), ts);
        assert_eq!(clock.now(), ts);
    }

    #[test]
    fn fixed_clock_works_as_trait_object() {
        let ts = Timestamp::from_unix_nanos(-7);
        let clock = FixedClock(ts);
        let dyn_clock: &dyn Clock = &clock;
        assert_eq!(dyn_clock.now(), ts);
    }
}
