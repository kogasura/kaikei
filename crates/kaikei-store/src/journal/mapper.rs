//! [`EntryRows`] → [`JournalEntry`] への変換。
//!
//! **`JournalEntry::rehydrate` を呼んでよい唯一の場所。**
//! `.github/workflows/architecture.yml` の「rehydrate の呼び出しは1箇所に
//! 限定」ステップがこれを grep で検査する（`kaikei-core/src/journal.rs` と
//! この `mapper.rs` 以外での呼び出しはビルドを失敗させる）。
//!
//! `rehydrate` は検証を一切行わない（`kaikei_core::JournalEntry::rehydrate`
//! の doc を参照。`lines` が空・通貨混在だと `currency()`/`debit_total()`/
//! `credit_total()` が呼び出し時に panic する）。呼び出す前に必ず以下の
//! 9項目を再検証し、違反は全て panic ではなく [`RepoError::Corrupt`] として
//! 返す（phase1計画 R4）。
//!
//! 1. `lines.len() >= 2`
//! 2. 全明細の `Currency` が同一
//! 3. `side` が 1|2
//! 4. `amount_minor > 0`
//! 5. `sum(debit) == sum(credit)`
//! 6. `description.trim()` が非空
//! 7. `reverses.is_some() == reverse_reason.is_some()`
//! 8. `line_no` が重複しない／昇順
//! 9. `entry_no` が `u32` に収まる
//!
//! **再検証してはいけないもの**（`rehydrate` の doc が明示）: 科目の存在・
//! `postable`・`TagSchema` 適合・会計年度範囲・締め状態。現在のマスタと
//! 過去の記帳の食い違いは仕様変更の履歴であってエラーではない。
//!
//! # テストの配置について
//!
//! [`JournalEntryRow`]/[`JournalLineRow`]/[`EntryRows`] は `pub(crate)`
//! （store crate の内部実装詳細であり、DB行の生表現を外部に公開しない設計。
//! `row.rs` のモジュール doc を参照）。`tests/` 配下の統合テストは別クレート
//! としてコンパイルされるため `pub(crate)` 項目には到達できず、これらの
//! 型を直接組み立てるテストは書けない。そのため、このファイルの
//! `#[cfg(test)] mod tests` にユニットテストとして置く（`convert.rs` /
//! `tags.rs` / `sqlstate.rs` と同じ、このクレート内で確立された配置規約）。

use super::row::EntryRows;
use crate::convert::{
    datetime_to_timestamp, entry_no_from_i32, money_from_columns, naive_date_to_accounting_date,
    side_from_i16,
};
use crate::tags::tag_set_from_json;
use kaikei_app::error::RepoError;
use kaikei_app::id::entry_id_from_uuid;
use kaikei_core::{sum_money, AccountCode, Currency, JournalEntry, JournalLine, Money, Side};

impl TryFrom<EntryRows> for JournalEntry {
    type Error = RepoError;

    fn try_from(rows: EntryRows) -> Result<Self, RepoError> {
        let EntryRows {
            entry,
            lines: line_rows,
        } = rows;

        let corrupt = |reason: String| RepoError::Corrupt { reason };

        // 1. lines.len() >= 2
        if line_rows.len() < 2 {
            return Err(corrupt(format!(
                "仕訳 {} の明細が {} 行しかありません（2行以上必要です）",
                entry.id,
                line_rows.len()
            )));
        }

        // 8. line_no が重複しない／昇順であること。
        let mut prev_line_no: Option<i16> = None;
        for row in &line_rows {
            if let Some(prev) = prev_line_no {
                if row.line_no <= prev {
                    return Err(corrupt(format!(
                        "仕訳 {} の明細の line_no が昇順ではありません（{prev} の次に {}）",
                        entry.id, row.line_no
                    )));
                }
            }
            prev_line_no = Some(row.line_no);
        }

        // 3・4・6（明細側の摘要は無いため実質は側・金額の検証）を兼ねて
        // JournalLine を組み立てる。
        let mut lines = Vec::with_capacity(line_rows.len());
        for row in &line_rows {
            let account = AccountCode::parse(&row.account_code).map_err(|e| {
                corrupt(format!(
                    "仕訳 {} の明細の科目コードが不正です: {e}",
                    entry.id
                ))
            })?;
            let side = side_from_i16(row.side)?;
            let amount =
                money_from_columns(row.amount_minor, &row.currency, row.currency_minor_unit)?;
            let tags = tag_set_from_json(&row.tags)?;
            let memo = row.memo.clone();
            let line = JournalLine::new(account, side, amount, tags, memo)
                .map_err(|e| corrupt(format!("仕訳 {} の明細を復元できません: {e}", entry.id)))?;
            lines.push(line);
        }

        // 2. 全明細の通貨が同一
        let currency = lines[0].amount().currency();
        for line in &lines {
            if line.amount().currency() != currency {
                return Err(corrupt(format!(
                    "仕訳 {} の明細で通貨が混在しています（{} と {}）",
                    entry.id,
                    currency.code(),
                    line.amount().currency().code()
                )));
            }
        }

        // 5. sum(debit) == sum(credit)（Money::add 経由の合算。オーバーフローも検出する）
        let debit_total = side_total(&lines, currency, Side::Debit, entry.id)?;
        let credit_total = side_total(&lines, currency, Side::Credit, entry.id)?;
        if debit_total.minor() != credit_total.minor() {
            return Err(corrupt(format!(
                "仕訳 {} は貸借が一致していません（借方={} 貸方={}）",
                entry.id,
                debit_total.to_display_string(),
                credit_total.to_display_string()
            )));
        }

        // 6. description.trim() が非空
        if entry.description.trim().is_empty() {
            return Err(corrupt(format!("仕訳 {} の摘要が空です", entry.id)));
        }

        // 7. reverses.is_some() == reverse_reason.is_some()
        if entry.reverses.is_some() != entry.reverse_reason.is_some() {
            return Err(corrupt(format!(
                "仕訳 {} の reverses と reverse_reason の組が不整合です",
                entry.id
            )));
        }

        // 9. entry_no が u32 に収まる（負値は Corrupt。DB 列は INTEGER なので
        //    i32 の上限を超えることは元々無い）。
        let entry_no = entry_no_from_i32(entry.entry_no)?;

        let entry_date = naive_date_to_accounting_date(entry.entry_date)?;
        let recorded_at = datetime_to_timestamp(entry.recorded_at);
        let id = entry_id_from_uuid(entry.id);
        let reverses = entry.reverses.map(entry_id_from_uuid);

        Ok(JournalEntry::rehydrate(
            id,
            entry.fiscal_year,
            entry_no,
            entry_date,
            entry.description,
            lines,
            Vec::new(),
            reverses,
            entry.reverse_reason,
            recorded_at,
        ))
    }
}

/// 指定した `side` の明細金額を合算する。該当する明細が無ければゼロを返す。
fn side_total(
    lines: &[JournalLine],
    currency: Currency,
    side: Side,
    entry_id: uuid::Uuid,
) -> Result<Money, RepoError> {
    let amounts = lines
        .iter()
        .filter(|line| line.side() == side)
        .map(JournalLine::amount);
    sum_money(amounts)
        .map_err(|e| RepoError::Corrupt {
            reason: format!("仕訳 {entry_id} の明細の合算に失敗しました: {e}"),
        })
        .map(|opt| opt.unwrap_or_else(|| Money::zero(currency)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::row::{JournalEntryRow, JournalLineRow};
    use chrono::{DateTime, NaiveDate, Utc};

    fn entry_row() -> JournalEntryRow {
        JournalEntryRow {
            id: uuid::Uuid::from_u128(1),
            fiscal_year: 2026,
            entry_no: 1,
            entry_date: NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
            description: "テスト仕訳".to_string(),
            reverses: None,
            reverse_reason: None,
            recorded_at: DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap(),
        }
    }

    fn line_row(line_no: i16, side: i16, amount_minor: i64, currency: &str) -> JournalLineRow {
        JournalLineRow {
            line_no,
            account_code: "100".to_string(),
            side,
            amount_minor,
            currency: currency.to_string(),
            currency_minor_unit: 0,
            tags: serde_json::json!({}),
            memo: None,
        }
    }

    fn balanced_lines() -> Vec<JournalLineRow> {
        vec![line_row(1, 1, 1_000, "JPY"), line_row(2, 2, 1_000, "JPY")]
    }

    fn assert_corrupt(rows: EntryRows) {
        match JournalEntry::try_from(rows) {
            Err(RepoError::Corrupt { .. }) => {}
            other => panic!("RepoError::Corrupt を期待しましたが {other:?} でした"),
        }
    }

    // 陽性対照: 貸借が一致した正常な行は正しく復元できる。
    #[test]
    fn valid_rows_are_restored_successfully() {
        let rows = EntryRows {
            entry: entry_row(),
            lines: balanced_lines(),
        };
        let entry = JournalEntry::try_from(rows).unwrap();
        assert_eq!(entry.fiscal_year(), 2026);
        assert_eq!(entry.lines().len(), 2);
        assert_eq!(entry.description(), "テスト仕訳");
    }

    // 1. lines.len() >= 2 （0行）
    #[test]
    fn empty_lines_is_corrupt_not_panic() {
        let rows = EntryRows {
            entry: entry_row(),
            lines: Vec::new(),
        };
        assert_corrupt(rows);
    }

    // 1. lines.len() >= 2 （1行）
    #[test]
    fn single_line_is_corrupt_not_panic() {
        let rows = EntryRows {
            entry: entry_row(),
            lines: vec![line_row(1, 1, 1_000, "JPY")],
        };
        assert_corrupt(rows);
    }

    // 2. 全明細の Currency が同一
    #[test]
    fn mixed_currency_is_corrupt_not_panic() {
        let rows = EntryRows {
            entry: entry_row(),
            lines: vec![line_row(1, 1, 1_000, "JPY"), line_row(2, 2, 1_000, "USD")],
        };
        assert_corrupt(rows);
    }

    // 3. side が 1|2 以外
    #[test]
    fn invalid_side_is_corrupt_not_panic() {
        let rows = EntryRows {
            entry: entry_row(),
            lines: vec![line_row(1, 3, 1_000, "JPY"), line_row(2, 2, 1_000, "JPY")],
        };
        assert_corrupt(rows);
    }

    // 4. amount_minor > 0（ゼロ）
    #[test]
    fn zero_amount_is_corrupt_not_panic() {
        let rows = EntryRows {
            entry: entry_row(),
            lines: vec![line_row(1, 1, 0, "JPY"), line_row(2, 2, 0, "JPY")],
        };
        assert_corrupt(rows);
    }

    // 4. amount_minor > 0（負値。DB の CHECK 制約が無いデータとして混入した想定）
    #[test]
    fn negative_amount_is_corrupt_not_panic() {
        let rows = EntryRows {
            entry: entry_row(),
            lines: vec![line_row(1, 1, -1_000, "JPY"), line_row(2, 2, -1_000, "JPY")],
        };
        assert_corrupt(rows);
    }

    // 5. sum(debit) == sum(credit)
    #[test]
    fn unbalanced_totals_is_corrupt_not_panic() {
        let rows = EntryRows {
            entry: entry_row(),
            lines: vec![line_row(1, 1, 1_000, "JPY"), line_row(2, 2, 500, "JPY")],
        };
        assert_corrupt(rows);
    }

    // 6. description.trim() が非空
    #[test]
    fn blank_description_is_corrupt_not_panic() {
        let mut entry = entry_row();
        entry.description = "   ".to_string();
        let rows = EntryRows {
            entry,
            lines: balanced_lines(),
        };
        assert_corrupt(rows);
    }

    // 7. reverses あり・reverse_reason 無し
    #[test]
    fn reverses_without_reason_is_corrupt_not_panic() {
        let mut entry = entry_row();
        entry.reverses = Some(uuid::Uuid::from_u128(2));
        entry.reverse_reason = None;
        let rows = EntryRows {
            entry,
            lines: balanced_lines(),
        };
        assert_corrupt(rows);
    }

    // 7. reverses 無し・reverse_reason あり
    #[test]
    fn reason_without_reverses_is_corrupt_not_panic() {
        let mut entry = entry_row();
        entry.reverses = None;
        entry.reverse_reason = Some("訂正".to_string());
        let rows = EntryRows {
            entry,
            lines: balanced_lines(),
        };
        assert_corrupt(rows);
    }

    // 7. 陽性対照: 両方 Some の組は正常
    #[test]
    fn reverses_with_reason_restores_successfully() {
        let mut entry = entry_row();
        entry.reverses = Some(uuid::Uuid::from_u128(2));
        entry.reverse_reason = Some("訂正".to_string());
        let rows = EntryRows {
            entry,
            lines: balanced_lines(),
        };
        let restored = JournalEntry::try_from(rows).unwrap();
        assert!(restored.is_reversal());
        assert_eq!(restored.reverse_reason(), Some("訂正"));
    }

    // 8. line_no の重複
    #[test]
    fn duplicate_line_no_is_corrupt_not_panic() {
        let rows = EntryRows {
            entry: entry_row(),
            lines: vec![line_row(1, 1, 1_000, "JPY"), line_row(1, 2, 1_000, "JPY")],
        };
        assert_corrupt(rows);
    }

    // 8. line_no の降順（昇順違反）
    #[test]
    fn descending_line_no_is_corrupt_not_panic() {
        let rows = EntryRows {
            entry: entry_row(),
            lines: vec![line_row(2, 1, 1_000, "JPY"), line_row(1, 2, 1_000, "JPY")],
        };
        assert_corrupt(rows);
    }

    // 9. entry_no が負値（DB の INTEGER 列に本来あってはならない値が混入した想定）
    #[test]
    fn negative_entry_no_is_corrupt_not_panic() {
        let mut entry = entry_row();
        entry.entry_no = -1;
        let rows = EntryRows {
            entry,
            lines: balanced_lines(),
        };
        assert_corrupt(rows);
    }

    // 不正なタグJSON（tags.rs が既に Corrupt を返すことは検証済みだが、
    // mapper 経由でも panic せず Corrupt が伝播することを確認する）。
    #[test]
    fn malformed_tags_json_is_corrupt_not_panic() {
        let mut lines = balanced_lines();
        lines[0].tags = serde_json::json!({"tax_category": {"t": "unknown", "v": "x"}});
        let rows = EntryRows {
            entry: entry_row(),
            lines,
        };
        assert_corrupt(rows);
    }

    // 不正な科目コード（AccountCode::parse が拒否する形式）。
    #[test]
    fn malformed_account_code_is_corrupt_not_panic() {
        let mut lines = balanced_lines();
        lines[0].account_code = "".to_string();
        let rows = EntryRows {
            entry: entry_row(),
            lines,
        };
        assert_corrupt(rows);
    }
}
