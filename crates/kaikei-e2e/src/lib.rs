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
//! - 置いてよい: 合成ルートが起動時に一度だけ行う**組み立て**
//!   （YAMLロード → policy 構築）を1箇所にまとめたヘルパ（[`compose`]）
//! - 置いてはいけない: 税額計算・按分・決算処理そのもの（それは
//!   `kaikei-jp` の責務）。この crate に業務ロジックを書き始めたら、それは
//!   本来 Phase 3 の `kaikei-mcp`（または Phase 4 の `kaikei-api`）に
//!   属するべきコードが紛れ込んでいるサインである
//!
//! # `JpStatementPolicy` の `chart` について（`DECISIONS.md` D-069）
//!
//! [`compose`] が返す [`Composition`] は `JpStatementPolicy` を**含まない**。
//!
//! `JpTaxPolicy`（年度別マスタ）や `JpSoleProprietorClosingPolicy`
//! （決算科目3つの実在検証）が保持するデータは YAML 由来で、変更するには
//! プロセス再起動が要る（`DECISIONS.md` D-025/D-057/D-066）。これらは
//! 起動時に一度組み立てて長期保持するのが自然である。
//!
//! 一方 `JpStatementPolicy` が保持する `chart` は**DBから読み直される
//! 可変データ**であり、`kaikei-app/src/context.rs` の
//! `load_posting_context` が記帳のたびに `tx.load_chart()` で読み直して
//! いるのと同じ性質を持つ（ユーザーが科目名を編集する経路が存在する）。
//! `JpStatementPolicy` を起動時に一度だけ構築して長期保持すると、
//! 「科目名を変更したのに決算書には古い名前が表示される」という
//! バグになりうる。
//!
//! `JpStatementPolicy::new` はYAML解釈や構築時検証を一切行わない単純な
//! ラッパ（`ChartOfAccounts` を保持するだけ）であり、構築コストは
//! 無視できるほど小さい。そのため方針は
//! **「決算書（BS/PL）を組み立てる直前に、その時点で読み込んだ `chart`
//! から都度 `JpStatementPolicy::new(chart)` する」**とし、`compose` の
//! 戻り値には含めない。呼び出し側（合成ルート）は決算書生成のリクエストの
//! たびに `chart` を読み直してから構築すること。

#![forbid(unsafe_code)]
#![warn(missing_docs)]

// 組み立て本体は `kaikei-jp` にある（`DECISIONS.md` D-068）。
// この crate は「実 DB に繋ぐ E2E テスト」だけを持ち、組み立ての実装は持たない。
pub use kaikei_jp::compose::{compose, ComposeError, ComposeOptions, Composition};
