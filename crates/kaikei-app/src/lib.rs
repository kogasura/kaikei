//! Phase 1: application 層。ユースケースとポート（trait）定義。I/O を行うのは
//! この層だけ。
//!
//! # この crate が持つもの
//!
//! - ポート（trait）: [`ports`] — domain が要求する穴（[`ports::Store`] /
//!   [`ports::JournalRepo`] 等）。★契約凍結点（PR-4）★
//! - トランザクションの起動・確定・破棄を1箇所に閉じるヘルパ: [`tx::with_tx`]
//! - ユースケース: [`usecase`] — 1ユースケース = 1ファイル = 1関数
//!   （`CLAUDE.md` §6）。具体的な実装は後続の PR で追加する
//! - DB 等の infra 依存を必要としない具象実装: [`clock::SystemClock`] /
//!   [`period_guard::ClosedPeriodGuard`] / [`id`] の関数群 / [`currency`] の
//!   関数群
//! - read model 用の DTO: [`view::BalanceRowView`] / [`view::TrialBalanceView`]
//! - テスト用のインメモリ fake: [`testing`]（`#[cfg(any(test, feature =
//!   "testing"))]`）
//!
//! # 依存方向
//!
//! `kaikei-core` と `kaikei-policy`（trait のみ）に依存する。`kaikei-jp` は
//! 実装として注入される側であり、この crate 自身は知らない。`sqlx` /
//! `kaikei-store` / `kaikei-jp` / `kaikei-import` への依存は CI
//! （`.github/workflows/architecture.yml`）が検査し、混入したら失敗する
//! （`CLAUDE.md` §1）。
//!
//! # `kaikei-policy` 型の再エクスポート
//!
//! この crate の**公開シグネチャに現れる** `kaikei-policy` の型は、すべて
//! ここで再エクスポートする。`kaikei-store` 等の実装者・呼び出し側は
//! `kaikei_policy::` を直接 `use` せず、必ずこの再エクスポート経由で参照する
//! こと（`kaikei-store` から `kaikei-policy` への直接依存は CI が禁じている。
//! `.github/workflows/architecture.yml` の「kaikei-store は kaikei-jp /
//! kaikei-policy に依存しない」ステップを参照）。
//!
//! | 型 | 現れる場所 |
//! |---|---|
//! | [`Counterparty`] / [`CounterpartyIndex`] | [`ports::ChartRepo::load_counterparties`] の戻り値 |
//! | [`PolicyError`] | [`error::AppError::Policy`] の中身 |
//! | [`TaxPolicy`] | [`usecase::post_entry::execute`] の引数（`&dyn TaxPolicy`） |
//! | [`TaxContext`] / [`TaxDerivation`] | [`TaxPolicy`] を実装するために必要 |
//!
//! 再エクスポートが足りないと、呼び出し側は「`kaikei-app` の関数を呼びたい
//! だけなのに `kaikei-policy` にも依存しなければならない」状態になる
//! （`DECISIONS.md` D-047）。新しい policy 型を公開シグネチャに出すときは、
//! この表と `pub use` の両方に足すこと。

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod clock;
pub mod context;
pub mod currency;
pub mod error;
pub mod id;
pub mod period_guard;
pub mod ports;
#[cfg(test)]
mod test_support;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod tx;
pub mod usecase;
pub mod view;

pub use kaikei_policy::{
    Counterparty, CounterpartyIndex, PolicyError, TaxContext, TaxDerivation, TaxPolicy,
};
