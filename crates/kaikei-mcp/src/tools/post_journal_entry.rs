//! `post_journal_entry` — 仕訳を起こす（`docs/07-mcp-server.md` §3）。
//!
//! # このファイルに監査ログの手順は無い
//!
//! 開始レコード → 操作 → 結果レコードは [`crate::dispatch::call`] が行う。
//! [`crate::dispatch::ToolContext`] は [`kaikei_app::ports::AuditSink`] を
//! 露出しないので、ここで監査ログを書くことも書き忘れることもできない
//! （`DECISIONS.md` D-084）。
//!
//! # 貸借不一致のときに `hint` を返す
//!
//! `docs/07-mcp-server.md` §3 が定める形。`auto_tax_lines: false` で
//! 貸借不一致になった場合に限り、**同じ明細を `auto_tax_lines: true` にして
//! [`kaikei_app::usecase::post_entry::preview`]（dry-run）を呼び直す**。
//! `Ok` なら確定後の明細を `hint.suggested_lines` に載せる。
//! `Err` でも、policy が積んだ注記があれば `hint.policy_notes` として渡す
//! （税込経理・免税事業者の設定では税額行が生成されないので `preview` も
//! 失敗するが、その**理由**は注記に入っている。渡さないと AI に届くのは
//! 差額だけになる。PR-F レビュー C-1）。注記も無ければ `hint` を返さない。
//!
//! **MCP 層で `with_tx` を開いて `load_posting_context` を呼び `TaxContext` を
//! 自前で組み立てない**（同 §3・§4。PR-B 1巡目で実際にそう書けてしまい、
//! コンパイルもテストも通った）。`preview` は `execute` と同じ関数
//! （`prepare` / `build_entry`）を通るので、検証の順序が乖離しない。

use kaikei_app::amount::strip_thousands_separators;
use kaikei_app::context::BookSettings;
use kaikei_app::currency::currency_from_code;
use kaikei_app::error::AppError;
use kaikei_app::id::entry_id_to_uuid_string;
use kaikei_app::ports::ChartRepo;
use kaikei_app::tx::{with_tx, with_tx_err};
use kaikei_app::usecase::post_entry::{self, PostEntryFailure, PostEntryInput, PostEntryOutput};
use kaikei_app::wire::side_from_code;
use kaikei_core::{AccountCode, AccountingDate, ChartOfAccounts, CoreError, JournalLine};
use kaikei_jp::compose::Composition;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

use crate::dispatch::{McpTool, ToolContext, ToolFailure, ToolSuccess};
use crate::error::ToolError;
use crate::tools::{core_error, parse_date};
use crate::wire::{lines_to_json, policy_notes_to_json, AmountStr};

/// 存在しない科目コードに対して挙げる候補の上限。
///
/// 全件を返さない（同梱テンプレートだけでも数十件あり、AI に読ませる意味が
/// 無いうえ応答と `audit_log` が膨らむ）。`docs/07-mcp-server.md` §10 MC-04 が
/// 「件数の上限を決める」と定めている。
const MAX_ACCOUNT_CANDIDATES: usize = 5;

/// `post_journal_entry`。
pub struct PostJournalEntry;

// ★この構造体の doc コメントは `tools/list` の応答に出る★
//
// `schemars` は doc コメントをそのまま `inputSchema` の `description` に
// 載せるので、ここに書いた文章は**AI が読む面**である。内部設計書への参照
// （`docs/...` / `CLAUDE.md` §n）・crate 名・Markdown の強調記法を書かない
// こと（PR-F レビュー D-2。`server.rs` の
// `every_input_schema_description_is_written_for_the_caller` が検査する）。
// 実装上の理由は、doc ではなくこの形の `//` コメントに書く。
//
// - `document_ids` は無い。証憑の紐付けは Phase 4 の `attach_document`。
//   知らないキーを黙って捨てると「証憑を付けたつもり」で記帳が成功して
//   しまうので `deny_unknown_fields` で拒否する（D-085）。
// - 形は `docs/07-mcp-server.md` §3。
/// 仕訳1件の入力。指定していないキーは受け付けません。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PostJournalEntryInput {
    // 記帳日ではない（`CLAUDE.md` §7）。
    /// 取引日。YYYY-MM-DD の形式で指定します。仕訳を記録した日ではなく、
    /// 取引そのものが発生した日です。
    pub entry_date: String,

    // 空文字・空白のみは `kaikei-core` が拒否する。
    /// 摘要。空文字や空白のみは受け付けません。
    pub description: String,

    /// 仕訳明細。2行以上を指定します。auto_tax_lines を使う場合は、
    /// 消費税額の行を含まない元の明細だけを渡します。
    pub lines: Vec<PostJournalEntryLine>,

    /// true にすると消費税額の行の生成を試みます。生成されるかどうかは
    /// 帳簿の設定（税抜経理か税込経理か・課税事業者か）で決まります。
    /// 生成されなかった場合は応答の policy_notes にその旨が入ります。
    #[serde(default)]
    pub auto_tax_lines: bool,
}

/// 仕訳明細1行。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PostJournalEntryLine {
    // ★実装済みのツールにしか誘導しない★（PR-F レビュー3巡目 D-1）
    //
    // 初版は「list_accounts で取得できるコードを指定します」と書いていたが、
    // 当時 `list_accounts` は未実装で `tools/list` に出ていなかった。指示どおり
    // 呼ぶと `-32602 tool not found` が返り、AI からは「サーバが壊れている」
    // ようにしか見えない（D-038 の誤診クラス）。**この面に書いてよいのは、
    // その時点で実際に呼べるツールだけである。**
    //
    // PR-G で `list_accounts` を登録したので、誘導を戻した（PR-F からの
    // 申し送り）。この文言が指すツールが登録されていることは、
    // `server.rs` の `no_description_points_the_caller_at_a_tool_that_is_not_registered`
    // が機械的に検査する（同じ事故を「戻し忘れ」の側からも塞ぐ）。
    /// 勘定科目コード。帳簿に登録されている科目コードを指定します
    /// （例: "135"）。使えるコードは list_accounts で取得できます。
    /// 登録されていないコードを指定した場合はエラーになり、
    /// コードが近い記帳可能な科目が候補として返ります。
    pub account: String,

    /// 借方なら debit、貸方なら credit を指定します。
    pub side: String,

    /// 金額。文字列で指定します（例: "110000"）。JSON の number は
    /// 受け付けません。
    pub amount: AmountStr,

    /// 通貨コード（例: "JPY"）。省略すると帳簿の通貨を使います。
    /// 1つの仕訳の中で通貨を混在させることはできません。
    #[serde(default)]
    pub currency: Option<String>,

    /// この明細だけに付ける備考。
    #[serde(default)]
    pub memo: Option<String>,

    // ★重複キーは検出できない★（PR-F レビュー B-2）
    //
    // MCP のリクエストは rmcp の stdio トランスポートが受け取った時点で
    // `serde_json` により丸ごとパースされており、`CallToolRequestParams`
    // の `arguments` は既に `serde_json::Map` である。つまり
    // `{"tax_category":"SALES_10","tax_category":"SALES_8_REDUCED"}` の
    // ような重複キーは**この層に届く前に後勝ちで畳み込まれている**。
    // ここで受け型を工夫しても検出できない（生の JSON テキストが無い）。
    // `JpError::DuplicateTagKeyInInput` は MCP 経由では到達不能であり、
    // 他の呼び出し元（CLI / kaikei-api）からのみ到達する。
    // 詳細は `DECISIONS.md` D-085 と `docs/07-mcp-server.md` §3。
    /// タグ。キーも値も文字列で指定します（例: {"tax_category": "SALES_10"}）。
    /// 同じキーを2回指定した場合、後に書いた指定だけが使われます。
    #[serde(default)]
    pub tags: BTreeMap<String, String>,
}

/// 記帳に渡す値のうち、失敗したときの `hint` 組み立てにも要るもの。
struct PostRequest {
    entry_date: AccountingDate,
    description: String,
    lines: Vec<JournalLine>,
    auto_tax_lines: bool,
}

impl McpTool for PostJournalEntry {
    type Input = PostJournalEntryInput;

    const NAME: &'static str = "post_journal_entry";

    const DESCRIPTION: &'static str = "\
複式簿記の仕訳を1件起こします。金額は文字列で指定します（例: \"110000\"。\
JSON の number は受け付けません）。entry_date は取引日（YYYY-MM-DD）で、\
記帳日ではありません。貸借が一致しない仕訳は記帳されません。\
1つの仕訳の中で通貨を混在させることはできません。\
記帳した仕訳は更新も削除もできず、訂正は reverse_journal_entry（逆仕訳）で行います。\
auto_tax_lines を true にすると消費税額の行の生成を試みますが、\
生成されるかどうかは事業者設定（税抜経理か・課税事業者か）で決まります。\
応答の policy_notes には確認すべき注記が入ることがあります。";

    async fn run(ctx: &ToolContext<'_>, input: Self::Input) -> Result<ToolSuccess, ToolFailure> {
        let composition = ctx.composition();
        let settings = ctx.book_settings();

        let entry_date = parse_date("entry_date", &input.entry_date)?;
        let lines = build_lines(&composition, &settings, &input.lines)?;

        let request = PostRequest {
            entry_date,
            description: input.description,
            lines,
            auto_tax_lines: input.auto_tax_lines,
        };

        let post_input = PostEntryInput {
            entry_date: request.entry_date,
            description: request.description.clone(),
            lines: request.lines.clone(),
            auto_tax_lines: request.auto_tax_lines,
        };

        let id_gen = ctx.id_gen();
        let clock = ctx.clock();
        let posted = with_tx_err(ctx.store(), move |tx| {
            Box::pin(async move {
                post_entry::execute(
                    tx,
                    &composition.tax_policy,
                    composition.tag_catalog.schema(),
                    &id_gen,
                    &clock,
                    &settings,
                    post_input,
                )
                .await
            })
        })
        .await;

        match posted {
            Ok(output) => Ok(success(&output)),
            Err(failure) => Err(describe_failure(ctx, &request, failure).await.into()),
        }
    }
}

/// 成功応答（`docs/07-mcp-server.md` §3）。
///
/// **確定後の明細を必ず返す**（AI が「何が記録されたか」を確認できるように
/// するため）。`policy_notes` は `kaikei-policy` が組み立てた文言をそのまま
/// 素通しする（`CLAUDE.md` §10）。
fn success(output: &PostEntryOutput) -> ToolSuccess {
    let entry = &output.entry;
    let mut body = Map::new();
    body.insert(
        "entry_id".to_string(),
        json!(entry_id_to_uuid_string(entry.id())),
    );
    // 件数・年度・仕訳番号は金額ではないので JSON number のままでよい（§5）。
    body.insert("entry_no".to_string(), json!(entry.entry_no().as_u32()));
    body.insert("fiscal_year".to_string(), json!(entry.fiscal_year()));
    body.insert(
        "entry_date".to_string(),
        json!(entry.entry_date().to_iso_string()),
    );
    body.insert("description".to_string(), json!(entry.description()));
    body.insert("lines".to_string(), lines_to_json(entry.lines()));
    body.insert(
        "debit_total".to_string(),
        json!(AmountStr::from_money(&entry.debit_total()).as_str()),
    );
    body.insert(
        "credit_total".to_string(),
        json!(AmountStr::from_money(&entry.credit_total()).as_str()),
    );
    body.insert(
        "policy_notes".to_string(),
        policy_notes_to_json(&output.notes),
    );

    ToolSuccess::new(body).with_entry_id(entry.id())
}

/// 線上の明細を [`JournalLine`] にする。
///
/// エラーには**何行目か**を添える（`CLAUDE.md` §11。下位層の文言は
/// 言い換えず、位置情報だけを足す）。
fn build_lines(
    composition: &Composition,
    settings: &BookSettings,
    lines: &[PostJournalEntryLine],
) -> Result<Vec<JournalLine>, ToolError> {
    let mut built = Vec::with_capacity(lines.len());
    for (index, line) in lines.iter().enumerate() {
        built.push(build_line(composition, settings, line).map_err(|error| at_line(index, error))?);
    }
    Ok(built)
}

fn build_line(
    composition: &Composition,
    settings: &BookSettings,
    line: &PostJournalEntryLine,
) -> Result<JournalLine, ToolError> {
    let account = AccountCode::parse(&line.account).map_err(core_error)?;
    let side = side_from_code(&line.side).map_err(core_error)?;
    // 通貨を省略したら帳簿通貨。未知のコードは桁数を推測せずエラーになる
    // （`CLAUDE.md` §8。`Currency::new(code, 0)` を書いて迂回しない）。
    let currency = match &line.currency {
        Some(code) => currency_from_code(code).map_err(core_error)?,
        None => settings.book_currency,
    };
    let amount = line.amount.to_money(currency).map_err(core_error)?;
    // タグの型付けと未登録キーの判定は `kaikei-jp` が持つ
    // （同じ判定を MCP 層に書き直さない。D-072）。
    //
    // 重複キーの判定（`JpError::DuplicateTagKeyInInput`）も同じ関数が
    // 持っているが、**MCP 経由では到達しない**。重複は rmcp の
    // トランスポートが JSON をパースする時点で既に畳み込まれているためで、
    // ここで受け型を工夫しても検出できない（B-2。D-085 の訂正注記）。
    let tags = composition
        .tag_catalog
        .parse_tag_set(line.tags.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .map_err(|error| ToolError::from_jp_error(&error))?;

    JournalLine::new(account, side, amount, tags, line.memo.clone()).map_err(core_error)
}

/// 「明細の何行目か」を本文と `line` 欄に添える（1 始まり）。
fn at_line(index: usize, error: ToolError) -> ToolError {
    let number = index + 1;
    ToolError::new(
        error.code(),
        format!("明細 {number} 行目: {}", error.message()),
    )
    .with_detail("line", json!(number))
}

/// 失敗応答を組み立てる（`docs/07-mcp-server.md` §3）。
///
/// **`policy_notes` は失敗時にも出す。** 注記が最も要るのは失敗したとき
/// である——「税込経理の設定のため税額行を生成していません」が無いと、
/// AI は「貸借不一致」だけを見て**金額を書き換える**という誤った修正に進む
/// （§1 ③ が空文になる）。
async fn describe_failure(
    ctx: &ToolContext<'_>,
    request: &PostRequest,
    failure: PostEntryFailure,
) -> ToolError {
    let mut error = ToolError::new(failure.code(), failure.public_message())
        .with_detail("policy_notes", policy_notes_to_json(&failure.notes));

    match &failure.error {
        AppError::Core(CoreError::Unbalanced {
            debit,
            credit,
            diff,
        }) => {
            // 機械可読フィールドは区切り無し。区切り付きが残るのは
            // `message` の文中だけである（§5）。
            error = error
                .with_detail("debit_total", json!(strip_thousands_separators(debit)))
                .with_detail("credit_total", json!(strip_thousands_separators(credit)))
                .with_detail("difference", json!(strip_thousands_separators(diff)));
            if !request.auto_tax_lines {
                if let Some(hint) = tax_line_hint(ctx, request).await {
                    error = error.with_detail("hint", hint);
                }
            }
            // `auto_tax_lines: true` で不一致だった場合は、税額行を足す提案が
            // できない。そのときに AI へ渡せるのは
            // `failure.notes`（policy が積んだ注記）であり、それは上の
            // `policy_notes` に既に載っている。
        }
        AppError::Core(CoreError::UnknownAccount { code }) => {
            if let Some(hint) = account_hint(ctx, code).await {
                error = error.with_detail("hint", hint);
            }
        }
        // `AppError` は `#[non_exhaustive]`。受け皿が必須である（§6）。
        _ => {}
    }

    error
}

/// `auto_tax_lines: false` で貸借不一致になったときの `hint`。
///
/// 帳簿には一切触れない（`preview` は採番も INSERT も行わない）。
///
/// # 税額行が生成されない設定でも「次の手ゼロ」にしない
///
/// `preview`（`auto_tax_lines: true` の dry-run）が成功すれば、確定後の明細を
/// `suggested_lines` として返す。**失敗しても、policy が積んだ注記があれば
/// それを返す**——税込経理・免税事業者の設定では `derive_tax_lines` が
/// 税額行を生成しないため `preview` も同じ貸借不一致で失敗するが、
/// そのとき `PostEntryFailure::notes` には
/// 「税込経理の設定のため税額行を生成していません」が積まれている。
/// これを渡さないと、AI に届くのは差額だけになり、**金額を書き換える**という
/// 誤った修正に進む（`docs/07-mcp-server.md` §3。PR-F レビュー C-1）。
///
/// **判定そのものを MCP 層に書かない。** 「どういう設定なら税額行が
/// 生成されないか」を知っているのは `kaikei-jp` であり、ここは
/// その注記を運ぶだけである（`DECISIONS.md` D-072）。
async fn tax_line_hint(ctx: &ToolContext<'_>, request: &PostRequest) -> Option<Value> {
    let composition = ctx.composition();
    let settings = ctx.book_settings();
    let clock = ctx.clock();
    let preview_input = PostEntryInput {
        entry_date: request.entry_date,
        description: request.description.clone(),
        lines: request.lines.clone(),
        auto_tax_lines: true,
    };

    let preview = with_tx_err(ctx.store(), move |tx| {
        Box::pin(async move {
            post_entry::preview(
                tx,
                &composition.tax_policy,
                composition.tag_catalog.schema(),
                &clock,
                &settings,
                preview_input,
            )
            .await
        })
    })
    .await;

    match preview {
        Ok(preview) => Some(json!({
            "message": "auto_tax_lines を true にして同じ明細を渡すと、\
                        下の suggested_lines の内容で貸借が一致します。\
                        この明細をそのまま lines に指定して auto_tax_lines を false の\
                        ままにしても同じ結果になります。どちらにするかの判断は\
                        このサーバーでは行いません",
            "suggested_lines": lines_to_json(&preview.lines),
            "debit_total": AmountStr::from_money(&preview.debit_total).as_str(),
            "credit_total": AmountStr::from_money(&preview.credit_total).as_str(),
            "policy_notes": policy_notes_to_json(&preview.notes),
        })),
        // 税額行を足しても一致しない。理由（注記）があるならそれを渡す。
        Err(failure) if !failure.notes.is_empty() => Some(json!({
            "message": "auto_tax_lines を true にしても貸借は一致しません。\
                        この帳簿の設定について policy から次の注記が出ています。\
                        金額の見直しが必要か、消費税額の行を lines に明示する必要が\
                        あるかの判断はこのサーバーでは行いません",
            "policy_notes": policy_notes_to_json(&failure.notes),
        })),
        Err(_) => None,
    }
}

/// 存在しない科目コードに対する候補（`docs/07-mcp-server.md` §10 MC-04）。
///
/// `CoreError::UnknownAccount` のメッセージは「勘定科目が見つかりません:
/// {code}」だけであり、**core に候補一覧を持たせない**（core の変更は人間の
/// 承認事項。`CLAUDE.md` §1）。候補の組み立てはこの層の仕事である。
async fn account_hint(ctx: &ToolContext<'_>, code: &str) -> Option<Value> {
    let chart = with_tx(ctx.store(), |tx| {
        Box::pin(async move { Ok(tx.load_chart().await?) })
    })
    .await
    .ok()?;

    let candidates = similar_accounts(&chart, code);

    // 候補が0件でも hint を返す。
    //
    // 前方一致で絞っているので、**1文字も共有しないコードでは候補が空になる**
    // （同梱テンプレートは 100〜690 しか無いので、0 / 7 / 8 / 9 で始まる
    // コードは全滅する。他社の科目表の癖で 800 番台を打つのはごく普通の
    // 間違いである）。そこで `None` を返すと、AI に届くのは
    // 「勘定科目が見つかりません: 800」だけになり**次の手が無くなる**
    // （`CLAUDE.md` §11）。候補が挙げられないときは、
    // 一覧の引き方そのものを次の手として返す。
    if candidates.is_empty() {
        return Some(json!({
            "message": format!(
                "勘定科目 {code} は勘定科目マスタにありません。\
                 コードの先頭が一致する科目が1つも無いため候補は挙げられません。\
                 list_accounts で登録済みの科目コードを確認してください。\
                 どの科目を使うかの判断はこのサーバーでは行いません"
            ),
            "candidate_accounts": Value::Array(Vec::new()),
        }));
    }

    Some(json!({
        "message": format!(
            "勘定科目 {code} は勘定科目マスタにありません。\
             コードが近い記帳可能な科目を最大 {MAX_ACCOUNT_CANDIDATES} 件挙げます。\
             list_accounts で一覧を確認することもできます。\
             どの科目を使うかの判断はこのサーバーでは行いません"
        ),
        "candidate_accounts": Value::Array(candidates),
    }))
}

/// 科目コードの**前方一致の長さ**が大きい順に、記帳可能な科目を挙げる。
///
/// 1文字も一致しない科目は挙げない（無関係な科目を並べても次の手にならない）。
fn similar_accounts(chart: &ChartOfAccounts, code: &str) -> Vec<Value> {
    let mut scored: Vec<(usize, &kaikei_core::AccountDef)> = chart
        .iter()
        .filter(|def| def.postable)
        .filter_map(|def| {
            let shared = common_prefix_len(def.code.as_str(), code);
            (shared > 0).then_some((shared, def))
        })
        .collect();
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.code.as_str().cmp(b.1.code.as_str()))
    });

    scored
        .into_iter()
        .take(MAX_ACCOUNT_CANDIDATES)
        .map(|(_, def)| {
            json!({
                "account": def.code.as_str(),
                "name": def.name,
                "account_type": kaikei_app::wire::account_type_code(def.account_type),
            })
        })
        .collect()
}

/// 先頭から何文字一致するか（`char` 単位）。
fn common_prefix_len(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_core::{AccountDef, AccountType};

    fn parse(json: &str) -> Result<PostJournalEntryInput, serde_json::Error> {
        serde_json::from_str(json)
    }

    // 設計書 §3 の例がそのまま受理される。
    #[test]
    fn the_documented_request_shape_is_accepted() {
        let input = parse(
            r#"{
                "entry_date": "2026-04-15",
                "description": "A社への請求",
                "lines": [
                    { "account": "135", "side": "debit",  "amount": "110000" },
                    { "account": "500", "side": "credit", "amount": "100000",
                      "memo": "4月分",
                      "tags": { "tax_category": "SALES_10", "counterparty": "CP0001" } }
                ],
                "auto_tax_lines": true
            }"#,
        )
        .expect("設計書 §3 の例は受理されるはず");

        assert_eq!(input.entry_date, "2026-04-15");
        assert!(input.auto_tax_lines);
        assert_eq!(input.lines.len(), 2);
        assert_eq!(input.lines[1].tags.len(), 2);
        assert_eq!(input.lines[0].amount.as_str(), "110000");
    }

    // MC-09 (1): 金額を JSON number で渡すと**日本語**のエラーになる。
    #[test]
    fn an_amount_given_as_a_json_number_is_rejected_in_japanese() {
        let err = parse(
            r#"{
                "entry_date": "2026-04-15",
                "description": "A社への請求",
                "lines": [ { "account": "135", "side": "debit", "amount": 110000 } ]
            }"#,
        )
        .expect_err("number は受理しない");
        let message = err.to_string();
        assert!(
            message.contains("金額は文字列で渡してください"),
            "{message}"
        );
        assert!(!message.contains("invalid type"), "{message}");
    }

    // 知らないキーを黙って捨てない（`document_ids` は Phase 4 の別ツール）。
    #[test]
    fn an_unknown_field_is_rejected_instead_of_being_dropped() {
        let err = parse(
            r#"{
                "entry_date": "2026-04-15",
                "description": "A社への請求",
                "lines": [],
                "document_ids": ["0192a7b3-1234-7abc-8def-0123456789ab"]
            }"#,
        )
        .expect_err("未知のキーは受理しない");
        assert!(err.to_string().contains("document_ids"), "{err}");
    }

    // `auto_tax_lines` の既定は false（「指定しなければ税額行を作らない」）。
    #[test]
    fn auto_tax_lines_defaults_to_false() {
        let input = parse(r#"{"entry_date":"2026-04-15","description":"x","lines":[]}"#).unwrap();
        assert!(!input.auto_tax_lines);
    }

    // 明細のエラーは何行目かを添える（下位層の文言は言い換えない）。
    #[test]
    fn a_line_error_carries_the_one_based_line_number() {
        let error = at_line(
            1,
            ToolError::new(
                kaikei_app::error::codes::INVALID_VALUE,
                "side の値が不正です",
            ),
        );
        assert!(
            error.message().starts_with("明細 2 行目: "),
            "{}",
            error.message()
        );
        assert!(error.message().contains("side の値が不正です"));
        assert_eq!(error.to_json()["line"], json!(2));
    }

    fn chart() -> ChartOfAccounts {
        ChartOfAccounts::new(vec![
            AccountDef {
                code: AccountCode::parse("130").unwrap(),
                name: "売掛金".to_string(),
                account_type: AccountType::Asset,
                parent: None,
                postable: true,
            },
            AccountDef {
                code: AccountCode::parse("135").unwrap(),
                name: "未収入金".to_string(),
                account_type: AccountType::Asset,
                parent: None,
                postable: true,
            },
            AccountDef {
                code: AccountCode::parse("500").unwrap(),
                name: "売上高".to_string(),
                account_type: AccountType::Revenue,
                parent: None,
                postable: true,
            },
            AccountDef {
                code: AccountCode::parse("900").unwrap(),
                name: "見出し".to_string(),
                account_type: AccountType::Expense,
                parent: None,
                postable: false,
            },
        ])
        .unwrap()
    }

    // 候補はコードの前方一致が長い順で、記帳できない科目は挙げない。
    #[test]
    fn account_candidates_are_ranked_by_shared_prefix_and_exclude_headings() {
        let candidates = similar_accounts(&chart(), "136");
        let codes: Vec<&str> = candidates
            .iter()
            .map(|c| c["account"].as_str().unwrap())
            .collect();
        assert_eq!(codes, vec!["130", "135"]);
        assert!(!codes.contains(&"900"));
        assert!(!codes.contains(&"500"));
    }

    // 1文字も一致しない場合は候補を出さない（無関係な羅列にしない）。
    #[test]
    fn no_candidate_is_offered_when_nothing_shares_a_prefix() {
        assert!(similar_accounts(&chart(), "AAA").is_empty());
    }

    // 候補は上限を超えない。
    #[test]
    fn account_candidates_are_capped() {
        let defs: Vec<AccountDef> = (0..20)
            .map(|i| AccountDef {
                code: AccountCode::parse(&format!("1{i:02}")).unwrap(),
                name: format!("科目{i}"),
                account_type: AccountType::Asset,
                parent: None,
                postable: true,
            })
            .collect();
        let chart = ChartOfAccounts::new(defs).unwrap();
        assert_eq!(
            similar_accounts(&chart, "199").len(),
            MAX_ACCOUNT_CANDIDATES
        );
    }

    // 説明文が `CLAUDE.md` §10 の禁止表現を含まず、§11 の「次の手」を含む。
    #[test]
    fn the_description_avoids_forbidden_claims_and_states_the_next_step() {
        let description = PostJournalEntry::DESCRIPTION;
        for forbidden in ["準拠", "法令対応", "JIIMA"] {
            assert!(!description.contains(forbidden), "{forbidden}");
        }
        assert!(description.contains("文字列"));
        assert!(description.contains("reverse_journal_entry"));
    }
}
