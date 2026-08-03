//! `get_settings` — 起動時に確定した事業者設定（`docs/07-mcp-server.md`
//! §2 / §7 / §10 MC-19）。
//!
//! # 日付引数を取らない
//!
//! 事業者設定は起動時に一度だけ合成され、取引日に応じて変わらない
//! （`DECISIONS.md` D-057）。**取引日で変わるのは税区分マスタ**であり、
//! そちらは `list_tax_categories` が日付を取る。
//!
//! # 既定値を返すことはない
//!
//! 設定が1つでも欠けていればサーバは**起動に失敗する**（同 §7 / D-082）ので、
//! このツールが「未設定なので既定値」を返す経路は存在しない。
//!
//! # ★`kept_existing` の出口を stderr だけにしない★（§7 の PR-G への申し送り）
//!
//! 起動時の勘定科目マスタ投入は、DB の科目定義がテンプレートと食い違っても
//! **既存を残して起動を続ける**（D-081）。記帳は DB の chart を正とするので
//! 帳簿は自己整合しており、起動を中止する理由は無い。
//!
//! しかし PR-E 時点では、その食い違い（`ImportChartOutput::kept_existing`）の
//! **唯一の出口が stderr** だった。D-082 は「未設定を警告付きで既定値にする」
//! 案を「**警告は stderr にしか出ず、AI にも利用者にも届かない**（MCP
//! クライアントがサーバの stderr を表示する保証は無い）」という理由で却下して
//! おり、同じ理由がここにも当てはまる。
//!
//! そこで合成ルートが `kept_existing` を [`crate::startup::Runtime`] に持たせ、
//! このツールが `chart_differences` として返す。AI は `list_accounts` が返す
//! 名称がテンプレートと違う理由を、応答だけで説明できるようになる
//! （`DECISIONS.md` D-086）。
//!
//! # ★`chart_differences` は起動時点のスナップショットである★（PR-G レビュー C-3）
//!
//! `list_accounts` は**毎回 DB の `accounts` を読む**のに対し、
//! `chart_differences` が返すのは [`crate::startup::assemble`] が起動時に
//! 採取した `Vec` である。稼働中に `accounts` が編集されると**両者は食い違う**
//! （`accounts` は帳簿本体と違い append-only ではない。`ports.rs` の
//! [`kaikei_app::ports::ChartWriteRepo`] の doc）。
//!
//! したがって値だけを裸で返さず、`{"as_of": "startup", "items": [...]}` の形で
//! **いつ時点の観測か**を応答に残す。この crate の他の箇所——0行の試算表でも
//! 通貨を名乗る（D-074）、`suggest_tax_category` が `filtered_by` を必ず返す——
//! と同じ規律（**その値がどういう条件で得られたかを応答に残す**）である。

use kaikei_app::usecase::import_chart::ChartDifference;
use kaikei_jp::tax::JpSettings;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::dispatch::{McpTool, ToolContext, ToolFailure, ToolSuccess};
use crate::wire::account_def_to_json;
use kaikei_app::context::BookSettings;

/// `get_settings`。
pub struct GetSettings;

// ★この構造体の doc コメントは `tools/list` の応答に出る★
// 内部設計書への参照・crate 名・Markdown の強調記法を書かないこと。
/// 設定の取得。引数はありません（指定していないキーは受け付けません）。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetSettingsInput {}

impl McpTool for GetSettings {
    type Input = GetSettingsInput;

    const NAME: &'static str = "get_settings";

    const DESCRIPTION: &'static str = "\
この帳簿の設定を返します。経理方式（tax_mode: exclusive は税抜経理、\
inclusive は税込経理）・端数処理の方式と単位・課税事業者かどうか・\
簡易課税かどうか・会計年度の区切り規則・帳簿通貨です。\
これらはサーバーの起動時に確定しており、取引日によって変わりません\
（取引日で変わる消費税区分は list_tax_categories で取得します）。\
auto_tax_lines を指定した記帳で消費税額の行が生成されるかどうかは、\
ここで返る tax_mode と is_taxable_business で決まります。\
勘定科目マスタの定義が同梱テンプレートと食い違っていた場合は、\
その内容を chart_differences.items に返します（帳簿は登録済みの定義で動いています）。\
chart_differences は起動時に採取した結果であり（as_of は startup）、\
サーバーの起動後に勘定科目が編集されても更新されません。\
勘定科目の現在の状態は list_accounts で取得してください。";

    async fn run(ctx: &ToolContext<'_>, _input: Self::Input) -> Result<ToolSuccess, ToolFailure> {
        let composition = ctx.composition();
        Ok(ToolSuccess::new(success_body(
            composition.tax_policy.settings(),
            &ctx.book_settings(),
            ctx.chart_differences(),
        )))
    }
}

/// 成功応答の本文。
///
/// 機械可読名は**すべて `kaikei-jp` / `kaikei-app` の入口**を通す
/// （`tax_mode` / `rounding` / `rounding_unit` は `kaikei_jp::tax`、
/// `fiscal_year_rule` は `kaikei_app::wire`。同じ綴りの表をこの層で
/// 作らない。`DECISIONS.md` D-072）。
fn success_body(
    settings: JpSettings,
    book: &BookSettings,
    differences: &[ChartDifference],
) -> Map<String, Value> {
    let mut body = Map::new();
    body.insert("tax_mode".to_string(), json!(settings.tax_mode.as_code()));
    body.insert(
        "rounding".to_string(),
        json!(kaikei_jp::tax::round_mode_code(settings.rounding)),
    );
    body.insert(
        "rounding_unit".to_string(),
        json!(settings.rounding_unit.as_code()),
    );
    body.insert(
        "is_taxable_business".to_string(),
        json!(settings.is_taxable_business),
    );
    body.insert(
        "simplified_taxation".to_string(),
        json!(settings.simplified_taxation),
    );
    body.insert(
        "fiscal_year_rule".to_string(),
        json!(kaikei_app::wire::fiscal_year_rule_code(
            book.fiscal_year_rule
        )),
    );
    // 帳簿通貨はコードと**小数桁数**の組である（桁数を1つ間違えると金額が
    // 100倍ずれる。`CLAUDE.md` §8）。両方返す。
    body.insert(
        "book_currency".to_string(),
        json!({
            "code": book.book_currency.code(),
            "minor_unit": book.book_currency.minor_unit(),
        }),
    );
    // ★いつ時点の観測かを応答に残す★（PR-G レビュー C-3。モジュール doc）
    // `as_of` は列挙値で、現在は `"startup"` の1つだけ。裸の配列に戻さないこと
    // （戻すと「毎回 DB を見た結果」と区別が付かなくなる）。
    body.insert(
        "chart_differences".to_string(),
        json!({
            "as_of": AS_OF_STARTUP,
            "items": Value::Array(differences.iter().map(difference_to_json).collect()),
        }),
    );
    body
}

/// `chart_differences.as_of` の値。**起動時に一度だけ採取した**ことを示す。
const AS_OF_STARTUP: &str = "startup";

/// テンプレートと食い違って**既存を残した**科目1件。
///
/// 文言（`message`）は [`ChartDifference::describe`] をそのまま使う
/// （起動時に stderr へ出しているものと同一。言い換えると2つの説明が育つ）。
fn difference_to_json(difference: &ChartDifference) -> Value {
    json!({
        "account": difference.code.as_str(),
        "fields": difference.fields,
        // 帳簿が使っているのは `existing` の側である（テンプレートで
        // 上書きしていない。D-081）。どちらが有効かを取り違えられない
        // ようにキー名で示す。
        "in_use": account_def_to_json(&difference.existing),
        "template": account_def_to_json(&difference.incoming),
        "message": difference.describe(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_app::context::FiscalYearRule;
    use kaikei_core::{AccountCode, AccountDef, AccountType, Currency, RoundMode};
    use kaikei_jp::tax::{RoundingUnit, TaxMode};

    fn settings() -> JpSettings {
        JpSettings {
            tax_mode: TaxMode::Exclusive,
            rounding: RoundMode::Floor,
            rounding_unit: RoundingUnit::Line,
            is_taxable_business: true,
            simplified_taxation: false,
        }
    }

    fn book() -> BookSettings {
        BookSettings {
            fiscal_year_rule: FiscalYearRule::CalendarYear,
            book_currency: Currency::JPY,
        }
    }

    fn difference() -> ChartDifference {
        let existing = AccountDef {
            code: AccountCode::parse("500").unwrap(),
            name: "売上（本業）".to_string(),
            account_type: AccountType::Revenue,
            parent: None,
            postable: true,
        };
        let incoming = AccountDef {
            name: "売上高".to_string(),
            ..existing.clone()
        };
        ChartDifference {
            code: AccountCode::parse("500").unwrap(),
            fields: vec!["name"],
            existing,
            incoming,
        }
    }

    // MC-19: 起動時に合成した設定をそのまま返す（機械可読名で）。
    #[test]
    fn the_composed_settings_are_returned_with_their_machine_readable_codes() {
        let body = Value::Object(success_body(settings(), &book(), &[]));

        assert_eq!(body["tax_mode"], json!("exclusive"));
        assert_eq!(body["rounding"], json!("floor"));
        assert_eq!(body["rounding_unit"], json!("line"));
        assert_eq!(body["is_taxable_business"], json!(true));
        assert_eq!(body["simplified_taxation"], json!(false));
        assert_eq!(body["fiscal_year_rule"], json!("calendar_year"));
        assert_eq!(body["book_currency"]["code"], json!("JPY"));
        // 小数桁数まで返す（金額の解釈に要る。`CLAUDE.md` §8）。
        assert_eq!(body["book_currency"]["minor_unit"], json!(0));
    }

    // 綴りは `kaikei-jp` / `kaikei-app` の入口と一致する（この層で作っていない）。
    #[test]
    fn the_codes_come_from_the_frozen_vocabulary_not_from_this_layer() {
        let settings = JpSettings {
            tax_mode: TaxMode::Inclusive,
            rounding: RoundMode::HalfUp,
            rounding_unit: RoundingUnit::Document,
            ..settings()
        };
        let body = Value::Object(success_body(settings, &book(), &[]));

        assert_eq!(body["tax_mode"], json!(TaxMode::Inclusive.as_code()));
        assert_eq!(
            body["rounding"],
            json!(kaikei_jp::tax::round_mode_code(RoundMode::HalfUp))
        );
        assert_eq!(
            body["rounding_unit"],
            json!(RoundingUnit::Document.as_code())
        );
        assert_eq!(
            body["fiscal_year_rule"],
            json!(kaikei_app::wire::fiscal_year_rule_code(
                FiscalYearRule::CalendarYear
            ))
        );
    }

    // ★PR-E からの申し送り★ テンプレートと食い違った科目が応答に出る
    // （stderr だけを出口にしない）。
    #[test]
    fn accounts_kept_from_the_database_are_reported_in_the_response() {
        let body = Value::Object(success_body(settings(), &book(), &[difference()]));

        let differences = body["chart_differences"]["items"].as_array().expect("配列");
        assert_eq!(differences.len(), 1, "{body}");
        let difference = &differences[0];
        assert_eq!(difference["account"], json!("500"));
        assert_eq!(difference["fields"], json!(["name"]));
        // 帳簿が使っている定義と、採用されなかったテンプレートの両方が分かる。
        assert_eq!(difference["in_use"]["name"], json!("売上（本業）"));
        assert_eq!(difference["template"]["name"], json!("売上高"));
        // 文言は起動時に stderr へ出すものと同一（言い換えていない）。
        assert_eq!(difference["message"], json!(self::difference().describe()));
        assert!(difference["message"]
            .as_str()
            .unwrap()
            .contains("既存の定義を残し"));
    }

    // 食い違いが無ければ空配列（キーは必ず出す。「載せ忘れ」と区別する）。
    #[test]
    fn no_difference_is_an_empty_array_and_the_key_is_still_present() {
        let body = Value::Object(success_body(settings(), &book(), &[]));
        assert_eq!(body["chart_differences"]["items"], json!([]));
    }

    // ★PR-G レビュー C-3★ **起動時点の観測であることが応答に残る。**
    //
    // `list_accounts` は毎回 DB を読むので、稼働中に `accounts` が編集されると
    // 両者は食い違う。裸の配列で返すと、その食い違いを説明する手掛かりが
    // 応答から消える。
    #[test]
    fn the_chart_differences_say_when_they_were_observed() {
        for differences in [&[][..], &[difference()][..]] {
            let body = Value::Object(success_body(settings(), &book(), differences));
            let reported = &body["chart_differences"];
            assert_eq!(reported["as_of"], json!("startup"), "{body}");
            assert!(reported["items"].is_array(), "{body}");
            // 裸の配列に戻していない（戻すと「毎回 DB を見た結果」と
            // 区別が付かなくなる）。
            assert!(!reported.is_array(), "{body}");
        }
    }

    // 日付引数を取らない（設定は取引日で変わらない。D-057）。
    #[test]
    fn the_input_takes_no_arguments() {
        serde_json::from_str::<GetSettingsInput>("{}").expect("引数なしで呼べる");
        let err = serde_json::from_str::<GetSettingsInput>(r#"{"date":"2026-04-15"}"#)
            .expect_err("日付は受け付けない");
        assert!(err.to_string().contains("date"), "{err}");
    }

    // 説明文が `CLAUDE.md` §10 の禁止表現を含まず、次に何を見ればよいか分かる。
    #[test]
    fn the_description_avoids_forbidden_claims_and_states_the_next_step() {
        let description = GetSettings::DESCRIPTION;
        for forbidden in ["準拠", "法令対応", "JIIMA"] {
            assert!(!description.contains(forbidden), "{forbidden}");
        }
        assert!(description.contains("list_tax_categories"));
        assert!(description.contains("chart_differences"));
        // 起動時点のスナップショットであることを説明文にも書く（C-3）。
        assert!(description.contains("起動時"), "{description}");
        assert!(description.contains("list_accounts"), "{description}");
    }
}
