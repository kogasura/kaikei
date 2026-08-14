//! `search_documents` — 証憑をメタデータで探す（`docs/06-documents.md` §4）。
//!
//! # ファイルの中身は返さない
//!
//! 返すのは**メタデータだけ**である。証憑の実体（PDF や画像）を応答に載せると、
//! 量がファイルの大きさに比例し、append-only の `audit_log` に毎回残る
//! （`docs/07-mcp-server.md` §9）。証憑そのものを取り出すのは
//! `kaikei report`（`documents/` へ書き出す）の仕事である。
//!
//! 代わりに内容の SHA-256 を返すので、保存先のどのファイルかは特定できる。
//!
//! # 検索要件の3項目
//!
//! 取引年月日・取引金額・取引先の組み合わせと範囲指定に対応する。これが
//! 電子取引データの検索要件の内容である（§4）。
//!
//! # 0 件は正常
//!
//! 条件に合う証憑が無いのは異常ではない。**空配列を返してエラーにしない**。
//! ただし「証憑が1件も登録されていない」のか「条件に合わなかった」のかは
//! 読む側が区別できるべきなので、帳簿全体の登録件数を添える。

use crate::dispatch::{McpTool, ToolContext, ToolFailure, ToolSuccess};
use crate::error::ToolError;
use kaikei_app::error::codes;
use kaikei_app::ports::DocumentQueryPort;
use kaikei_app::view::DocumentQuery;
use kaikei_core::AccountingDate;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Map, Value};

/// 一度に返す上限。
///
/// 応答の量が帳簿の大きさに比例しないようにする（`docs/07-mcp-server.md` §5）。
const DEFAULT_LIMIT: u32 = 50;
const MAX_LIMIT: u32 = 200;

/// `search_documents`。
pub struct SearchDocuments;

// ★この構造体の doc コメントは `tools/list` の応答に出る★
/// 証憑の検索条件。指定していないキーは受け付けません。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchDocumentsInput {
    /// 取引年月日の下限（この日を含む）。YYYY-MM-DD。
    #[serde(default)]
    pub date_from: Option<String>,
    /// 取引年月日の上限（この日を含む）。YYYY-MM-DD。
    #[serde(default)]
    pub date_to: Option<String>,
    /// 取引金額の下限（この額を含む）。円。
    #[serde(default)]
    pub amount_min: Option<i64>,
    /// 取引金額の上限（この額を含む）。円。
    #[serde(default)]
    pub amount_max: Option<i64>,
    /// 取引先（完全一致）。
    #[serde(default)]
    pub counterparty: Option<String>,
    /// 種別（invoice / receipt / contract / other）。
    #[serde(default)]
    pub doc_type: Option<String>,
    /// 取得件数。既定は 50、最大 200。
    #[serde(default)]
    pub limit: Option<u32>,
}

impl McpTool for SearchDocuments {
    type Input = SearchDocumentsInput;

    const NAME: &'static str = "search_documents";

    const DESCRIPTION: &'static str = "\
証憑（請求書・領収書・契約書など）をメタデータで探します。\
取引年月日・取引金額・取引先の組み合わせと範囲指定に対応します。\
金額は文字列で返します（数値にすると読み直しで誤差が出るため）。\
ファイルの中身は返しません。返すのは内容の SHA-256 とメタデータだけです\
（実体を取り出すには kaikei report を使い、documents/ へ書き出します）。\
条件に合う証憑が無い場合は空の配列を返します。これは異常ではありません。\
帳簿に登録されている総数を total_registered に添えるので、\
「1件も登録されていない」のか「条件に合わなかった」のかを区別できます。\
取引金額の無い証憑（契約書など）は amount_minor が null になります。\
金額の条件を指定すると、これらは結果に含まれません。";

    async fn run(ctx: &ToolContext<'_>, input: Self::Input) -> Result<ToolSuccess, ToolFailure> {
        let limit = input.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);

        let query = DocumentQuery {
            date_from: parse_date(input.date_from.as_deref(), "date_from")?,
            date_to: parse_date(input.date_to.as_deref(), "date_to")?,
            amount_min: input.amount_min,
            amount_max: input.amount_max,
            counterparty: input.counterparty,
            doc_type: input.doc_type,
        };

        // **期間の逆指定を黙って 0 件にしない。** 指定を間違えたのか本当に
        // 無いのかを、AI が区別できるようにする。
        if let (Some(from), Some(to)) = (query.date_from, query.date_to) {
            if from > to {
                return Err(ToolError::new(
                    codes::INVALID_VALUE,
                    format!(
                        "date_from（{}）が date_to（{}）より後です。入れ替えるか、正しい期間を指定してください",
                        from.to_iso_string(),
                        to.to_iso_string()
                    ),
                )
                .into());
            }
        }

        let documents = ctx.document_query();
        let found = documents
            .search_documents(&query, limit)
            .await
            .map_err(|error| ToolFailure::from(ToolError::from_app_error(&error.into())))?;

        // 「1件も登録されていない」と「条件に合わなかった」を区別できるように、
        // 帳簿全体の件数を添える。
        let total_registered = documents
            .all_blob_hashes()
            .await
            .map_err(|error| ToolFailure::from(ToolError::from_app_error(&error.into())))?
            .len();

        let items: Vec<Value> = found.iter().map(to_json).collect();
        let mut body = Map::new();
        body.insert("documents".to_string(), Value::Array(items));
        body.insert("count".to_string(), json!(found.len()));
        body.insert("total_registered".to_string(), json!(total_registered));
        body.insert("limit".to_string(), json!(limit));
        Ok(ToolSuccess::new(body))
    }
}

fn to_json(document: &kaikei_app::view::DocumentView) -> Value {
    json!({
        "id": document.id,
        // 保存先のどのファイルかを特定できるようにする（中身は返さない）。
        "blob_hash": document.blob_hash,
        "original_name": document.original_name,
        "mime_type": document.mime_type,
        "byte_size": document.byte_size,
        "doc_date": document.doc_date.to_iso_string(),
        // **金額は文字列。** 数値にすると読み直しで誤差が出る。
        // 金額の無い証憑は null（0 で埋めない）。
        "amount_minor": document.amount_minor.map(|value| value.to_string()),
        "counterparty": document.counterparty,
        "doc_type": document.doc_type,
        "received_via": document.received_via,
        "note": document.note,
    })
}

fn parse_date(text: Option<&str>, field: &str) -> Result<Option<AccountingDate>, ToolFailure> {
    match text {
        None => Ok(None),
        Some(text) => AccountingDate::parse(text).map(Some).map_err(|error| {
            ToolError::new(
                codes::INVALID_VALUE,
                format!("{field} は YYYY-MM-DD の形式で指定してください（受け取った値: {text}。{error}）"),
            )
            .into()
        }),
    }
}
