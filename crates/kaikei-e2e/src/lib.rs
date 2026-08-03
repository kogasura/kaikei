//! `kaikei-e2e`: 合成ルートを模した層。**E2Eテスト専用crate。**
//!
//! # このcrateが存在する理由
//!
//! `kaikei-store` は `kaikei-jp` / `kaikei-policy` を知らない
//! （`CLAUDE.md` §1、`.github/workflows/architecture.yml` の
//! 「kaikei-store は kaikei-jp / kaikei-policy に依存しない」ステップが
//! 機械的に検査する）。逆に `kaikei-jp` も `kaikei-store`（DB・sqlx・tokio）を
//! 知らない（同ワークフローの「kaikei-jp は infra を知らない」ステップ）。
//!
//! つまり「税抜経理の消費税行が**実際にPostgreSQLへ記帳できる**」
//! 「`household_split` の3行仕訳が記帳できる」「決算振替仕訳が実際に記帳
//! できる」ことを検証するテストは、`kaikei-store` にも `kaikei-jp` にも
//! 置けない。置ける先は「両方を知ってよい最上位の層」＝合成ルートだけである
//! （`docs/04-jp-tax.md` §2、`DECISIONS.md` D-064 の訂正注記を参照）。
//!
//! 本番の合成ルートは Phase 3 の `kaikei-mcp`（または Phase 4 の
//! `kaikei-api`）になる予定だが、それを先取りして作るのは時期尚早
//! （YAGNI）である。そこで E2E テストの置き場として、この専用crateを
//! 新設した（`DECISIONS.md` D-068）。
//!
//! # 他のどのcrateからも依存されない
//!
//! `kaikei-e2e` は**テスト専用**であり、`kaikei-app` / `kaikei-store` /
//! `kaikei-jp` を含む他のどのcrateの `Cargo.toml` にも（`dev-dependencies`
//! も含めて）現れてはならない。`.github/workflows/architecture.yml` の
//! 「kaikei-e2e は誰からも依存されない」ステップが `cargo tree` でこれを
//! 検査する。依存される側に回った瞬間、「両方を知ってよい最上位の層」と
//! いうこのcrateの位置づけが崩れる。
//!
//! # ここに置いてよいもの・置いてはいけないもの
//!
//! - 置いてよい: **実 DB に繋ぐ E2E テストだけ**
//! - 置いてはいけない: 組み立て（[`compose`]）の実装。本体は `kaikei-jp` にあり
//!   （`DECISIONS.md` D-068 の訂正注記）、この crate はそれを再エクスポート
//!   しているだけである
//! - 置いてはいけない: 税額計算・按分・決算処理そのもの（それは
//!   `kaikei-jp` の責務）。この crate に業務ロジックを書き始めたら、それは
//!   本来 Phase 3 の `kaikei-mcp`（または Phase 4 の `kaikei-api`）に
//!   属するべきコードが紛れ込んでいるサインである
//!
//! # `JpStatementPolicy` の `chart` について（`DECISIONS.md` D-069）
//!
//! [`compose`] が返す [`Composition`] は `JpStatementPolicy` を**含まない**。
//! 決算書（BS/PL）を組み立てる**直前**に、その時点で読み込んだ `chart` から
//! 都度 `JpStatementPolicy::new(chart)` すること。
//!
//! 理由（`chart` は記帳のたびに読み直される可変データであり、長期保持すると
//! 「科目名を変更したのに決算書には古い名前が表示される」バグになる）は
//! `kaikei-jp` のクレート doc「`JpStatementPolicy` の `chart` について」に
//! 置いてある。**方針の置き場は `kaikei-jp` 側**であり、`kaikei-e2e` に
//! 依存できない Phase 3 以降の合成ルート（`kaikei-mcp`）からも辿れる。

#![forbid(unsafe_code)]
#![warn(missing_docs)]

// 組み立て本体は `kaikei-jp` にある（`DECISIONS.md` D-068）。
// この crate は「実 DB に繋ぐ E2E テスト」だけを持ち、組み立ての実装は持たない。
pub use kaikei_jp::compose::{compose, ComposeError, ComposeOptions, Composition};
