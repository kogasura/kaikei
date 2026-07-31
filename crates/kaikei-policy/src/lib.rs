//! Phase 1: 可変部（税制・決算・様式・採番規則）の抽象。**trait 定義のみ。**
//!
//! `kaikei-jp`（Phase 2）が実装する。このクレート自身は実装を持たない
//! （`test-doubles` feature 配下のテスト用ダミーのみ例外）。
//!
//! # 依存方向と純関数の原則
//!
//! `kaikei-core` の型のみに依存する（`Cargo.toml` の依存は `kaikei-core` と
//! `thiserror` のみ）。I/O（DB・ファイル・ネットワーク）は一切行わない。
//! 全 trait メソッドは同期の純関数として定義する（`async fn` にしない・
//! `async_trait` を使わない。`CLAUDE.md` §3 / `DECISIONS.md` D-005）。
//! 必要なデータは呼び出し側（`kaikei-app`）が事前にロードし、[`TaxContext`]
//! のような引数に詰めて渡す。
//!
//! # この crate が定義する5つの trait
//!
//! これが「変わる部分」の全リストであり、ここに現れないものは不変層
//! （`kaikei-core`）に置いてよい。新しい国・事業形態を追加するときは
//! これらを実装するだけでよい（`docs/04-jp-tax.md` §2）。
//!
//! - [`TaxPolicy`] — 税額計算と税区分の妥当性
//! - [`ClosingPolicy`] — 決算振替仕訳の生成。**現状 [`TaxContext`] を取らない**
//!   （`TrialBalance` の行が既に科目種別を持つため Phase 1 では不要と判断した）。
//!   将来 `kaikei-jp` の実装で `TaxContext` 相当の情報が必要になった場合、
//!   `closing_entries` / `opening_entries` にその引数を足すのはシグネチャ変更
//!   ＝破壊的変更であることに注意
//! - [`StatementPolicy`] — 財務諸表の様式
//! - [`EntryValidator`] — 追加検証
//! - [`Numbering`] — 仕訳番号の採番規則
//!
//! # ここに置かない2つの trait
//!
//! - **`FxPolicy` は定義しない。** 外貨は `Currency` として型だけ用意し、
//!   換算ポリシーは Phase 後半で導入する（`DECISIONS.md` D-016）。
//! - **`ChartPolicy` は作らない。** 勘定科目体系（`ChartOfAccounts`）は
//!   ユーザーが YAML で自由に定義・編集する**データ**であり、「税制ごとに
//!   変わるロジック」ではない（②層＝データ。`ARCHITECTURE.md` §9 R6）。

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod closing;
mod context;
mod counterparty;
mod error;
mod note;
mod numbering;
mod proposal;
mod statement;
mod tax;
mod validation;

#[cfg(feature = "test-doubles")]
pub mod testing;

pub use closing::*;
pub use context::*;
pub use counterparty::*;
pub use error::*;
pub use note::*;
pub use numbering::*;
pub use proposal::*;
pub use statement::*;
pub use tax::*;
pub use validation::*;
