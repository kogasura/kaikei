//! ★Phase 3 の完了条件そのもの★ **AI が実際にやる一連の流れ**を、
//! 実バイナリに stdio で1本通す（Phase 3 PR-I）。
//!
//! `ROADMAP.md` Phase 3 の完了条件は4つある。
//!
//! | 完了条件 | このファイルが見るもの |
//! |---|---|
//! | Claude Code から記帳できる | 実バイナリを子プロセスとして起動し、`tools/call` だけで帳簿を付け切る |
//! | 貸借不一致のとき、AI が自己修正できるエラーが返る | 1回目の記帳をわざと `auto_tax_lines` 無しで送り、返ってきた `hint.suggested_lines` に従って**送り直して成功させる** |
//! | 全操作が audit_log に記録される | 通しの全呼び出しが「開始・結果の2行」で残り、**登録済み11ツールが1つ残らず現れる**（一覧は `kaikei_mcp::server::registered_tool_names` から導出する） |
//! | 削除系ツールが存在しない | `tools/list` の応答に4件のいずれも無く、その名前で `tools/call` するとプロトコルエラーになり、**`audit_log` に1行も残らない** |
//!
//! # なぜ「1本の通し」が要るのか（既存のテストで足りない理由）
//!
//! `tests/mcp_write_tools.rs` / `tests/mcp_search_ledger.rs` は
//! `dispatch::call` を直接呼び、`tests/mcp_stdio_server.rs` は実バイナリを
//! 使うが、いずれも**ツール1つずつ**の検査である。AI が帳簿を付けるときに
//! 起きるのは、
//!
//! 1. 帳簿の設定と使える科目・税区分を**先に確かめる**
//! 2. 記帳する
//! 3. **読み戻して**確かめる
//! 4. 検索・元帳・試算表で**間違いに気づく**
//! 5. 逆仕訳で訂正する
//! 6. 訂正済みであることが**読み取れる**
//! 7. 正しい金額で記帳し直す
//!
//! という**ツールをまたぐ流れ**であり、ここで壊れるのは個々のツールでは
//! なく「前のツールの応答を次のツールの入力にできるか」である。
//! 実際、`get_entry` が訂正済みかどうかを返していなかった欠陥（PR-G
//! レビュー B）は、ツール単体の応答としては正しく見えていた。
//!
//! # `tests/mcp_stdio_server.rs` との違い
//!
//! あちらは「**プロトコルの入口から監査ログまでが1本に繋がっていること**」
//! を、ツールごと・失敗経路ごとに切って見る場所である。
//! こちらは**業務の流れとして通ること**を見る。ハーネス（実バイナリの起動と
//! stdio の読み書き）は `tests/common/mcp_stdio.rs` で共有している。

#![cfg(feature = "pg-tests")]

mod common;

use common::mcp_stdio::{
    audit_rows, audited_calls, body, is_error, journal_entry_count, McpServer,
};
use serde_json::{json, Value};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::collections::BTreeSet;

/// **存在させないツール**（`docs/07-mcp-server.md` §2）。
///
/// 4件は理由が同じではない（`crates/kaikei-mcp/tests/forbidden_tools.rs` の
/// doc）。ここで見るのは**プロトコルの入口から見て存在しないこと**である。
const FORBIDDEN_TOOLS: [&str; 4] = [
    "delete_journal_entry",
    "update_journal_entry",
    "execute_sql",
    "reopen_period",
];

/// 取り込んだ明細を1件仕込む。
///
/// 取込の経路（`kaikei import`）は CLI にあり、ここから呼べない。**読む側と
/// 仕訳にする側は本番と同じ経路**（MCP のツール）を通す。
async fn seed_imported_line(pool: &sqlx::PgPool) -> String {
    sqlx::query_scalar(
        "INSERT INTO imported_transactions          (id, source, external_key, occurred_on, amount_minor, direction,           raw_description, balance_after, raw_row, status, imported_at)          VALUES (gen_random_uuid(), 'mizuho', 'k1', DATE '2026-06-15', 19800, 2,                  'ｶ)ｱﾏｿﾞﾝ', 500000, '[]', 'pending', now())          RETURNING id::text",
    )
    .fetch_one(pool)
    .await
    .expect("取込明細を入れられること")
}

fn entry_id_of(result: &Value) -> String {
    body(result)["entry_id"]
        .as_str()
        .unwrap_or_else(|| panic!("entry_id が無い: {result}"))
        .to_string()
}

// ---------------------------------------------------------------------------
// 通し
// ---------------------------------------------------------------------------

/// ★本命★ 科目を確認 → 記帳 → 読み戻し → 検索 → 元帳 → 試算表 →
/// 間違いに気づく → 逆仕訳 → 訂正済みと分かる → 監査ログに全部残っている。
///
/// # 帳簿の内容
///
/// | # | 取引日 | 摘要 | 内容 |
/// |---|---|---|---|
/// | 1 | 2026-04-15 | A社への請求 | 売掛金 110,000 / 売上高 100,000（+ 仮受消費税 10,000） |
/// | 2 | 2026-05-08 | B社への請求 | **桁を1つ間違える**（550,000 / 500,000 + 50,000） |
/// | 3 | 2026-05-20 | 【訂正】B社への請求 | 2 の赤伝 |
/// | 4 | 2026-05-08 | B社への請求（金額訂正後） | 55,000 / 50,000（+ 5,000） |
///
/// 4 の取引日が 2026-05-08 なのは、**取引日は取引が起きた日**であって
/// 記帳した日ではないからである（`CLAUDE.md` §7）。訂正のために過去日へ
/// 追記が起きる——読み取り系のページングを keyset にした理由でもある
/// （`DECISIONS.md` D-089）。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn an_ai_keeps_the_books_end_to_end_through_the_real_binary(
    _pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts.clone()).await;
    let mut server = McpServer::start(&conn_opts).await;

    // -----------------------------------------------------------------
    // 1. 帳簿の前提を確かめる（設定・科目・税区分）
    // -----------------------------------------------------------------
    //
    // 税抜経理なのか税込経理なのか、課税事業者なのかで**同じ入力の結果が
    // 変わる**（税額行が生成されるかどうか）。AI が最初に読むのはここである。
    let settings = server.call_tool("get_settings", json!({})).await;
    assert!(!is_error(&settings), "{settings}");
    assert_eq!(body(&settings)["tax_mode"], json!("exclusive"));
    assert_eq!(body(&settings)["is_taxable_business"], json!(true));
    assert_eq!(body(&settings)["book_currency"]["code"], json!("JPY"));

    let accounts = server.call_tool("list_accounts", json!({})).await;
    assert!(!is_error(&accounts), "{accounts}");
    let listed = body(&accounts)["accounts"].as_array().expect("配列");
    for code in ["135", "500"] {
        let account = listed
            .iter()
            .find(|account| account["account"] == json!(code))
            .unwrap_or_else(|| panic!("使おうとした科目 {code} が一覧に無い: {accounts}"));
        // 記帳可否が分かる（見出し科目に記帳して初めて分かる形にしない）。
        assert_eq!(account["postable"], json!(true), "{account}");
    }

    let categories = server
        .call_tool("list_tax_categories", json!({ "date": "2026-04-15" }))
        .await;
    assert!(!is_error(&categories), "{categories}");
    // **どのマスタを見た結果か**が応答から分かる（取引日で切り替わるため）。
    assert!(
        body(&categories)["table"]["range"].is_string(),
        "{categories}"
    );
    assert!(
        body(&categories)["categories"]
            .as_array()
            .expect("配列")
            .iter()
            .any(|category| category["code"] == json!("SALES_10")),
        "{categories}"
    );

    // 提案は候補と根拠までで、確定はしない（`CLAUDE.md` §10）。
    let suggested = server
        .call_tool(
            "suggest_tax_category",
            json!({ "date": "2026-04-15", "direction": "sales",
                    "description": "A社へのコンサル料" }),
        )
        .await;
    assert!(!is_error(&suggested), "{suggested}");
    assert!(
        body(&suggested)["candidates"]
            .as_array()
            .expect("配列")
            .len()
            > 1,
        "1件に絞り込まれています（判断はサーバーが行わない）: {suggested}"
    );

    // 取引先の登録番号は**形式だけ**を確認する（実在確認はしない）。
    let invoice = server
        .call_tool(
            "validate_invoice_number",
            json!({ "registration_number": "T7123456789012" }),
        )
        .await;
    assert!(!is_error(&invoice), "{invoice}");
    assert_eq!(body(&invoice)["format_valid"], json!(true));
    assert!(
        !body(&invoice)["not_checked"]
            .as_array()
            .expect("配列")
            .is_empty(),
        "実在確認をしていないことが応答に残る: {invoice}"
    );

    // -----------------------------------------------------------------
    // 2. 記帳（1回目は失敗し、応答の hint に従って自己修正する）
    // -----------------------------------------------------------------
    //
    // ★`ROADMAP.md` Phase 3 の完了条件「貸借不一致のとき、AI が自己修正
    // できるエラーが返る」はここ★。差額だけを返すと AI は**金額を書き換える**
    // という誤った修正に進む（`docs/07-mcp-server.md` §1 ③）。
    let first_attempt = server
        .call_tool(
            "post_journal_entry",
            json!({
                "entry_date": "2026-04-15",
                "description": "A社への請求",
                "lines": [
                    { "account": "135", "side": "debit",  "amount": "110000" },
                    { "account": "500", "side": "credit", "amount": "100000",
                      "tags": { "tax_category": "SALES_10" } }
                ]
            }),
        )
        .await;
    assert!(is_error(&first_attempt), "{first_attempt}");
    assert_eq!(body(&first_attempt)["error"], json!("unbalanced"));
    assert_eq!(body(&first_attempt)["difference"], json!("10000"));
    let hint = &body(&first_attempt)["hint"];
    let suggested_lines = hint["suggested_lines"]
        .as_array()
        .unwrap_or_else(|| panic!("hint に suggested_lines が無い: {first_attempt}"));
    // 消費税額の行（仮受消費税等）が足された明細が提示される。
    assert_eq!(suggested_lines.len(), 3, "{first_attempt}");
    assert!(
        suggested_lines
            .iter()
            .any(|line| line["account"] == json!("330") && line["amount"] == json!("10000")),
        "{first_attempt}"
    );
    assert_eq!(hint["debit_total"], hint["credit_total"], "{first_attempt}");

    // hint のとおり `auto_tax_lines` を立てて送り直す。
    let posted = server
        .call_tool(
            "post_journal_entry",
            json!({
                "entry_date": "2026-04-15",
                "description": "A社への請求",
                "lines": [
                    { "account": "135", "side": "debit",  "amount": "110000" },
                    { "account": "500", "side": "credit", "amount": "100000",
                      "tags": { "tax_category": "SALES_10" } }
                ],
                "auto_tax_lines": true
            }),
        )
        .await;
    assert!(!is_error(&posted), "{posted}");
    let entry_1 = entry_id_of(&posted);
    // 確定後の明細が返る（何が記録されたかを AI が確認できる）。
    assert_eq!(body(&posted)["lines"].as_array().expect("配列").len(), 3);
    assert_eq!(body(&posted)["debit_total"], json!("110000"));
    assert_eq!(body(&posted)["credit_total"], json!("110000"));

    // -----------------------------------------------------------------
    // 3. 読み戻す
    // -----------------------------------------------------------------
    let read_back = server
        .call_tool("get_entry", json!({ "entry_id": entry_1 }))
        .await;
    assert!(!is_error(&read_back), "{read_back}");
    assert_eq!(body(&read_back)["entry_date"], json!("2026-04-15"));
    assert_eq!(body(&read_back)["description"], json!("A社への請求"));
    assert_eq!(body(&read_back)["lines"], body(&posted)["lines"]);
    // まだ誰も訂正していない（キーごと出ない）。
    assert!(body(&read_back).get("reversed_by").is_none(), "{read_back}");

    // -----------------------------------------------------------------
    // 4. 2件目を記帳する（★桁を1つ間違える★）
    // -----------------------------------------------------------------
    let wrong = server
        .call_tool(
            "post_journal_entry",
            json!({
                "entry_date": "2026-05-08",
                "description": "B社への請求",
                "lines": [
                    { "account": "135", "side": "debit",  "amount": "550000" },
                    { "account": "500", "side": "credit", "amount": "500000",
                      "tags": { "tax_category": "SALES_10" } }
                ],
                "auto_tax_lines": true
            }),
        )
        .await;
    assert!(!is_error(&wrong), "{wrong}");
    let entry_2 = entry_id_of(&wrong);

    // -----------------------------------------------------------------
    // 5. 検索・元帳・試算表 — ここで間違いに気づく
    // -----------------------------------------------------------------
    let found = server
        .call_tool("search_entries", json!({ "description": "請求" }))
        .await;
    assert!(!is_error(&found), "{found}");
    assert_eq!(body(&found)["total_matches"], json!(2), "{found}");
    // 切れていないことが応答から分かる（無言の truncation にしない）。
    assert_eq!(body(&found)["has_more"], json!(false), "{found}");

    let ledger_before = server
        .call_tool(
            "get_ledger",
            json!({ "account": "500", "from": "2026-01-01", "to": "2026-12-31" }),
        )
        .await;
    assert!(!is_error(&ledger_before), "{ledger_before}");
    // ★AI が「500,000 のはずがない」と気づく地点★
    assert_eq!(body(&ledger_before)["closing_balance"], json!("600000"));
    assert_eq!(body(&ledger_before)["total_lines"], json!(2));

    let trial_before = server
        .call_tool(
            "get_trial_balance",
            json!({ "from": "2026-01-01", "to": "2026-12-31" }),
        )
        .await;
    assert!(!is_error(&trial_before), "{trial_before}");
    assert_eq!(body(&trial_before)["debit_total"], json!("660000"));
    assert_eq!(
        body(&trial_before)["debit_total"],
        body(&trial_before)["credit_total"],
        "貸借が一致しない試算表が成功で返っています: {trial_before}"
    );

    // -----------------------------------------------------------------
    // 6. 逆仕訳で訂正する（更新も削除もできない。`CLAUDE.md` §2）
    // -----------------------------------------------------------------
    let reversed = server
        .call_tool(
            "reverse_journal_entry",
            json!({
                "original_id": entry_2,
                "reverse_date": "2026-05-20",
                "reason": "請求金額の桁誤り（550,000 ではなく 55,000）"
            }),
        )
        .await;
    assert!(!is_error(&reversed), "{reversed}");
    let entry_3 = entry_id_of(&reversed);
    assert_eq!(body(&reversed)["reverses"], json!(entry_2));
    assert_eq!(body(&reversed)["description"], json!("【訂正】B社への請求"));

    // -----------------------------------------------------------------
    // 7. ★訂正済みであることが応答から読み取れる★
    // -----------------------------------------------------------------
    //
    // 帳簿は追記のみなので、原仕訳そのものは1バイトも変わらない。それでも
    // 「もう訂正した」ことが読めなければ、AI は同じ仕訳をもう一度訂正しよう
    // として `already_reversed` を踏む（`docs/07-mcp-server.md` §3 の
    // `get_entry`。PR-G レビュー B）。
    let corrected_view = server
        .call_tool("get_entry", json!({ "entry_id": entry_2 }))
        .await;
    assert!(!is_error(&corrected_view), "{corrected_view}");
    assert_eq!(body(&corrected_view)["reversed_by"], json!(entry_3));
    assert!(body(&corrected_view)["reversed_by_entry_no"].is_number());
    // 向きを取り違えていない（この仕訳自身は誰も訂正していない）。
    assert!(body(&corrected_view).get("reverses").is_none());

    // 検索結果でも、取り消された仕訳と赤伝の両方にそれと分かる欄が付く
    // （取り消された仕訳を隠さない。`DECISIONS.md` D-088）。
    let after_reversal = server
        .call_tool("search_entries", json!({ "description": "請求" }))
        .await;
    assert!(!is_error(&after_reversal), "{after_reversal}");
    assert_eq!(body(&after_reversal)["total_matches"], json!(3));
    let entries = body(&after_reversal)["entries"].as_array().expect("配列");
    let cancelled = entries
        .iter()
        .find(|entry| entry["entry_id"] == json!(entry_2))
        .unwrap_or_else(|| panic!("取り消された仕訳が検索から消えています: {after_reversal}"));
    assert_eq!(cancelled["reversed_by"]["entry_id"], json!(entry_3));
    let red_slip = entries
        .iter()
        .find(|entry| entry["entry_id"] == json!(entry_3))
        .unwrap_or_else(|| panic!("赤伝が検索に出ません: {after_reversal}"));
    assert_eq!(red_slip["reverses"], json!(entry_2));
    assert!(
        red_slip["reverse_reason"]
            .as_str()
            .expect("訂正理由")
            .contains("桁誤り"),
        "{red_slip}"
    );

    // -----------------------------------------------------------------
    // 8. 正しい金額で記帳し直す（取引日は取引が起きた日のまま）
    // -----------------------------------------------------------------
    let repost = server
        .call_tool(
            "post_journal_entry",
            json!({
                "entry_date": "2026-05-08",
                "description": "B社への請求（金額訂正後）",
                "lines": [
                    { "account": "135", "side": "debit",  "amount": "55000" },
                    { "account": "500", "side": "credit", "amount": "50000",
                      "tags": { "tax_category": "SALES_10" } }
                ],
                "auto_tax_lines": true
            }),
        )
        .await;
    assert!(!is_error(&repost), "{repost}");
    let entry_4 = entry_id_of(&repost);

    // -----------------------------------------------------------------
    // 9. 帳簿が意図どおりの姿になっている
    // -----------------------------------------------------------------
    let trial_after = server
        .call_tool(
            "get_trial_balance",
            json!({ "from": "2026-01-01", "to": "2026-12-31" }),
        )
        .await;
    assert!(!is_error(&trial_after), "{trial_after}");
    let rows = body(&trial_after)["rows"].as_array().expect("配列");
    let sales = rows
        .iter()
        .find(|row| row["account"] == json!("500"))
        .unwrap_or_else(|| panic!("売上高の行が無い: {trial_after}"));
    // 100,000（A社）+ 50,000（B社の訂正後）。誤記帳ぶんは赤伝で相殺されている。
    assert_eq!(sales["balance"], json!("150000"), "{trial_after}");
    let tax = rows
        .iter()
        .find(|row| row["account"] == json!("330"))
        .unwrap_or_else(|| panic!("仮受消費税等の行が無い: {trial_after}"));
    assert_eq!(tax["balance"], json!("15000"), "{trial_after}");

    let ledger_after = server
        .call_tool(
            "get_ledger",
            json!({ "account": "500", "from": "2026-01-01", "to": "2026-12-31" }),
        )
        .await;
    assert!(!is_error(&ledger_after), "{ledger_after}");
    let ledger = body(&ledger_after);
    assert_eq!(ledger["closing_balance"], json!("150000"), "{ledger}");
    assert_eq!(ledger["total_lines"], json!(4), "{ledger}");
    // 並びは取引日 → 仕訳番号 → 仕訳ID → 明細行番号。訂正のための追記が
    // 過去日に入るので、赤伝（5/20）は最後に来る。
    let ledger_rows = ledger["rows"].as_array().expect("配列");
    let order: Vec<&Value> = ledger_rows.iter().map(|row| &row["entry_id"]).collect();
    assert_eq!(
        order,
        vec![
            &json!(entry_1),
            &json!(entry_2),
            &json!(entry_4),
            &json!(entry_3)
        ],
        "{ledger}"
    );
    // 残高の推移（期首残高からの累計）。赤伝の行で戻る。
    let running: Vec<&Value> = ledger_rows
        .iter()
        .map(|row| &row["running_balance"])
        .collect();
    assert_eq!(
        running,
        vec![
            &json!("100000"),
            &json!("600000"),
            &json!("650000"),
            &json!("150000")
        ],
        "{ledger}"
    );
    // ★赤伝の行だけを見て「なぜ取り消されたか」が読める★
    // （`search_entries` に引き直さなくてよい。D-088）。
    let red_row = ledger_rows
        .iter()
        .find(|row| row["entry_id"] == json!(entry_3))
        .expect("赤伝の行");
    assert!(red_row["reverse_reason"]
        .as_str()
        .expect("訂正理由")
        .contains("桁誤り"));

    // -----------------------------------------------------------------
    // 9-b. 決算書（B/S・P/L）を同じ帳簿から組み立てる（D-093）
    // -----------------------------------------------------------------
    //
    // 試算表（read model の SQL 集計）と決算書（帳簿のドメインモデルから
    // 組み立て直したもの）は経路が完全に別である。**同じ帳簿から同じ数字が
    // 出ることをここで確かめる**——食い違えばどちらかにバグがある。
    let statements = server
        .call_tool(
            "get_statements",
            json!({ "from": "2026-01-01", "to": "2026-12-31" }),
        )
        .await;
    assert!(!is_error(&statements), "{statements}");
    let st = body(&statements);

    // 記帳した4件すべてが集計対象に入っている。
    assert_eq!(st["entry_count"], json!(4), "{st}");
    assert_eq!(st["first_entry_date"], json!("2026-04-15"), "{st}");

    // 損益計算書の収益が試算表の売上高と一致する（別経路・同じ数字）。
    let revenue = st["income_statement"]["sections"]
        .as_array()
        .expect("区分の配列")
        .iter()
        .find(|section| section["title"] == json!("収益"))
        .unwrap_or_else(|| panic!("収益の区分が無い: {st}"));
    assert_eq!(
        revenue["subtotal"],
        json!("150000"),
        "試算表の売上高（150000）と食い違っています: {st}"
    );

    // 期首残高の注記が出る。この帳簿には 1/1 付けの開始仕訳が無く、
    // 最初の仕訳は 4/10 なので、貸借対照表は前期繰越を含んでいない
    // （実装の誤りではない。呼び出し側が気づけるように伝えている）。
    let note = st["balance_sheet_note"]
        .as_str()
        .unwrap_or_else(|| panic!("期首残高の注記が出ていない: {st}"));
    assert!(note.contains("2026-04-15"), "{note}");
    assert!(note.contains("期首残高"), "{note}");

    // -----------------------------------------------------------------
    // 9-c. 決算振替仕訳を提案させる（D-094）
    // -----------------------------------------------------------------
    //
    // 提案するだけで記帳はしない。**提案が返っただけで決算が済んだと
    // 読み違えられるのが最も危険**なので、応答がそう読めない形になっている
    // ことをここで確かめる。
    let closing = server
        .call_tool("propose_closing_entries", json!({ "fiscal_year": 2026 }))
        .await;
    assert!(!is_error(&closing), "{closing}");
    let cl = body(&closing);

    assert_eq!(
        cl["posted"],
        json!(false),
        "記帳していないことを必ず言う: {cl}"
    );
    assert_eq!(cl["period_start"], json!("2026-01-01"), "{cl}");
    assert_eq!(cl["period_end"], json!("2026-12-31"), "{cl}");
    assert_eq!(cl["entry_count"], json!(4), "{cl}");

    // 収益（売上高 150,000）が残っているので提案が1本出る。
    let proposals = cl["proposals"].as_array().expect("提案の配列");
    assert_eq!(proposals.len(), 1, "{cl}");
    assert_eq!(proposals[0]["entry_date"], json!("2026-12-31"), "{cl}");

    // 明細は post_journal_entry にそのまま渡せる形（呼び出し側が組み替えると
    // 写し間違いが起きる）。売上高 500 を借方に落とす明細が含まれる。
    let lines = proposals[0]["lines"].as_array().expect("明細の配列");
    let sales_line = lines
        .iter()
        .find(|line| line["account"] == json!("500"))
        .unwrap_or_else(|| panic!("売上高をゼロ化する明細が無い: {cl}"));
    assert_eq!(sales_line["side"], json!("debit"), "{cl}");
    assert_eq!(sales_line["amount"], json!("150000"), "{cl}");

    // 次の手が示されている（`CLAUDE.md` §11）。
    assert!(
        cl["next_step"]
            .as_str()
            .expect("next_step")
            .contains("post_journal_entry"),
        "{cl}"
    );

    // 帳簿はまだ4件のまま（提案では増えない）。
    assert_eq!(journal_entry_count(&app).await, 4);

    // -----------------------------------------------------------------
    // 9-d. 減価償却費を提案させる（D-109）
    // -----------------------------------------------------------------
    //
    // この帳簿には固定資産を1件も登録していない。**0件は成功**である。
    // ただし「台帳が空」と「その年度に償却するものが無い」で次にやることが
    // 違うので、AI が区別できることをここで確かめる。
    let depreciation = server
        .call_tool(
            "propose_depreciation_entries",
            json!({ "fiscal_year": 2026 }),
        )
        .await;
    assert!(
        !is_error(&depreciation),
        "0件はエラーにしない: {depreciation}"
    );
    let dp = body(&depreciation);

    assert_eq!(
        dp["posted"],
        json!(false),
        "記帳していないことを必ず言う: {dp}"
    );
    assert_eq!(dp["asset_count"], json!(0), "台帳が空であること: {dp}");
    assert_eq!(dp["proposals"].as_array().expect("提案の配列").len(), 0);
    assert!(
        dp["next_step"]
            .as_str()
            .expect("next_step")
            .contains("登録がありません"),
        "台帳が空だと分かる案内であること（「償却するものが無い」とは別）: {dp}"
    );
    assert_eq!(dp["period_start"], json!("2026-01-01"), "{dp}");
    assert_eq!(dp["period_end"], json!("2026-12-31"), "{dp}");

    // ── 証憑を探す（Phase 4）──────────────────────────────
    //
    // この帳簿には証憑を1件も登録していない。**0件は成功**であり、
    // エラーにしない。ただし「1件も登録されていない」のか「条件に合わなかった」
    // のかを AI が区別できるよう、total_registered を添える。
    let documents = server
        .call_tool(
            "search_documents",
            json!({ "date_from": "2026-01-01", "date_to": "2026-12-31" }),
        )
        .await;
    assert!(!is_error(&documents), "0件はエラーにしない: {documents}");
    let docs = body(&documents);
    assert_eq!(docs["count"], json!(0), "{docs}");
    assert_eq!(
        docs["total_registered"],
        json!(0),
        "1件も登録されていないことが読み取れること: {docs}"
    );
    assert_eq!(docs["documents"], json!([]), "{docs}");

    // ── 取り込んだ明細を見る（Phase 4）────────────────────
    //
    // この帳簿には明細を1件も取り込んでいない。**0件は成功**である。
    // ただし「全部片付いた」のか「まだ1件も取り込んでいない」のかで、
    // 次にやることが正反対になる。counts の合計がそれを分ける。
    let pending = server
        .call_tool("list_pending_transactions", json!({}))
        .await;
    assert!(!is_error(&pending), "0件はエラーにしない: {pending}");
    let txs = body(&pending);
    assert_eq!(txs["count"], json!(0), "{txs}");
    assert_eq!(
        txs["counts"]["total"],
        json!(0),
        "まだ取り込んでいないことが読み取れること: {txs}"
    );
    assert_eq!(txs["transactions"], json!([]), "{txs}");
    assert_eq!(txs["status"], json!("pending"), "既定は未処理: {txs}");

    // **惜しい指定を黙って0件にしない。** `Pending` が通ると、条件に
    // 合わないだけなのに「未処理は無い」と読み違える。
    let near_miss = server
        .call_tool("list_pending_transactions", json!({ "status": "Pending" }))
        .await;
    assert!(is_error(&near_miss), "拒否されること: {near_miss}");

    // ── 明細を仕訳にする（Phase 4）──────────────────────
    //
    // 取り込んだ明細を1件用意し、仕訳にする。**記帳と状態遷移が1つの
    // まとまりとして起きる**ことを、実バイナリの経路で確かめる。
    let imported_id = seed_imported_line(&app).await;

    // 桁を落とした仕訳は止まる。貸借は合っているので、ここで止めなければ
    // 帳簿に入ってしまい、決算書を見ても分からない。
    let dropped_digit = server
        .call_tool(
            "journalize_transaction",
            json!({
                "imported_tx_id": imported_id,
                "lines": [
                    { "account": "609", "side": "debit", "amount": "1980",
                      "tags": { "tax_category": "PURCHASE_10_NON_QUALIFIED" } },
                    { "account": "110", "side": "credit", "amount": "1980" }
                ]
            }),
        )
        .await;
    assert!(
        is_error(&dropped_digit),
        "桁落ちは止めること: {dropped_digit}"
    );
    // 止めたのだから、帳簿も明細も動いていない。
    assert_eq!(journal_entry_count(&app).await, 4);

    // 過去の記帳を根拠に候補を出す。**記帳はしない。**
    // この帳簿には同じ摘要の記帳がまだ無いので候補は空になる——0件は
    // 異常ではなく、「似た取引が無い」と読めることが要る。
    let suggested = server
        .call_tool(
            "suggest_journal_entry",
            json!({ "imported_tx_id": imported_id }),
        )
        .await;
    assert!(!is_error(&suggested), "0件はエラーにしない: {suggested}");
    let suggestion = body(&suggested);
    assert_eq!(suggestion["has_suggestion"], json!(false), "{suggestion}");
    // 提案では帳簿が動かない。
    assert_eq!(journal_entry_count(&app).await, 4);

    let journalized = server
        .call_tool(
            "journalize_transaction",
            json!({
                "imported_tx_id": imported_id,
                "lines": [
                    { "account": "609", "side": "debit", "amount": "19800",
                      "tags": { "tax_category": "PURCHASE_10_NON_QUALIFIED" } },
                    { "account": "110", "side": "credit", "amount": "19800" }
                ]
            }),
        )
        .await;
    assert!(!is_error(&journalized), "{journalized}");
    let posted = body(&journalized);
    // 取引日と摘要は明細から採る（AI に組み立てさせない）。
    assert_eq!(posted["entry_date"], json!("2026-06-15"), "{posted}");
    assert_eq!(posted["imported_tx_id"], json!(imported_id), "{posted}");
    assert_eq!(journal_entry_count(&app).await, 5, "帳簿が1件増えること");

    // 明細は仕訳済みになり、未処理からは消える。
    let after = server
        .call_tool("list_pending_transactions", json!({}))
        .await;
    let after = body(&after);
    assert_eq!(after["count"], json!(0), "{after}");
    assert_eq!(after["counts"]["journalized"], json!(1), "{after}");

    server.shutdown().await;

    // -----------------------------------------------------------------
    // 10. ★全操作が audit_log に記録されている★
    // -----------------------------------------------------------------
    //
    // 帳簿に残るのは5件（原仕訳も誤記帳も消えない。`CLAUDE.md` §2）。
    // 4件はこの通しで AI が起こしたもの、1件は取り込んだ明細から起こしたもの。
    assert_eq!(journal_entry_count(&app).await, 5);

    let calls = audited_calls(&audit_rows(&app).await);
    assert_eq!(
        calls,
        vec![
            ("get_settings".to_string(), "ok".to_string()),
            ("list_accounts".to_string(), "ok".to_string()),
            ("list_tax_categories".to_string(), "ok".to_string()),
            ("suggest_tax_category".to_string(), "ok".to_string()),
            ("validate_invoice_number".to_string(), "ok".to_string()),
            // 失敗した記帳も残る（「AI が何をしようとしたか」。D-070）。
            ("post_journal_entry".to_string(), "error".to_string()),
            ("post_journal_entry".to_string(), "ok".to_string()),
            ("get_entry".to_string(), "ok".to_string()),
            ("post_journal_entry".to_string(), "ok".to_string()),
            ("search_entries".to_string(), "ok".to_string()),
            ("get_ledger".to_string(), "ok".to_string()),
            ("get_trial_balance".to_string(), "ok".to_string()),
            ("reverse_journal_entry".to_string(), "ok".to_string()),
            ("get_entry".to_string(), "ok".to_string()),
            ("search_entries".to_string(), "ok".to_string()),
            ("post_journal_entry".to_string(), "ok".to_string()),
            ("get_trial_balance".to_string(), "ok".to_string()),
            ("get_ledger".to_string(), "ok".to_string()),
            ("get_statements".to_string(), "ok".to_string()),
            ("propose_closing_entries".to_string(), "ok".to_string()),
            ("propose_depreciation_entries".to_string(), "ok".to_string()),
            ("search_documents".to_string(), "ok".to_string()),
            ("list_pending_transactions".to_string(), "ok".to_string()),
            ("list_pending_transactions".to_string(), "error".to_string()),
            ("journalize_transaction".to_string(), "error".to_string()),
            ("suggest_journal_entry".to_string(), "ok".to_string()),
            ("journalize_transaction".to_string(), "ok".to_string()),
            ("list_pending_transactions".to_string(), "ok".to_string()),
        ],
        "呼び出した順に「開始・結果の2行」が並ぶこと（読み取り系も同じ経路）"
    );

    // ★MC-11 の「全11ツールに対して総当たり」★
    //
    // 一覧を**テスト側に手で書かない**（`PROGRESS.md` Phase 1 の教訓6
    // 「手で維持する一覧は必ず腐る」）。レジストリから導出するので、
    // ツールを1つ足してこの通しで呼ばなければここが落ちる。
    let exercised: BTreeSet<String> = calls.into_iter().map(|(tool, _)| tool).collect();
    let registered: BTreeSet<String> = kaikei_mcp::server::registered_tool_names()
        .into_iter()
        .collect();
    assert_eq!(
        exercised, registered,
        "登録済みのツールがこの通しで1度も呼ばれていません。\
         ツールを足したら、この通し E2E にも組み込むこと\
         （ROADMAP.md Phase 3 の完了条件「全操作が audit_log に記録される」）"
    );
}

// ---------------------------------------------------------------------------
// 削除系ツールが存在しない（MC-10 をプロトコルの入口から見る）
// ---------------------------------------------------------------------------

/// `tools/list` に禁止4件が現れず、その名前で `tools/call` すると
/// **プロトコルエラー**になり、`audit_log` にも帳簿にも痕跡が残らない。
///
/// # なぜレジストリの検査だけでは足りないのか
///
/// `crates/kaikei-mcp/tests/forbidden_tools.rs` は
/// `kaikei_mcp::server::{registered_tool_names, is_registered_tool}` を見る。
/// これは `tools/list` が返すのと**同じ集合**だが、それを確かめているのは
/// `dispatch.rs` の手書き実装の**読み**であって、実際のプロトコル応答では
/// ない（同ファイルは「実バイナリに送って確かめる経路が別にある」と書いて
/// いたが、PR-I まで**その経路は存在しなかった**）。
///
/// # 未知のツール名だけはプロトコルエラーでよい
///
/// `docs/07-mcp-server.md` §6 が認めている唯一の例外である
/// （ツール呼び出しに**到達できない**異常）。到達しないので
/// `audit_log` にも行は残らない——`AuditCall.tool` にクライアント由来の
/// 文字列を載せないための構造（`ToolName::resolve`）の裏返しである（§9）。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn the_forbidden_tools_are_invisible_and_unreachable_through_the_protocol(
    _pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts.clone()).await;
    let mut server = McpServer::start(&conn_opts).await;

    // (1) `tools/list` に現れない。
    let listed = server.list_tools().await;
    let names: BTreeSet<String> = listed
        .iter()
        .map(|tool| {
            tool["name"]
                .as_str()
                .unwrap_or_else(|| panic!("tools/list の要素に name が無い: {tool}"))
                .to_string()
        })
        .collect();
    for forbidden in FORBIDDEN_TOOLS {
        assert!(
            !names.contains(forbidden),
            "存在させないツールが tools/list に出ています: {forbidden}"
        );
    }
    // 空の一覧に対して緑になっていないこと（レジストリと同じ集合であること）。
    assert_eq!(
        names,
        kaikei_mcp::server::registered_tool_names()
            .into_iter()
            .collect::<BTreeSet<String>>(),
        "tools/list の応答がレジストリと食い違っています"
    );

    // (2) その名前で `tools/call` すると呼び出しに到達しない。
    for forbidden in FORBIDDEN_TOOLS {
        let message = server
            .raw_request("tools/call", json!({ "name": forbidden, "arguments": {} }))
            .await;
        assert!(
            message.get("error").is_some(),
            "{forbidden} が呼び出せています: {message}"
        );
        assert!(message.get("result").is_none(), "{forbidden}: {message}");
    }

    server.shutdown().await;

    assert_eq!(journal_entry_count(&app).await, 0);
    assert!(
        audit_rows(&app).await.is_empty(),
        "ツール呼び出しに到達していないのに audit_log に行があります\
         （クライアント由来の名前が tool 列に載っている疑い。§9）"
    );
}

// ---------------------------------------------------------------------------
// 拒否のされ方（MC-04 / MC-05 / MC-09 を実バイナリ経由で見る）
// ---------------------------------------------------------------------------

/// 存在しない科目コード・締め済み期間・JSON number の金額。
///
/// いずれも `crates/kaikei-e2e/tests/mcp_write_tools.rs` が
/// `dispatch::call` を直接呼ぶ形で見ているが、**実バイナリ経由では
/// 通っていなかった**。とくに MC-09（金額を JSON number で渡す）は
/// 経路が本質的に違う——`rmcp` のトランスポートが JSON をパースし、
/// `dispatch::call` が `with_audit` の**操作の中**でデシリアライズする
/// （`Parameters<T>` を却下した理由そのもの。D-085）。ここが退行すると
/// 「入力エラーだけ監査ログに残らない」形になる。
#[sqlx::test(migrations = "../kaikei-store/migrations")]
async fn the_refusals_keep_their_next_step_through_the_real_binary(
    _pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let app = common::app_pool(conn_opts.clone()).await;

    // Phase 3 に `close_period` は無いので、締め状態は `period_snapshots` へ
    // 直接 INSERT して作る（`kaikei_app` は INSERT 権限を持つ。MC-05）。
    sqlx::query(
        "INSERT INTO period_snapshots \
         (fiscal_year, period_end, closed_at, balances, currency, currency_minor_unit, \
          entry_count, last_entry_no, checksum) \
         VALUES (2026, DATE '2026-03-31', now(), '{}'::jsonb, 'JPY', 0, 0, 0, 'x')",
    )
    .execute(&app)
    .await
    .expect("締め状態を作れること");

    let mut server = McpServer::start(&conn_opts).await;

    // MC-04: 存在しない科目コードには候補が付く（全件を返さない）。
    let unknown_account = server
        .call_tool(
            "post_journal_entry",
            json!({
                "entry_date": "2026-04-15",
                "description": "科目コードの打ち間違い",
                "lines": [
                    { "account": "136", "side": "debit",  "amount": "1000" },
                    { "account": "500", "side": "credit", "amount": "1000",
                      "tags": { "tax_category": "SALES_10" } }
                ]
            }),
        )
        .await;
    assert!(is_error(&unknown_account), "{unknown_account}");
    assert_eq!(body(&unknown_account)["error"], json!("unknown_account"));
    let candidates = body(&unknown_account)["hint"]["candidate_accounts"]
        .as_array()
        .unwrap_or_else(|| panic!("候補が無い: {unknown_account}"));
    assert!(
        !candidates.is_empty() && candidates.len() <= 5,
        "{unknown_account}"
    );

    // MC-05: 締め済み期間への記帳は拒否される。
    let closed = server
        .call_tool(
            "post_journal_entry",
            json!({
                "entry_date": "2026-02-10",
                "description": "締め済み期間への記帳",
                "lines": [
                    { "account": "135", "side": "debit",  "amount": "1000" },
                    { "account": "500", "side": "credit", "amount": "1000",
                      "tags": { "tax_category": "SALES_10" } }
                ]
            }),
        )
        .await;
    assert!(is_error(&closed), "{closed}");
    assert_eq!(body(&closed)["error"], json!("period_closed"), "{closed}");

    // MC-09: 金額を JSON number で渡すと**日本語**のエラーになる。
    let number_amount = server
        .call_tool(
            "post_journal_entry",
            json!({
                "entry_date": "2026-04-15",
                "description": "金額を number で渡す",
                "lines": [
                    { "account": "135", "side": "debit",  "amount": 1000 },
                    { "account": "500", "side": "credit", "amount": "1000" }
                ]
            }),
        )
        .await;
    assert!(is_error(&number_amount), "{number_amount}");
    // 「入力を直せば通る」拒否である（サーバ都合の失敗と混同させない）。
    assert_eq!(body(&number_amount)["error"], json!("rejected"));
    let text = body(&number_amount)["message"].as_str().expect("message");
    assert!(text.contains("金額は文字列で渡してください"), "{text}");
    assert!(
        !text.contains("invalid type"),
        "英語の型エラーに落ちています: {text}"
    );

    server.shutdown().await;

    assert_eq!(journal_entry_count(&app).await, 0, "帳簿が変わっています");
    // ★3件とも監査ログに2行ずつ残る★
    //
    // MC-09 は `dispatch::call` の**外側**でデシリアライズする実装
    // （`Parameters<T>`）に退行すると、この呼び出しだけが 0 行になる。
    assert_eq!(
        audited_calls(&audit_rows(&app).await),
        vec![
            ("post_journal_entry".to_string(), "error".to_string()),
            ("post_journal_entry".to_string(), "error".to_string()),
            ("post_journal_entry".to_string(), "error".to_string()),
        ],
    );
}
