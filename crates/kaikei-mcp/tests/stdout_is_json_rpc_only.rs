//! **stdout は JSON-RPC 専用チャネル**（`docs/07-mcp-server.md` §4）の
//! ソースレベルの検査。
//!
//! stdio トランスポートでは、stdout に1行でも余計な出力が混ざると
//! MCP のフレーミングが壊れて接続ごと落ちる。しかも壊れ方が
//! 「サーバが応答しない」という形になるため、原因が `println!` 1行だと
//! 気づくのに時間がかかる。
//!
//! 実際に起動して確かめる検査は別にある（`tests/startup_config.rs` の
//! 「起動失敗時に stdout が空」、`tests/startup_pg.rs` の「起動後の stdout が
//! JSON-RPC の行だけ」）。ここで見るのは**書いた瞬間に落ちる**方の検査で、
//! DB も設定も要らずに走る。
//!
//! `.github/workflows/architecture.yml` に grep のステップを増やすのではなく
//! テストにしたのは、ローカルで `cargo test` を回すだけで踏めるようにする
//! ため（`PROGRESS.md` Phase 2 の教訓「手元で回せない検査は回されなくなる」）。

use std::fs;
use std::path::{Path, PathBuf};

/// stdout に書く可能性がある呼び出しの断片。
///
/// `write!`/`writeln!` は `Formatter` や `Vec<u8>` に対しても使うため
/// ここでは見ない（stdout に向けるには `io::stdout()` を経由する必要があり、
/// それは下の一覧で捕まる）。
const FORBIDDEN: &[&str] = &[
    "println!",
    "print!",
    "io::stdout",
    "std::io::stdout",
    "stdout()",
];

fn source_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rust_sources(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("src/ を走査できること") {
        let path = entry.expect("ディレクトリ項目を読めること").path();
        if path.is_dir() {
            rust_sources(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push(path);
        }
    }
}

/// コメント行（`//` `///` `//!` `*`）を除いた行だけを見る。
///
/// 「stdout に書かない」と**説明している doc コメント**が検査に引っかかって
/// 落ちる誤検知は、このリポジトリで既に2回起きている
/// （`PROGRESS.md` Phase 2 の教訓3）。同じ轍を踏まない。
fn is_comment(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("//") || trimmed.starts_with('*')
}

/// 識別子の途中に現れた一致は数えない。
///
/// これが無いと **`eprintln!` が `println!` として検出される**——つまり
/// 「stderr に出せ」と指示しておきながら、そのとおりに書いたコードが
/// 落ちる検査になる。`.github/workflows/architecture.yml` の grep 群が
/// `grep -qw` の単語境界で繰り返し嵌まったのと同型の問題
/// （`DECISIONS.md` D-078）。
fn contains_call(line: &str, needle: &str) -> bool {
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

#[test]
fn no_source_file_writes_to_stdout() {
    let mut sources = Vec::new();
    rust_sources(&source_root(), &mut sources);
    assert!(
        !sources.is_empty(),
        "検査対象のソースが0件（検査が働いていない）"
    );

    let mut hits: Vec<String> = Vec::new();
    for path in &sources {
        let text = fs::read_to_string(path).expect("ソースを読めること");
        for (index, line) in text.lines().enumerate() {
            if is_comment(line) {
                continue;
            }
            for needle in FORBIDDEN {
                if contains_call(line, needle) {
                    hits.push(format!("{}:{}: {}", path.display(), index + 1, line.trim()));
                }
            }
        }
    }

    assert!(
        hits.is_empty(),
        "stdout へ書く可能性のある呼び出しが見つかりました。\
         stdio トランスポートでは stdout が JSON-RPC 専用チャネルです\
         （docs/07-mcp-server.md §4）。診断は eprintln!（stderr）に出してください。\n{}",
        hits.join("\n")
    );
}

/// 検査そのものが働いていることの確認（検出器が常に空を返す状態に
/// 退行したら、上のテストは緑のまま無意味になる）。
#[test]
fn the_detector_actually_flags_a_stdout_write() {
    for sample in [
        "fn main() { println!(\"漏れた\"); }",
        "    print!(\"漏れた\");",
        "    let mut out = std::io::stdout();",
        "    write!(io::stdout(), \"漏れた\")?;",
    ] {
        assert!(
            FORBIDDEN.iter().any(|needle| contains_call(sample, needle)),
            "検出できていない: {sample}"
        );
    }

    // コメント行は誤検知しない。
    assert!(is_comment("// println! は使わない"));

    // ★eprintln! / eprint! を println! / print! と取り違えない★
    // 取り違えると「stderr に出せ」という指示に従ったコードが落ちる。
    for stderr_call in ["    eprintln!(\"{error}\");", "    eprint!(\"x\");"] {
        assert!(
            !FORBIDDEN
                .iter()
                .any(|needle| contains_call(stderr_call, needle)),
            "stderr への出力を誤検知している: {stderr_call}"
        );
    }
}
