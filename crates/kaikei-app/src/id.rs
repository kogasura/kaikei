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

/// 仕訳IDを **UUID の正準表記**（小文字ハイフン付き36文字）の文字列にする。
///
/// 仕訳IDを人間・AI に見せる場所（エラーメッセージ、MCP / API の応答、
/// `audit_log.entry_id`）は**必ずこの表記に揃える**
/// （`docs/07-mcp-server.md` §3）。
///
/// `EntryId::as_u128()` の10進表記（最大39桁）を使ってはならない。
/// AI が送ってきた UUID 文字列と突き合わせられず、「見つからない仕訳ID」を
/// 提示されても次に何をすればよいか分からなくなる（`CLAUDE.md` §11）。
/// この関数を1箇所に置くのは、その表記が実装のあちこちで再発明されないため。
pub fn entry_id_to_uuid_string(id: EntryId) -> String {
    entry_id_to_uuid(id).to_string()
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
    fn entry_id_to_uuid_string_uses_the_canonical_hyphenated_form() {
        let uuid = Uuid::parse_str("0192a7b3-1234-7abc-8def-0123456789ab").unwrap();
        let id = entry_id_from_uuid(uuid);
        let text = entry_id_to_uuid_string(id);

        assert_eq!(text, "0192a7b3-1234-7abc-8def-0123456789ab");
        assert_eq!(text.len(), 36);
        assert_eq!(text.matches('-').count(), 4);
        // 10進表記（39桁になりうる）ではないこと。
        assert!(text.contains('-'));
        assert_ne!(text, id.as_u128().to_string());
    }

    #[test]
    fn entry_id_to_uuid_string_round_trips_through_parse() {
        let id = new_entry_id();
        let parsed = Uuid::parse_str(&entry_id_to_uuid_string(id)).unwrap();
        assert_eq!(entry_id_from_uuid(parsed).as_u128(), id.as_u128());
    }

    #[test]
    fn uuid_v7_id_generator_implements_id_generator() {
        let generator = UuidV7IdGenerator;
        let a = generator.new_entry_id();
        let b = generator.new_entry_id();
        assert_ne!(a.as_u128(), b.as_u128());
    }
}
