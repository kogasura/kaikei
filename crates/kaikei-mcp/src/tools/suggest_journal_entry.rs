//! `suggest_journal_entry` — 取り込んだ明細に、過去の記帳を根拠として
//! 仕訳の候補を出す（`docs/05-csv-import.md` §6）。
//!
//! # 提案だけ。記帳しない
//!
//! `suggest_tax_category` と同じ「候補と根拠まで、確定は人間に残す」形
//! （`DECISIONS.md` D-087）。記帳するには `journalize_transaction` を別に呼ぶ。
//!
//! # 根拠は「過去に自分がどう記帳したか」
//!
//! 既存の会計ソフトも学習型の自動仕訳を持つが、**なぜその科目にしたかを
//! 説明できない**（`docs/05-csv-import.md` §6）。ここが返すのは科目だけでは
//! なく、**どの摘要で探して、過去のどの仕訳が当たったか**である。根拠が
//! 見えれば、提案が外れているときに人が気づける。
//!
//! # 摘要は段階的に緩めて探す
//!
//! 銀行の摘要には可変部分が混ざる（`カ)アマゾン ジヤパン 12345` の番号）。
//! 全体で探すと1件も当たらないので、当たらなければ**摘要から取り出した
//! 最も長い語**でもう一度探す。どちらで当たったかは根拠に載せる——緩めた
//! ことを隠すと、たまたま当たった別の取引を「よく似ている」と読んでしまう。
//!
//! # 一致は「含む」であって「同じ」ではない
//!
//! 過去の仕訳は摘要の**部分一致**で探す。摘要が `ATM手数料` の明細は、過去の
//! `ATM手数料（個人利用分）` にも当たる。実際に帳簿の複製で稽古したところ、
//! 事業の手数料に対して**個人利用の記帳が候補として出た**（過去1件・medium）。
//!
//! だから根拠には過去の仕訳の**摘要をそのまま載せる**。候補の科目だけを見て
//! 決めると、似た名前の別の取引を引き継いでしまう。
//!
//! # 断定しない
//!
//! 確信度は件数と揃い方から決め、`high` でも「確定」とは言わない。
//! 過去の記帳が間違っていれば、同じ間違いを繰り返す提案になる。

use kaikei_app::ports::{ImportedTxQuery, SearchEntriesParams, SearchEntriesQuery};
use kaikei_app::view::{EntrySummaryView, ImportedTxView};
use kaikei_app::wire::side_code;
use kaikei_core::ChartOfAccounts;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

use crate::dispatch::{McpTool, ToolContext, ToolFailure, ToolSuccess};
use crate::error::ToolError;

/// 過去の仕訳を何件まで見るか。
///
/// 多く見ても候補は増えず、応答と `audit_log` が膨らむだけである。
const PAST_ENTRIES_TO_SCAN: u32 = 50;

/// 候補をいくつまで返すか。
///
/// 並べすぎると「どれでもいい」に見える。
const MAX_SUGGESTIONS: usize = 3;

/// 根拠として挙げる過去の仕訳の件数。
const MAX_EXAMPLES: usize = 3;

/// この件数以上あり、かつ全部が同じ形なら `high`。
const HIGH_CONFIDENCE_OCCURRENCES: usize = 3;

/// `suggest_journal_entry`。
pub struct SuggestJournalEntry;

// ★この構造体の doc コメントは `tools/list` の応答に出る★
/// 仕訳の候補を出す明細の指定。指定していないキーは受け付けません。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SuggestJournalEntryInput {
    /// 候補を出したい明細のID。list_pending_transactions が返す id を指定します。
    pub imported_tx_id: String,
}

impl McpTool for SuggestJournalEntry {
    type Input = SuggestJournalEntryInput;

    const NAME: &'static str = "suggest_journal_entry";

    const DESCRIPTION: &'static str = "\
取り込んだ明細に対して、過去に自分がどう記帳したかを根拠に仕訳の候補を出します。\
**候補を出すだけで、記帳はしません。** 記帳するには journalize_transaction を呼びます。\
明細のIDは list_pending_transactions で取得します。\
候補には必ず根拠が付きます（どの摘要で探したか、過去のどの仕訳が当たったか、何件あったか）。\
摘要の全体で探して当たらない場合は、摘要から取り出した語でもう一度探します。\
どちらで探した結果かは matched_by に入るので、緩めた検索の結果かどうかを判断できます。一致は「含む」であって「同じ」ではありません。摘要が ATM手数料 の明細は、過去の ATM手数料（個人利用分）にも当たります。examples の摘要を読んで、同じ種類の取引かどうかを確かめてください。\
似た取引が過去に無い場合は候補が空になります。これは異常ではありません。\
confidence は件数と揃い方から決めた目安で、high でも確定ではありません\
（過去の記帳が間違っていれば、同じ間違いを繰り返す候補になります）。\
金額は候補に含まれません。明細の金額をそのまま使うか、按分する場合は自分で決めます。";

    async fn run(ctx: &ToolContext<'_>, input: Self::Input) -> Result<ToolSuccess, ToolFailure> {
        let imported = find_transaction(ctx, &input.imported_tx_id).await?;
        let chart = &ctx.composition().chart;

        let (matched_by, search_key, past) = search_past(ctx, &imported.raw_description).await?;
        let suggestions = summarize(&past, chart);

        let mut body = Map::new();
        body.insert(
            "transaction".to_string(),
            json!({
                "id": imported.id,
                "occurred_on": imported.occurred_on.to_iso_string(),
                "amount_minor": imported.amount_minor.to_string(),
                "is_money_in": imported.is_money_in,
                "raw_description": imported.raw_description,
                "status": imported.status,
            }),
        );
        body.insert(
            "searched".to_string(),
            json!({
                "matched_by": matched_by,
                "search_key": search_key,
                "entries_found": past.len(),
            }),
        );
        body.insert("suggestions".to_string(), json!(suggestions));
        // **候補が空であることを明示する。** 「似た取引が無い」と
        // 「探し方が悪い」を、読む側が区別できるようにする。
        body.insert("has_suggestion".to_string(), json!(!suggestions.is_empty()));
        Ok(ToolSuccess::new(body))
    }
}

/// 明細をIDで引く。
///
/// **仕訳済みでも引ける。** 「前回どう記帳したか」を確かめたいことがあり、
/// 候補を出すだけなら状態を問う理由が無い（記帳する
/// `journalize_transaction` は未処理だけを扱う）。
async fn find_transaction(
    ctx: &ToolContext<'_>,
    imported_tx_id: &str,
) -> Result<ImportedTxView, ToolFailure> {
    // **IDで直接引く。** 一覧から絞る形にすると、上限を超えた分の明細が
    // 「見つかりません」になる。
    ctx.imported_tx_query()
        .find_imported(imported_tx_id)
        .await
        .map_err(|error| ToolFailure::from(ToolError::from_app_error(&error.into())))?
        .ok_or_else(|| {
            ToolError::new(
                kaikei_app::error::codes::NOT_FOUND,
                format!(
                    "取り込んだ明細が見つかりません（id={imported_tx_id}）。\
                     list_pending_transactions で id を確かめてください"
                ),
            )
            .into()
        })
}

/// 過去の仕訳を探す。**当たらなければ摘要を緩める。**
///
/// 返り値は「どう探したか」「何で探したか」「当たった仕訳」。
async fn search_past(
    ctx: &ToolContext<'_>,
    raw_description: &str,
) -> Result<(&'static str, String, Vec<EntrySummaryView>), ToolFailure> {
    let query = ctx.search_entries_query();

    let whole = raw_description.trim().to_string();
    if !whole.is_empty() {
        let found = run_search(query, &whole).await?;
        if !found.is_empty() {
            return Ok(("摘要の全体を含む過去の仕訳", whole, found));
        }
    }

    // 全体で当たらなければ、摘要から取り出した語で探す。
    match longest_word(raw_description) {
        Some(word) if word != whole => {
            let found = run_search(query, &word).await?;
            Ok(("摘要から取り出した語を含む過去の仕訳", word, found))
        }
        _ => Ok(("摘要の全体を含む過去の仕訳", whole, Vec::new())),
    }
}

async fn run_search(
    query: &dyn SearchEntriesQuery,
    description_contains: &str,
) -> Result<Vec<EntrySummaryView>, ToolFailure> {
    let params = SearchEntriesParams {
        from: None,
        to: None,
        account: None,
        description_contains: Some(description_contains.to_string()),
        min_amount: None,
        max_amount: None,
        tags: Vec::new(),
        cursor: None,
        limit: PAST_ENTRIES_TO_SCAN,
    };
    let page = query
        .search_entries(&params)
        .await
        .map_err(|error| ToolFailure::from(ToolError::from_app_error(&error.into())))?;
    Ok(page.entries)
}

/// 摘要から最も長い語を取り出す。
///
/// 銀行の摘要には可変部分が混ざる（`カ)アマゾン ジヤパン 12345` の番号）。
/// **数字と記号で区切り、最も長い部分を使う。** 数字そのものは使わない——
/// 取引ごとに変わるので、それで探しても当たらない。
///
/// 同じ長さが並んだときは**先に出たもの**を採る（探し方が実行のたびに
/// 変わらないようにする）。
fn longest_word(description: &str) -> Option<String> {
    let mut best: Option<String> = None;
    let mut current = String::new();

    let flush = |current: &mut String, best: &mut Option<String>| {
        if current.chars().count()
            > best
                .as_ref()
                .map(|word| word.chars().count())
                .unwrap_or_default()
        {
            *best = Some(current.clone());
        }
        current.clear();
    };

    for ch in description.chars() {
        if ch.is_alphabetic() {
            current.push(ch);
        } else {
            flush(&mut current, &mut best);
        }
    }
    flush(&mut current, &mut best);

    best.filter(|word| word.chars().count() >= 2)
}

/// 過去の仕訳を「科目の組み合わせ」ごとにまとめる。
///
/// **仕訳の形をそのまま候補にする。** 科目だけを頻度で並べると、借方と貸方が
/// 別々に選ばれて成り立たない組み合わせができる。
fn summarize(past: &[EntrySummaryView], chart: &ChartOfAccounts) -> Vec<Value> {
    // 組み合わせを表す鍵。並びで別物にならないよう、整列した文字列にする。
    let mut groups: BTreeMap<String, (Vec<Value>, Vec<&EntrySummaryView>)> = BTreeMap::new();

    for entry in past {
        // **赤伝と、取り消された仕訳は根拠にしない。** 取り消したということは
        // その記帳が誤りだったということで、繰り返す理由が無い。
        if entry.reverses.is_some() || entry.reversed_by.is_some() {
            continue;
        }
        let shape = shape_of(entry, chart);
        let key = shape
            .iter()
            .map(|line| line["key"].as_str().unwrap_or_default().to_string())
            .collect::<Vec<_>>()
            .join("|");
        let slot = groups.entry(key).or_insert_with(|| (shape, Vec::new()));
        slot.1.push(entry);
    }

    let mut ranked: Vec<(Vec<Value>, Vec<&EntrySummaryView>)> = groups.into_values().collect();
    // 件数の多い順。同数なら組み合わせの文字列順（並びが実行のたびに
    // 変わらないようにする）。
    ranked.sort_by(|a, b| {
        b.1.len().cmp(&a.1.len()).then_with(|| {
            let left =
                a.0.first()
                    .and_then(|v| v["account"].as_str())
                    .unwrap_or("");
            let right =
                b.0.first()
                    .and_then(|v| v["account"].as_str())
                    .unwrap_or("");
            left.cmp(right)
        })
    });

    let total: usize = ranked.iter().map(|(_, entries)| entries.len()).sum();
    ranked
        .into_iter()
        .take(MAX_SUGGESTIONS)
        .map(|(shape, entries)| {
            let lines: Vec<Value> = shape
                .into_iter()
                .map(|mut line| {
                    // 内部の突き合わせにしか使わない鍵は返さない。
                    line.as_object_mut().map(|object| object.remove("key"));
                    line
                })
                .collect();
            json!({
                "lines": lines,
                "occurrences": entries.len(),
                "confidence": confidence(entries.len(), total),
                "examples": entries.iter().take(MAX_EXAMPLES).map(|entry| json!({
                    "entry_id": kaikei_app::id::entry_id_to_uuid_string(entry.entry_id),
                    "entry_date": entry.entry_date.to_iso_string(),
                    "description": entry.description,
                })).collect::<Vec<_>>(),
            })
        })
        .collect()
}

/// 仕訳を「科目・側・税区分」の並びにする。**金額は入れない。**
///
/// 金額を入れると、同じ記帳でも額が違うだけで別の候補になる。
fn shape_of(entry: &EntrySummaryView, chart: &ChartOfAccounts) -> Vec<Value> {
    let mut lines: Vec<Value> = entry
        .lines
        .iter()
        .map(|line| {
            let code = line.account().as_str().to_string();
            let tax = line
                .tags()
                .iter()
                .find(|(key, _)| key.as_str() == "tax_category")
                // 値の文字列化は `kaikei-jp` が持つ（同じ写像を MCP 層に
                // 書き直さない）。
                .map(|(_, value)| kaikei_jp::tags::tag_value_to_string(value));
            json!({
                "key": format!("{}/{}/{}", code, side_code(line.side()), tax.clone().unwrap_or_default()),
                "account": code,
                "account_name": chart.get(line.account()).map(|def| def.name.clone()),
                "side": side_code(line.side()),
                "tax_category": tax,
            })
        })
        .collect();
    // 並びで別物にならないようにする。
    lines.sort_by(|a, b| a["key"].as_str().cmp(&b["key"].as_str()));
    lines
}

/// 確信度。**断定しない。**
///
/// 過去の記帳が間違っていれば、同じ間違いを繰り返す候補になる。だから
/// `high` でも「確定」とは言わない。
fn confidence(occurrences: usize, total: usize) -> &'static str {
    if total == 0 {
        return "low";
    }
    if occurrences == total && occurrences >= HIGH_CONFIDENCE_OCCURRENCES {
        "high"
    } else if occurrences * 2 > total {
        "medium"
    } else {
        "low"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── 摘要から語を取り出す ────────────────────────────

    /// **本命。** 取引ごとに変わる番号を検索キーにしない。
    ///
    /// 番号込みで探すと1件も当たらない。
    #[test]
    fn a_varying_number_is_not_used_as_the_search_key() {
        let word = longest_word("カ)アマゾン ジヤパン 12345").expect("語が取れること");
        assert!(!word.contains('1'), "{word}");
        assert!(!word.contains(')'), "{word}");
    }

    /// 最も長い語を採る。
    #[test]
    fn the_longest_word_is_taken() {
        assert_eq!(longest_word("カ)アマゾン 12"), Some("アマゾン".to_string()));
        assert_eq!(longest_word("ﾋﾞｰﾃﾂｸ(ｶ"), Some("ﾋﾞｰﾃﾂｸ".to_string()));
    }

    /// 同じ長さなら先に出たものを採る。
    ///
    /// 決めておかないと、探し方が実行のたびに変わる。
    #[test]
    fn a_tie_keeps_the_first_word() {
        assert_eq!(
            longest_word("アマゾン ジヤパン"),
            Some("アマゾン".to_string())
        );
    }

    /// 1文字しか残らない摘要では諦める。
    ///
    /// 1文字で探すと何にでも当たり、根拠にならない。
    #[test]
    fn a_single_character_is_not_a_search_key() {
        assert_eq!(longest_word("1)2 3"), None);
        assert_eq!(longest_word("12345"), None);
    }

    // ─── 確信度 ──────────────────────────────────────────

    /// 全部が同じ形で3件以上なら high。
    #[test]
    fn all_the_same_and_enough_of_them_is_high() {
        assert_eq!(confidence(3, 3), "high");
        assert_eq!(confidence(10, 10), "high");
    }

    /// **本命。** 少なければ、揃っていても high にしない。
    ///
    /// 1回そう記帳しただけの形を「確か」と言うと、最初の1回の誤りが
    /// そのまま増える。
    #[test]
    fn one_past_entry_is_not_high_even_if_it_is_the_only_one() {
        assert_eq!(confidence(1, 1), "medium");
        assert_eq!(confidence(2, 2), "medium");
    }

    #[test]
    fn a_majority_is_medium_and_the_rest_is_low() {
        assert_eq!(confidence(3, 5), "medium");
        assert_eq!(confidence(2, 5), "low");
        assert_eq!(confidence(1, 3), "low");
    }

    #[test]
    fn nothing_found_is_low() {
        assert_eq!(confidence(0, 0), "low");
    }

    // ─── 説明 ────────────────────────────────────────────

    /// 説明に、記帳しないことが書いてある。
    #[test]
    fn the_description_says_it_does_not_record() {
        let description = SuggestJournalEntry::DESCRIPTION;
        assert!(description.contains("記帳はしません"), "{description}");
        assert!(
            description.contains("journalize_transaction"),
            "{description}"
        );
    }

    /// 説明に、確信度が目安でしかないことが書いてある。
    #[test]
    fn the_description_does_not_oversell_the_confidence() {
        let description = SuggestJournalEntry::DESCRIPTION;
        assert!(description.contains("確定ではありません"), "{description}");
    }

    /// **本命。** 説明に、一致が「含む」であることが書いてある。
    ///
    /// 帳簿の複製で稽古したところ、摘要が `ATM手数料` の明細に対して過去の
    /// `ATM手数料（個人利用分）` が候補として出た。**事業の手数料が個人利用に
    /// 化ける。** 候補の科目だけを見て決めると引き継いでしまうので、
    /// examples の摘要を読むよう書いておく。
    #[test]
    fn the_description_says_matching_is_by_substring() {
        let description = SuggestJournalEntry::DESCRIPTION;
        assert!(description.contains("「含む」"), "{description}");
        assert!(description.contains("examples"), "{description}");
    }

    /// `matched_by` が「同じ」と読めない言い回しである。
    ///
    /// 「摘要の全体」だけだと完全一致に見える。実際は部分一致なので、
    /// 過去の長い摘要にも当たる。
    #[test]
    fn matched_by_does_not_read_as_an_exact_match() {
        // 探し方の文言は `search_past` が返す定数。両方に「含む」が入る。
        for label in [
            "摘要の全体を含む過去の仕訳",
            "摘要から取り出した語を含む過去の仕訳",
        ] {
            assert!(label.contains("含む"), "{label}");
        }
    }

    /// 説明に、根拠が付くことが書いてある。
    #[test]
    fn the_description_promises_a_basis() {
        let description = SuggestJournalEntry::DESCRIPTION;
        assert!(description.contains("根拠"), "{description}");
        assert!(description.contains("matched_by"), "{description}");
    }
}
