//! 統合テスト（`tests/*.rs`）から `kaikei_app::testing` の fake が実際に
//! 見えることを確認する回帰テスト。
//!
//! `tests/*.rs` は crate を外部依存としてリンクするため、`src/lib.rs` 側の
//! `#[cfg(test)]` は有効にならない（`testing` feature を明示しないと
//! `unresolved import` になる。PR-4 レビューで実測確認済み）。
//! `Cargo.toml` の `[dev-dependencies]` に `kaikei-app` 自身への
//! `features = ["testing"]` を付けた自己依存を追加することでこれを解決した。
//! 後続 PR（PR-7 等）が `tests/{post_entry,reverse_entry,report}.rs` を
//! 追加した際に同じ穴を再度踏まないための最小限の回帰テストとして、
//! ここに置く。

use kaikei_app::testing::{InMemoryStore, SequentialIdGenerator};

#[tokio::test]
async fn in_memory_store_is_usable_from_an_integration_test() {
    let store = InMemoryStore::new();
    assert!(store.committed_entries().is_empty());
}

#[test]
fn sequential_id_generator_is_usable_from_an_integration_test() {
    use kaikei_app::ports::IdGenerator;

    let generator = SequentialIdGenerator::starting_at(1);
    assert_eq!(generator.new_entry_id().as_u128(), 1);
}
