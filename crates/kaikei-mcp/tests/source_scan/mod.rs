//! `src/` 配下のソース走査（`audit_is_structural.rs` と
//! `stdout_is_json_rpc_only.rs` が共有する）。
//!
//! # なぜ共通化したのか（PR-F レビュー4巡目 B）
//!
//! 同じ走査（`CARGO_MANIFEST_DIR/src` を再帰し、`.rs` だけを読む）が2つの
//! テストに**複製**されていた。そのため片方で見つかった穴——
//! **`.rs` 以外のファイルを一度も読まない**——を塞いでも、もう片方は開いた
//! ままだった。実際に3巡目の迂回は
//!
//! ```text
//! #[path = "../probe_handler.rs"] mod probe_handler;   // src/ の外
//! include!("probe_handler.inc");                        // .rs ではない
//! ```
//!
//! という形で走査の外へ出ており、`stdout_is_json_rpc_only.rs` にも同じ形が
//! そのまま効く（stdio では stdout に1行混ざれば接続ごと壊れるので、
//! `println!` を走査の外のファイルに置ければ同じ事故が起きる）。
//!
//! **走査の穴は1箇所で塞ぐ。** 走査ヘルパをここに集め、両方のテストが
//! これを使う。
//!
//! # ここでの方針
//!
//! | | |
//! |---|---|
//! | 対象 | `src/` 配下の**全ファイル**（拡張子で絞らない） |
//! | 読み方 | バイト列を lossy に UTF-8 とみなす（バイナリが混ざっても走査が死なない） |
//! | 追加の検査 | crate の中に `#[path` と `include!` が**現れない**こと（[`assert_no_out_of_tree_inclusion`]） |
//!
//! 拡張子で絞らないだけでは足りない。`#[path = "../foo.rs"]` は `src/` の
//! **外**を指せるので、走査の対象をどう広げても届かない。だから
//! 「走査の外を指す構文そのものを禁じる」検査を別に置く。
//!
//! # それでもこれは二線目である（4巡目 A）
//!
//! 走査は「ソースがどう書かれているか」しか見られない。網羅を担うのは
//! `crates/kaikei-e2e/tests/mcp_stdio_server.rs`（**実バイナリに
//! `tools/call` を送り、`audit_log` に2行残ることを見る振る舞い検査**）で
//! あり、そちらは書き方に依存しない。ここは「書いた瞬間に手元で落ちる」
//! ことに価値がある層である。

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

/// 走査対象1件（`src/` からの相対パス, 本文）。
pub type Source = (String, String);

fn source_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// crate のルート（`Cargo.toml` がある場所）。
fn crate_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn collect(dir: &Path, root: &Path, found: &mut Vec<Source>) {
    for entry in fs::read_dir(dir).expect("走査できること") {
        let path = entry.expect("ディレクトリ項目を読めること").path();
        if path.is_dir() {
            collect(&path, root, found);
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .expect("走査対象の配下のはず")
            .to_string_lossy()
            .replace('\\', "/");
        // ★拡張子で絞らない★ `.inc` / `.in` / 拡張子なしのファイルを
        // `include!` で取り込む迂回を、走査の外に置けないようにする。
        let bytes = fs::read(&path).expect("ファイルを読めること");
        found.push((relative, String::from_utf8_lossy(&bytes).into_owned()));
    }
}

/// `src/` 配下の**全ファイル**（拡張子を問わない）。
pub fn sources() -> Vec<Source> {
    let root = source_root();
    let mut found = Vec::new();
    collect(&root, &root, &mut found);
    assert!(
        found.len() >= 5,
        "src/ から {} 件しか集められませんでした。走査が働いていません",
        found.len()
    );
    found
}

/// コメント行（`//` `///` `//!` `*`）か。
///
/// 「なぜこれを1箇所に閉じるのか」を**説明している doc コメント**が検査に
/// 引っかかって落ちる誤検知は、このリポジトリで既に3回起きている
/// （`PROGRESS.md` Phase 2 の教訓3）。**説明は書ける。実行できるコードは
/// 書けない。**
pub fn is_comment(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("//") || trimmed.starts_with('*')
}

/// 識別子として現れているか（前後が識別子構成文字でない）。
///
/// 素の `contains` だと `ToolRoute` が `ToolRouter` に一致してしまい、
/// **`ToolRouter` を使っているだけのファイルが誤検知される**。
pub fn mentions(line: &str, token: &str) -> bool {
    let bytes = line.as_bytes();
    line.match_indices(token).any(|(at, _)| {
        let before_ok = at == 0 || !is_ident_byte(bytes[at - 1]);
        let after = at + token.len();
        let after_ok = after >= bytes.len() || !is_ident_byte(bytes[after]);
        before_ok && after_ok
    })
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// 呼び出しの断片（`println!` など）が、識別子の途中でなく現れているか。
///
/// これが無いと **`eprintln!` が `println!` として検出される**——つまり
/// 「stderr に出せ」と指示しておきながら、そのとおりに書いたコードが
/// 落ちる検査になる（`DECISIONS.md` D-078 と同型）。
pub fn contains_call(line: &str, needle: &str) -> bool {
    let mut offset = 0;
    while let Some(found) = line[offset..].find(needle) {
        let start = offset + found;
        let preceded_by_identifier_char = line[..start]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        if !preceded_by_identifier_char {
            return true;
        }
        offset = start + needle.len();
    }
    false
}

/// **走査の外にソースを置く構文が crate に無いこと。**
///
/// | 構文 | 何が起きるか |
/// |---|---|
/// | `#[path = "../foo.rs"] mod foo;` | `src/` の外のファイルが crate の一部としてコンパイルされる。走査対象をどう広げても届かない |
/// | `include!("foo.inc")` | 任意のファイルの中身がその場に展開される。`.rs` ですらないので拡張子を広げても足りない |
///
/// 3巡目の迂回はこの2つで、**監査ログを通らない別の `ServerHandler` を
/// `main.rs` から実際に待ち受けさせた状態で** `cargo build` /
/// `clippy -D warnings` / `fmt --check` / `cargo test -p kaikei-mcp` が
/// 全緑だった。
///
/// この crate は1ファイル1モジュールの素直な構成であり、この2つが要る
/// 理由が無い（`build.rs` も生成コードも持たない）。**要るようになったら、
/// それは走査の前提が変わったということなので、`DECISIONS.md` に理由を
/// 残したうえでこの検査を直すこと。**
///
/// その1行が走査の外からソースを取り込んでいるか。
///
/// 空白を落としてから見る。`#[ path = ".." ]` も `include !(..)` も Rust と
/// しては同じものであり、素の `contains` では素通りする。
pub fn pulls_in_out_of_tree_source(line: &str) -> bool {
    let squeezed: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    // `include!(` は識別子境界を見る（`include_str!(` / `include_bytes!(` は
    // 資源の埋め込みであってソースの取り込みではないので落とさない）。
    if contains_call(&squeezed, "include!(") {
        return true;
    }
    // 属性の中の `path = "..."` を見る。`#[path]` を直に書く形だけでなく
    // `#[cfg_attr(cond, path = "...")]` も同じものなので、両方を1つの規則で
    // 捕まえる（PR-F レビュー4巡目の指摘。`#[path` だけを見ていた版は
    // `cfg_attr` 経由を素通りさせていた）。
    if !squeezed.contains("#[") {
        return false;
    }
    // `path` の直前が区切り文字であることを見る。これが無いと
    // `let path = "x";`（空白を落とすと `letpath="x";`）を誤検知する。
    squeezed
        .match_indices("path=")
        .any(|(at, _)| at > 0 && matches!(squeezed.as_bytes()[at - 1], b'[' | b'(' | b','))
}

/// `reason` には「この走査が何を守っているか」を渡す（失敗メッセージに出す）。
pub fn assert_no_out_of_tree_inclusion(reason: &str) {
    let mut violations = Vec::new();
    for (path, text) in sources() {
        for (number, line) in text.lines().enumerate() {
            if is_comment(line) || !pulls_in_out_of_tree_source(line) {
                continue;
            }
            violations.push(format!("{path}:{}: {}", number + 1, line.trim()));
        }
    }

    assert!(
        violations.is_empty(),
        "走査の外にソースを置く構文が使われています。\n\
         {reason}\n\
         この2つ（#[path = \"...\"] と include!）は src/ の外・.rs 以外の\
         ファイルを crate に取り込むので、走査対象をどう広げても届きません\
         （PR-F レビュー3巡目の迂回はこの形でした）:\n{}",
        violations.join("\n")
    );
}
