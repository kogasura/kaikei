//! 勘定科目テンプレート（`kaikei-jp-data/chart/*.yaml`）→ `kaikei_core::ChartOfAccounts`。
//!
//! `docs/04-jp-tax.md` §5「勘定科目テンプレート」に対応する。`ChartOfAccounts`
//! は `kaikei-core` の型であり、この module から直接メソッドを生やせない
//! （orphan rule）ため、[`load_embedded`] / [`load_from_path`] /
//! [`load_from_str`] という自由関数を公開する（`crate::yaml` と同じ命名で、
//! 呼び出し側は `chart::load_embedded(...)` のようにモジュール修飾して呼ぶ）。
//!
//! # YAML に無いフィールドの扱い（`DECISIONS.md` D-061）
//!
//! `kaikei_core::AccountDef` は `parent` / `postable` を持つが、同梱の
//! `sole_proprietor.yaml` にはどちらも書かれていない。一方で YAML 自身の
//! 先頭コメントは `postable: false` なら見出し科目になる、と明記している。
//! そこで:
//!
//! - `postable`: 省略時は `true`（記帳可能）として扱う。見出し科目
//!   （集計専用）を持つ科目表を書きたいユーザーは `postable: false` を
//!   明示すればよい
//! - `parent`: 省略時は `None`（親を持たない）。将来、階層を持つ科目表
//!   （見出し科目の配下に明細科目をぶら下げる形）を書きたい場合に使える
//! - `sort`: `kaikei_core::AccountDef` に対応するフィールドが無いため、
//!   ドメイン変換では**破棄する**（`deny_unknown_fields` によるスキーマ
//!   完全性検証のためだけに受け取る）。`ChartOfAccounts::iter()` は
//!   `AccountCode` の辞書順（`BTreeMap` 由来）で決定的に並ぶが、これは
//!   `sort` の値そのものではない。同梱テンプレートは科目コードが固定桁数の
//!   数字文字列であるため両者はたまたま一致するが、桁数が異なるコード体系
//!   では一致しない。表示順専用のフィールドを `kaikei_core::AccountDef` に
//!   追加するかどうかは、実際にその順序を使う画面・帳票が出てくるまで
//!   判断しない（YAGNI。必要になったら `kaikei-core` 側の変更として
//!   別途レビューする）

use crate::error::JpError;
use kaikei_core::{AccountCode, AccountDef, AccountType, ChartOfAccounts};
use kaikei_jp_data::EmbeddedYaml;
use serde::Deserialize;
use std::path::Path;

/// この PR 時点でこの crate が読める唯一のスキーマ版。
///
/// 未知のバージョンは構築時に拒否する（`tax/table.rs` の
/// `TaxCategoryTable::SUPPORTED_VERSION` と同じ方針。`DECISIONS.md` D-056）。
const SUPPORTED_VERSION: u32 = 1;

/// `kaikei-jp-data` の埋め込み YAML から勘定科目表を読み込む。
///
/// 例: `chart::load_embedded(kaikei_jp_data::CHART_SOLE_PROPRIETOR)`。
pub fn load_embedded(embedded: EmbeddedYaml) -> Result<ChartOfAccounts, JpError> {
    let raw: ChartRaw = crate::yaml::load_embedded(embedded)?;
    from_raw(embedded.label, raw)
}

/// 任意のファイルパスから勘定科目表を読み込む（ユーザーが自分の科目表に
/// 差し替える経路）。
pub fn load_from_path(path: &Path) -> Result<ChartOfAccounts, JpError> {
    let raw: ChartRaw = crate::yaml::load_from_path(path)?;
    from_raw(&path.display().to_string(), raw)
}

/// YAML 文字列から勘定科目表を読み込む（テスト、および上2つの共通経路）。
pub fn load_from_str(source: &str, label: &str) -> Result<ChartOfAccounts, JpError> {
    let raw: ChartRaw = crate::yaml::load_str(source, label)?;
    from_raw(label, raw)
}

fn from_raw(label: &str, raw: ChartRaw) -> Result<ChartOfAccounts, JpError> {
    let invalid = |reason: String| JpError::InvalidChart {
        label: label.to_string(),
        reason,
    };

    if raw.version != SUPPORTED_VERSION {
        return Err(invalid(format!(
            "対応していないスキーマバージョンです: {}（対応: {}）。新しいバージョンの \
             スキーマを読むには kaikei-jp 側の対応が必要です",
            raw.version, SUPPORTED_VERSION
        )));
    }

    let defs = raw
        .accounts
        .into_iter()
        .map(account_def_from_raw)
        .collect::<Result<Vec<_>, String>>()
        .map_err(invalid)?;

    // `ChartOfAccounts::new` が科目コードの重複・親の不在・循環参照を検証する。
    // `CoreError` の `Display` をそのまま `reason` に含めるため、元の理由は
    // 失われない（PR の要求事項。`kaikei_core::CoreError` は `thiserror` の
    // `#[error(...)]` で「勘定科目表が不正です: {reason}」を返す）。
    ChartOfAccounts::new(defs).map_err(|source| invalid(format!("{source}")))
}

fn account_def_from_raw(raw: AccountRaw) -> Result<AccountDef, String> {
    let code = AccountCode::parse(&raw.code)
        .map_err(|source| format!("科目コードが不正です: \"{}\": {source}", raw.code))?;
    let account_type = parse_account_type("type", &raw.account_type)
        .map_err(|reason| format!("code={}: {reason}", raw.code))?;
    let parent = raw
        .parent
        .as_deref()
        .map(AccountCode::parse)
        .transpose()
        .map_err(|source| format!("code={}: parent が不正です: {source}", raw.code))?;

    Ok(AccountDef {
        code,
        name: raw.name,
        account_type,
        parent,
        postable: raw.postable,
    })
}

/// `Asset` | `Liability` | `Equity` | `Revenue` | `Expense` の5値を
/// `kaikei_core::AccountType` に写像する。`tags.rs` の `required_for` 要素の
/// 解釈にも使う（`pub(crate)`）ため、フィールド名をエラーメッセージに含める
/// 呼び出し側（`field_name`。`chart.rs` では `"type"`、`tags.rs` では
/// `"required_for"`）に渡してもらう。
pub(crate) fn parse_account_type(field_name: &str, s: &str) -> Result<AccountType, String> {
    match s {
        "Asset" => Ok(AccountType::Asset),
        "Liability" => Ok(AccountType::Liability),
        "Equity" => Ok(AccountType::Equity),
        "Revenue" => Ok(AccountType::Revenue),
        "Expense" => Ok(AccountType::Expense),
        other => Err(format!(
            "{field_name} の値が不正です: \"{other}\"（有効な値: Asset, Liability, Equity, Revenue, Expense）"
        )),
    }
}

fn default_postable() -> bool {
    true
}

/// [`ChartOfAccounts`] の YAML 上の生の形。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChartRaw {
    version: u32,
    /// テンプレートの表示名。`kaikei_core::ChartOfAccounts` に対応するフィールドが
    /// 無いため、ドメイン変換では使わない（人間がテンプレートを選ぶ際の
    /// 案内用の文字列で、この PR のスコープ外）。
    #[allow(dead_code)]
    name: String,
    accounts: Vec<AccountRaw>,
}

/// 科目1件分の YAML 上の生の形。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountRaw {
    code: String,
    name: String,
    #[serde(rename = "type")]
    account_type: String,
    /// 記帳可否。省略時は記帳可能（`true`）（`DECISIONS.md` D-061）。
    #[serde(default = "default_postable")]
    postable: bool,
    /// 親科目コード。省略時は `None`（`DECISIONS.md` D-061）。
    #[serde(default)]
    parent: Option<String>,
    /// 表示順のヒント。ドメイン変換では破棄する（`DECISIONS.md` D-061）。
    /// フィールド自体は `deny_unknown_fields` によるスキーマ完全性検証のため
    /// 受け取る。
    #[allow(dead_code)]
    sort: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_YAML: &str = r#"
version: 1
name: "テスト用テンプレート"
accounts:
  - { code: "100", name: "現金", type: Asset, sort: 100 }
  - { code: "500", name: "売上高", type: Revenue, sort: 500 }
"#;

    /// 実データ（`kaikei_jp_data::CHART_SOLE_PROPRIETOR`）がパースできること。
    #[test]
    fn load_embedded_parses_the_bundled_template() {
        let chart = load_embedded(kaikei_jp_data::CHART_SOLE_PROPRIETOR).unwrap();
        assert!(chart.iter().count() > 0);
        // 現金（100）は個人事業主テンプレートに必ず含まれる。
        let cash = AccountCode::parse("100").unwrap();
        let def = chart.get(&cash).expect("現金(100)が存在するはず");
        assert_eq!(def.name, "現金");
        assert!(
            def.postable,
            "YAML に postable が無い科目は記帳可能な既定値になるはず"
        );
        assert_eq!(
            def.parent, None,
            "YAML に parent が無い科目は None になるはず"
        );
    }

    #[test]
    fn load_from_str_parses_valid_chart() {
        let chart = load_from_str(VALID_YAML, "test").unwrap();
        assert_eq!(chart.iter().count(), 2);
    }

    #[test]
    fn load_from_str_rejects_unknown_top_level_field() {
        let yaml = format!("{VALID_YAML}\nextra_field: true\n");
        let err = load_from_str(&yaml, "test").unwrap_err();
        assert!(matches!(err, JpError::YamlParse { .. }));
    }

    #[test]
    fn load_from_str_rejects_unknown_account_field() {
        let yaml = VALID_YAML.replace(
            "{ code: \"100\", name: \"現金\", type: Asset, sort: 100 }",
            "{ code: \"100\", name: \"現金\", type: Asset, sort: 100, unexpected: true }",
        );
        let err = load_from_str(&yaml, "test").unwrap_err();
        assert!(matches!(err, JpError::YamlParse { .. }));
    }

    #[test]
    fn load_from_str_unsupported_version_is_error() {
        let yaml = VALID_YAML.replace("version: 1", "version: 2");
        let err = load_from_str(&yaml, "test").unwrap_err();
        match err {
            JpError::InvalidChart { reason, .. } => {
                assert!(reason.contains('2'), "reason = {reason}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn load_from_str_unknown_account_type_lists_valid_values() {
        let yaml = VALID_YAML.replace("type: Asset", "type: Cash");
        let err = load_from_str(&yaml, "test").unwrap_err();
        match err {
            JpError::InvalidChart { reason, .. } => {
                assert!(reason.contains("Cash"), "reason = {reason}");
                assert!(reason.contains("Asset"), "reason = {reason}");
                assert!(reason.contains("Liability"), "reason = {reason}");
                assert!(reason.contains("Equity"), "reason = {reason}");
                assert!(reason.contains("Revenue"), "reason = {reason}");
                assert!(reason.contains("Expense"), "reason = {reason}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn load_from_str_duplicate_account_code_is_error_and_keeps_original_reason() {
        let yaml = VALID_YAML.replace(
            "{ code: \"500\", name: \"売上高\", type: Revenue, sort: 500 }",
            "{ code: \"100\", name: \"売上高\", type: Revenue, sort: 500 }",
        );
        let err = load_from_str(&yaml, "test").unwrap_err();
        match err {
            JpError::InvalidChart { reason, .. } => {
                // kaikei_core::CoreError::InvalidChart の元の文言が残っていること。
                assert!(
                    reason.contains("勘定科目コードが重複しています"),
                    "reason = {reason}"
                );
                assert!(reason.contains("100"), "reason = {reason}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn load_from_str_missing_parent_is_error_and_keeps_original_reason() {
        let yaml = VALID_YAML.replace(
            "{ code: \"500\", name: \"売上高\", type: Revenue, sort: 500 }",
            "{ code: \"500\", name: \"売上高\", type: Revenue, sort: 500, parent: \"999\" }",
        );
        let err = load_from_str(&yaml, "test").unwrap_err();
        match err {
            JpError::InvalidChart { reason, .. } => {
                assert!(reason.contains("999"), "reason = {reason}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn load_from_str_cyclic_parent_is_error_and_keeps_original_reason() {
        let yaml = r#"
version: 1
name: "テスト用テンプレート"
accounts:
  - { code: "A", name: "A", type: Asset, sort: 1, parent: "B" }
  - { code: "B", name: "B", type: Asset, sort: 2, parent: "A" }
"#;
        let err = load_from_str(yaml, "test").unwrap_err();
        match err {
            JpError::InvalidChart { reason, .. } => {
                assert!(reason.contains("循環参照"), "reason = {reason}");
                assert!(reason.contains('A'), "reason = {reason}");
                assert!(reason.contains('B'), "reason = {reason}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn load_from_str_explicit_postable_false_is_respected() {
        let yaml = VALID_YAML.replace(
            "{ code: \"100\", name: \"現金\", type: Asset, sort: 100 }",
            "{ code: \"100\", name: \"現金\", type: Asset, sort: 100, postable: false }",
        );
        let chart = load_from_str(&yaml, "test").unwrap();
        let cash = AccountCode::parse("100").unwrap();
        assert!(!chart.get(&cash).unwrap().postable);
    }

    #[test]
    fn load_from_str_explicit_parent_is_respected() {
        let yaml = r#"
version: 1
name: "テスト用テンプレート"
accounts:
  - { code: "100", name: "見出し", type: Asset, sort: 100, postable: false }
  - { code: "110", name: "現金", type: Asset, sort: 110, parent: "100" }
"#;
        let chart = load_from_str(yaml, "test").unwrap();
        let cash = AccountCode::parse("110").unwrap();
        assert_eq!(
            chart.get(&cash).unwrap().parent,
            Some(AccountCode::parse("100").unwrap())
        );
    }

    /// 埋め込みと差し替えで検証の強さが変わらないこと。
    #[test]
    fn load_from_path_rejects_unknown_fields_just_like_embedded() {
        let path =
            std::env::temp_dir().join(format!("kaikei_jp_chart_test_{}.yaml", std::process::id()));
        std::fs::write(&path, format!("{VALID_YAML}\nextra_field: true\n")).unwrap();
        let err = load_from_path(&path).unwrap_err();
        let _ = std::fs::remove_file(&path);
        assert!(matches!(err, JpError::YamlParse { .. }));
    }

    /// ロード結果が決定的であること（同じ YAML を複数回ロードして同じ順序・内容になる）。
    ///
    /// `AccountDef` は `Debug` を導出しているため、`{:?}` で科目コード・名称・
    /// 種別・親・記帳可否まで含めて比較できる。実データ（`sole_proprietor.yaml`）
    /// を使い、`ChartOfAccounts::iter()`（`BTreeMap` 由来で科目コード順）の
    /// 並びが実行のたびに変わらないことを確認する。
    #[test]
    fn load_embedded_is_deterministic_across_repeated_loads() {
        let snapshots: Vec<Vec<String>> = (0..5)
            .map(|_| {
                let chart = load_embedded(kaikei_jp_data::CHART_SOLE_PROPRIETOR).unwrap();
                chart.iter().map(|d| format!("{d:?}")).collect()
            })
            .collect();

        for snapshot in &snapshots[1..] {
            assert_eq!(snapshot, &snapshots[0]);
        }
    }
}
