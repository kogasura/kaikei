//! テスト用のダミー実装。`test-doubles` feature でのみコンパイルされる。
//!
//! 他 crate（`kaikei-app` / `kaikei-store` 等）のテストから使うことを想定するため
//! `#[cfg(test)]` ではなく feature で切る（`#[cfg(test)]` は自クレート内の
//! テストからしか見えず、他 crate からは参照できないため要件を満たさない）。
//!
//! ここに置くのは「常に一定の挙動をする」最小限のダミーのみ。実際の税制ロジック
//! （`kaikei-jp`）の代替にはならない。

use crate::closing::ClosingPolicy;
use crate::context::TaxContext;
use crate::error::PolicyError;
use crate::numbering::Numbering;
use crate::proposal::ProposedEntry;
use crate::statement::{Statement, StatementLine, StatementPolicy, StatementSection};
use crate::tax::{TaxDerivation, TaxPolicy};
use crate::validation::EntryValidator;
use kaikei_core::{
    AccountDef, AccountType, BalanceRow, Currency, EntryNumber, FiscalYear, JournalEntry,
    JournalLine, Money, RoundMode, TagSet, TrialBalance,
};

/// 消費税行を一切生成しない `TaxPolicy`。免税事業者・税込経理の最小テストに使う。
#[derive(Debug, Clone, Copy, Default)]
pub struct NoTaxPolicy;

impl TaxPolicy for NoTaxPolicy {
    fn validate_tag(
        &self,
        _ctx: &TaxContext<'_>,
        _tags: &TagSet,
        _account: &AccountDef,
    ) -> Result<(), PolicyError> {
        Ok(())
    }

    fn derive_tax_lines(
        &self,
        _ctx: &TaxContext<'_>,
        lines: &[JournalLine],
    ) -> Result<TaxDerivation, PolicyError> {
        Ok(TaxDerivation {
            lines: lines.to_vec(),
            notes: Vec::new(),
        })
    }

    fn round_mode(&self, _ctx: &TaxContext<'_>) -> RoundMode {
        RoundMode::Floor
    }
}

/// 全ての明細に対して一律に `rate` を掛けた税額行を同じ側に追加する `TaxPolicy`。
///
/// 税区分ごとの判定（`direction` / `deductible` / 適格請求書の要否等）は
/// 一切行わない最小実装であり、実際の税制ロジックの代替にはならない。
/// 税額が 0 になる明細については税額行を追加しない。
#[derive(Debug, Clone, Copy)]
pub struct FlatRateTaxPolicy {
    /// 一律に適用する税率。
    pub rate: kaikei_core::Ratio,
    /// 税額行に使う勘定科目コード。
    pub tax_account: &'static str,
}

impl TaxPolicy for FlatRateTaxPolicy {
    fn validate_tag(
        &self,
        _ctx: &TaxContext<'_>,
        _tags: &TagSet,
        _account: &AccountDef,
    ) -> Result<(), PolicyError> {
        Ok(())
    }

    fn derive_tax_lines(
        &self,
        ctx: &TaxContext<'_>,
        lines: &[JournalLine],
    ) -> Result<TaxDerivation, PolicyError> {
        let tax_account = kaikei_core::AccountCode::parse(self.tax_account)?;
        let mut result: Vec<JournalLine> = lines.to_vec();
        for line in lines {
            let tax_amount = self.apply_ratio(ctx, *line.amount(), self.rate)?;
            if tax_amount.is_zero() {
                continue;
            }
            result.push(JournalLine::new(
                tax_account.clone(),
                line.side(),
                tax_amount,
                line.tags().clone(),
                None,
            )?);
        }
        Ok(TaxDerivation {
            lines: result,
            notes: Vec::new(),
        })
    }

    fn round_mode(&self, _ctx: &TaxContext<'_>) -> RoundMode {
        RoundMode::Floor
    }
}

/// 常に検証を通す `EntryValidator`。
#[derive(Debug, Clone, Copy, Default)]
pub struct AlwaysValid;

impl EntryValidator for AlwaysValid {
    fn validate(&self, _ctx: &TaxContext<'_>, _entry: &JournalEntry) -> Result<(), PolicyError> {
        Ok(())
    }
}

/// 直近の払い出し済み番号の次を返すだけの `Numbering`。
#[derive(Debug, Clone, Copy, Default)]
pub struct SequentialNumbering;

impl Numbering for SequentialNumbering {
    fn peek(
        &self,
        _fy: &FiscalYear,
        issued: Option<EntryNumber>,
    ) -> Result<EntryNumber, PolicyError> {
        Ok(EntryNumber::new(issued.map_or(1, |n| n.as_u32() + 1)))
    }
}

/// 決算振替仕訳を一切生成しない `ClosingPolicy`。
#[derive(Debug, Clone, Copy, Default)]
pub struct NoClosing;

impl ClosingPolicy for NoClosing {
    fn closing_entries(
        &self,
        _tb: &TrialBalance,
        _fy: &FiscalYear,
    ) -> Result<Vec<ProposedEntry>, PolicyError> {
        Ok(Vec::new())
    }
}

/// 科目種別（5要素）ごとに1区分としてまとめるだけの最小限の `StatementPolicy`。
///
/// 流動/固定の区分など実際の様式は一切考慮しない。
#[derive(Debug, Clone, Copy, Default)]
pub struct ByAccountTypeStatement;

impl StatementPolicy for ByAccountTypeStatement {
    fn balance_sheet(&self, tb: &TrialBalance) -> Statement {
        build_statement_by_type(
            tb,
            "貸借対照表",
            &[
                (AccountType::Asset, "資産"),
                (AccountType::Liability, "負債"),
                (AccountType::Equity, "純資産"),
            ],
        )
    }

    fn income_statement(&self, tb: &TrialBalance) -> Statement {
        build_statement_by_type(
            tb,
            "損益計算書",
            &[
                (AccountType::Revenue, "収益"),
                (AccountType::Expense, "費用"),
            ],
        )
    }
}

fn build_statement_by_type(
    tb: &TrialBalance,
    title: &str,
    groups: &[(AccountType, &str)],
) -> Statement {
    let currency = tb
        .rows()
        .first()
        .map(|row: &BalanceRow| row.balance.currency())
        .unwrap_or(Currency::JPY);
    let mut sections = Vec::with_capacity(groups.len());
    let mut total = Money::zero(currency);
    for (account_type, section_title) in groups {
        let lines: Vec<StatementLine> = tb
            .rows()
            .iter()
            .filter(|row| row.account_type == *account_type)
            .map(|row| StatementLine {
                account: row.account.clone(),
                label: row.account.as_str().to_string(),
                amount: row.balance,
            })
            .collect();
        let subtotal = tb.total_by_type(*account_type);
        total = total
            .add(&subtotal)
            .expect("同一 TrialBalance 内の残高は同一通貨であるため、合算は失敗しない");
        sections.push(StatementSection {
            title: (*section_title).to_string(),
            lines,
            subtotal,
        });
    }
    Statement {
        title: title.to_string(),
        sections,
        total,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_core::{AccountCode, AccountingDate, ChartOfAccounts, Side, TagSchema};

    fn sample_context<'a>(
        chart: &'a ChartOfAccounts,
        schema: &'a TagSchema,
        counterparties: &'a crate::counterparty::CounterpartyIndex,
    ) -> TaxContext<'a> {
        TaxContext {
            as_of: AccountingDate::new(2026, 4, 1).unwrap(),
            chart,
            tag_schema: schema,
            counterparties,
        }
    }

    #[test]
    fn no_tax_policy_returns_lines_unchanged() {
        let policy = NoTaxPolicy;
        let chart = ChartOfAccounts::new(vec![]).unwrap();
        let schema = TagSchema::empty();
        let counterparties = crate::counterparty::CounterpartyIndex::empty();
        let ctx = sample_context(&chart, &schema, &counterparties);

        let lines = vec![JournalLine::new(
            AccountCode::parse("100").unwrap(),
            Side::Debit,
            Money::from_minor(1_000, Currency::JPY),
            TagSet::new(),
            None,
        )
        .unwrap()];

        let derivation = policy.derive_tax_lines(&ctx, &lines).unwrap();
        assert_eq!(derivation.lines.len(), 1);
    }

    #[test]
    fn flat_rate_tax_policy_adds_tax_line_on_same_side() {
        let policy = FlatRateTaxPolicy {
            rate: kaikei_core::Ratio::parse_rate("0.10").unwrap(),
            tax_account: "330",
        };
        let chart = ChartOfAccounts::new(vec![]).unwrap();
        let schema = TagSchema::empty();
        let counterparties = crate::counterparty::CounterpartyIndex::empty();
        let ctx = sample_context(&chart, &schema, &counterparties);

        let lines = vec![JournalLine::new(
            AccountCode::parse("500").unwrap(),
            Side::Credit,
            Money::from_minor(100_000, Currency::JPY),
            TagSet::new(),
            None,
        )
        .unwrap()];

        let derivation = policy.derive_tax_lines(&ctx, &lines).unwrap();
        assert_eq!(derivation.lines.len(), 2);
        let tax_line = &derivation.lines[1];
        assert_eq!(tax_line.account().as_str(), "330");
        assert_eq!(tax_line.amount().minor(), 10_000);
        assert!(
            !tax_line.is_debit(),
            "税額行は元の明細と同じ側（貸方）である"
        );
    }

    #[test]
    fn flat_rate_tax_policy_skips_zero_tax_amount() {
        let policy = FlatRateTaxPolicy {
            rate: kaikei_core::Ratio::parse_rate("0.10").unwrap(),
            tax_account: "330",
        };
        let chart = ChartOfAccounts::new(vec![]).unwrap();
        let schema = TagSchema::empty();
        let counterparties = crate::counterparty::CounterpartyIndex::empty();
        let ctx = sample_context(&chart, &schema, &counterparties);

        // 100,000 の 10% は 10,000 になる。0円になる明細を確認するため、
        // 1円という小さな金額で floor 丸めにより税額が 0 になるケースを使う。
        let lines = vec![JournalLine::new(
            AccountCode::parse("500").unwrap(),
            Side::Credit,
            Money::from_minor(1, Currency::JPY),
            TagSet::new(),
            None,
        )
        .unwrap()];

        let derivation = policy.derive_tax_lines(&ctx, &lines).unwrap();
        assert_eq!(derivation.lines.len(), 1, "税額 0 の行は生成しない");
    }

    #[test]
    fn sequential_numbering_increments_from_issued() {
        let numbering = SequentialNumbering;
        let fy = FiscalYear::calendar_year(2026);
        assert_eq!(numbering.peek(&fy, None).unwrap().as_u32(), 1);
        assert_eq!(
            numbering
                .peek(&fy, Some(EntryNumber::new(41)))
                .unwrap()
                .as_u32(),
            42
        );
    }

    #[test]
    fn no_closing_returns_empty_entries() {
        let policy = NoClosing;
        let fy = FiscalYear::calendar_year(2026);
        let tb = TrialBalance::from_entries(
            std::iter::empty(),
            &ChartOfAccounts::new(vec![]).unwrap(),
            &TagSchema::empty(),
            &[],
        )
        .unwrap();
        assert!(policy.closing_entries(&tb, &fy).unwrap().is_empty());
        assert!(policy.opening_entries(&tb, &fy).unwrap().is_empty());
    }

    #[test]
    fn by_account_type_statement_groups_rows_by_account_type() {
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
        let clock = kaikei_core::FixedClock(kaikei_core::Timestamp::from_unix_nanos(0));

        struct AllOpen;
        impl kaikei_core::PeriodGuard for AllOpen {
            fn status(&self, _date: AccountingDate) -> kaikei_core::PeriodStatus {
                kaikei_core::PeriodStatus::Open
            }
        }

        let lines = vec![
            JournalLine::new(
                AccountCode::parse("100").unwrap(),
                Side::Debit,
                Money::from_minor(1_000, Currency::JPY),
                TagSet::new(),
                None,
            )
            .unwrap(),
            JournalLine::new(
                AccountCode::parse("500").unwrap(),
                Side::Credit,
                Money::from_minor(1_000, Currency::JPY),
                TagSet::new(),
                None,
            )
            .unwrap(),
        ];
        let entry = JournalEntry::new(
            kaikei_core::NewEntry {
                id: kaikei_core::EntryId::new(1),
                entry_no: EntryNumber::new(1),
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
        .unwrap();

        let tb = TrialBalance::from_entries(std::iter::once(&entry), &chart, &schema, &[]).unwrap();

        let policy = ByAccountTypeStatement;
        let bs = policy.balance_sheet(&tb);
        assert_eq!(bs.title, "貸借対照表");
        let asset_section = bs
            .sections
            .iter()
            .find(|s| s.title == "資産")
            .expect("資産区分が存在する");
        assert_eq!(asset_section.subtotal.minor(), 1_000);

        let is = policy.income_statement(&tb);
        let revenue_section = is
            .sections
            .iter()
            .find(|s| s.title == "収益")
            .expect("収益区分が存在する");
        assert_eq!(revenue_section.subtotal.minor(), 1_000);
    }
}
