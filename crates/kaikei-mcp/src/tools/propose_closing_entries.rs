//! `propose_closing_entries` — 決算振替仕訳の提案（`docs/07-mcp-server.md` §3）。
//!
//! # 提案するだけで、記帳はしない
//!
//! 返すのは仕訳の**案**であり、帳簿には何も書かない。記帳するかどうかは
//! 呼び出し側（AI と、その先の人間）の判断で、記帳するときは
//! `post_journal_entry` を通る。`suggest_tax_category` と同じ「候補と根拠まで、
//! 確定は人間に残す」形である（`DECISIONS.md` D-087、`CLAUDE.md` §10）。
//!
//! 応答には `posted: false` を必ず入れる。**「提案が返った＝決算が済んだ」と
//! 誤解されるのが最も危険な読み違い**であり、決算が済んでいないことに
//! 気づかないまま確定申告に進むと帳簿と申告が食い違う。
//!
//! # `close_period`（締め）とは別物である
//!
//! こちらは仕訳を提案するだけで、期間の締め（`period_snapshots` への記録・
//! 以後の記帳の拒否）は行わない。`close_period` は Phase 4 以降のままである
//! （§2。checksum の計算式と canonical JSON の定義が Phase 5 の
//! `kaikei verify` と揃っている必要がある）。
//!
//! # 二重の決算振替は自然に防がれる
//!
//! 既に決算振替が記帳されている年度では、収益・費用がゼロ化されているので
//! **提案が空になる**。空であることは異常ではないので、エラーにはしない。
//! ただし「帳簿がその年度で空」なのか「既に決算が済んでいる」のかは
//! 読み手が区別できる必要があるので、`entry_count` を必ず返す。

use kaikei_app::error::AppError;
use kaikei_app::policy::ProposedEntry;
use kaikei_app::tx::with_tx_err;
use kaikei_app::usecase::closing::{self, ClosingInput, ClosingOutput};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::dispatch::{McpTool, ToolContext, ToolFailure, ToolSuccess};
use crate::error::ToolError;
use crate::wire::lines_to_json;

/// `propose_closing_entries`。
pub struct ProposeClosingEntries;

// ★この構造体の doc コメントは `tools/list` の応答に出る★
// 内部設計書への参照・crate 名・Markdown の強調記法を書かないこと。
/// 決算振替仕訳を提案する会計年度。指定していないキーは受け付けません。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProposeClosingEntriesInput {
    /// 決算する会計年度（西暦の年）。必須です。
    /// 会計年度は暦年なので、この年の 1 月 1 日から 12 月 31 日までの仕訳が
    /// 集計対象になります。集計期間を日付で指定することはできません。
    pub fiscal_year: i32,
}

impl McpTool for ProposeClosingEntries {
    type Input = ProposeClosingEntriesInput;

    const NAME: &'static str = "propose_closing_entries";

    const DESCRIPTION: &'static str = "\
指定した会計年度の帳簿から、決算振替仕訳（収益と費用の残高をゼロにし、\
元入金へ振り替える仕訳）を組み立てて提案します。\
提案するだけで、帳簿には何も記帳しません（応答の posted は常に false です）。\
記帳する場合は、返ってきた明細をそのまま post_journal_entry に渡してください。\
その際に貸借一致・締め状態・タグの検証を受けます。\
集計対象はその年の 1 月 1 日から 12 月 31 日までの仕訳です。\
集計期間を日付で指定することはできません。\
既に決算振替を記帳した年度では、収益と費用の残高がゼロになっているため\
提案は空になります。これは異常ではありません。\
帳簿にその年度の仕訳が無い場合も提案は空になるので、\
どちらであるかは entry_count で判別してください。\
期間の締め（以後の記帳を拒否する操作）はこのツールでは行いません。\
事業主貸と事業主借は元入金へ振り替えません。\
当年度末と翌年期首のどちらで振り替えるか、\
振替仕訳を起こすか期首残高として設定するかが決まっていないためで、実装漏れではありません。\
応答の scope_note を読んでください。\
決算の内容が税務上適切かどうかの判断はこのサーバーでは行いません。";

    async fn run(ctx: &ToolContext<'_>, input: Self::Input) -> Result<ToolSuccess, ToolFailure> {
        let composition = ctx.composition();
        let tag_schema = composition.tag_catalog.schema().clone();
        let policy = composition.closing_policy.clone();

        let output: ClosingOutput = with_tx_err(ctx.store(), move |tx| {
            let tag_schema = tag_schema.clone();
            let policy = policy.clone();
            Box::pin(async move {
                closing::execute(
                    tx,
                    &policy,
                    &tag_schema,
                    ClosingInput {
                        fiscal_year: input.fiscal_year,
                    },
                )
                .await
            })
        })
        .await
        .map_err(|error: AppError| ToolError::from_app_error(&error))?;

        let body = success_body(input.fiscal_year, &output);
        let summary = audit_summary(&body);
        Ok(ToolSuccess::new(body).with_audit_summary(summary))
    }
}

/// 成功応答。
///
/// 金額はすべて**区切り無しの文字列**（`docs/07-mcp-server.md` §5）。
fn success_body(fiscal_year: i32, output: &ClosingOutput) -> Map<String, Value> {
    let mut body = Map::new();
    body.insert("fiscal_year".to_string(), json!(fiscal_year));
    body.insert(
        "period_start".to_string(),
        json!(output.period_start.to_iso_string()),
    );
    body.insert(
        "period_end".to_string(),
        json!(output.period_end.to_iso_string()),
    );
    body.insert("entry_count".to_string(), json!(output.entry_count));

    // ★提案が返っただけでは決算は済んでいない★
    // このキーは常に false で、条件によって省いたりしない——「有るときだけ
    // 出す」形にすると、無いことに意味があるのか単に忘れたのかが読めない。
    body.insert("posted".to_string(), json!(false));

    let proposals: Vec<Value> = output.proposals.iter().map(proposal_to_json).collect();
    body.insert("proposals".to_string(), Value::Array(proposals));

    // **当年度末の分と翌年期首の分を別のキーにする。** 混ぜると、日付の
    // 違う2本の仕訳が1つの配列に並び、どちらをいつ記帳するのかが読めない。
    let opening: Vec<Value> = output
        .opening_proposals
        .iter()
        .map(proposal_to_json)
        .collect();
    body.insert("opening_proposals".to_string(), Value::Array(opening));

    body.insert(
        "next_step".to_string(),
        json!(next_step(output.proposals.len(), output.entry_count)),
    );

    // ★2本の仕訳の日付が違うことを必ず言う★（`DECISIONS.md` D-102）
    //
    // `proposals` は当年度末（12/31）、`opening_proposals` は翌年期首（1/1）に
    // 記帳する。同じ日に両方入れると、青色申告決算書の貸借対照表から
    // 事業主貸・事業主借が消える（様式には両方の欄がある）。
    //
    // `posted` と同じく**条件によって省かない**——無いことに意味があるのか
    // 単に忘れたのかが読めなくなる。
    body.insert("scope_note".to_string(), json!(SCOPE_NOTE));
    body
}

/// 2つの提案の使い分け（`DECISIONS.md` D-102）。
const SCOPE_NOTE: &str = "提案は2本あり、記帳する日が違います。proposals は当年度末（12月31日）に記帳する分で、収益と費用を元入金へ振り替えます。opening_proposals は翌年の1月1日に記帳する分で、事業主貸と事業主借をゼロにして差額を元入金へ振り替えます。opening_proposals を当年度末に記帳しないでください。青色申告決算書の貸借対照表には事業主貸と事業主借の欄があり、期末残高をそのまま書く様式なので、年内にゼロにすると様式と食い違います。減価償却費・家事按分の年次調整・棚卸は、この提案には含まれません。";

/// 次の手（`CLAUDE.md` §11）。
///
/// 提案が空のときに「何も無かった」で終わらせない。**空の理由は2つあり、
/// 次にすることが違う**——帳簿が空なら記帳から、決算済みなら何もしなくてよい。
fn next_step(proposal_count: usize, entry_count: usize) -> String {
    if proposal_count > 0 {
        return "提案された明細を post_journal_entry に渡すと記帳されます。\
                記帳するまで決算振替は帳簿に反映されません"
            .to_string();
    }
    if entry_count == 0 {
        return "この会計年度には仕訳が1件もありません。\
                年度の指定を確認するか、記帳してからもう一度実行してください"
            .to_string();
    }
    "この会計年度には仕訳がありますが、ゼロにすべき収益・費用の残高が\
     残っていません。決算振替が既に記帳されている可能性があります。\
     search_entries で年度末の仕訳を確認してください"
        .to_string()
}

/// 提案1件の JSON。
///
/// 仕訳IDも仕訳番号も持たない（まだ記帳されていないので採番されていない）。
/// **`post_journal_entry` の入力にそのまま渡せる形**にしておく——
/// 呼び出し側が明細を組み替える必要があると、そこで写し間違いが起きる。
fn proposal_to_json(proposal: &ProposedEntry) -> Value {
    json!({
        "entry_date": proposal.entry_date.to_iso_string(),
        "description": proposal.description,
        "lines": lines_to_json(&proposal.lines),
    })
}

/// `audit_log.output` に残す要約（`DECISIONS.md` D-089 決定6）。
///
/// 明細を落とし、件数・期間・`posted` は残す。要約は**応答本文から落として
/// 作る**（別に組み立てると値が食い違う）。
fn audit_summary(body: &Map<String, Value>) -> Map<String, Value> {
    let mut summary = body.clone();
    let proposal_count = summary
        .get("proposals")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    summary.remove("proposals");
    summary.insert("proposal_count".to_string(), json!(proposal_count));
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_core::{AccountCode, AccountingDate, Currency, JournalLine, Money, Side, TagSet};

    fn date(year: i32, month: u8, day: u8) -> AccountingDate {
        AccountingDate::new(year, month, day).unwrap()
    }

    /// **本命。** 何を振り替えていないかを必ず言う。
    ///
    /// この提案は収益・費用しか振り替えない。そうと知らずに記帳すると、
    /// 翌年度へ持ち越された事業主貸・事業主借の残高を見て「決算が失敗した」
    /// と読む。実装漏れではないことも併せて伝える。
    /// 注記が「2本あって記帳する日が違う」ことを言う。
    ///
    /// 期首振替を当年度末に記帳すると、青色申告決算書の貸借対照表から
    /// 事業主貸・事業主借が消える（様式には両方の欄がある）。追記型なので
    /// 記帳してからでは戻せない。
    #[test]
    fn the_response_says_what_it_does_not_transfer() {
        let output = ClosingOutput {
            period_start: date(2026, 1, 1),
            period_end: date(2026, 12, 31),
            entry_count: 10,
            proposals: vec![proposal()],
            opening_proposals: Vec::new(),
        };

        let body = success_body(2026, &output);

        let note = body["scope_note"].as_str().expect("scope_note があること");
        assert!(note.contains("事業主貸"), "{note}");
        assert!(note.contains("事業主借"), "{note}");
        assert!(
            note.contains("1月1日"),
            "期首振替をいつ記帳するかを言うこと: {note}"
        );
        assert!(
            note.contains("当年度末に記帳しないでください"),
            "やってはいけないことを言うこと: {note}"
        );
        assert!(
            body["opening_proposals"].is_array(),
            "opening_proposals は常に出すこと（空でも）"
        );
    }

    /// 提案が空でも注記は出る。
    ///
    /// **条件によって省かない**——無いことに意味があるのか単に忘れたのかが
    /// 読めなくなる（`posted` と同じ扱い）。
    #[test]
    fn the_scope_note_is_there_even_when_nothing_is_proposed() {
        let output = ClosingOutput {
            period_start: date(2026, 1, 1),
            period_end: date(2026, 12, 31),
            entry_count: 0,
            proposals: Vec::new(),
            opening_proposals: Vec::new(),
        };

        let body = success_body(2026, &output);

        assert!(body.contains_key("scope_note"), "{body:?}");
    }

    /// 説明にも、事業主貸・事業主借を振り替えないことが書いてある。
    ///
    /// 応答を読む前に「決算振替＝全部やってくれる」と思われると、
    /// 記帳してから気づくことになる。
    #[test]
    fn the_description_says_the_owner_accounts_are_left_alone() {
        let description = ProposeClosingEntries::DESCRIPTION;
        assert!(description.contains("事業主貸"), "{description}");
        assert!(description.contains("scope_note"), "{description}");
    }

    fn proposal() -> ProposedEntry {
        ProposedEntry {
            entry_date: date(2026, 12, 31),
            description: "決算振替".to_string(),
            lines: vec![
                JournalLine::new(
                    AccountCode::parse("500").unwrap(),
                    Side::Debit,
                    Money::from_minor(150_000, Currency::JPY),
                    TagSet::new(),
                    None,
                )
                .unwrap(),
                JournalLine::new(
                    AccountCode::parse("400").unwrap(),
                    Side::Credit,
                    Money::from_minor(150_000, Currency::JPY),
                    TagSet::new(),
                    None,
                )
                .unwrap(),
            ],
        }
    }

    fn output(proposals: Vec<ProposedEntry>, entry_count: usize) -> ClosingOutput {
        ClosingOutput {
            proposals,
            opening_proposals: Vec::new(),
            entry_count,
            period_start: date(2026, 1, 1),
            period_end: date(2026, 12, 31),
        }
    }

    // 提案が返っても記帳はされていない。`posted` は常に出る。
    #[test]
    fn the_response_always_says_it_is_not_posted() {
        for out in [output(vec![proposal()], 4), output(Vec::new(), 0)] {
            let body = success_body(2026, &out);
            assert_eq!(
                body.get("posted"),
                Some(&json!(false)),
                "posted を省かないこと: {body:?}"
            );
        }
    }

    // 明細は `post_journal_entry` にそのまま渡せる形で返る。
    #[test]
    fn the_lines_are_shaped_for_post_journal_entry() {
        let body = success_body(2026, &output(vec![proposal()], 4));
        let lines = body["proposals"][0]["lines"]
            .as_array()
            .expect("明細の配列");

        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["account"], json!("500"));
        assert_eq!(lines[0]["side"], json!("debit"));
        // 金額は桁区切りの無い文字列（§5）。
        assert_eq!(lines[0]["amount"], json!("150000"));
    }

    // 提案が空のとき、理由によって次の手が変わる。
    #[test]
    fn an_empty_proposal_explains_which_of_the_two_reasons_it_is() {
        let empty_book = success_body(2026, &output(Vec::new(), 0));
        let already_closed = success_body(2026, &output(Vec::new(), 12));

        let a = empty_book["next_step"].as_str().unwrap();
        let b = already_closed["next_step"].as_str().unwrap();
        assert!(a.contains("仕訳が1件もありません"), "{a}");
        assert!(b.contains("既に記帳されている可能性"), "{b}");
        assert_ne!(a, b, "2つの空を同じ文言で説明しないこと");
    }

    // 提案があるときは記帳の手順を案内する。
    #[test]
    fn a_non_empty_proposal_points_at_post_journal_entry() {
        let body = success_body(2026, &output(vec![proposal()], 4));
        let next = body["next_step"].as_str().unwrap();
        assert!(next.contains("post_journal_entry"), "{next}");
    }

    // 監査ログの要約は明細を落とし、件数に置き換える。
    #[test]
    fn the_audit_summary_replaces_the_proposals_with_a_count() {
        let body = success_body(2026, &output(vec![proposal()], 4));
        let summary = audit_summary(&body);

        assert!(summary.get("proposals").is_none());
        assert_eq!(summary.get("proposal_count"), Some(&json!(1)));
        assert_eq!(summary.get("posted"), Some(&json!(false)));
        assert_eq!(summary.get("entry_count"), Some(&json!(4)));
        // 本文側は落とさない（要約は複製に対して行う）。
        assert!(body.get("proposals").is_some());
    }

    // 応答本文に `warnings` キーを使わない（dispatch 層が予約している）。
    #[test]
    fn the_body_does_not_use_the_reserved_warnings_key() {
        let body = success_body(2026, &output(vec![proposal()], 4));
        assert!(body.get("warnings").is_none());
    }

    // 説明文が禁止表現を含まず、記帳されないことと締めでないことに触れている。
    #[test]
    fn the_description_says_it_neither_posts_nor_closes() {
        let description = ProposeClosingEntries::DESCRIPTION;
        for forbidden in ["準拠", "法令対応", "JIIMA"] {
            assert!(!description.contains(forbidden), "{forbidden}");
        }
        assert!(description.contains("記帳しません"));
        assert!(description.contains("post_journal_entry"));
        assert!(description.contains("締め"));
        assert!(description.contains("判断はこのサーバーでは行いません"));
    }
}
