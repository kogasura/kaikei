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

/// CLI（`kaikei`）の実行ファイルの場所。**古ければ落とす。**
///
/// # なぜ「在るかどうか」だけでは足りないのか
///
/// E2E は `cargo` が組み立てたものではなく、`target/<profile>/kaikei` を
/// **そのまま起動する**（`kaikei-e2e` は `kaikei-cli` に依存していない。
/// 依存させると「誰からも依存されない」という architecture.yml の検査に
/// 引っかかる）。つまり `cargo test -p kaikei-e2e` は CLI を組み直さない。
///
/// これで実際に騙された。`main.rs` に変異を入れて E2E が落ちるかを確かめた
/// ところ、18件すべてが通った。**テストが弱いのではなく、古い実行ファイルを
/// 起動していた**だけだった。変異を入れたつもりで何も変えていない、という
/// のはいちばん質の悪い誤りである（テストが強いという誤った確信が残る）。
///
/// そこで、実行ファイルより新しい `.rs` があれば落とす。
pub fn cli_binary_or_panic(source_roots: &[&std::path::Path]) -> std::path::PathBuf {
    let test_exe = std::env::current_exe().expect("テスト実行ファイルの場所を取れること");
    let profile_dir = test_exe
        .parent()
        .and_then(std::path::Path::parent)
        .expect("<target>/<profile>/deps/ の2つ上");
    let binary = profile_dir.join(format!("kaikei{}", std::env::consts::EXE_SUFFIX));
    let built_at = std::fs::metadata(&binary).and_then(|m| m.modified()).unwrap_or_else(|error| {
        panic!(
            "kaikei の実行ファイルを読めません: {}（{error}）\n先に cargo build -p kaikei-cli を実行してください。",
            binary.display()
        )
    });

    if let Some(newer) = source_roots
        .iter()
        .find_map(|root| newest_source_after(root, built_at))
    {
        panic!(
            "kaikei の実行ファイルが古いままです。\n  実行ファイル: {}\n  これより新しいソース: {}\n\
             cargo test -p kaikei-e2e は CLI を組み直しません。\n先に cargo build -p kaikei-cli を実行してください。",
            binary.display(),
            newer.display()
        );
    }
    binary
}

/// `root` 以下で `built_at` より新しい `.rs` を1つ返す。
fn newest_source_after(
    root: &std::path::Path,
    built_at: std::time::SystemTime,
) -> Option<std::path::PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs")
                && entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .is_ok_and(|at| at > built_at)
            {
                return Some(path);
            }
        }
    }
    None
}
