//! **監査ログを通らずにツールを実行する経路が存在しない**ことの機械的な検査
//! （`docs/07-mcp-server.md` §9、`DECISIONS.md` D-084）。
//!
//! # 主役は「`rmcp` を名指しできるファイルの許可リスト」である
//!
//! MCP のツールを登録・実行する能力は、すべて `rmcp` の API から来る。
//! そこで **`src/` のうち `rmcp` という識別子を書いてよいファイルを
//! 2つに限る**（[`RMCP_ALLOWED_FILES`]）。どんな API を使う迂回であっても
//! `rmcp` の名前は必要なので、迂回は必ずその2ファイルのどちらかに現れる。
//!
//! ## なぜ禁止リストから許可リストに反転したのか（PR-F レビュー3巡目 B）
//!
//! **識別子の禁止リストは原理的に不完全**であり、実際に2巡続けて破られた。
//!
//! | 巡 | 破り方 | 当時の禁止リストに無かった識別子 |
//! |---|---|---|
//! | 1 | `ToolRouter::with_async_tool::<T>()` / `with_sync_tool` / `(Tool, handler)` タプル | `with_async_tool` / `AsyncTool` / `ToolBase` / `IntoToolRoute` |
//! | 2 | `#[tool_handler]` の impl に `call_tool` を**手書き**する | `call_tool` / `CallToolRequestParams` / `ToolCallContext` / `into_call_tool_result` |
//!
//! 2 は特に静かで、`tools/list` は正規の2件のまま `tools/call` だけが
//! 別経路に差し替わり（`rmcp-macros` の `#[tool_handler]` は
//! `if !has_method("call_tool", ..)` で条件付き生成する）、
//! `journal_entries` に1件・`audit_log` に0行を残しながら
//! `cargo build` / `clippy -D warnings` / `cargo test` は全緑だった。
//!
//! レビューはこの時点でまだ塞がっていない口として `ToolRouter::add_route` /
//! `merge`、`IntoToolRoute` の `WithToolAttr` /
//! `ToolAttrGenerateFunctionAdapter`、`CallToolHandlerExt` も挙げていた
//! （最後のものは `mentions` の識別子境界判定により既存の
//! `CallToolHandler` 規則に**一致しない**——規則が1つあるように見えて
//! 効いていなかった）。識別子を足し続ける限りこれは終わらない。
//!
//! 許可リスト方式は `docs/07-mcp-server.md` §10 MC-30（`kaikei-mcp` の依存の
//! 許可リスト）や `tests/forbidden_tools.rs` の
//! `every_registered_tool_is_one_of_the_eleven_phase_3_tools`（禁止4件だけ
//! でなく許可11件の側からも閉じる）と**同じ形**であり、このリポジトリが
//! 既に採っている方式である。
//!
//! # 型で閉じている部分（こちらは検査ではない）
//!
//! - ツールは `CallToolResult` を組み立てられない（`McpTool::run` の戻り値は
//!   `Result<ToolSuccess, ToolFailure>`）
//! - ツールは `AuditSink` に触れない（`ToolContext` が露出しない。
//!   `Runtime` 自体が渡らない）
//! - `ToolContext` は `dispatch` の外で作れない（フィールドも `new` も private）
//! - `dispatch::ToolRegistry` にツールを載せる口は `with::<T: McpTool>` だけ
//!
//! **「`rmcp` が `dispatch` の外から見えない」とは書かない**（3巡目 C-1）。
//! `rmcp` は `kaikei-mcp` の直接依存で `ToolRouter` は `pub` であり、
//! 同一 crate の他モジュールからの import を妨げる仕組みは Rust に無い。
//! 別のルータ・別の `ServerHandler` を**書き足す**ことを止めているのは
//! 型ではなくこの許可リストである。
//!
//! # 識別子の閉じ込め（[`CONFINED`]）は second line として残す
//!
//! 許可リストの唯一の穴は「`dispatch.rs` が `rmcp` の型を再輸出し、
//! 他のファイルがその再輸出経由で登録経路へ届く」形である。
//! [`CONFINED`] はそこを見張る（`ToolRouter` / `ToolRoute` /
//! `with_async_tool` … が許可された場所以外に現れない）。
//! **網羅は主張しない**（3巡目 D-4）。網羅を担っているのは許可リストである。
//! `with_audit` / `AuditCall` / `audit_sink` / `into_parts_unchecked` は
//! `kaikei-app` 側の識別子なので、そもそも許可リストの守備範囲外であり
//! [`CONFINED`] が一次の担保である。
//!
//! `.github/workflows/architecture.yml` に grep を足すのではなくテストに
//! したのは、`tests/stdout_is_json_rpc_only.rs` と同じ理由——**手元で
//! `cargo test` を回すだけで踏める**ようにするため
//! （`PROGRESS.md` Phase 2 の教訓「手元で回せない検査は回されなくなる」）。

use std::fs;
use std::path::{Path, PathBuf};

/// **`rmcp` という識別子を書いてよい `src/` 配下のファイル。**
///
/// | ファイル | なぜ必要か |
/// |---|---|
/// | `dispatch.rs` | ルータ（`ToolRouter` / `ToolRoute`）、`ServerHandler` の実装（`call_tool` / `list_tools` / `get_tool` / `get_info`）、stdio トランスポートの起動 |
/// | `error.rs` | `ToolError::into_call_tool_result`（ツール結果エラーの組み立て。`DECISIONS.md` D-071） |
///
/// **この一覧を増やすのは設計判断である。** 増やした瞬間、そのファイルは
/// 「監査ログを通らない経路を書ける場所」になる。増やすときは
/// `DECISIONS.md` に理由を残すこと。
const RMCP_ALLOWED_FILES: &[&str] = &["dispatch.rs", "error.rs"];

/// 「この識別子は、この相対パスのファイルにしか現れてはいけない」という規則。
///
/// **これは second line である**（モジュール doc）。`rmcp` 由来の識別子に
/// ついては [`RMCP_ALLOWED_FILES`] が一次の担保であり、ここは
/// 「`dispatch.rs` が再輸出したら他ファイルから使えてしまう」穴だけを見る。
/// `kaikei-app` 由来の識別子（`with_audit` / `AuditCall` / `audit_sink` /
/// `into_parts_unchecked`）については、ここが一次の担保である。
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
        reason: "rmcp の ToolRouter は with_route のほかに with_async_tool / with_sync_tool / \
                 add_route / merge を持ち、そこからは ToolRoute も CallToolResult も書かずに\
                 ツールを載せられる。ルータを持てる場所を dispatch 層に閉じ、外には\
                 dispatch::ToolRegistry（載せる口が with::<T: McpTool> しか無い型）だけを\
                 見せる（D-084 の訂正注記）。dispatch.rs がこの型を再輸出したら\
                 RMCP_ALLOWED_FILES を素通りするので、その穴をここで見る",
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
        allowed: &[],
        reason: "rmcp-macros 3.1.0 の #[tool_handler] は has_method(\"call_tool\", ..) が偽の\
                 ときだけ call_tool を生成する。つまり同じ impl に call_tool を手書きすると\
                 dispatch 経路が黙って置き換わる（3巡目 B の実測: tools/list は正規の2件の\
                 まま、tools/call 1回で journal_entries に1件・audit_log に0行）。\
                 ServerHandler の4メソッドは dispatch.rs で手書きしてあるので、\
                 このマクロはこの crate では使わない",
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

// ---------------------------------------------------------------------------
// ★主役★ `rmcp` を名指しできるファイルの許可リスト（3巡目 B）
// ---------------------------------------------------------------------------

/// **`rmcp` という識別子が [`RMCP_ALLOWED_FILES`] 以外に現れないこと。**
///
/// MCP のツールを登録・実行する能力は全て `rmcp` の API から来るので、
/// どの API を使う迂回であっても `rmcp` の名前が必要になる。
/// この1本で、`ToolRouter::add_route` / `merge`、`IntoToolRoute` の各 impl、
/// `CallToolHandlerExt`、`ServerHandler::call_tool` の手書き、`#[tool_handler]`
/// のマクロ差し替え、そして**まだ誰も見つけていない経路**が同時に落ちる。
///
/// コメント行を見ないのは [`is_comment`] の doc のとおり
/// （「なぜここに閉じるのか」を説明する doc コメントが自分で落ちる誤検知は
/// このリポジトリで既に3回起きている）。**説明は書ける。実行できるコードは
/// 書けない。**
#[test]
fn rmcp_is_named_only_in_the_files_allowed_to_name_it() {
    let mut violations: Vec<String> = Vec::new();

    for (path, text) in sources() {
        if RMCP_ALLOWED_FILES.contains(&path.as_str()) {
            continue;
        }
        for (number, line) in text.lines().enumerate() {
            if is_comment(line) || !mentions(line, "rmcp") {
                continue;
            }
            violations.push(format!("{path}:{}: {}", number + 1, line.trim()));
        }
    }

    assert!(
        violations.is_empty(),
        "`rmcp` を名指しできるのは {} だけです（DECISIONS.md D-084 の訂正注記3 / \
         docs/07-mcp-server.md §4）。\n\
         MCP のツールを登録・実行する能力は全て rmcp から来るので、ここを開けると\
         「監査ログを通らないツール」を書けます。ツールは crate::dispatch::McpTool を\
         実装し、crate::server::tool_registry() に並べてください。\n\
         プロトコルの型が要る場合は crate::dispatch が再輸出しているものを使い、\
         足りなければ dispatch.rs 側に足すこと（登録経路に関わる型は再輸出しない）:\n{}",
        RMCP_ALLOWED_FILES.join(" / "),
        violations.join("\n")
    );
}

/// 許可リストが**広がっていない**こと。
///
/// 「現れない」ことを主張する検査は、許可リストにファイルを1行足すだけで
/// 黙って無力化できる。ここで一覧そのものを固定して、広げる操作が
/// **このテストの変更としてレビューに出る**ようにする。
///
/// `tests/forbidden_tools.rs` の
/// `every_registered_tool_is_one_of_the_eleven_phase_3_tools`（禁止4件だけ
/// でなく許可11件の側からも閉じる）と同じ姿勢である。
#[test]
fn the_rmcp_allowlist_has_not_been_widened() {
    assert_eq!(
        RMCP_ALLOWED_FILES,
        ["dispatch.rs", "error.rs"],
        "rmcp を名指しできるファイルを増やしています。\
         増やしたファイルは「監査ログを通らない経路を書ける場所」になります。\
         本当に必要なら DECISIONS.md に理由を残してからこの assert を直すこと"
    );
}

/// この検査が働いていることの対照実験。
///
/// 「現れない」ことを主張する検査は、走査やマッチングが壊れていても緑に
/// なりうる。許可された側に `rmcp` が**実際に現れている**ことを見る
/// （現れなくなったら、それは走査が壊れたか、`rmcp` の使用箇所が
/// 別のどこかへ移ったかのどちらかである）。
#[test]
fn the_allowed_files_actually_name_rmcp() {
    let sources = sources();
    for allowed in RMCP_ALLOWED_FILES {
        let (_, text) = sources
            .iter()
            .find(|(path, _)| path == allowed)
            .unwrap_or_else(|| panic!("{allowed} が src/ に見つかりません"));
        assert!(
            text.lines()
                .any(|line| !is_comment(line) && mentions(line, "rmcp")),
            "{allowed} に rmcp の使用がありません。\
             走査が壊れているか、許可リストから外せる状態です"
        );
    }
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
         crate::dispatch::McpTool を実装し、crate::server::tool_registry() に\
         並べること（DECISIONS.md D-084）:\n{}",
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
    // ★許可リストの検出力★ 3巡目までに実際に使われた／指摘された迂回が
    // 全て `rmcp` の名前を必要とすることを、実際の書き方で見る。
    for bypass in [
        "use rmcp::handler::server::router::tool::ToolRouter;",
        "        let router = rmcp::handler::server::router::tool::ToolRouter::new();",
        "use rmcp::model::CallToolRequestParams;",
        "    #[rmcp::tool_handler(router = self.tools)]",
        "use rmcp::handler::server::tool::CallToolHandlerExt;",
        "    router.add_route(::rmcp::handler::server::router::tool::ToolRoute::new_dyn(a, h));",
        "use rmcp::ServerHandler;",
    ] {
        assert!(mentions(bypass, "rmcp"), "{bypass}");
        assert!(!is_comment(bypass), "{bypass}");
    }
    // 識別子の途中には一致しない（`kaikei_rmcp_probe` を誤検知しない）。
    assert!(!mentions("let kaikei_rmcp_probe = 1;", "rmcp"));
    assert!(!mentions("let rmcpx = 1;", "rmcp"));
    // 説明する doc コメントは落ちない（誤検知は既に3回起きている）。
    assert!(is_comment(
        "//! `rmcp` を名指しできるのは dispatch.rs だけである"
    ));

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

/// second line として一度入れた `rmcp` 由来の識別子が、黙って消えていないこと。
///
/// # 「1つ残らず」とは書かない（PR-F レビュー3巡目 D-4）
///
/// 以前この検査の doc は「`rmcp` の別経路が**1つ残らず**規則に入っている」と
/// 主張していたが、**入っていなかった**——`ToolRouter::add_route` / `merge`、
/// `IntoToolRoute` の `WithToolAttr` / `ToolAttrGenerateFunctionAdapter`、
/// `CallToolHandlerExt`（`mentions` の識別子境界判定により
/// `CallToolHandler` 規則には一致しない）が抜けていた。
/// **手で維持する識別子の一覧が網羅であることは、原理的に主張できない。**
///
/// 網羅を担っているのは
/// [`rmcp_is_named_only_in_the_files_allowed_to_name_it`]（ファイル許可リスト）
/// である。ここが担うのはその唯一の穴——**`dispatch.rs` が `rmcp` の型を
/// 再輸出したとき**——に対する second line であり、この検査が固定するのは
/// 「一度入れた規則が黙って消えない」ことだけである。
#[test]
fn the_second_line_rules_for_rmcp_identifiers_are_still_present() {
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
        "CallToolResult",
    ] {
        assert!(
            CONFINED.iter().any(|rule| rule.token == token),
            "{token} の閉じ込め規則が消えています。\
             dispatch.rs がこの型を再輸出した場合、ファイル許可リストだけでは\
             他モジュールから登録経路に届くのを止められません"
        );
    }
}
