//! `list_accounts` — 勘定科目一覧（`docs/07-mcp-server.md` §2 / §10 MC-13）。
//!
//! # 何を返すか
//!
//! 科目コード・名称・5要素分類（`account_type`）・親科目（`parent`）・
//! **記帳可否（`postable`）**。`postable: false` は見出し科目で、記帳に使うと
//! `not_postable` になるため必ず返す。並びは**科目コード昇順**
//! （`ChartOfAccounts::iter` が `BTreeMap` 由来で決定的。表示順（テンプレートの
//! `sort`）は `kaikei_core::AccountDef` が保持していないので返さない。
//! `DECISIONS.md` D-061）。
//!
//! # 読むのは DB の `accounts` である
//!
//! `kaikei_jp::compose` が返す `chart`（埋め込みテンプレート由来）ではなく、
//! [`kaikei_app::ports::ChartRepo::load_chart`] が読む **DB の `accounts`** を
//! 返す（`docs/07-mcp-server.md` §4 の経路表）。記帳が科目を解決するのも
//! こちらなので、**このツールの応答に無いコードは記帳にも使えない**という
//! 対応が保たれる。起動時の投入でテンプレートと食い違って既存を残した科目が
//! ある場合、その理由は `get_settings` が返す（`DECISIONS.md` D-081 / D-086）。

use kaikei_app::ports::ChartRepo;
use kaikei_app::tx::with_tx;
use kaikei_core::ChartOfAccounts;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::dispatch::{McpTool, ToolContext, ToolFailure, ToolSuccess};
use crate::error::ToolError;
use crate::wire::account_def_to_json;

/// `list_accounts`。
pub struct ListAccounts;

// ★この構造体の doc コメントは `tools/list` の応答に出る★
// 内部設計書への参照・crate 名・Markdown の強調記法を書かないこと
// （PR-F レビュー D-2。`server.rs` の
// `every_input_schema_description_is_written_for_the_caller` が検査する）。
/// 勘定科目一覧の取得。指定していないキーは受け付けません。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListAccountsInput {
    /// true にすると記帳に使える科目だけを返します。省略すると、記帳に
    /// 使えない見出し科目も含めた全件を返します。
    #[serde(default)]
    pub postable_only: bool,
}

impl McpTool for ListAccounts {
    type Input = ListAccountsInput;

    const NAME: &'static str = "list_accounts";

    const DESCRIPTION: &'static str = "\
帳簿に登録されている勘定科目の一覧を返します。\
各科目について、科目コード（account）・名称・5要素分類（account_type: \
asset / liability / equity / revenue / expense）・親科目（parent）・\
記帳可否（postable）を返します。\
postable が false の科目は見出しであり、仕訳の明細に指定すると拒否されます。\
並びは科目コードの昇順です。帳簿は科目コードで科目を解決するので、\
post_journal_entry の account にはここで返るコードをそのまま指定します。\
この一覧に無いコードを指定した場合は記帳が拒否されます。";

    async fn run(ctx: &ToolContext<'_>, input: Self::Input) -> Result<ToolSuccess, ToolFailure> {
        let chart = with_tx(ctx.store(), |tx| {
            Box::pin(async move { Ok(tx.load_chart().await?) })
        })
        .await
        .map_err(|error| ToolError::from_app_error(&error))?;

        Ok(ToolSuccess::new(success_body(&chart, input.postable_only)))
    }
}

/// 成功応答の本文。
///
/// `count` は**返した件数**（`postable_only` で絞った後）である。
/// 絞ったことが応答から読めるよう、要求した絞り込みも一緒に返す
/// （「件数が少ない」のが帳簿の状態なのか絞り込みの結果なのかを、
/// 呼び出し元が応答だけで判断できるようにする。`CLAUDE.md` §11）。
fn success_body(chart: &ChartOfAccounts, postable_only: bool) -> Map<String, Value> {
    let accounts: Vec<Value> = chart
        .iter()
        .filter(|def| !postable_only || def.postable)
        .map(account_def_to_json)
        .collect();

    let mut body = Map::new();
    // 件数は金額ではないので JSON number のままでよい（§5）。
    body.insert("count".to_string(), json!(accounts.len()));
    body.insert("postable_only".to_string(), json!(postable_only));
    body.insert("accounts".to_string(), Value::Array(accounts));
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_core::{AccountCode, AccountDef, AccountType};

    fn chart() -> ChartOfAccounts {
        ChartOfAccounts::new(vec![
            AccountDef {
                code: AccountCode::parse("500").unwrap(),
                name: "売上高".to_string(),
                account_type: AccountType::Revenue,
                parent: None,
                postable: true,
            },
            AccountDef {
                code: AccountCode::parse("100").unwrap(),
                name: "現金".to_string(),
                account_type: AccountType::Asset,
                parent: None,
                postable: true,
            },
            AccountDef {
                code: AccountCode::parse("900").unwrap(),
                name: "経費".to_string(),
                account_type: AccountType::Expense,
                parent: None,
                postable: false,
            },
        ])
        .unwrap()
    }

    fn accounts(body: &Value) -> Vec<&Value> {
        body["accounts"].as_array().expect("配列").iter().collect()
    }

    fn body_of(chart: &ChartOfAccounts, postable_only: bool) -> Value {
        Value::Object(success_body(chart, postable_only))
    }

    // MC-13: 科目種別と**記帳可否**を含めて返す。並びは科目コード昇順。
    #[test]
    fn every_account_carries_its_type_and_whether_it_can_be_posted_to() {
        let body = body_of(&chart(), false);

        assert_eq!(body["count"], json!(3));
        let accounts = accounts(&body);
        let codes: Vec<&str> = accounts
            .iter()
            .map(|a| a["account"].as_str().unwrap())
            .collect();
        assert_eq!(codes, vec!["100", "500", "900"], "科目コード昇順であること");

        for account in &accounts {
            assert!(account.get("account_type").is_some(), "{account}");
            assert!(
                account.get("postable").is_some(),
                "postable は必ず返す（見出し科目に記帳しようとして初めて\
                 分かる、という形にしない）: {account}"
            );
        }
        assert_eq!(accounts[2]["postable"], json!(false));
    }

    // 絞り込みは「記帳に使える科目だけ」を返し、絞ったことを応答に残す。
    #[test]
    fn postable_only_hides_the_headings_and_says_so_in_the_response() {
        let body = body_of(&chart(), true);

        assert_eq!(body["count"], json!(2));
        assert_eq!(body["postable_only"], json!(true));
        for account in accounts(&body) {
            assert_eq!(account["postable"], json!(true), "{account}");
        }
    }

    // 科目が1件も無い帳簿は**空の成功**（「見つからない」ではない）。
    #[test]
    fn an_empty_chart_is_a_successful_empty_list_not_a_not_found() {
        let body = body_of(&ChartOfAccounts::new(Vec::new()).unwrap(), false);
        assert_eq!(body["count"], json!(0));
        assert_eq!(body["accounts"], json!([]));
    }

    // 既定は全件（指定しないと勝手に絞られる、という形にしない）。
    #[test]
    fn postable_only_defaults_to_false() {
        let input: ListAccountsInput = serde_json::from_str("{}").unwrap();
        assert!(!input.postable_only);
    }

    // 説明文が `CLAUDE.md` §10 の禁止表現を含まず、§11 の「次の手」を含む。
    #[test]
    fn the_description_avoids_forbidden_claims_and_states_the_next_step() {
        let description = ListAccounts::DESCRIPTION;
        for forbidden in ["準拠", "法令対応", "JIIMA"] {
            assert!(!description.contains(forbidden), "{forbidden}");
        }
        assert!(description.contains("postable"));
        assert!(description.contains("post_journal_entry"));
    }
}
