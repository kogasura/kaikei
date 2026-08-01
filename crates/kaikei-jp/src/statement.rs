//! 財務諸表の様式（[`JpStatementPolicy`]）。
//!
//! `docs/04-jp-tax.md` §9・§10、`crates/kaikei-policy/src/statement.rs`
//! （`kaikei-policy::StatementPolicy`）、`DECISIONS.md` D-067 を参照。
//!
//! # これは「青色申告決算書」ではない（`CLAUDE.md` §10）
//!
//! ここで組み立てる `Statement` は、与えられた試算表（`TrialBalance`）を
//! 勘定科目種別（`AccountType`）で区分・整形しただけの構造体であり、
//! 税務申告書ではない。以下は一切扱わない（`docs/04-jp-tax.md` §9「実装上の
//! 注意」）。
//!
//! - **青色申告特別控除**（65万/55万/10万）。これは帳簿科目ではなく申告書上の
//!   控除であり、仕訳も財務諸表の行も作らない。控除の適用は
//!   `kaikei-report` の決算書出力（この PR のスコープ外）が扱う
//! - 減価償却費の年次調整、家事按分の年次調整、棚卸（Phase 5 の検討事項）
//!
//! # 決算振替は適用しない
//!
//! `balance_sheet` / `income_statement` は渡された `TrialBalance` をそのまま
//! 整形するだけであり、`ClosingPolicy::closing_entries` が生成する決算振替を
//! 内部で適用したりはしない。決算整理後の残高を見たい場合は、
//! `closing_entries` の結果を反映した試算表を呼び出し側で構築してから渡すこと。
//! そのため、決算振替前の試算表を渡すと「資産合計 ≠ 負債・純資産合計」
//! （差額は未振替の当期純利益相当）という状態になりうるが、これはバグではなく
//! 「試算表をそのまま反映している」ことの帰結である。
//!
//! # 勘定科目名の解決
//!
//! `TrialBalance::rows()`（`BalanceRow`）は科目コードのみを持ち、表示名を
//! 持たない。表示名は構築時に保持する `ChartOfAccounts` から引く
//! （`JpTaxPolicy` がマスタを構築時に持つのと同じパターン）。
//!
//! # `StatementPolicy` が `Result` を返さないことへの対処（`DECISIONS.md` D-067）
//!
//! `kaikei-policy::StatementPolicy::balance_sheet` / `income_statement` は
//! `Statement`（`Result` ではない）を返す trait シグネチャになっている
//! （凍結層のため変更しない）。この実装で失敗しうる箇所は2つあるが、
//! どちらも `Statement` を返せなくなるような致命的な失敗にはしない。
//!
//! 1. **科目コードが構築時に保持した `ChartOfAccounts` に無い**
//!    （呼び出し側が科目表を後から差し替えた等）: その科目コードそのものを
//!    表示名として使う（`AccountCode::as_str()`）。行自体は落とさない
//! 2. **純利益（収益合計 − 費用合計）の計算がオーバーフローする**:
//!    `TrialBalance` 内の残高は同一通貨であることが構築時に保証されているため
//!    （`kaikei_core::TrialBalance::total_by_type` の doc）、失敗しうるのは
//!    合算が `i128` の表現上限を超える場合のみであり、これは
//!    `TrialBalance::totals()` / `total_by_type()` 自身が同じ理由で
//!    `.expect(...)`（オーバーフロー時は panic）としている前提を踏襲する

use kaikei_core::{AccountCode, AccountType, ChartOfAccounts, TrialBalance};
use kaikei_policy::{Statement, StatementLine, StatementPolicy, StatementSection};

/// 個人事業主向けの `StatementPolicy` 実装。
///
/// 勘定科目表を構築時に保持する（表示名の解決に使う）。
#[derive(Debug, Clone)]
pub struct JpStatementPolicy {
    chart: ChartOfAccounts,
}

impl JpStatementPolicy {
    /// 勘定科目表から構築する。
    pub fn new(chart: ChartOfAccounts) -> Self {
        JpStatementPolicy { chart }
    }

    /// 保持している勘定科目表を返す。
    pub fn chart(&self) -> &ChartOfAccounts {
        &self.chart
    }

    /// 指定した科目種別の行だけを集めた `StatementSection` を組み立てる。
    ///
    /// `TrialBalance::rows()` は科目コード（と group_by 指定時はグループ）の
    /// 昇順で決定的に並ぶため、`filter` で部分列を取っても順序は決定的なまま。
    fn section(
        &self,
        title: &str,
        account_type: AccountType,
        tb: &TrialBalance,
    ) -> StatementSection {
        let lines = tb
            .rows()
            .iter()
            .filter(|row| row.account_type == account_type)
            .map(|row| StatementLine {
                account: row.account.clone(),
                label: self.label_of(&row.account),
                amount: row.balance,
            })
            .collect();

        StatementSection {
            title: title.to_string(),
            lines,
            subtotal: tb.total_by_type(account_type),
        }
    }

    /// 科目コードから表示名を引く。`chart` に存在しなければ科目コード自体を返す
    /// （モジュール doc「`StatementPolicy` が `Result` を返さないことへの対処」）。
    fn label_of(&self, account: &AccountCode) -> String {
        self.chart
            .get(account)
            .map(|def| def.name.clone())
            .unwrap_or_else(|| account.as_str().to_string())
    }
}

impl StatementPolicy for JpStatementPolicy {
    /// 貸借対照表を組み立てる。資産・負債・純資産の3区分を常に含む
    /// （該当する残高が無い区分も、行0件・小計0円のセクションとして含める。
    /// 出力構造を試算表の内容に依存させないための決定的な設計）。
    ///
    /// `total` は資産合計（`TrialBalance::total_by_type(AccountType::Asset)`）。
    /// 決算振替を適用済みの試算表であれば、これは負債・純資産合計とも一致する
    /// （モジュール doc「決算振替は適用しない」を参照。この関数自身は検算しない）。
    fn balance_sheet(&self, tb: &TrialBalance) -> Statement {
        let sections = vec![
            self.section("資産", AccountType::Asset, tb),
            self.section("負債", AccountType::Liability, tb),
            self.section("純資産", AccountType::Equity, tb),
        ];
        let total = tb.total_by_type(AccountType::Asset);
        Statement {
            title: "貸借対照表".to_string(),
            sections,
            total,
        }
    }

    /// 損益計算書を組み立てる。収益・費用の2区分を常に含む。
    ///
    /// `total` は収益合計 − 費用合計（当期純利益。マイナスなら純損失）。
    /// これは `kaikei-jp::closing::JpSoleProprietorClosingPolicy` が
    /// `closing_entries` で元入金へ振り替える金額と同じ計算式だが、
    /// **税務上の所得金額（申告額）ではない**（`CLAUDE.md` §10。青色申告
    /// 特別控除等は未反映）。
    fn income_statement(&self, tb: &TrialBalance) -> Statement {
        let sections = vec![
            self.section("収益", AccountType::Revenue, tb),
            self.section("費用", AccountType::Expense, tb),
        ];
        let revenue_total = tb.total_by_type(AccountType::Revenue);
        let expense_total = tb.total_by_type(AccountType::Expense);
        let total = revenue_total.sub(&expense_total).expect(
            "収益合計・費用合計は同一 TrialBalance 由来で通貨が一致することが保証されている \
             （TrialBalance::total_by_type の doc）ため、i128 の表現上限を超えない限り失敗しない \
             （モジュール doc「StatementPolicy が Result を返さないことへの対処」）",
        );
        Statement {
            title: "損益計算書".to_string(),
            sections,
            total,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::new_entry;
    use kaikei_core::{
        AccountDef, Currency, FiscalYear, JournalEntry, JournalLine, Money, Side, TagSchema, TagSet,
    };
    use proptest::prelude::*;

    fn test_chart() -> ChartOfAccounts {
        ChartOfAccounts::new(vec![
            account("100", "現金", AccountType::Asset),
            account("320", "借入金", AccountType::Liability),
            account("400", "元入金", AccountType::Equity),
            account("500", "売上高", AccountType::Revenue),
            account("600", "仕入高", AccountType::Expense),
        ])
        .unwrap()
    }

    fn account(code: &str, name: &str, account_type: AccountType) -> AccountDef {
        AccountDef {
            code: AccountCode::parse(code).unwrap(),
            name: name.to_string(),
            account_type,
            parent: None,
            postable: true,
        }
    }

    fn fy() -> FiscalYear {
        FiscalYear::calendar_year(2026)
    }

    fn schema() -> TagSchema {
        TagSchema::empty()
    }

    #[allow(clippy::too_many_arguments)]
    fn two_line_entry(
        chart: &ChartOfAccounts,
        schema: &TagSchema,
        fy: &FiscalYear,
        id: u128,
        a_code: &str,
        a_side: Side,
        b_code: &str,
        b_side: Side,
        amount_minor: i128,
    ) -> JournalEntry {
        let amount = Money::from_minor(amount_minor, Currency::JPY);
        let lines = vec![
            JournalLine::new(
                AccountCode::parse(a_code).unwrap(),
                a_side,
                amount,
                TagSet::new(),
                None,
            )
            .unwrap(),
            JournalLine::new(
                AccountCode::parse(b_code).unwrap(),
                b_side,
                amount,
                TagSet::new(),
                None,
            )
            .unwrap(),
        ];
        new_entry(id, id as u32, fy, chart, schema, fy.start(), "test", lines)
    }

    // ---- 空の試算表 ----

    #[test]
    fn balance_sheet_of_empty_trial_balance_has_all_sections_with_zero_subtotal() {
        let chart = test_chart();
        let schema = schema();
        let tb = TrialBalance::from_entries(std::iter::empty(), &chart, &schema, &[]).unwrap();
        let policy = JpStatementPolicy::new(chart);

        let bs = policy.balance_sheet(&tb);
        assert_eq!(bs.title, "貸借対照表");
        assert_eq!(bs.sections.len(), 3);
        assert_eq!(
            bs.sections.iter().map(|s| &s.title).collect::<Vec<_>>(),
            vec!["資産", "負債", "純資産"]
        );
        assert!(bs.sections.iter().all(|s| s.lines.is_empty()));
        assert!(bs.sections.iter().all(|s| s.subtotal.is_zero()));
        assert!(bs.total.is_zero());
    }

    #[test]
    fn income_statement_of_empty_trial_balance_has_all_sections_with_zero_subtotal() {
        let chart = test_chart();
        let schema = schema();
        let tb = TrialBalance::from_entries(std::iter::empty(), &chart, &schema, &[]).unwrap();
        let policy = JpStatementPolicy::new(chart);

        let is = policy.income_statement(&tb);
        assert_eq!(is.title, "損益計算書");
        assert_eq!(is.sections.len(), 2);
        assert_eq!(
            is.sections.iter().map(|s| &s.title).collect::<Vec<_>>(),
            vec!["収益", "費用"]
        );
        assert!(is.sections.iter().all(|s| s.lines.is_empty()));
        assert!(is.total.is_zero());
    }

    // ---- 試算表の内容がそのまま反映されること ----

    #[test]
    fn balance_sheet_reflects_trial_balance_with_correct_labels_and_subtotals() {
        let chart = test_chart();
        let schema = schema();
        let fy = fy();
        let entries = [
            two_line_entry(
                &chart,
                &schema,
                &fy,
                1,
                "100",
                Side::Debit,
                "400",
                Side::Credit,
                1_000_000,
            ),
            two_line_entry(
                &chart,
                &schema,
                &fy,
                2,
                "100",
                Side::Debit,
                "320",
                Side::Credit,
                300_000,
            ),
        ];
        let tb = TrialBalance::from_entries(entries.iter(), &chart, &schema, &[]).unwrap();
        let policy = JpStatementPolicy::new(chart);

        let bs = policy.balance_sheet(&tb);
        let asset_section = &bs.sections[0];
        assert_eq!(asset_section.lines.len(), 1);
        assert_eq!(asset_section.lines[0].label, "現金");
        assert_eq!(asset_section.lines[0].amount.minor(), 1_300_000);
        assert_eq!(asset_section.subtotal.minor(), 1_300_000);

        let liability_section = &bs.sections[1];
        assert_eq!(liability_section.lines[0].label, "借入金");
        assert_eq!(liability_section.lines[0].amount.minor(), 300_000);

        let equity_section = &bs.sections[2];
        assert_eq!(equity_section.lines[0].label, "元入金");
        assert_eq!(equity_section.lines[0].amount.minor(), 1_000_000);

        // total = 資産合計。
        assert_eq!(bs.total.minor(), 1_300_000);
    }

    #[test]
    fn income_statement_total_is_revenue_minus_expense() {
        let chart = test_chart();
        let schema = schema();
        let fy = fy();
        let entries = [
            two_line_entry(
                &chart,
                &schema,
                &fy,
                1,
                "100",
                Side::Debit,
                "500",
                Side::Credit,
                1_000_000,
            ),
            two_line_entry(
                &chart,
                &schema,
                &fy,
                2,
                "600",
                Side::Debit,
                "100",
                Side::Credit,
                400_000,
            ),
        ];
        let tb = TrialBalance::from_entries(entries.iter(), &chart, &schema, &[]).unwrap();
        let policy = JpStatementPolicy::new(chart);

        let is = policy.income_statement(&tb);
        assert_eq!(is.sections[0].subtotal.minor(), 1_000_000);
        assert_eq!(is.sections[1].subtotal.minor(), 400_000);
        assert_eq!(is.total.minor(), 600_000);
    }

    #[test]
    fn income_statement_total_can_be_negative_for_a_loss() {
        let chart = test_chart();
        let schema = schema();
        let fy = fy();
        let entries = [
            two_line_entry(
                &chart,
                &schema,
                &fy,
                1,
                "100",
                Side::Debit,
                "500",
                Side::Credit,
                300_000,
            ),
            two_line_entry(
                &chart,
                &schema,
                &fy,
                2,
                "600",
                Side::Debit,
                "100",
                Side::Credit,
                500_000,
            ),
        ];
        let tb = TrialBalance::from_entries(entries.iter(), &chart, &schema, &[]).unwrap();
        let policy = JpStatementPolicy::new(chart);

        let is = policy.income_statement(&tb);
        assert_eq!(is.total.minor(), -200_000);
        assert!(is.total.is_negative());
    }

    // ---- 科目表に無い科目コードは科目コード自体を表示名にする ----

    #[test]
    fn label_falls_back_to_account_code_when_missing_from_chart() {
        // 統計対象の試算表を「200」という chart に存在しない資産科目で構築する。
        let mut accounts = vec![
            account("100", "現金", AccountType::Asset),
            account("200", "unused-in-chart", AccountType::Asset),
        ];
        let build_chart = ChartOfAccounts::new(accounts.clone()).unwrap();
        let schema = schema();
        let fy = fy();
        let entry = two_line_entry(
            &build_chart,
            &schema,
            &fy,
            1,
            "200",
            Side::Debit,
            "100",
            Side::Credit,
            500,
        );
        let tb = TrialBalance::from_entries(std::iter::once(&entry), &build_chart, &schema, &[])
            .unwrap();

        // 表示名解決に使う chart からは "200" を取り除いておく。
        accounts.retain(|a| a.code.as_str() != "200");
        let display_chart = ChartOfAccounts::new(accounts).unwrap();
        let policy = JpStatementPolicy::new(display_chart);

        let bs = policy.balance_sheet(&tb);
        let unknown_line = bs.sections[0]
            .lines
            .iter()
            .find(|l| l.account.as_str() == "200")
            .unwrap();
        assert_eq!(unknown_line.label, "200");
    }

    // ---- 出力順の決定性 ----

    #[test]
    fn statement_output_is_deterministic_across_repeated_calls() {
        let chart = test_chart();
        let schema = schema();
        let fy = fy();
        let entries = [
            two_line_entry(
                &chart,
                &schema,
                &fy,
                1,
                "100",
                Side::Debit,
                "500",
                Side::Credit,
                100_003,
            ),
            two_line_entry(
                &chart,
                &schema,
                &fy,
                2,
                "600",
                Side::Debit,
                "100",
                Side::Credit,
                30_011,
            ),
        ];
        let tb = TrialBalance::from_entries(entries.iter(), &chart, &schema, &[]).unwrap();
        let policy = JpStatementPolicy::new(chart);

        let first_bs = policy.balance_sheet(&tb);
        let first_is = policy.income_statement(&tb);
        for i in 1..20 {
            assert_eq!(policy.balance_sheet(&tb), first_bs, "{i}回目のBSが変わった");
            assert_eq!(
                policy.income_statement(&tb),
                first_is,
                "{i}回目のPLが変わった"
            );
        }
    }

    // ---- プロパティテスト ----
    //
    // `PROGRESS.md` Phase 0 の教訓に従い、端数・境界値（1, 大きな値）を
    // `prop_oneof!` で明示的に含める。

    fn any_amount_minor() -> impl Strategy<Value = i128> {
        prop_oneof![
            6 => 1i128..=1_000_000i128,
            1 => Just(1i128),
            1 => Just(999_999_999i128),
        ]
    }

    proptest! {
        /// 任意の金額（負債・収益・費用）で、貸借対照表・損益計算書の各セクション
        /// 小計と `total` が、入力から独立に手計算した期待値と一致すること。
        #[test]
        fn statement_subtotals_and_totals_match_hand_computed_expectations(
            liability_minor in any_amount_minor(),
            revenue_minor in any_amount_minor(),
            expense_minor in any_amount_minor(),
        ) {
            let chart = test_chart();
            let schema = schema();
            let fy = fy();
            let entries = [
                two_line_entry(&chart, &schema, &fy, 1, "100", Side::Debit, "320", Side::Credit, liability_minor),
                two_line_entry(&chart, &schema, &fy, 2, "100", Side::Debit, "500", Side::Credit, revenue_minor),
                two_line_entry(&chart, &schema, &fy, 3, "600", Side::Debit, "100", Side::Credit, expense_minor),
            ];
            let tb = TrialBalance::from_entries(entries.iter(), &chart, &schema, &[]).unwrap();
            let policy = JpStatementPolicy::new(chart);

            // 資産(100) = liability_minor + revenue_minor − expense_minor。
            let expected_asset = liability_minor + revenue_minor - expense_minor;

            let bs = policy.balance_sheet(&tb);
            prop_assert_eq!(bs.sections[0].subtotal.minor(), expected_asset);
            prop_assert_eq!(bs.sections[1].subtotal.minor(), liability_minor);
            prop_assert_eq!(bs.sections[2].subtotal.minor(), 0); // 純資産（元入金等）は無取引。
            prop_assert_eq!(bs.total.minor(), expected_asset);

            let is = policy.income_statement(&tb);
            prop_assert_eq!(is.sections[0].subtotal.minor(), revenue_minor);
            prop_assert_eq!(is.sections[1].subtotal.minor(), expense_minor);
            prop_assert_eq!(is.total.minor(), revenue_minor - expense_minor);
        }
    }
}
