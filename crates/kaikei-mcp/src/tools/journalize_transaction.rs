//! `journalize_transaction` — 取り込んだ明細を仕訳にする
//! （`docs/05-csv-import.md` §6）。
//!
//! # 記帳と状態遷移を同じトランザクションで行う
//!
//! **これがこのツールの存在理由である。** `post_journal_entry` で仕訳を起こし、
//! 別の呼び出しで明細を仕訳済みにする形だと、間に落ちたときに**帳簿に仕訳
//! だけが残って明細は未処理のまま**になる。次にもう一度仕訳化すれば二重計上
//! であり、帳簿は追記のみなので手で逆仕訳を起こすまで金額がずれ続ける。
//!
//! 2回の呼び出しを1つのトランザクションに束ねる方法は MCP には無い。だから
//! 1つのツールにする。
//!
//! # 取引日は明細から採る
//!
//! 既定では明細の取引年月日をそのまま使う。AI に日付を組み立てさせると、
//! 打ち間違いが**別の年度の仕訳**になって現れる（貸借は一致したままなので
//! 決算書を見ても気づけない）。
//!
//! ただし上書きは許す。カードの明細では「引落日」と「購入日」が違い、費用の
//! 計上は購入日に寄せるのが正しいためである。
//!
//! # 金額の取り違えを見つける
//!
//! 明細が 19,800 円なのに 1,980 円で記帳しても、貸借さえ合っていれば帳簿は
//! 受け取る。**桁の落ちた仕訳は決算書を見ても分からない。**
//!
//! そこで「口座が動いた側の行に、明細と同じ金額が1行あるか」を見る。
//! 振込手数料が差し引かれた入金のように**合計が一致しない仕訳は正当に存在
//! する**ので、合計では見ない（入金 100,000 ＋ 手数料 10,000 ＝ 売上 110,000）。
//!
//! それでも一致しない形はあるので、`allow_amount_mismatch` で意図的に
//! 通せるようにする。**既定では通さない**——黙って通ると、桁落ちが
//! 見つかるのは1年後になる。

use kaikei_app::error::AppError;
use kaikei_app::ports::{ImportedTxQuery, ImportedTxRepo};
use kaikei_app::tx::with_tx_err;
use kaikei_app::usecase::post_entry::{self, PostEntryInput};
use kaikei_app::view::ImportedTxQuerySpec;
use kaikei_core::{JournalLine, Side};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::dispatch::{McpTool, ToolContext, ToolFailure, ToolSuccess};
use crate::error::ToolError;
use crate::tools::parse_date;
use crate::tools::post_journal_entry::{build_lines, entry_body, PostJournalEntryLine};

/// 明細を1件引くための上限。
///
/// IDで引くので1件しか返らないが、read model には「IDで1件引く」口が無い。
const LOOKUP_LIMIT: u32 = 200;

/// `journalize_transaction`。
pub struct JournalizeTransaction;

// ★この構造体の doc コメントは `tools/list` の応答に出る★
/// 取り込んだ明細を仕訳にするための指定。指定していないキーは受け付けません。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JournalizeTransactionInput {
    /// 仕訳にする明細のID。list_pending_transactions が返す id を指定します。
    pub imported_tx_id: String,

    /// 仕訳明細。2行以上を指定します。auto_tax_lines を使う場合は、
    /// 消費税額の行を含まない元の明細だけを渡します。
    pub lines: Vec<PostJournalEntryLine>,

    /// 摘要。省略すると明細の摘要（raw_description）をそのまま使います。
    #[serde(default)]
    pub description: Option<String>,

    /// 取引日。YYYY-MM-DD。省略すると明細の取引年月日を使います。
    /// カードの明細で引落日と購入日が違う場合など、費用の計上日を
    /// 明細の日付と変えたいときだけ指定します。
    #[serde(default)]
    pub entry_date: Option<String>,

    /// true にすると消費税額の行の生成を試みます。生成されるかどうかは
    /// 帳簿の設定（税抜経理か税込経理か・課税事業者か）で決まります。
    #[serde(default)]
    pub auto_tax_lines: bool,

    /// 明細の金額と一致する行が無くても記帳します。
    /// 振込手数料が差し引かれた入金など、意図して一致しない場合にだけ
    /// 指定します。既定は false で、一致しなければエラーになります。
    #[serde(default)]
    pub allow_amount_mismatch: bool,
}

impl McpTool for JournalizeTransaction {
    type Input = JournalizeTransactionInput;

    const NAME: &'static str = "journalize_transaction";

    const DESCRIPTION: &'static str = "\
取り込んだ明細を仕訳にします。仕訳を起こすことと、その明細を仕訳済みにすることを、\
1つのまとまりとして行います（途中で失敗した場合はどちらも行われません）。\
明細のIDは list_pending_transactions で取得します。\
未処理でない明細は仕訳にできません（既に仕訳済みのものを別の仕訳で塗り替えると、\
先に作った仕訳が帳簿に残ったまま誰からも参照されなくなります）。\
取引日は明細の取引年月日を使います。カードの引落日と購入日が違う場合など、\
計上日を変えたいときだけ entry_date を指定します。\
摘要を省略すると明細の摘要をそのまま使います。\
金額は文字列で指定します（例: \"110000\"。JSON の number は受け付けません）。\
明細の金額と同じ金額の行が、口座が動いた側に1行も無い場合はエラーになります。\
振込手数料が差し引かれた入金など、意図して一致しない場合は \
allow_amount_mismatch を true にします。\
記帳した仕訳は更新も削除もできず、訂正は reverse_journal_entry（逆仕訳）で行います。";

    async fn run(ctx: &ToolContext<'_>, input: Self::Input) -> Result<ToolSuccess, ToolFailure> {
        let composition = ctx.composition();
        let settings = ctx.book_settings();

        // **明細を先に引く。** 取引日も摘要もここから採るので、無ければ
        // 何も組み立てられない。
        let imported = find_pending(ctx, &input.imported_tx_id).await?;

        let entry_date = match &input.entry_date {
            Some(text) => parse_date("entry_date", text)?,
            None => imported.occurred_on,
        };
        let description = input
            .description
            .clone()
            .unwrap_or_else(|| imported.raw_description.clone());

        let lines = build_lines(&composition, &settings, &input.lines)?;

        let post_input = PostEntryInput {
            entry_date,
            description,
            lines,
            auto_tax_lines: input.auto_tax_lines,
        };

        let id_gen = ctx.id_gen();
        let clock = ctx.clock();
        let imported_id = input.imported_tx_id.clone();
        let amount_minor = imported.amount_minor;
        let is_money_in = imported.is_money_in;
        let allow_mismatch = input.allow_amount_mismatch;

        // ★記帳と状態遷移を同じトランザクションで行う★
        //
        // 片方だけ残ると、帳簿に仕訳だけがあって明細は未処理のまま——
        // つまり二重計上の入口——になる。
        let posted = with_tx_err(ctx.store(), move |tx| {
            let post_input = post_input.clone();
            let imported_id = imported_id.clone();
            Box::pin(async move {
                let output = post_entry::execute(
                    tx,
                    &composition.tax_policy,
                    composition.tag_catalog.schema(),
                    &id_gen,
                    &clock,
                    &settings,
                    post_input,
                )
                .await
                .map_err(AppError::from)?;

                // **確定後の明細で見る。** 税抜経理では消費税行が後から
                // 足されるので、渡した明細だけを見ても足りない。
                if !allow_mismatch {
                    check_amount(output.entry.lines(), amount_minor, is_money_in)?;
                }

                tx.mark_journalized(&imported_id, output.entry.id()).await?;
                Ok::<_, AppError>(output)
            })
        })
        .await
        .map_err(|error: AppError| ToolFailure::from(ToolError::from_app_error(&error)))?;

        // 記帳の応答は `post_journal_entry` と同じ形にし、どの明細から
        // 起こしたかだけを足す。
        let mut body = entry_body(&posted);
        body.insert(
            "imported_tx_id".to_string(),
            json!(input.imported_tx_id.clone()),
        );
        Ok(ToolSuccess::new(body).with_entry_id(posted.entry.id()))
    }
}

/// 明細をIDで引く。
///
/// **見つからないことと、未処理でないことを区別する。** 前者はIDの打ち間違い、
/// 後者は既に片付いた明細をもう一度仕訳にしようとしている——直し方が違う。
async fn find_pending(
    ctx: &ToolContext<'_>,
    imported_tx_id: &str,
) -> Result<kaikei_app::view::ImportedTxView, ToolFailure> {
    let query = ctx.imported_tx_query();
    let all = query
        .list_imported(&ImportedTxQuerySpec::default(), LOOKUP_LIMIT)
        .await
        .map_err(|error| ToolFailure::from(ToolError::from_app_error(&error.into())))?;

    let found = all.into_iter().find(|tx| tx.id == imported_tx_id);
    match found {
        Some(tx) if tx.status == "pending" => Ok(tx),
        Some(tx) => Err(ToolError::new(
            kaikei_app::error::codes::INVALID_VALUE,
            format!(
                "この明細は既に「{}」です。未処理の明細だけを仕訳にできます（id={imported_tx_id}）",
                tx.status
            ),
        )
        .into()),
        None => Err(ToolError::new(
            kaikei_app::error::codes::NOT_FOUND,
            format!(
                "取り込んだ明細が見つかりません（id={imported_tx_id}）。\
                 list_pending_transactions で id を確かめてください"
            ),
        )
        .into()),
    }
}

/// 口座が動いた側に、明細と同じ金額の行があるか。
///
/// **合計では見ない。** 振込手数料が差し引かれた入金のように、合計が明細と
/// 一致しない仕訳は正当に存在する（入金 100,000 ＋ 手数料 10,000 ＝
/// 売上 110,000）。合計で見ると、この普通の記帳ができなくなる。
fn check_amount(
    lines: &[JournalLine],
    amount_minor: i64,
    is_money_in: bool,
) -> Result<(), AppError> {
    // 入金なら口座が増える＝借方。出金なら減る＝貸方。
    let expected_side = if is_money_in {
        Side::Debit
    } else {
        Side::Credit
    };
    let found = lines.iter().any(|line| {
        line.side() == expected_side && line.amount().minor() == i128::from(amount_minor)
    });

    if found {
        return Ok(());
    }
    Err(AppError::from(kaikei_core::CoreError::InvalidAmount {
        reason: format!(
            "明細の金額（{amount_minor}）と同じ金額の行が{}側にありません。\
             桁を確かめてください。意図して一致しない場合（振込手数料が差し引かれた入金など）は \
             allow_amount_mismatch を true にしてください",
            if is_money_in { "借方" } else { "貸方" }
        ),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_core::{AccountCode, Currency, Money, TagSet};

    fn line(code: &str, side: Side, amount: i128) -> JournalLine {
        JournalLine::new(
            AccountCode::parse(code).unwrap(),
            side,
            Money::from_minor(amount, Currency::JPY),
            TagSet::new(),
            None,
        )
        .unwrap()
    }

    /// 出金は貸方に明細と同じ金額があればよい。
    #[test]
    fn a_payment_matches_on_the_credit_side() {
        let posted = [
            line("609", Side::Debit, 1_980),
            line("110", Side::Credit, 1_980),
        ];
        assert!(check_amount(&posted, 1_980, false).is_ok());
    }

    /// 入金は借方に明細と同じ金額があればよい。
    #[test]
    fn a_deposit_matches_on_the_debit_side() {
        let posted = [
            line("110", Side::Debit, 550_000),
            line("500", Side::Credit, 550_000),
        ];
        assert!(check_amount(&posted, 550_000, true).is_ok());
    }

    /// **本命。** 桁を落とした仕訳を止める。
    ///
    /// 貸借さえ合っていれば帳簿は受け取る。決算書を見ても分からない。
    #[test]
    fn an_entry_with_a_dropped_digit_is_rejected() {
        let posted = [
            line("609", Side::Debit, 1_980),
            line("110", Side::Credit, 1_980),
        ];

        let error = check_amount(&posted, 19_800, false).expect_err("止めること");

        let message = error.to_string();
        assert!(message.contains("19800"), "{message}");
        assert!(message.contains("allow_amount_mismatch"), "{message}");
    }

    /// **本命。** 振込手数料が差し引かれた入金が通る。
    ///
    /// 合計（110,000）は明細（100,000）と一致しないが、口座の行は一致する。
    /// 合計で見ると、この普通の記帳ができなくなる。
    #[test]
    fn a_deposit_net_of_a_transfer_fee_is_accepted() {
        let posted = [
            line("110", Side::Debit, 100_000),
            line("618", Side::Debit, 10_000),
            line("500", Side::Credit, 110_000),
        ];

        assert!(
            check_amount(&posted, 100_000, true).is_ok(),
            "手数料差引きの入金は通ること"
        );
    }

    /// 家事按分（費用と事業主貸に割る）が通る。
    #[test]
    fn a_household_split_is_accepted() {
        let posted = [
            line("614", Side::Debit, 1_188),
            line("410", Side::Debit, 792),
            line("110", Side::Credit, 1_980),
        ];
        assert!(check_amount(&posted, 1_980, false).is_ok());
    }

    /// 同じ金額でも側が違えば一致とみなさない。
    ///
    /// 入金を出金として記帳すると、収入と経費が入れ替わる。
    #[test]
    fn the_same_amount_on_the_wrong_side_is_not_a_match() {
        let posted = [
            line("609", Side::Debit, 1_980),
            line("110", Side::Credit, 1_980),
        ];
        // 入金（借方に 1,980 がある）なので通る。
        assert!(check_amount(&posted, 1_980, true).is_ok());

        let only_credit = [
            line("110", Side::Debit, 1_000),
            line("609", Side::Debit, 980),
            line("500", Side::Credit, 1_980),
        ];
        // 入金なのに借方に 1,980 が無い。
        assert!(check_amount(&only_credit, 1_980, true).is_err());
    }

    /// 説明に、まとまりとして行うことが書いてある。
    #[test]
    fn the_description_says_it_is_one_unit_of_work() {
        let description = JournalizeTransaction::DESCRIPTION;
        assert!(description.contains("1つのまとまり"), "{description}");
        assert!(
            description.contains("list_pending_transactions"),
            "{description}"
        );
    }

    /// 説明に、未処理でない明細を扱えないことが書いてある。
    #[test]
    fn the_description_says_only_pending_lines_can_be_journalized() {
        let description = JournalizeTransaction::DESCRIPTION;
        assert!(description.contains("未処理でない明細"), "{description}");
    }
}
