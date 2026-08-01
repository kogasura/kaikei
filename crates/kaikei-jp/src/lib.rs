//! Phase 2: 日本・個人事業主アダプタ。`kaikei-policy` の trait を実装する層。
//!
//! # モジュール構成
//!
//! - [`yaml`][]: YAML 文字列 → `T: DeserializeOwned` の共通ローダ
//!   （姉妹 crate `kaikei-jp-data` の再エクスポートではない。名前が紛らわしい
//!   ため `data` ではなく `yaml` にしてある）
//!   （埋め込みデータ / 任意パスの両方に対応。PR-1）
//! - [`error`][]: [`error::JpError`]（`thiserror`。PR-1）
//! - [`invoice`][]: [`invoice::InvoiceRegistrationNo`]（`T` + 13桁の形式検証・
//!   チェックデジット検証。実在確認・適格性の判定は行わない。
//!   `docs/04-jp-tax.md` §6・`docs/08-compliance.md` §6。PR-2）
//! - [`tax`][]: 税区分マスタ（[`tax::TaxCategoryTable`]）、取引日による
//!   適用マスタの選択（[`tax::TaxRuleSets`]。PR-3）、および
//!   `kaikei-policy::TaxPolicy` の実装（[`tax::JpTaxPolicy`] /
//!   [`tax::JpSettings`]。PR-4）
//! - [`chart`][]: 勘定科目テンプレート（`kaikei-jp-data/chart/*.yaml`）→
//!   `kaikei_core::ChartOfAccounts`（`docs/04-jp-tax.md` §5。PR-5）
//! - [`tags`][]: タグスキーマ（`kaikei-jp-data/tags.yaml`）→
//!   `kaikei_core::TagSchema`（`docs/04-jp-tax.md` §4。PR-5）
//!
//! # 依存方向
//!
//! `kaikei-core` / `kaikei-policy` / `kaikei-jp-data` にのみ依存する。
//! `sqlx` / `tokio` / `kaikei-app` / `kaikei-store` / `kaikei-import` には
//! 依存しない（`CLAUDE.md` §1、CI `.github/workflows/architecture.yml` の
//! 「kaikei-jp は infra を知らない」ステップが機械的に検査する）。
//!
//! `kaikei-policy` と異なり、`kaikei-jp` は I/O を禁止されていない
//! （`docs/04-jp-tax.md` §2「YAML の読み込みは合成ルートの起動時 I/O」）。
//! ただし `TaxPolicy` 等の trait メソッド自体は純関数を保つ必要があるため、
//! YAML のロードは構築時に済ませ、各メソッド内で読み直さないこと
//! （`DECISIONS.md` D-025）。
//!
//! # 免責
//!
//! 本 crate は日本の税制に対応した処理を提供しますが、税務上の正しさを
//! 保証しません。税区分の判定、経費性の判断、控除の適用可否は税理士等の
//! 専門家に確認してください。本 crate は電子帳簿保存法の機能要件を意識した
//! 設計ですが、JIIMA 認証を取得しておらず、運用要件（事務処理規程の備付け等）
//! は利用者の責任です（`docs/04-jp-tax.md` §11）。

#![forbid(unsafe_code)]
#![warn(missing_docs)]

// `mod` の並びは `cargo fmt`（`reorder_modules`）がアルファベット順に強制する。
// 読み手向けの解説順（土台 → その上に乗るもの）は上の「モジュール構成」に譲り、
// ここでは fmt に任せる。
pub mod chart;
pub mod error;
pub mod invoice;
pub mod tags;
pub mod tax;
pub mod yaml;
