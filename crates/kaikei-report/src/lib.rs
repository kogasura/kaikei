//! 帳簿の出力（CSV / 印刷用 HTML）。
//!
//! # この crate は I/O を持たない
//!
//! 入力は既に手元にあるデータ（`Vec<JournalEntry>` / `TrialBalanceView` /
//! `Statement`）、出力は `String`。DB から取ってくるのは呼び出し側
//! （CLI バイナリ・`kaikei-mcp`）の仕事である（`docs/10-report.md` §3）。
//!
//! そのおかげでこの crate のテストは**DB を要らない**。
//! `.github/workflows/architecture.yml` が `sqlx` / `tokio` への依存を検査する。
//!
//! # 出力形式
//!
//! - **CSV**（[`csv`]）… ダウンロードの求めへの対応、他ソフトへの取り込み
//! - **印刷用 HTML**（今後）… 電子帳簿保存法の見読可能性（施行規則第2条第2項
//!   第2号「ディスプレイの画面**及び書面**に」）。PDF は作らない
//!   （日本語フォントの同梱・探索を抱えないため。`docs/10-report.md` §2-2）
//!
//! **このソフトウェアが法令要件を満たすと名乗ることはしない**（`CLAUDE.md` §10）。
//! 満たしうる形で出力するところまでを担う。

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod blue_return;
pub mod blue_return_bs;
pub mod blue_return_depreciation;
pub mod consumption_tax;
pub mod csv;
pub mod documents;
pub mod export;
pub mod html;
pub mod invoices_to_collect;
pub mod journal_book;
pub mod ledger;
pub mod monthly_sales;
pub mod statement;
pub mod trial_balance;
pub mod yayoi;
