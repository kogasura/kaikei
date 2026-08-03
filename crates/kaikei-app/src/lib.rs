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
//! - **線上（応答 JSON）に出る表現**の唯一の置き場:
//!   [`error::codes`]（エラーの分類コード）/ [`wire`]（列挙型の機械可読名）/
//!   [`amount`]（金額の区切り無し文字列）/ [`id::entry_id_to_uuid_string`] と
//!   [`id::entry_id_from_uuid_string`]（仕訳IDの UUID 表記）。
//!   presentation 層（`kaikei-mcp` / 将来の `kaikei-api`）と
//!   `audit_log` の3箇所で同じ表を手書きすると必ず綴りがずれるため、
//!   ここに1箇所だけ持つ（`DECISIONS.md` D-072）
//! - read model 用の DTO: [`view::BalanceRowView`] / [`view::TrialBalanceView`]
//! - 監査ログ: [`ports::AuditSink`]（ポート）と [`audit`]（記録する値と、
//!   fail-closed / fail-open の手順 [`audit::with_audit`]）。
//!   **帳簿とは別のコネクションで2回書く**（`DECISIONS.md` D-070）
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
//! `kaikei-store` 等の実装者・呼び出し側は `kaikei_policy::` を直接 `use`
//! せず、必ずこの crate 経由で参照すること（`kaikei-store` から
//! `kaikei-policy` への直接依存は CI が禁じている。
//! `.github/workflows/architecture.yml` の「kaikei-store は kaikei-jp /
//! kaikei-policy に依存しない」ステップを参照）。再エクスポートが足りないと、
//! 呼び出し側は「`kaikei-app` の関数を呼びたいだけなのに `kaikei-policy` にも
//! 依存しなければならない」状態になる（`DECISIONS.md` D-047）。
//!
//! 参照経路は2つある。
//!
//! ## 1. クレートルート（公開シグネチャに直接現れる型）
//!
//! | 型 | 現れる場所 |
//! |---|---|
//! | [`Counterparty`] / [`CounterpartyIndex`] | [`ports::ChartRepo::load_counterparties`] の戻り値 |
//! | [`PolicyError`] | [`error::AppError::Policy`] の中身 |
//! | [`TaxPolicy`] | [`usecase::post_entry::execute`] の引数（`&dyn TaxPolicy`） |
//! | [`TaxContext`] / [`TaxDerivation`] | [`TaxPolicy`] を実装するために必要 |
//! | [`PolicyNote`] / [`NoteSeverity`] | [`TaxDerivation::notes`] の要素型とそのフィールド型 |
//!
//! ## 2. [`policy`] モジュール（`kaikei-policy` の公開型すべて）
//!
//! 上の表は**手で維持している以上いずれ漏れる**。実際、D-047 を追加した
//! コミット自身が `PolicyNote` / `NoteSeverity`（[`TaxDerivation::notes`] の
//! 要素型）を落としており、レビューで実際のコンパイルエラーとして検出された。
//! 表に載っていない policy の型が必要になったら [`policy`] から取れば、
//! 表の更新漏れがそのまま「下位層が `kaikei-policy` に依存せざるを得ない」
//! 状態に化けることはない。
//!
//! ルート側の再エクスポートを残すのは、**どの型がこの crate の契約の一部か**
//! を読み手に示すため（`policy` からすべて取れるからといって、
//! `ClosingPolicy` 等が `kaikei-app` の契約に含まれるわけではない）。

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod amount;
pub mod audit;
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
pub mod wire;

pub use kaikei_policy::{
    Counterparty, CounterpartyIndex, NoteSeverity, PolicyError, PolicyNote, TaxContext,
    TaxDerivation, TaxPolicy,
};

/// `kaikei-policy` の公開型すべてへの経路。
///
/// クレートルートの再エクスポート（この crate の公開シグネチャに直接現れる型）
/// から漏れた型が必要になったときの受け皿。詳細はクレート doc の
/// 「`kaikei-policy` 型の再エクスポート」を参照。
///
/// `kaikei-app` 自身の型と名前が衝突しないよう、ルートに glob で撒くのではなく
/// このモジュールに閉じている（衝突は glob 側が黙って負けるため、
/// 気づかないうちに別の型を指す事故になりうる）。
pub mod policy {
    pub use kaikei_policy::*;
}
