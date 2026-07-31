//! ユースケース（`post_entry` / `reverse_entry` / `report`）。
//!
//! 各ユースケースは「1ファイル = 1関数」の原則に従う（`CLAUDE.md` §6）。
//! `AccountingService` のような巨大構造体は作らない。
//!
//! 具体的な実装（`usecase/post_entry.rs` 等）はポート層を凍結する本 PR の
//! 対象外であり、後続の PR で追加される。ここではモジュールとしての置き場を
//! 用意するのみ。
