//! Phase 2: 日本・個人事業主アダプタ。`kaikei-policy` の trait を実装する層。
//!
//! # この PR（PR-2）のスコープ
//!
//! PR-1（ワークスペース組込みと YAML ローダの共通基盤）に加えて、
//! インボイス登録番号の形式検証を追加する。
//!
//! - [`yaml`]: YAML 文字列 → `T: DeserializeOwned` の共通ローダ
//!   （姉妹 crate `kaikei-jp-data` の再エクスポートではない。名前が紛らわしい
//!   ため `data` ではなく `yaml` にしてある）
//!   （埋め込みデータ / 任意パスの両方に対応）
//! - [`error`][]: [`error::JpError`]（`thiserror`）
//! - [`invoice`]: [`invoice::InvoiceRegistrationNo`]（`T` + 13桁の形式検証・
//!   チェックデジット検証。実在確認・適格性の判定は行わない。
//!   `docs/04-jp-tax.md` §6・`docs/08-compliance.md` §6）
//!
//! 税区分マスタの解釈（PR-3）・`JpTaxPolicy`（`kaikei-policy::TaxPolicy` の実装。
//! PR-4）・科目表 / `TagSchema` のロード（PR-5）は、いずれもこの PR では
//! 実装しない。
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

pub mod error;
pub mod invoice;
pub mod yaml;
