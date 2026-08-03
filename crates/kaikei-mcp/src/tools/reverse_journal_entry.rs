//! `reverse_journal_entry` — 逆仕訳（赤伝）を起こす（`docs/07-mcp-server.md` §3）。
//!
//! # 検証を重ねて書かない
//!
//! 訂正理由（`reason`）の非空検証は **`kaikei-app` のユースケース層**にある
//! （`reverse_entry::execute` が I/O より前に `AppError::EmptyReverseReason`
//! を返す。`DECISIONS.md` D-074）。MCP 層はそれを写像するだけで、同じ検証を
//! ここに書かない——書くと MCP 以外の呼び出し元（将来の CLI / `kaikei-api`）に
//! 規律が効かなくなる。
//!
//! `reason` を `Option` にしないので、「省略」だけはデシリアライズで弾かれる。
//! 空文字・空白のみ（全角スペースを含む）はユースケース層が弾く。
//!
//! # `policy_notes` を返さない（キーごと出さない）
//!
//! `reverse_entry::execute` は `TaxPolicy` を引数に取らず、注記の発生経路が
//! 存在しない。`"policy_notes": []` を置くと「policy を通したが注記が
//! 無かった」と区別できず、AI を誤った方向へ導く
//! （`PROGRESS.md` Phase 1 の教訓3）。

use kaikei_app::error::AppError;
use kaikei_app::id::{entry_id_from_uuid_string, entry_id_to_uuid_string};
use kaikei_app::tx::with_tx;
use kaikei_app::usecase::reverse_entry::{self, ReverseEntryInput, ReverseEntryOutput};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Map};

use crate::dispatch::{McpTool, ToolContext, ToolFailure, ToolSuccess};
use crate::error::ToolError;
use crate::tools::{in_field, parse_date};
use crate::wire::lines_to_json;

/// `reverse_journal_entry`。
pub struct ReverseJournalEntry;

// ★この構造体の doc コメントは `tools/list` の応答に出る★
// 内部設計書への参照・crate 名・Markdown の強調記法を書かないこと
// （PR-F レビュー D-2。形は `docs/07-mcp-server.md` §3）。
/// 逆仕訳1件の入力。指定していないキーは受け付けません。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReverseJournalEntryInput {
    /// 訂正対象の仕訳ID。post_journal_entry が返した entry_id
    /// （ハイフン付き36文字の UUID）をそのまま指定します。
    pub original_id: String,

    /// 逆仕訳の取引日。YYYY-MM-DD の形式で指定します。会計年度はこの日付で
    /// 決まります（元の仕訳が別の年度でも、逆仕訳はこの日付の年度に入ります）。
    pub reverse_date: String,

    /// 訂正理由。必須です（空文字や空白のみは受け付けません）。
    /// 入力したままの文言が帳簿に残ります。
    pub reason: String,

    /// 既に訂正済みの仕訳をもう一度訂正することを明示的に許可します。
    /// 既定は false で、そのままだと二重の訂正は拒否されます。
    #[serde(default)]
    pub allow_double_reversal: bool,
}

impl McpTool for ReverseJournalEntry {
    type Input = ReverseJournalEntryInput;

    const NAME: &'static str = "reverse_journal_entry";

    const DESCRIPTION: &'static str = "\
既存の仕訳を逆仕訳（赤伝）で訂正します。帳簿は追記のみで、元の仕訳は書き換わりません。\
original_id は仕訳ID（UUID の正準表記）、reverse_date は逆仕訳の取引日（YYYY-MM-DD）で、\
会計年度はこの日付で決まります。reason（訂正理由）は必須で、空文字や空白のみは拒否されます。\
理由は入力したままの文言が帳簿に残ります。\
既に赤伝済みの仕訳をもう一度訂正する場合だけ allow_double_reversal に true を指定します。\
金額の指定はありません（明細は元の仕訳の貸借を入れ替えて複製されます）。";

    async fn run(ctx: &ToolContext<'_>, input: Self::Input) -> Result<ToolSuccess, ToolFailure> {
        // UUID のパースは `kaikei-app` の入口を通す（`uuid::Uuid::parse_str` を
        // 直に書かない。D-047 と同型の問題。`docs/07-mcp-server.md` §3）。
        let original_id = entry_id_from_uuid_string(&input.original_id)
            .map_err(|error| in_field("original_id", ToolError::from_app_error(&error)))?;
        let reverse_date = parse_date("reverse_date", &input.reverse_date)?;

        let reverse_input = ReverseEntryInput {
            original_id,
            reverse_date,
            reason: input.reason,
            allow_double_reversal: input.allow_double_reversal,
        };

        let composition = ctx.composition();
        let settings = ctx.book_settings();
        let id_gen = ctx.id_gen();
        let clock = ctx.clock();

        let reversed = with_tx(ctx.store(), move |tx| {
            Box::pin(async move {
                reverse_entry::execute(
                    tx,
                    composition.tag_catalog.schema(),
                    &id_gen,
                    &clock,
                    &settings,
                    reverse_input,
                )
                .await
            })
        })
        .await;

        match reversed {
            Ok(output) => Ok(success(&output)),
            Err(error) => Err(describe_failure(&error).into()),
        }
    }
}

/// 成功応答（`docs/07-mcp-server.md` §3）。
fn success(output: &ReverseEntryOutput) -> ToolSuccess {
    let entry = &output.entry;
    let mut body = Map::new();
    body.insert(
        "entry_id".to_string(),
        json!(entry_id_to_uuid_string(entry.id())),
    );
    body.insert("entry_no".to_string(), json!(entry.entry_no().as_u32()));
    body.insert("fiscal_year".to_string(), json!(entry.fiscal_year()));
    body.insert(
        "entry_date".to_string(),
        json!(entry.entry_date().to_iso_string()),
    );
    // 訂正対象の仕訳ID。`reverses` が `None` になることはない（`reverse` が
    // 必ず設定する）が、型は `Option` なので握り潰さず素直に写す。
    body.insert(
        "reverses".to_string(),
        match entry.reverses() {
            Some(id) => json!(entry_id_to_uuid_string(id)),
            None => json!(null),
        },
    );
    body.insert("description".to_string(), json!(entry.description()));
    body.insert("lines".to_string(), lines_to_json(entry.lines()));

    ToolSuccess::new(body).with_entry_id(entry.id())
}

/// 失敗応答。
///
/// 二重訂正のときは**既存の赤伝の仕訳ID**を返す。呼び出し元が仕訳を指すのは
/// UUID であって通し番号ではなく、番号だけ返しても AI はその赤伝を
/// `get_entry` で開けない（`docs/07-mcp-server.md` §3）。
fn describe_failure(error: &AppError) -> ToolError {
    let tool_error = ToolError::from_app_error(error);
    match error {
        AppError::AlreadyReversed {
            reversal_no,
            reversal_id,
            ..
        } => tool_error
            .with_detail("reversal_id", json!(entry_id_to_uuid_string(*reversal_id)))
            .with_detail("reversal_no", json!(reversal_no.as_u32())),
        // `AppError` は `#[non_exhaustive]`。受け皿が必須である（§6）。
        _ => tool_error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_app::error::codes;
    use kaikei_core::{EntryId, EntryNumber};

    fn parse(json: &str) -> Result<ReverseJournalEntryInput, serde_json::Error> {
        serde_json::from_str(json)
    }

    // 設計書 §3 の例がそのまま受理される。
    #[test]
    fn the_documented_request_shape_is_accepted() {
        let input = parse(
            r#"{
                "original_id": "0192a7b3-1234-7abc-8def-0123456789ab",
                "reverse_date": "2026-05-01",
                "reason": "請求金額の誤り（税率の適用誤り）",
                "allow_double_reversal": false
            }"#,
        )
        .expect("設計書 §3 の例は受理されるはず");
        assert!(!input.allow_double_reversal);
        assert_eq!(input.reason, "請求金額の誤り（税率の適用誤り）");
    }

    // MC-12 の「省略」だけはデシリアライズで弾かれる（`reason` は `Option`
    // にしない）。空文字・空白のみは `kaikei-app` が弾くのでここでは見ない。
    #[test]
    fn omitting_the_reason_is_rejected_by_deserialization() {
        let err = parse(
            r#"{"original_id":"0192a7b3-1234-7abc-8def-0123456789ab","reverse_date":"2026-05-01"}"#,
        )
        .expect_err("reason の省略は受理しない");
        assert!(err.to_string().contains("reason"), "{err}");
    }

    // 二重訂正のエラーは既存の赤伝を UUID で示す（10進表記にしない）。
    #[test]
    fn a_double_reversal_error_points_at_the_existing_reversal_by_uuid() {
        let reversal_id = EntryId::new(0x0192_b1c4_1234_7abc_8def_0123_4567_89ab);
        let error = describe_failure(&AppError::AlreadyReversed {
            entry_no: EntryNumber::new(42),
            reversal_no: EntryNumber::new(43),
            reversal_id,
        });

        assert_eq!(error.code(), codes::ALREADY_REVERSED);
        let body = error.to_json();
        assert_eq!(
            body["reversal_id"],
            json!(entry_id_to_uuid_string(reversal_id))
        );
        assert_eq!(body["reversal_no"], json!(43));
        // 10進表記（最大39桁）で漏れていないこと。
        assert!(!body
            .to_string()
            .contains(&reversal_id.as_u128().to_string()));
        // 次の手が本文に含まれる（`CLAUDE.md` §11）。
        assert!(
            error.message().contains("allow_double_reversal"),
            "{}",
            error.message()
        );
    }

    // 説明文が `CLAUDE.md` §10 の禁止表現を含まない。
    #[test]
    fn the_description_avoids_forbidden_claims() {
        for forbidden in ["準拠", "法令対応", "JIIMA"] {
            assert!(
                !ReverseJournalEntry::DESCRIPTION.contains(forbidden),
                "{forbidden}"
            );
        }
        assert!(ReverseJournalEntry::DESCRIPTION.contains("追記のみ"));
    }
}
