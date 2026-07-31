//! 追加検証（`EntryValidator`）。
//!
//! `JournalEntry::new` が行う不変条件検証（貸借一致・科目存在・タグスキーマ適合等）に
//! 加えて、国・事業者固有の追加ルールを課したい場合に使う。

use crate::context::TaxContext;
use crate::error::PolicyError;
use kaikei_core::JournalEntry;

/// 確定した仕訳に対する追加検証。
pub trait EntryValidator: Send + Sync {
    /// 確定した仕訳が追加ルールに適合するか検証する。
    fn validate(&self, ctx: &TaxContext<'_>, entry: &JournalEntry) -> Result<(), PolicyError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_core::{
        AccountCode, AccountDef, AccountType, AccountingDate, ChartOfAccounts, FiscalYear,
        FixedClock, JournalLine, Money, NewEntry, PeriodGuard, PeriodStatus, Side, TagSchema,
        TagSet, Timestamp,
    };
    use std::sync::Arc;

    /// 常に検証を通す最小実装。dyn 互換性の検査専用。
    struct AlwaysValid;

    impl EntryValidator for AlwaysValid {
        fn validate(
            &self,
            _ctx: &TaxContext<'_>,
            _entry: &JournalEntry,
        ) -> Result<(), PolicyError> {
            Ok(())
        }
    }

    struct AllOpen;
    impl PeriodGuard for AllOpen {
        fn status(&self, _date: AccountingDate) -> PeriodStatus {
            PeriodStatus::Open
        }
    }

    fn sample_entry() -> JournalEntry {
        let chart = ChartOfAccounts::new(vec![
            AccountDef {
                code: AccountCode::parse("100").unwrap(),
                name: "現金".to_string(),
                account_type: AccountType::Asset,
                parent: None,
                postable: true,
            },
            AccountDef {
                code: AccountCode::parse("500").unwrap(),
                name: "売上高".to_string(),
                account_type: AccountType::Revenue,
                parent: None,
                postable: true,
            },
        ])
        .unwrap();
        let schema = TagSchema::empty();
        let fy = FiscalYear::calendar_year(2026);
        let clock = FixedClock(Timestamp::from_unix_nanos(0));

        let lines = vec![
            JournalLine::new(
                AccountCode::parse("100").unwrap(),
                Side::Debit,
                Money::from_minor(1_000, kaikei_core::Currency::JPY),
                TagSet::new(),
                None,
            )
            .unwrap(),
            JournalLine::new(
                AccountCode::parse("500").unwrap(),
                Side::Credit,
                Money::from_minor(1_000, kaikei_core::Currency::JPY),
                TagSet::new(),
                None,
            )
            .unwrap(),
        ];

        JournalEntry::new(
            NewEntry {
                id: kaikei_core::EntryId::new(1),
                entry_no: kaikei_core::EntryNumber::new(1),
                entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
                description: "テスト仕訳".to_string(),
                lines,
                document_refs: Vec::new(),
            },
            &fy,
            &chart,
            &schema,
            &AllOpen,
            &clock,
        )
        .unwrap()
    }

    // dyn 互換性の静的検査。
    fn _dyn(_: &dyn EntryValidator) {}

    #[test]
    fn entry_validator_is_object_safe() {
        let validator = AlwaysValid;
        _dyn(&validator);
    }

    #[test]
    fn entry_validator_can_be_used_as_arc_dyn() {
        let validator: Arc<dyn EntryValidator> = Arc::new(AlwaysValid);
        let entry = sample_entry();
        let chart = ChartOfAccounts::new(vec![]).unwrap();
        let schema = TagSchema::empty();
        let counterparties = crate::counterparty::CounterpartyIndex::empty();
        let ctx = TaxContext {
            as_of: entry.entry_date(),
            chart: &chart,
            tag_schema: &schema,
            counterparties: &counterparties,
        };
        assert!(validator.validate(&ctx, &entry).is_ok());
    }
}
