//! 仕訳IDの生成と `uuid::Uuid` との相互変換。
//!
//! `kaikei_core::EntryId` は `u128` の意味を core が規定しない（生成方法は
//! 外部から渡される）。UUID v7 として生成する規約と、DB の `UUID` 型と行き来
//! する変換をここに固定し、実装が複数箇所に散らばらないようにする。

use crate::error::AppError;
use crate::ports::IdGenerator;
use kaikei_core::EntryId;
use uuid::Uuid;

/// [`entry_id_from_uuid_string`] がエラーに載せる入力文字列の上限（文字数）。
///
/// 仕訳IDの正準表記は36文字なので、これを超える入力は既に誤りである。
/// 呼び出し元が送ってきた任意長の文字列をそのままエラー本文に載せると、
/// 応答と `audit_log` が入力次第でいくらでも膨らむ。
const MAX_ECHOED_INPUT_CHARS: usize = 64;

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

/// **UUID 文字列 → 仕訳ID**。[`entry_id_to_uuid_string`] の逆向き。
///
/// 呼び出し元（`kaikei-mcp` の `reverse_journal_entry` / `get_entry` 等）が
/// 受け取る `original_id` / `entry_id` は JSON の文字列であり、
/// `EntryId` にするには必ずどこかでパースが要る。その入口をここに1つ置く。
///
/// **下流が `uuid::Uuid::parse_str` を直に書かないこと。** `kaikei-app` は
/// `uuid` を再エクスポートしていないため、直に書くと下流が自分の
/// `Cargo.toml` に `uuid` を足してバージョンを合わせる羽目になり、
/// 「`kaikei-app` の関数を呼びたいだけなのに別の crate にも依存する」
/// という `DECISIONS.md` D-047 が潰したのと同じ状態が再発する。
///
/// # 受理する表記
///
/// `uuid::Uuid::parse_str` が受理する形（ハイフン付き36文字、ハイフン無し
/// 32文字、波括弧付き、`urn:uuid:` 付き）をそのまま受理する。
/// **出力は常にハイフン付きの正準表記に揃う**ので、入力の揺れが応答に
/// 伝播することはない。バージョン（v7 かどうか）は検証しない
/// （`EntryId` の生成規則は将来変わりうるが、既に保存済みのIDは引けねばならない）。
///
/// # Errors
///
/// UUID として解釈できない場合は [`AppError::InvalidEntryId`]
/// （コード `invalid_entry_id`）。**`RepoError::NotFound` にしない。**
/// 「そのIDの仕訳が無い」（IDを調べ直す）と「文字列が UUID ですらない」
/// （表記を直す）は AI が取るべき次の手が違う。
pub fn entry_id_from_uuid_string(text: &str) -> Result<EntryId, AppError> {
    Uuid::parse_str(text)
        .map(entry_id_from_uuid)
        .map_err(|_| AppError::InvalidEntryId {
            input: truncate_for_message(text),
        })
}

/// エラー本文に載せる入力文字列を [`MAX_ECHOED_INPUT_CHARS`] 文字までに切り詰める。
///
/// `char` 単位で数える（バイト境界で切ると UTF-8 が壊れる）。
fn truncate_for_message(text: &str) -> String {
    if text.chars().count() <= MAX_ECHOED_INPUT_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(MAX_ECHOED_INPUT_CHARS).collect();
    format!("{head}…")
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

    // ID-6（PR-B 2巡目）: 文字列 → EntryId → 文字列 のラウンドトリップ。
    #[test]
    fn entry_id_from_uuid_string_round_trips_with_entry_id_to_uuid_string() {
        let canonical = "0192a7b3-1234-7abc-8def-0123456789ab";
        let id = entry_id_from_uuid_string(canonical).unwrap();
        assert_eq!(entry_id_to_uuid_string(id), canonical);

        let generated = new_entry_id();
        let text = entry_id_to_uuid_string(generated);
        assert_eq!(
            entry_id_from_uuid_string(&text).unwrap().as_u128(),
            generated.as_u128()
        );
    }

    // ID-7: 入力表記が揺れても出力は正準表記に揃う。
    #[test]
    fn entry_id_from_uuid_string_normalizes_accepted_spellings() {
        let canonical = "0192a7b3-1234-7abc-8def-0123456789ab";
        for input in [
            canonical,
            "0192A7B3-1234-7ABC-8DEF-0123456789AB",
            "0192a7b312347abc8def0123456789ab",
            "{0192a7b3-1234-7abc-8def-0123456789ab}",
            "urn:uuid:0192a7b3-1234-7abc-8def-0123456789ab",
        ] {
            let id = entry_id_from_uuid_string(input)
                .unwrap_or_else(|err| panic!("\"{input}\" を受理できない: {err}"));
            assert_eq!(entry_id_to_uuid_string(id), canonical, "input={input}");
        }
    }

    // ID-8: UUID ですらない入力は NotFound ではなく InvalidEntryId になる。
    #[test]
    fn entry_id_from_uuid_string_rejects_non_uuid_input_with_a_dedicated_error() {
        for input in ["", "42", "0192a7b3", "not-a-uuid", "１９２"] {
            let err = entry_id_from_uuid_string(input).unwrap_err();
            assert_eq!(
                err.code(),
                crate::error::codes::INVALID_ENTRY_ID,
                "input={input}"
            );
            // 次の手（正しい表記の例）が本文に含まれる（`CLAUDE.md` §11）。
            assert!(err.to_string().contains("UUID"), "{err}");
        }
    }

    // ID-9: 長すぎる入力はエラー本文に丸ごと載らない（応答と audit_log の肥大を防ぐ）。
    #[test]
    fn entry_id_from_uuid_string_truncates_an_overlong_input_in_the_message() {
        let long = "あ".repeat(1_000);
        let err = entry_id_from_uuid_string(&long).unwrap_err();
        let message = err.to_string();
        assert!(message.chars().count() < 300, "本文が長すぎる: {message}");
        assert!(message.contains('…'), "切り詰めたことが分かる印が無い");
    }

    #[test]
    fn uuid_v7_id_generator_implements_id_generator() {
        let generator = UuidV7IdGenerator;
        let a = generator.new_entry_id();
        let b = generator.new_entry_id();
        assert_ne!(a.as_u128(), b.as_u128());
    }
}
