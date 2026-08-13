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
//!   `kaikei_core::TagSchema`（`docs/04-jp-tax.md` §4。PR-5）、および
//!   線上（JSON）の文字列 → `kaikei_core::TagSet` の変換
//!   （[`tags::TagCatalog`]。Phase 3 PR-B）
//! - [`household_split`][]: 家事按分ヘルパー（[`household_split::household_split`]）。
//!   `kaikei-policy::TaxPolicy` の実装ではなく単独の関数（`docs/04-jp-tax.md`
//!   §8。PR-6）
//! - [`closing`][]: 決算振替仕訳（[`closing::JpSoleProprietorClosingPolicy`]。
//!   `kaikei-policy::ClosingPolicy` の実装。`docs/04-jp-tax.md` §9。PR-7）
//! - [`statement`][]: 財務諸表の様式（[`statement::JpStatementPolicy`]。
//!   `kaikei-policy::StatementPolicy` の実装。`docs/04-jp-tax.md` §9・§10。PR-7）
//! - [`compose`][]: 合成ルートが起動時に一度だけ行う組み立て（YAMLロード →
//!   policy 構築。[`compose::compose`] / [`compose::Composition`]）。
//!   Phase 3 の `kaikei-mcp` と Phase 4 の `kaikei-api` はいずれもここを入口に
//!   する（`DECISIONS.md` D-068、`docs/07-mcp-server.md` §4。PR-23）
//!
//! # `JpStatementPolicy` の `chart` について（`DECISIONS.md` D-069）
//!
//! [`compose::compose`] が返す [`compose::Composition`] は
//! [`statement::JpStatementPolicy`] を**含まない**。
//!
//! [`tax::JpTaxPolicy`]（年度別マスタ）や
//! [`closing::JpSoleProprietorClosingPolicy`]（決算科目3つの実在検証）が
//! 保持するデータは YAML 由来で、変更するにはプロセス再起動が要る
//! （`DECISIONS.md` D-025/D-057/D-066）。これらは起動時に一度組み立てて
//! 長期保持するのが自然である。
//!
//! 一方 `JpStatementPolicy` が保持する `chart` は**DBから読み直される可変
//! データ**であり、`kaikei-app/src/context.rs` の `load_posting_context` が
//! 記帳のたびに `tx.load_chart()` で読み直しているのと同じ性質を持つ
//! （ユーザーが科目名を編集する経路が存在する）。`JpStatementPolicy` を
//! 起動時に一度だけ構築して長期保持すると、「科目名を変更したのに決算書には
//! 古い名前が表示される」というバグになりうる。
//!
//! `JpStatementPolicy::new` はYAML解釈や構築時検証を一切行わない単純な
//! ラッパ（`ChartOfAccounts` を保持するだけ）であり、構築コストは無視できる。
//! そのため方針は**「決算書（BS/PL）を組み立てる直前に、その時点で読み込んだ
//! `chart` から都度 `JpStatementPolicy::new(chart)` する」**とし、`compose` の
//! 戻り値には含めない。合成ルートは決算書生成のリクエストのたびに `chart` を
//! 読み直してから構築すること。
//!
//! # ローダの命名規約
//!
//! YAML を読む入口は2種類あり、**名前でどちらかが分かる**ようにしてある。
//!
//! | 形 | 命名 | 例 |
//! |---|---|---|
//! | 自由関数（返す型が `kaikei-core` のもので、孤児則によりメソッドを生やせない） | `load_*` | [`chart::load_embedded`] / [`tags::load_from_path`] |
//! | 関連関数（返す型が `kaikei-jp` 自身のもの＝コンストラクタ） | `Type::from_*` | [`tax::TaxCategoryTable::from_embedded`] |
//!
//! `load_` と `from_` が混在しているのは統一漏れではなく、
//! **`Type::from_x` が Rust の慣用的なコンストラクタ名**であるのに対し、
//! 自由関数側は「何を返すか」が名前から分からないため動詞を残している、という区別。
//! 新しいローダを足すときはこの表に従うこと。
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
mod account_type;
pub mod blue_return;
pub mod chart;
pub mod closing;
pub mod compose;
pub mod error;
pub mod household_split;
pub mod invoice;
pub mod statement;
pub mod tags;
pub mod tax;
#[cfg(test)]
mod test_support;
pub mod yaml;
