//! YAML の科目種別文字列 → [`kaikei_core::AccountType`] の変換。
//!
//! `chart.rs`（科目テンプレートの `type`）と `tags.rs`（タグスキーマの
//! `required_for`）の**両方**が使う。どちらか一方のファイルに置くと、
//! 他方がそのモジュール名に依存する形になり「どちらの持ち物か」が
//! 曖昧になるため、変換そのものを1つのモジュールに切り出してある
//! （`CLAUDE.md` §6）。

use kaikei_core::AccountType;

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
