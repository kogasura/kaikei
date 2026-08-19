//! `propose_depreciation_entries` — 減価償却費の仕訳の提案（`DECISIONS.md` D-109）。
//!
//! 固定資産台帳から年度の償却費を計算し、
//! `借方 減価償却費 / 貸方 それぞれの資産の科目` の1本にまとめて返す。
//! **記帳はしない**（`propose_closing_entries` と同じ形）。
//!
//! # 台帳の値をそのまま使う
//!
//! 耐用年数も償却方法も**台帳に入っている値**を使う。このツールは推定しない
//! （`DECISIONS.md` D-103）。資産名から耐用年数を当てにいくと、誤りに
//! 気づけないまま申告に載る。

use crate::dispatch::{McpTool, ToolContext, ToolFailure, ToolSuccess};
use crate::error::ToolError;
use kaikei_app::error::AppError;
use kaikei_app::ports::{FixedAssetRepo, FixedAssetRow};
use kaikei_app::tx::with_tx_err;
use kaikei_core::{AccountCode, Currency, FiscalYear, Money};
use kaikei_jp::depreciation::{DepreciationMethod, FixedAsset};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Map, Value};

/// 減価償却費を計上する科目。
///
/// **設定から採らない。** `KAIKEI_CLOSING_ACCOUNT_*` のような決算科目の設定は
/// 元入金・事業主貸・事業主借の3つで、減価償却費は含まれていない。
/// 同梱の勘定科目表（`sole_proprietor.yaml`）が `depreciation.expense_account`
/// として持っているので、そちらを使う。
const FALLBACK_DEPRECIATION_ACCOUNT: &str = "610";

/// `propose_depreciation_entries`。
pub struct ProposeDepreciationEntries;

// ★この構造体の doc コメントは `tools/list` の応答に出る★
// 内部設計書への参照・crate 名・Markdown の強調記法を書かないこと。
/// 減価償却費を提案する会計年度。指定していないキーは受け付けません。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProposeDepreciationEntriesInput {
    /// 償却費を出す会計年度（西暦の年）。必須です。
    /// 会計年度は暦年なので、この年の 1 月 1 日から 12 月 31 日までが対象です。
    pub fiscal_year: i32,
}

impl McpTool for ProposeDepreciationEntries {
    type Input = ProposeDepreciationEntriesInput;

    const NAME: &'static str = "propose_depreciation_entries";

    const DESCRIPTION: &'static str = "\
固定資産台帳から、指定した会計年度の減価償却費の仕訳を組み立てて提案します。\
借方が減価償却費、貸方がそれぞれの資産の科目になります。\
提案するだけで、帳簿には何も記帳しません（応答の posted は常に false です）。\
記帳する場合は、返ってきた明細をそのまま post_journal_entry に渡してください。\
耐用年数と償却方法は台帳に入っている値をそのまま使います。\
資産の名前から耐用年数を推定することはしません。\
同じ資産でも扱いを選べることがあり、初年度の償却費が何倍も変わるためです。\
どの扱いを選ぶかは申告上の判断なので、このサーバーでは決めません。\
台帳が空の場合と、その年度に償却するものが無い場合の両方で提案は空になります。\
どちらであるかは asset_count で判別してください。\
取得していない年、償却し終わった資産、除却した年以降の資産は対象外です。\
償却費が税務上適切かどうかの判断はこのサーバーでは行いません。";

    async fn run(ctx: &ToolContext<'_>, input: Self::Input) -> Result<ToolSuccess, ToolFailure> {
        let fiscal_year = input.fiscal_year;
        let year = FiscalYear::calendar_year(fiscal_year);

        let assets: Vec<FixedAssetRow> = with_tx_err(ctx.store(), |tx| {
            Box::pin(async move { tx.list_fixed_assets().await })
        })
        .await
        .map_err(|error: kaikei_app::error::RepoError| {
            ToolError::from_app_error(&AppError::Repo(error))
        })?;

        let asset_count = assets.len();
        let mut rows = Vec::new();
        let mut total: i128 = 0;
        for asset in &assets {
            if is_outside_the_ledger_for(asset, fiscal_year) {
                continue;
            }
            let input = to_fixed_asset(asset)?;
            let schedule = kaikei_jp::depreciation::schedule(&input)
                .map_err(|error| ToolError::from_jp_error(&error))?;
            let Some(entry) = schedule.year(fiscal_year) else {
                continue;
            };
            if entry.amount.minor() == 0 {
                continue;
            }
            total += entry.amount.minor();
            rows.push((asset, entry.amount));
        }

        let mut body = Map::new();
        body.insert("fiscal_year".to_string(), json!(fiscal_year));
        body.insert(
            "period_start".to_string(),
            json!(year.start().to_iso_string()),
        );
        body.insert("period_end".to_string(), json!(year.end().to_iso_string()));
        // **常に false。** 条件によって省かない（`propose_closing_entries` と同じ）。
        body.insert("posted".to_string(), json!(false));
        // 「台帳が空」と「その年度に償却するものが無い」を区別できるようにする。
        body.insert("asset_count".to_string(), json!(asset_count));

        let proposals = if rows.is_empty() {
            Vec::new()
        } else {
            vec![build_proposal(&rows, total, &year)?]
        };
        let proposal_count = proposals.len();
        body.insert("proposals".to_string(), Value::Array(proposals));
        body.insert(
            "next_step".to_string(),
            json!(next_step(rows.len(), asset_count)),
        );
        body.insert("scope_note".to_string(), json!(SCOPE_NOTE));

        // 監査ログには提案の中身を載せない（`propose_closing_entries` と同じ。
        // 件数と合計だけで、何を提案したかは帳簿の側が持つ）。
        let mut summary = body.clone();
        summary.remove("proposals");
        summary.insert("proposal_count".to_string(), json!(proposal_count));
        summary.insert("total_amount".to_string(), json!(total.to_string()));
        Ok(ToolSuccess::new(body).with_audit_summary(summary))
    }
}

/// この提案が扱わない範囲。
const SCOPE_NOTE: &str = "耐用年数と償却方法は固定資産台帳に入っている値をそのまま使います。資産の名前からの推定はしません。どの扱いを選ぶか（定額法・一括償却資産・少額減価償却資産）は申告上の判断です。除却した年以降の資産は対象外で、除却時の未償却残高をどう処理するか（除却損・売却損益）もこの提案には含まれません。";

/// 提案する仕訳を1本組み立てる。
fn build_proposal(
    rows: &[(&FixedAssetRow, Money)],
    total: i128,
    year: &FiscalYear,
) -> Result<Value, ToolFailure> {
    let currency = Currency::JPY;
    let mut lines = Vec::with_capacity(rows.len() + 1);
    // 借方は減価償却費の1行にまとめる。**資産ごとに分けない**——決算書の
    // 損益計算書に出るのは合計であり、内訳は台帳と「減価償却費の計算」欄が持つ。
    lines.push(json!({
        "account": FALLBACK_DEPRECIATION_ACCOUNT,
        "side": "debit",
        "amount": total.to_string(),
        "currency": currency.code(),
        "tags": { "tax_category": "NOT_APPLICABLE" },
    }));
    let mut breakdown = Vec::with_capacity(rows.len());
    for (asset, amount) in rows {
        // **`lines` には post_journal_entry が受け付けるキーだけを入れる。**
        // 資産名を混ぜると `unknown field` で弾かれ、「そのまま渡してください」
        // という案内が嘘になる（実際に踏んだ）。名前は breakdown 側に置く。
        lines.push(json!({
            "account": asset.account.as_str(),
            "side": "credit",
            "amount": amount.minor().to_string(),
            "currency": currency.code(),
        }));
        breakdown.push(json!({
            "asset_name": asset.name,
            "account": asset.account.as_str(),
            "amount": amount.minor().to_string(),
        }));
    }
    Ok(json!({
        "entry_date": year.end().to_iso_string(),
        "description": format!("減価償却費の計上（{}年分）", year.label()),
        "lines": lines,
        // どの資産がいくらかは人が読むためのもの。**記帳には渡さない。**
        "breakdown": breakdown,
    }))
}

/// 次の手。
///
/// **提案が空のときに「何も無かった」で終わらせない。** 空の理由は2つあり、
/// 次にすることが違う——台帳が空なら登録から、償却するものが無ければ何もしなくてよい。
fn next_step(proposed_lines: usize, asset_count: usize) -> String {
    if proposed_lines > 0 {
        return "提案された明細を post_journal_entry に渡すと記帳されます。\
                記帳するまで減価償却費は帳簿に反映されません"
            .to_string();
    }
    if asset_count == 0 {
        return "固定資産台帳に登録がありません。\
                取得日・取得価額・償却方法・耐用年数を登録してからもう一度実行してください"
            .to_string();
    }
    "この年度に償却する資産はありません。\
     取得前・償却し終わっている・除却済みのいずれかです"
        .to_string()
}

/// その年度に台帳の計算対象から外すか（`DECISIONS.md` D-108）。
fn is_outside_the_ledger_for(asset: &FixedAssetRow, fiscal_year: i32) -> bool {
    if asset.acquired_on.year() > fiscal_year {
        return true;
    }
    match asset.disposed_on {
        Some(disposed) => disposed.year() <= fiscal_year,
        None => false,
    }
}

/// 台帳の1行を、償却計算の入力に翻訳する。
fn to_fixed_asset(row: &FixedAssetRow) -> Result<FixedAsset, ToolFailure> {
    let invalid = |reason: String| -> ToolFailure {
        ToolError::from_jp_error(&kaikei_jp::error::JpError::InvalidFixedAsset { reason }).into()
    };

    let method = match row.method {
        1 => {
            let life = row.useful_life_years.ok_or_else(|| {
                invalid(format!("{}: 定額法なのに耐用年数がありません", row.name))
            })?;
            DepreciationMethod::StraightLine {
                useful_life_years: u8::try_from(life)
                    .map_err(|_| invalid(format!("{}: 耐用年数が範囲外です: {life}", row.name)))?,
            }
        }
        2 => DepreciationMethod::LumpSumOverThreeYears,
        3 => DepreciationMethod::ImmediateExpense,
        other => {
            return Err(invalid(format!(
                "{}: 知らない償却方法です: {other}",
                row.name
            )));
        }
    };

    let business_ratio = match &row.business_ratio {
        Some(text) => Some(kaikei_core::Ratio::parse_fraction(text).map_err(|error| {
            invalid(format!("{}: 事業専用割合を読めません: {error}", row.name))
        })?),
        None => None,
    };

    Ok(FixedAsset {
        name: row.name.clone(),
        acquired_on: row.acquired_on,
        acquisition_cost: row.acquisition_cost,
        method,
        business_ratio,
    })
}

/// 使わない定数を消さないための参照（科目コードの形が壊れていないか）。
#[allow(dead_code)]
fn depreciation_account() -> Result<AccountCode, kaikei_core::CoreError> {
    AccountCode::parse(FALLBACK_DEPRECIATION_ACCOUNT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_core::AccountingDate;

    fn row(
        account: &str,
        acquired: (i32, u8, u8),
        cost: i128,
        method: i16,
        life: Option<i16>,
    ) -> FixedAssetRow {
        FixedAssetRow {
            id: "11111111-1111-1111-1111-111111111111".to_string(),
            name: format!("資産{account}"),
            account: AccountCode::parse(account).unwrap(),
            acquired_on: AccountingDate::new(acquired.0, acquired.1, acquired.2).unwrap(),
            acquisition_cost: Money::from_minor(cost, Currency::JPY),
            method,
            useful_life_years: life,
            business_ratio: None,
            disposed_on: None,
            note: None,
        }
    }

    #[test]
    fn the_depreciation_account_code_is_valid() {
        assert!(depreciation_account().is_ok());
    }

    // **本命。** 借方は1行にまとめ、貸方は資産ごとに分ける。
    #[test]
    fn the_proposal_has_one_debit_and_one_credit_per_asset() {
        let a = row("220", (2025, 3, 10), 105_600, 1, Some(2));
        let b = row("210", (2025, 7, 24), 227_412, 1, Some(4));
        let rows = vec![
            (&a, Money::from_minor(54_000, Currency::JPY)),
            (&b, Money::from_minor(56_853, Currency::JPY)),
        ];
        let year = FiscalYear::calendar_year(2026);

        let proposal = build_proposal(&rows, 109_653, &year).unwrap();
        let lines = proposal["lines"].as_array().unwrap();

        assert_eq!(lines.len(), 3, "借方1 + 貸方2");
        assert_eq!(lines[0]["account"], json!("610"));
        assert_eq!(lines[0]["side"], json!("debit"));
        assert_eq!(lines[0]["amount"], json!("109653"));
        assert_eq!(
            lines[0]["tags"]["tax_category"],
            json!("NOT_APPLICABLE"),
            "減価償却費は消費税の対象外"
        );
        assert_eq!(lines[1]["account"], json!("220"));
        assert_eq!(lines[1]["amount"], json!("54000"));
        assert_eq!(lines[2]["account"], json!("210"));
        assert_eq!(proposal["entry_date"], json!("2026-12-31"));

        // **本命。** post_journal_entry が受け付けるキーだけであること。
        // 余分なキーがあると `unknown field` で弾かれ、
        // 「そのまま渡してください」という案内が嘘になる。
        let accepted = ["account", "side", "amount", "currency", "memo", "tags"];
        for line in lines {
            for key in line.as_object().unwrap().keys() {
                assert!(
                    accepted.contains(&key.as_str()),
                    "post_journal_entry が受け付けないキーが混ざっています: {key}"
                );
            }
        }

        // 資産名は breakdown 側にある（人が読むため。記帳には渡さない）。
        let breakdown = proposal["breakdown"].as_array().unwrap();
        assert_eq!(breakdown.len(), 2);
        assert_eq!(breakdown[0]["asset_name"], json!("資産220"));
    }

    // **本命。** 空の理由を区別する。
    #[test]
    fn the_next_step_tells_the_two_empty_cases_apart() {
        assert!(next_step(0, 0).contains("登録がありません"));
        assert!(next_step(0, 3).contains("償却する資産はありません"));
        assert!(next_step(2, 3).contains("post_journal_entry"));
    }

    // 除却した年から対象外（D-108）。
    #[test]
    fn a_disposed_asset_is_outside_from_the_year_of_disposal() {
        let mut asset = row("220", (2025, 3, 10), 105_600, 1, Some(2));
        asset.disposed_on = Some(AccountingDate::new(2026, 6, 30).unwrap());
        assert!(!is_outside_the_ledger_for(&asset, 2025));
        assert!(is_outside_the_ledger_for(&asset, 2026));
    }

    #[test]
    fn an_asset_acquired_later_is_outside() {
        let asset = row("210", (2027, 1, 1), 100_000, 1, Some(2));
        assert!(is_outside_the_ledger_for(&asset, 2026));
    }

    // 定額法で耐用年数が無ければ、推定せずエラーにする。
    #[test]
    fn a_straight_line_row_without_a_life_is_an_error() {
        let asset = row("210", (2025, 1, 1), 100_000, 1, None);
        assert!(to_fixed_asset(&asset).is_err());
    }

    #[test]
    fn an_unknown_method_is_an_error() {
        let asset = row("210", (2025, 1, 1), 100_000, 9, None);
        assert!(to_fixed_asset(&asset).is_err());
    }
}
