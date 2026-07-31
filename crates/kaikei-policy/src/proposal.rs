//! policy が提案する未確定の仕訳（`ProposedEntry`）。
//!
//! `ClosingPolicy` 等が仕訳を生成する際、`EntryId` / `EntryNumber` の採番は
//! store の I/O であるため、policy 自身では割り当てられない
//! （`DECISIONS.md` D-027）。呼び出し側（`kaikei-app`）が採番したうえで
//! `kaikei_core::NewEntry` に詰め替え、`JournalEntry::new` で最終的な
//! 不変条件検証（貸借一致・科目存在・タグスキーマ適合等）を行う。

use kaikei_core::{AccountingDate, JournalLine};

/// policy が提案する仕訳。`EntryId` / `EntryNumber` を持たない点が
/// `kaikei_core::NewEntry` との違い。
#[derive(Debug, Clone)]
pub struct ProposedEntry {
    /// 取引日。
    pub entry_date: AccountingDate,
    /// 摘要。
    pub description: String,
    /// 仕訳明細。
    pub lines: Vec<JournalLine>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_core::{AccountCode, Currency, Money, Side, TagSet};

    #[test]
    fn proposed_entry_holds_lines_without_id_or_entry_no() {
        let lines = vec![
            JournalLine::new(
                AccountCode::parse("400").unwrap(),
                Side::Debit,
                Money::from_minor(100_000, Currency::JPY),
                TagSet::new(),
                None,
            )
            .unwrap(),
            JournalLine::new(
                AccountCode::parse("500").unwrap(),
                Side::Credit,
                Money::from_minor(100_000, Currency::JPY),
                TagSet::new(),
                None,
            )
            .unwrap(),
        ];
        let proposed = ProposedEntry {
            entry_date: AccountingDate::new(2026, 12, 31).unwrap(),
            description: "決算振替".to_string(),
            lines,
        };
        assert_eq!(proposed.lines.len(), 2);
        assert_eq!(proposed.description, "決算振替");
    }
}
