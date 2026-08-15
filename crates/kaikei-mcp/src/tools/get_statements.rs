//! `get_statements` — 貸借対照表・損益計算書（`docs/07-mcp-server.md` §3）。
//!
//! # なぜ Phase 3 で延期されていたか、なぜ今できるか
//!
//! D-031 が延期理由として挙げていたのは「`TrialBalance` / `BalanceRow` は
//! `kaikei-core` の外から構築できない（`GroupKey` に公開コンストラクタが無い）」
//! ことだった。read model が返す DTO（`TrialBalanceView`）では
//! [`kaikei_app::policy::StatementPolicy`] の入力にならない。
//!
//! [`kaikei_app::ports::JournalRepo::list_entries_in_period`] が入ったことで、
//! **帳簿のドメインモデルから `TrialBalance::from_entries` で組み立て直す**
//! 経路ができた。このツールはその上に乗る
//! （[`kaikei_app::usecase::statements`]）。
//!
//! # `Tx` を開く読み取り系である
//!
//! `get_trial_balance` は read model に直行する（`Tx` を開かない）が、こちらは
//! ドメインモデルが要るので `Tx` を開く。**勘定科目表と仕訳を同じ
//! トランザクションで読む**ことにも意味がある——間に科目名が変わると、
//! 決算書のラベルと集計対象がずれる。
//!
//! # 様式（`JpStatementPolicy`）はここで都度組み立てる
//!
//! `DECISIONS.md` D-069。`JpStatementPolicy` が保持する勘定科目表は DB から
//! 頻繁に読み直される可変データなので、合成ルートが長期保持する
//! `Composition` に**含めない**。起動時に固めると「科目名を変更したのに
//! 決算書には古い名前が出る」というバグになる。
//!
//! そのため勘定科目表を2回読む（様式の構築用と、ユースケース内の集計用）。
//! 同一トランザクション内なので食い違いは起きない。読み直しを1回にするには
//! ユースケースの引数で受け渡す形になるが、それは「呼び出し側が正しい
//! 勘定科目表を渡す」責任を増やすだけで、D-069 が避けたい事故には効かない。
//!
//! # 貸借対照表には期首残高が要る
//!
//! 集計するのは**指定された期間の仕訳だけ**なので、期間が会計年度の途中から
//! 始まっていると、前期繰越の現預金も元入金も落ちた**成立していない貸借
//! 対照表**が返る（損益計算書は期間の損益そのものなので素直に効く）。
//! これは会計の性質であって実装の誤りではないため補正しない。代わりに
//! 応答で伝える:
//!
//! - `entry_count` / `first_entry_date` を常に返す
//! - `from` が会計年度の開始日でなければ `balance_sheet_note` を添える
//!
//! **`warnings` キーは使わない**（dispatch 層が fail-open の注記のために
//! 予約している。`PROGRESS.md` Phase 3 の申し送り）。

use kaikei_app::amount::money_to_plain_string;
use kaikei_app::error::AppError;
use kaikei_app::policy::Statement;
use kaikei_app::ports::ChartRepo;
use kaikei_app::tx::with_tx_err;
use kaikei_app::usecase::statements::{self, StatementsInput, StatementsOutput};
use kaikei_core::AccountingDate;
use kaikei_jp::statement::JpStatementPolicy;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::dispatch::{McpTool, ToolContext, ToolFailure, ToolSuccess};
use crate::error::ToolError;
use crate::tools::parse_date;

/// `get_statements`。
pub struct GetStatements;

// ★この構造体の doc コメントは `tools/list` の応答に出る★
// 内部設計書への参照・crate 名・Markdown の強調記法を書かないこと。
/// 財務諸表を出す期間。指定していないキーは受け付けません。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetStatementsInput {
    /// 集計期間の開始日（取引日、この日を含む）。YYYY-MM-DD の形式で
    /// 指定します。必須です。貸借対照表を出す場合は、会計年度の開始日
    /// （暦年なら 1 月 1 日）を指定してください。年度の途中の日付を指定すると、
    /// 前期から繰り越した残高が貸借対照表に含まれません。
    pub from: String,

    /// 集計期間の終了日（取引日、この日を含む）。YYYY-MM-DD の形式で
    /// 指定します。必須です。開始日より前の日付を指定した場合はエラーに
    /// なります（0 件の財務諸表としては扱いません）。
    pub to: String,
}

impl McpTool for GetStatements {
    type Input = GetStatementsInput;

    const NAME: &'static str = "get_statements";

    const DESCRIPTION: &'static str = "\
指定した期間の帳簿から貸借対照表と損益計算書を組み立てて返します。\
from と to は取引日で、両端を含みます。どちらも必須です。\
金額はすべて文字列で返します（例: \"110000\"。桁区切りは入りません）。\
集計対象は指定した期間の仕訳だけです。\
そのため貸借対照表を出すときは、会計年度の開始日を from に指定してください。\
年度の途中から始まる期間では、前期から繰り越した残高が貸借対照表に含まれず、\
資産・負債・資本がその期間中の増減だけになります\
（損益計算書は期間の損益そのものなので、この影響を受けません）。\
応答には集計に使った仕訳の件数（entry_count）と最も古い取引日\
（first_entry_date）を付けます。\
期首残高を帳簿に入れるには、期首の日付で開始仕訳を記帳してください。\
このサーバーが期首の振替仕訳を自動で作ることはありません。\
決算書の様式が税務上適切かどうかの判断はこのサーバーでは行いません。";

    async fn run(ctx: &ToolContext<'_>, input: Self::Input) -> Result<ToolSuccess, ToolFailure> {
        let from = parse_date("from", &input.from)?;
        let to = parse_date("to", &input.to)?;

        let composition = ctx.composition();
        let tag_schema = composition.tag_catalog.schema().clone();

        let output: StatementsOutput = with_tx_err(ctx.store(), move |tx| {
            let tag_schema = tag_schema.clone();
            Box::pin(async move {
                // D-069: 様式が持つ勘定科目表は、この呼び出しのために読み直した
                // ものから作る（起動時に固めない）。
                let chart = tx.load_chart().await?;
                let policy = JpStatementPolicy::new(chart);
                statements::execute(
                    tx,
                    &policy,
                    &tag_schema,
                    StatementsInput {
                        from,
                        to,
                        // 決算書と同じ見え方にする（決算振替を外す）。
                        // 外さないと、決算振替を記帳した年度だけ売上0になる。
                        exclude_closing: true,
                    },
                )
                .await
            })
        })
        .await
        .map_err(|error: AppError| ToolError::from_app_error(&error))?;

        let body = success_body(&input.from, &input.to, from, &output);
        let summary = audit_summary(&body);
        Ok(ToolSuccess::new(body).with_audit_summary(summary))
    }
}

/// 成功応答。
///
/// 金額はすべて**区切り無しの文字列**（`docs/07-mcp-server.md` §5）。
/// 件数は JSON number。
fn success_body(
    from_text: &str,
    to_text: &str,
    from: AccountingDate,
    output: &StatementsOutput,
) -> Map<String, Value> {
    let mut body = Map::new();
    body.insert("from".to_string(), json!(from_text));
    body.insert("to".to_string(), json!(to_text));
    body.insert("entry_count".to_string(), json!(output.entry_count));
    body.insert(
        "first_entry_date".to_string(),
        match output.first_entry_date {
            Some(date) => json!(date.to_iso_string()),
            None => Value::Null,
        },
    );
    body.insert(
        "balance_sheet".to_string(),
        statement_to_json(&output.balance_sheet),
    );
    body.insert(
        "income_statement".to_string(),
        statement_to_json(&output.income_statement),
    );

    // 期首残高が落ちている可能性を、読み手（AI）が次の手に繋げられる形で伝える。
    if let Some(note) = balance_sheet_note(from, output) {
        body.insert("balance_sheet_note".to_string(), json!(note));
    }
    body
}

/// 貸借対照表が期首残高を欠いている可能性への注記。
///
/// 判定は2つ。**どちらも「疑わしい」までしか言わない**——実際に期首残高が
/// 要るかは帳簿の中身と事業の状況によるので、断定すると `CLAUDE.md` §10 の
/// 「税務判断を断定しない」に触れる。
///
/// 1. `from` が 1 月 1 日でない（会計年度は暦年のみ対応。`FiscalYearRule`）
/// 2. `from` は年度開始日だが、最も古い仕訳がそこから離れている
///
/// 2 は「1〜2 月に取引が無かった」場合にも出る。それでも出すのは、
/// **期首残高の入れ忘れは黙って進むと決算まで気づけない**からである。
fn balance_sheet_note(from: AccountingDate, output: &StatementsOutput) -> Option<String> {
    if output.entry_count == 0 {
        return None; // 0 件は `entry_count` が語る。二重に言わない
    }

    let starts_at_fiscal_year_start = from.month() == 1 && from.day() == 1;
    if !starts_at_fiscal_year_start {
        return Some(format!(
            "集計期間の開始日（{}）が会計年度の開始日ではありません。\
             貸借対照表には前期から繰り越した残高が含まれていないため、\
             資産・負債・資本はこの期間中の増減だけを表しています。\
             年度末時点の貸借対照表が必要な場合は from に会計年度の開始日を\
             指定してください。損益計算書はこの影響を受けません",
            from.to_iso_string()
        ));
    }

    let first = output.first_entry_date?;
    if first > from {
        return Some(format!(
            "集計対象で最も古い仕訳は {} で、集計期間の開始日（{}）から離れています。\
             その間に取引が無かっただけであれば問題ありませんが、\
             期首残高の仕訳が帳簿に無い場合、貸借対照表には前期から繰り越した\
             残高が含まれません。期首残高が必要なら、期首の日付で開始仕訳を\
             記帳してください",
            first.to_iso_string(),
            from.to_iso_string()
        ));
    }
    None
}

/// 財務諸表1つ分の JSON。
fn statement_to_json(statement: &Statement) -> Value {
    let sections: Vec<Value> = statement
        .sections
        .iter()
        .map(|section| {
            let lines: Vec<Value> = section
                .lines
                .iter()
                .map(|line| {
                    json!({
                        "account": line.account.as_str(),
                        "label": line.label,
                        "amount": money_to_plain_string(&line.amount),
                    })
                })
                .collect();
            json!({
                "title": section.title,
                "lines": lines,
                "subtotal": money_to_plain_string(&section.subtotal),
            })
        })
        .collect();

    json!({
        "title": statement.title,
        "sections": sections,
        "total": money_to_plain_string(&statement.total),
    })
}

/// `audit_log.output` に残す要約（`DECISIONS.md` D-089 決定6）。
///
/// 明細（`sections` の中の `lines`）を落とし、区分の小計と合計は残す。
/// **要約は応答本文から落として作る**——別に組み立てると値が食い違う
/// （`PROGRESS.md` Phase 3 の申し送り）。
///
/// 読み取りは AI が最も多く呼ぶ操作であり、`audit_log` は append-only で
/// 消せない。科目数に比例する明細をそのまま残すと際限なく膨らむ。
fn audit_summary(body: &Map<String, Value>) -> Map<String, Value> {
    let mut summary = body.clone();
    for key in ["balance_sheet", "income_statement"] {
        if let Some(statement) = summary.get_mut(key) {
            strip_lines(statement);
        }
    }
    summary
}

/// 財務諸表の JSON から各区分の `lines` を落とす（小計・合計は残す）。
fn strip_lines(statement: &mut Value) {
    let Some(sections) = statement.get_mut("sections").and_then(Value::as_array_mut) else {
        return;
    };
    for section in sections {
        if let Some(object) = section.as_object_mut() {
            object.remove("lines");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_app::policy::{StatementLine, StatementSection};
    use kaikei_core::{AccountCode, Currency, Money};

    fn date(year: i32, month: u8, day: u8) -> AccountingDate {
        AccountingDate::new(year, month, day).unwrap()
    }

    fn statement(title: &str) -> Statement {
        Statement {
            title: title.to_string(),
            sections: vec![StatementSection {
                title: "資産".to_string(),
                lines: vec![StatementLine {
                    account: AccountCode::parse("100").unwrap(),
                    label: "現金".to_string(),
                    amount: Money::from_minor(11_000, Currency::JPY),
                }],
                subtotal: Money::from_minor(11_000, Currency::JPY),
            }],
            total: Money::from_minor(11_000, Currency::JPY),
        }
    }

    fn output(entry_count: usize, first: Option<AccountingDate>) -> StatementsOutput {
        StatementsOutput {
            balance_sheet: statement("貸借対照表"),
            income_statement: statement("損益計算書"),
            entry_count,
            first_entry_date: first,
        }
    }

    // 年度の途中から始まる期間には注記が付く。
    #[test]
    fn a_period_starting_mid_year_carries_a_note() {
        let from = date(2026, 6, 1);
        let body = success_body("2026-06-01", "2026-12-31", from, &output(5, Some(from)));

        let note = body
            .get("balance_sheet_note")
            .and_then(Value::as_str)
            .expect("注記が付くはず");
        assert!(note.contains("会計年度の開始日ではありません"), "{note}");
        // 断定しない（`CLAUDE.md` §10）。「誤り」「不正」とは言わない。
        assert!(!note.contains("誤り"), "{note}");
    }

    // 年度開始日から始まり、最初の仕訳もその日なら注記は付かない。
    #[test]
    fn a_full_year_starting_with_an_entry_on_day_one_has_no_note() {
        let from = date(2026, 1, 1);
        let body = success_body("2026-01-01", "2026-12-31", from, &output(5, Some(from)));

        assert!(body.get("balance_sheet_note").is_none(), "{body:?}");
    }

    // 年度開始日から始まるが最初の仕訳が離れている場合は、期首残高の
    // 入れ忘れを疑える注記が付く。
    #[test]
    fn a_gap_before_the_first_entry_carries_a_note() {
        let from = date(2026, 1, 1);
        let body = success_body(
            "2026-01-01",
            "2026-12-31",
            from,
            &output(5, Some(date(2026, 3, 10))),
        );

        let note = body
            .get("balance_sheet_note")
            .and_then(Value::as_str)
            .expect("注記が付くはず");
        assert!(note.contains("2026-03-10"), "{note}");
        assert!(note.contains("期首残高"), "{note}");
    }

    // 0 件の期間では注記を出さない（`entry_count` が語るので二重に言わない）。
    #[test]
    fn an_empty_period_reports_the_count_without_a_note() {
        let from = date(2026, 6, 1);
        let body = success_body("2026-06-01", "2026-06-30", from, &output(0, None));

        assert_eq!(body.get("entry_count"), Some(&json!(0)));
        assert_eq!(body.get("first_entry_date"), Some(&Value::Null));
        assert!(body.get("balance_sheet_note").is_none(), "{body:?}");
    }

    // 監査ログの要約は明細を落とし、小計・合計・注記は残す。
    #[test]
    fn the_audit_summary_drops_the_lines_but_keeps_the_totals() {
        let from = date(2026, 1, 1);
        let body = success_body("2026-01-01", "2026-12-31", from, &output(5, Some(from)));
        let summary = audit_summary(&body);

        for key in ["balance_sheet", "income_statement"] {
            let sections = summary[key]["sections"].as_array().unwrap();
            for section in sections {
                assert!(
                    section.get("lines").is_none(),
                    "{key} の明細が残っている: {section}"
                );
                assert!(section.get("subtotal").is_some(), "小計は残すこと");
            }
            assert!(summary[key].get("total").is_some(), "合計は残すこと");
        }
        // 本文側は落とさない（要約は複製に対して行う）。
        assert!(body["balance_sheet"]["sections"][0].get("lines").is_some());
    }

    // 応答本文に `warnings` キーを使わない（dispatch 層が予約している）。
    #[test]
    fn the_body_does_not_use_the_reserved_warnings_key() {
        let from = date(2026, 6, 1);
        let body = success_body("2026-06-01", "2026-12-31", from, &output(5, Some(from)));
        assert!(body.get("warnings").is_none());
    }

    // 説明文が禁止表現を含まず、期首残高の落とし穴に触れている。
    #[test]
    fn the_description_warns_about_the_opening_balance_without_forbidden_claims() {
        let description = GetStatements::DESCRIPTION;
        for forbidden in ["準拠", "法令対応", "JIIMA"] {
            assert!(!description.contains(forbidden), "{forbidden}");
        }
        assert!(description.contains("会計年度の開始日"));
        assert!(description.contains("判断はこのサーバーでは行いません"));
    }
}
