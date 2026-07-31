//! 仕訳IDの生成と `uuid::Uuid` との相互変換。
//!
//! `kaikei_core::EntryId` は `u128` の意味を core が規定しない（生成方法は
//! 外部から渡される）。UUID v7 として生成する規約と、DB の `UUID` 型と行き来
//! する変換をここに固定し、実装が複数箇所に散らばらないようにする。

use crate::ports::IdGenerator;
use kaikei_core::EntryId;
use uuid::Uuid;

/// 新しい仕訳IDを UUID v7 として生成する。
///
/// UUID v7 はビット列にタイムスタンプを埋め込むが、これは ID の一意性・
/// 生成順序の維持のためのものであり、`CLAUDE.md` §7 が求める「記帳時刻の
/// 取得は `Clock` 経由」の対象ではない（記帳時刻そのものは
/// `JournalEntry::recorded_at` として別途 [`crate::clock::SystemClock`] から
/// 取得される）。
pub fn new_entry_id() -> EntryId {
    entry_id_from_uuid(Uuid::now_v7())
}

/// 仕訳IDを `uuid::Uuid` に変換する。
pub fn entry_id_to_uuid(id: EntryId) -> Uuid {
    Uuid::from_u128(id.as_u128())
}

/// `uuid::Uuid` から仕訳IDを作る（DB からの復元用）。
pub fn entry_id_from_uuid(uuid: Uuid) -> EntryId {
    EntryId::new(uuid.as_u128())
}

/// 実行時に UUID v7 で仕訳IDを生成する [`IdGenerator`] 実装。
#[derive(Debug, Clone, Copy, Default)]
pub struct UuidV7IdGenerator;

impl IdGenerator for UuidV7IdGenerator {
    fn new_entry_id(&self) -> EntryId {
        new_entry_id()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_id_uuid_round_trip_preserves_value() {
        let uuid = Uuid::now_v7();
        let id = entry_id_from_uuid(uuid);
        assert_eq!(entry_id_to_uuid(id), uuid);
    }

    #[test]
    fn new_entry_id_generates_distinct_values() {
        let a = new_entry_id();
        let b = new_entry_id();
        assert_ne!(a.as_u128(), b.as_u128());
    }

    #[test]
    fn uuid_v7_id_generator_implements_id_generator() {
        let generator = UuidV7IdGenerator;
        let a = generator.new_entry_id();
        let b = generator.new_entry_id();
        assert_ne!(a.as_u128(), b.as_u128());
    }
}
