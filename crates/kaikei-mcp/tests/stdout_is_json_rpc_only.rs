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
//! JSON-RPC の行だけ」、`crates/kaikei-e2e/tests/mcp_stdio_server.rs` の
//! 「`tools/call` の往復が最後まで JSON-RPC で成立する」）。
//! ここで見るのは**書いた瞬間に落ちる**方の検査で、DB も設定も要らずに走る。
//!
//! `.github/workflows/architecture.yml` に grep のステップを増やすのではなく
//! テストにしたのは、ローカルで `cargo test` を回すだけで踏めるようにする
//! ため（`PROGRESS.md` Phase 2 の教訓「手元で回せない検査は回されなくなる」）。
//!
//! # 走査ヘルパは `audit_is_structural.rs` と共有する（4巡目 B）
//!
//! 以前はここと `tests/audit_is_structural.rs` に**同じ走査が複製**されて
//! いた（`CARGO_MANIFEST_DIR/src` を再帰し `.rs` だけを読む）。そのため
//! 3巡目に見つかった穴——`#[path = "../foo.rs"]` と `include!("foo.inc")` で
//! 走査の外にファイルを置く——が、あちらを直しても**こちらに残る**形に
//! なっていた。stdio では stdout に1行混ざれば接続ごと壊れるので、
//! `println!` を走査の外のファイルに置ければ同じ事故が起きる。
//! 走査は `tests/source_scan/mod.rs` に集約した。

mod source_scan;

use source_scan::{assert_no_out_of_tree_inclusion, contains_call, is_comment, sources};

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

#[test]
fn no_source_file_writes_to_stdout() {
    let sources = sources();

    let mut hits: Vec<String> = Vec::new();
    for (path, text) in &sources {
        for (index, line) in text.lines().enumerate() {
            if is_comment(line) {
                continue;
            }
            for needle in FORBIDDEN {
                if contains_call(line, needle) {
                    hits.push(format!("{path}:{}: {}", index + 1, line.trim()));
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

/// **走査の外にソースを置く構文が使われていないこと**（4巡目 B）。
///
/// 上の検査は `src/` 配下のファイルしか読まない。`#[path = "../foo.rs"]` や
/// `include!("foo.inc")` で取り込んだファイルの中の `println!` は
/// **一度も読まれない**まま stdout に出る（そして stdio の接続が壊れる）。
/// 走査対象は `.rs` 以外にも広げてあるが、`#[path]` は `src/` の外を指せる
/// ので、それだけでは届かない。
#[test]
fn no_source_is_pulled_in_from_outside_the_scan() {
    assert_no_out_of_tree_inclusion(
        "この走査（stdout への書き込み）は src/ 配下のファイルしか読みません。",
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
