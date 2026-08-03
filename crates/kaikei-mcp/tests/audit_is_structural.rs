//! **監査ログを通らずにツールを実行する経路が存在しない**ことの機械的な検査
//! （`docs/07-mcp-server.md` §9、`DECISIONS.md` D-084）。
//!
//! # 主役は型であって、このテストではない
//!
//! 「呼び忘れる形が存在しない」の本体は `src/dispatch.rs` が型で閉じている
//! 部分である:
//!
//! - ツールは `CallToolResult` を組み立てられない（`McpTool::run` の戻り値は
//!   `Result<ToolSuccess, ToolFailure>`）
//! - ツールは `AuditSink` に触れない（`ToolContext` が露出しない。
//!   `Runtime` 自体が渡らない）
//! - `ToolContext` は `dispatch` の外で作れない（フィールドも `new` も private）
//! - **`rmcp` の `ToolRouter` は `dispatch` の外に出ない。**
//!   `server.rs` が持つのは `dispatch::ToolRegistry` で、ツールを載せる口は
//!   `with::<T: McpTool>` だけである（PR-F レビュー B-1。それ以前は
//!   `ToolRouter::with_async_tool::<T>()` で `dispatch::call` を通らない
//!   ツールを登録でき、しかもその形はこの走査を全て素通りしていた）
//!
//! 型で閉じられないのは「`ToolRoute` を直接組み立てる」「`with_audit` を
//! ツール側で呼ぶ」という**書き足し**である（Rust の可視性はモジュール単位
//! なので、同一 crate 内から `kaikei_app::audit::with_audit` を呼ぶこと自体は
//! 止められない）。そこだけをこのテストが見張る。
//!
//! `rmcp` の別経路（`AsyncTool` / `SyncTool` / `with_async_tool` …）も
//! **型で閉じたうえで**規則に入れてある。型の側が崩れたときに気づける
//! second line であって、これが主ではない。
//!
//! `.github/workflows/architecture.yml` に grep を足すのではなくテストに
//! したのは、`tests/stdout_is_json_rpc_only.rs` と同じ理由——**手元で
//! `cargo test` を回すだけで踏める**ようにするため
//! （`PROGRESS.md` Phase 2 の教訓「手元で回せない検査は回されなくなる」）。

use std::fs;
use std::path::{Path, PathBuf};

/// 「この識別子は、この相対パスのファイルにしか現れてはいけない」という規則。
struct Confined {
    /// 探す識別子。
    token: &'static str,
    /// 現れてよいファイル（`src/` からの相対パス）。空なら**どこにも**
    /// 現れてはいけない。
    allowed: &'static [&'static str],
    /// なぜ閉じ込めているのか（失敗メッセージに出す）。
    reason: &'static str,
}

const CONFINED: &[Confined] = &[
    Confined {
        token: "with_audit",
        allowed: &["dispatch.rs"],
        reason: "監査ログで挟む手順は dispatch 層に1箇所だけ置く。ツール側で呼べる形に\
                 すると、11ツールのうち1つで呼び忘れても正常系のテストは全て緑のまま通る\
                 （DECISIONS.md D-076 / D-084）",
    },
    Confined {
        token: "AuditCall",
        allowed: &["dispatch.rs"],
        reason: "AuditCall.tool は audit_log の TEXT 列に直接入り、input/output に掛かる\
                 無害化を通らない。レジストリ由来の名前しか載らないことを保つため、\
                 組み立てるのは dispatch 層の1箇所だけにする（docs/07-mcp-server.md §9）",
    },
    Confined {
        token: "audit_sink",
        allowed: &["dispatch.rs", "startup.rs"],
        reason: "監査ログの記録先に触れてよいのは、合成ルート（組み立て）と dispatch 層\
                 （記録）だけである。ツールから見えるようにすると、ツールが自前で\
                 監査ログを書けてしまい dispatch 層が唯一の経路でなくなる",
    },
    Confined {
        token: "into_parts_unchecked",
        allowed: &[],
        reason: "fail-open の警告を握り潰せる逃げ道（DECISIONS.md D-076）。\
                 この crate では既定経路 into_result_noting_outcome(&mut notes) だけを使い、\
                 積まれた警告は必ず応答の warnings に載せる",
    },
    Confined {
        token: "ToolRoute",
        allowed: &["dispatch.rs"],
        reason: "ルータに載せる経路を1つに絞るための要（dispatch::route）。\
                 ここ以外で ToolRoute を組み立てると、監査ログを通らないツールを\
                 登録できてしまう",
    },
    Confined {
        token: "ToolRouter",
        allowed: &["dispatch.rs"],
        reason: "rmcp の ToolRouter は with_route のほかに with_async_tool / with_sync_tool を\
                 持ち、そこからは ToolRoute も CallToolResult も書かずにツールを載せられる。\
                 ルータを持てる場所を dispatch 層に閉じ、外には dispatch::ToolRegistry\
                 （載せる口が with::<T: McpTool> しか無い型）だけを見せる（D-084 の訂正注記）",
    },
    Confined {
        token: "with_async_tool",
        allowed: &[],
        reason: "rmcp の AsyncTool 実装型をルータに直接載せる口。通ると dispatch::call を\
                 経由しないツールができる（ハンドラ本体は rmcp の async_tool_wrapper が\
                 組み立てるので CallToolResult すらソースに現れない）",
    },
    Confined {
        token: "with_sync_tool",
        allowed: &[],
        reason: "with_async_tool と同じ（SyncTool 版）",
    },
    Confined {
        token: "AsyncTool",
        allowed: &[],
        reason: "実装すると with_async_tool でルータに載せられる。rmcp 3.1 の module doc は\
                 「1ツール1ファイルならこちら」とこの形を勧めているが、invoke の失敗値が\
                 ErrorData 固定でありドメインのエラーをツール結果エラーで返せない（D-071）。\
                 ツールは crate::dispatch::McpTool を実装すること",
    },
    Confined {
        token: "SyncTool",
        allowed: &[],
        reason: "AsyncTool と同じ",
    },
    Confined {
        token: "ToolBase",
        allowed: &[],
        reason: "AsyncTool / SyncTool の前提となる trait。実装する動機はこの crate に無い",
    },
    Confined {
        token: "IntoToolRoute",
        allowed: &[],
        reason: "(Tool, handler) のタプルなどを with_route に渡せるようにしている trait。\
                 ここを経由すると dispatch::route を通らないルートを組み立てられる",
    },
    Confined {
        token: "CallToolHandler",
        allowed: &[],
        reason: "ツールのハンドラ本体をその場に書くための trait。dispatch::call 以外の\
                 ハンドラが生まれる",
    },
    Confined {
        token: "CallToolResult",
        allowed: &["dispatch.rs", "error.rs"],
        reason: "応答（isError を含む）を組み立てるのは dispatch 層と ToolError だけ。\
                 ツールが自分で CallToolResult を返せる形にすると、監査ログを通らずに\
                 応答できる経路ができる",
    },
    Confined {
        token: "tool_handler",
        allowed: &["server.rs"],
        reason: "rmcp のマクロで別経路のツールを生やさない（#[tool] / #[tool_router] は\
                 使わない。ハンドラ本体を書けてしまい dispatch 層を迂回できる）",
    },
];

fn source_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rust_sources(dir: &Path, root: &Path, found: &mut Vec<(String, String)>) {
    for entry in fs::read_dir(dir).expect("src/ を走査できること") {
        let path = entry.expect("ディレクトリ項目を読めること").path();
        if path.is_dir() {
            rust_sources(&path, root, found);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let relative = path
                .strip_prefix(root)
                .expect("src/ 配下のはず")
                .to_string_lossy()
                .replace('\\', "/");
            let text = fs::read_to_string(&path).expect("ソースを読めること");
            found.push((relative, text));
        }
    }
}

fn sources() -> Vec<(String, String)> {
    let root = source_root();
    let mut found = Vec::new();
    rust_sources(&root, &root, &mut found);
    assert!(
        found.len() >= 5,
        "src/ から .rs を {} 件しか集められませんでした。走査が働いていません",
        found.len()
    );
    found
}

/// コメント行（`//` `///` `//!` `*`）は見ない。
///
/// 「なぜこれを1箇所に閉じるのか」を**説明している doc コメント**が検査に
/// 引っかかって落ちる誤検知は、このリポジトリで既に3回起きている
/// （`PROGRESS.md` Phase 2 の教訓3）。
fn is_comment(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("//") || trimmed.starts_with('*')
}

/// 識別子として現れているか（前後が識別子構成文字でない）。
///
/// 素の `contains` だと `ToolRoute` が `ToolRouter` に一致してしまい、
/// **`ToolRouter` を使っているだけのファイルが誤検知される**。
fn mentions(line: &str, token: &str) -> bool {
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

/// 監査ログの経路に関わる識別子が、決めた場所にしか現れないこと。
#[test]
fn the_audit_path_identifiers_are_confined_to_the_dispatch_layer() {
    let sources = sources();
    let mut violations: Vec<String> = Vec::new();

    for rule in CONFINED {
        for (path, text) in &sources {
            if rule.allowed.contains(&path.as_str()) {
                continue;
            }
            for (number, line) in text.lines().enumerate() {
                if is_comment(line) || !mentions(line, rule.token) {
                    continue;
                }
                violations.push(format!(
                    "{path}:{}: {} が現れています\n    → {}\n    {}",
                    number + 1,
                    rule.token,
                    if rule.allowed.is_empty() {
                        "この crate では使わないこと".to_string()
                    } else {
                        format!("使ってよいのは {} だけ", rule.allowed.join(" / "))
                    },
                    rule.reason,
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "監査ログを通らない経路ができています（docs/07-mcp-server.md §9 / D-084）:\n{}",
        violations.join("\n")
    );
}

/// `rmcp` のツールマクロ（`#[tool]` / `#[tool_router]`）を使っていないこと。
///
/// マクロで書いたツールは**ハンドラ本体をその場に書く**形になるため、
/// `dispatch::call` を経由しない登録経路が生まれる。
#[test]
fn no_tool_is_defined_with_the_rmcp_macros() {
    let mut violations = Vec::new();
    for (path, text) in sources() {
        for (number, line) in text.lines().enumerate() {
            if is_comment(line) {
                continue;
            }
            let trimmed = line.trim_start();
            if trimmed.starts_with("#[tool(") || trimmed.starts_with("#[tool_router") {
                violations.push(format!("{path}:{}: {trimmed}", number + 1));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "rmcp のツールマクロでツールを定義しています。ツールは \
         crate::dispatch::McpTool を実装し、crate::dispatch::route で登録すること\
         （DECISIONS.md D-084）:\n{}",
        violations.join("\n")
    );
}

/// ツールの実装（`src/tools/`）が `kaikei_app::audit` を知らないこと。
///
/// 上の閉じ込め検査と重なるが、こちらは「ツール側から見て何が見えていては
/// いけないか」を述べている。監査ログの語彙が1つでもツールに漏れたら、
/// そのツールは自分で記録できる（＝記録しないこともできる）。
#[test]
fn the_tool_implementations_do_not_know_about_the_audit_module() {
    let mut violations = Vec::new();
    for (path, text) in sources() {
        if !path.starts_with("tools/") {
            continue;
        }
        for (number, line) in text.lines().enumerate() {
            if is_comment(line) {
                continue;
            }
            if line.contains("kaikei_app::audit") || line.contains("audit::") {
                violations.push(format!("{path}:{}: {}", number + 1, line.trim()));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "ツールの実装が監査ログのモジュールを参照しています:\n{}",
        violations.join("\n")
    );
}

/// この検査自体が働いていることの対照実験。
///
/// 「現れていない」ことを主張する検査は、走査やマッチングが壊れていても
/// 緑になりうる。実際に該当する行を与えて、検出できることを見る。
#[test]
fn the_detector_actually_flags_a_bypass() {
    assert!(mentions(
        "    let x = with_audit(sink, clock);",
        "with_audit"
    ));
    assert!(mentions("use kaikei_app::audit::AuditCall;", "AuditCall"));
    assert!(mentions("ToolRoute::new_dyn(attr, handler)", "ToolRoute"));
    // B-1 の実際の抜け道（型で閉じたうえで、走査でも見張る）。
    assert!(mentions(
        "    ToolRouter::new().with_async_tool::<Probe>()",
        "with_async_tool"
    ));
    assert!(mentions(
        "impl AsyncTool<KaikeiServer> for Probe {",
        "AsyncTool"
    ));
    assert!(mentions("impl ToolBase for Probe {", "ToolBase"));
    // 識別子の途中には一致しない（`ToolRouter` を誤検知しない）。
    assert!(!mentions("ToolRouter::new()", "ToolRoute"));
    assert!(!mentions("IntoToolRoute for (Tool, H)", "ToolRoute"));
    assert!(!mentions("let with_audit_log = 1;", "with_audit"));
    // コメント行は見ない。
    assert!(is_comment("    /// with_audit に閉じてある"));
    assert!(is_comment("//! ToolRoute を直接組み立てない"));
    assert!(!is_comment("    let route = ToolRoute::new_dyn(..);"));
}

/// 閉じ込め規則そのものが空になっていないこと（規則を消して緑にできない）。
#[test]
fn the_confinement_rules_are_not_empty() {
    assert!(CONFINED.len() >= 15, "閉じ込め規則が減っています");
    for rule in CONFINED {
        assert!(!rule.token.is_empty());
        assert!(
            !rule.reason.is_empty(),
            "{}: 理由が書かれていません",
            rule.token
        );
    }
}

/// `rmcp` の別経路（`AsyncTool` / `SyncTool` / `with_async_tool` …）が
/// **1つ残らず**規則に入っていること。
///
/// この一覧は手で維持しているので、`rmcp` の登録口を1つ書き落とすと
/// そこだけが素通しになる（PR-F レビュー B-1 で実際に踏んだ形）。
/// せめて「一度入れたものが黙って消えない」ことは固定する。
#[test]
fn every_known_rmcp_registration_path_is_confined() {
    for token in [
        "ToolRouter",
        "ToolRoute",
        "with_async_tool",
        "with_sync_tool",
        "AsyncTool",
        "SyncTool",
        "ToolBase",
        "IntoToolRoute",
        "CallToolHandler",
        "tool_handler",
    ] {
        assert!(
            CONFINED.iter().any(|rule| rule.token == token),
            "{token} の閉じ込め規則が消えています。\
             rmcp はこの経路からも dispatch::call を通らないツールを登録できます"
        );
    }
}
