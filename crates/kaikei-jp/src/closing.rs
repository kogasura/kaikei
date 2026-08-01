//! 個人事業主の決算振替仕訳（[`JpSoleProprietorClosingPolicy`]）。
//!
//! `docs/04-jp-tax.md` §9、`crates/kaikei-policy/src/closing.rs`
//! （`kaikei-policy::ClosingPolicy`）、`DECISIONS.md` D-065/D-066 を参照。
//!
//! # 実装する範囲（§9 手順1・2・3の一部）
//!
//! 1. 収益・費用を集計して所得を算出する（`所得 = 収益合計 − 費用合計`）
//! 2. 収益・費用の各科目をゼロにする振替仕訳を生成する
//! 3. **当年度末に計上する分のみ**: 所得を元入金へ振り替える
//!
//! これら3つを**1本の `ProposedEntry`**にまとめて返す（理由は
//! [`zeroing_side`] のドキュメントと下記「貸借が一致する理由」を参照）。
//!
//! # `opening_entries` は実装しない（`DECISIONS.md` D-065）
//!
//! `kaikei-policy::ClosingPolicy::opening_entries` の既定実装（何も生成しない）を
//! **そのまま使う**。個人事業主の元入金振替のうち「事業主借 − 事業主貸」を
//! 反映する部分を当年度末と翌年期首のどちらに計上するか、事業主貸・事業主借の
//! 期首リセットを振替仕訳で行うか期首残高の直接設定で行うかは、
//! `docs/04-jp-tax.md` §9 と `docs/08-compliance.md` §9-4 が明示するとおり
//! 未解決の税理士確認事項であるため、この PR では判断せず実装しない。
//!
//! # 実装しないこと（`docs/04-jp-tax.md` §9「実装上の注意」）
//!
//! - **青色申告特別控除**（65万/55万/10万）は帳簿科目ではないため仕訳を作らない
//!   （申告書上の控除。`kaikei-report` の決算書出力の領域）
//! - **減価償却費の年次調整・家事按分の年次調整・棚卸**は Phase 5 の検討事項
//!
//! # `tax_category` タグを付与しない（未解決の設計上の制約）
//!
//! `kaikei-jp-data/tags.yaml` は `tax_category` を `required_for: [Revenue,
//! Expense]` としている。一方 `kaikei-policy::ClosingPolicy::closing_entries`
//! は `TagSchema`（および `tax_category` の候補を判定する材料になる
//! `TaxCategoryTable`）を一切受け取らない trait シグネチャになっている
//! （`kaikei-policy` は凍結層のため本 PR では変更しない）。そのため、ここで
//! 生成する収益・費用のゼロ化明細には `tax_category` タグを付けられない。
//!
//! この `ProposedEntry` を最終的に `kaikei_core::JournalEntry::new` に通す際
//! （`DECISIONS.md` D-027 のとおり `kaikei-app` が担う）、実運用の `TagSchema`
//! が `tax_category` を必須にしていると `CoreError::MissingRequiredTag` で
//! 拒否される可能性がある。決算振替仕訳は売上・仕入といった消費税の課税取引
//! ではないため `tax_category` を要求すること自体が適切かどうかを含め、
//! この PR のスコープでは判断せず、次工程（`kaikei-app` で `ClosingPolicy` を
//! 呼び出す実装）への申し送り事項として報告する。
//!
//! # 貸借が一致する理由
//!
//! 収益・費用の各行を「反対側」に立ててゼロにすると、生成される明細群だけでは
//! 貸借が一致しない（収益側の合計と費用側の合計が異なるため）。その差額は
//! 定義上ちょうど所得（収益合計 − 費用合計）に等しく、元入金への1行
//! （所得が正なら貸方・負なら借方）で過不足なく埋め合わされる。
//! `verify_balanced` はこれを実行時に検算する最後の砦。

use kaikei_core::{
    sum_money, AccountCode, AccountType, ChartOfAccounts, CoreError, Currency, FiscalYear,
    JournalLine, Money, Side, TagSet, TrialBalance,
};
use kaikei_policy::{ClosingPolicy, PolicyError, ProposedEntry};

use crate::error::JpError;

/// 個人事業主（青色申告）向けの決算振替仕訳を生成する `ClosingPolicy` 実装。
///
/// 元入金・事業主貸・事業主借の科目コードは構築時に保持する
/// （`JpTaxPolicy` がマスタを構築時に持つのと同じパターン。
/// `docs/04-jp-tax.md` §9「実装上の注意」）。事業主貸・事業主借は現時点の
/// `closing_entries`（手順1・2・3の当年度末分）では使わないが、
/// [`ClosingPolicy::opening_entries`] を将来実装する際に必要になるため
/// 構築時にまとめて検証・保持する（`DECISIONS.md` D-066）。
#[derive(Debug, Clone)]
pub struct JpSoleProprietorClosingPolicy {
    capital_account: AccountCode,
    owner_drawings_account: AccountCode,
    owner_contributions_account: AccountCode,
}

impl JpSoleProprietorClosingPolicy {
    /// 決算科目（元入金・事業主貸・事業主借）の科目コードと勘定科目表から構築する。
    ///
    /// 3科目それぞれが `chart` に存在することを構築時に検証する。存在しなければ
    /// `JpError::MissingClosingAccount`（どの科目コードが見つからなかったかを
    /// 含む）を返す。実行時（決算処理の最中）ではなく構築時に失敗させることで、
    /// 記帳作業の途中で決算処理だけが失敗する事態を避ける。
    pub fn new(
        chart: &ChartOfAccounts,
        capital_account: AccountCode,
        owner_drawings_account: AccountCode,
        owner_contributions_account: AccountCode,
    ) -> Result<Self, JpError> {
        require_account(chart, &capital_account, "元入金")?;
        require_account(chart, &owner_drawings_account, "事業主貸")?;
        require_account(chart, &owner_contributions_account, "事業主借")?;

        Ok(JpSoleProprietorClosingPolicy {
            capital_account,
            owner_drawings_account,
            owner_contributions_account,
        })
    }

    /// 元入金の科目コードを返す。
    pub fn capital_account(&self) -> &AccountCode {
        &self.capital_account
    }

    /// 事業主貸の科目コードを返す。
    pub fn owner_drawings_account(&self) -> &AccountCode {
        &self.owner_drawings_account
    }

    /// 事業主借の科目コードを返す。
    pub fn owner_contributions_account(&self) -> &AccountCode {
        &self.owner_contributions_account
    }
}

fn require_account(chart: &ChartOfAccounts, code: &AccountCode, role: &str) -> Result<(), JpError> {
    if chart.get(code).is_none() {
        return Err(JpError::MissingClosingAccount {
            role: role.to_string(),
            code: code.as_str().to_string(),
        });
    }
    Ok(())
}

impl ClosingPolicy for JpSoleProprietorClosingPolicy {
    /// `docs/04-jp-tax.md` §9 の手順1・2・3（当年度末に計上する分）を実装する。
    ///
    /// 収益・費用に非ゼロの残高が1つも無ければ（＝所得も必ず0になる）、
    /// 提案することが無いため空の `Vec` を返す。それ以外は常に1本の
    /// `ProposedEntry`（`entry_date` は `fy.end()`）を返す。
    fn closing_entries(
        &self,
        tb: &TrialBalance,
        fy: &FiscalYear,
    ) -> Result<Vec<ProposedEntry>, PolicyError> {
        let mut lines = Vec::new();

        // 手順2: 収益・費用の各科目をゼロにする振替仕訳。
        // `TrialBalance::rows()` は科目コード（と group_by を指定した場合は
        // グループ）の昇順で決定的に並ぶ（`BTreeMap` 由来）。
        for row in tb.rows() {
            if !matches!(
                row.account_type,
                AccountType::Revenue | AccountType::Expense
            ) {
                continue;
            }
            if row.balance.is_zero() {
                // 残高0の科目には明細を作らない（`JournalLine::new` が0円を拒否する）。
                continue;
            }
            lines.push(JournalLine::new(
                row.account.clone(),
                zeroing_side(row.account_type, &row.balance),
                row.balance.abs(),
                TagSet::new(),
                None,
            )?);
        }

        // 手順1: 所得 = 収益合計 − 費用合計。
        let revenue_total = tb.total_by_type(AccountType::Revenue);
        let expense_total = tb.total_by_type(AccountType::Expense);
        let income = revenue_total.sub(&expense_total)?;

        // 手順3のうち当年度末に計上する分: 所得を元入金へ振り替える。
        if !income.is_zero() {
            let side = if income.is_negative() {
                Side::Debit
            } else {
                Side::Credit
            };
            lines.push(JournalLine::new(
                self.capital_account.clone(),
                side,
                income.abs(),
                TagSet::new(),
                None,
            )?);
        }

        if lines.is_empty() {
            return Ok(Vec::new());
        }

        verify_balanced(&lines)?;

        Ok(vec![ProposedEntry {
            entry_date: fy.end(),
            description: format!("決算振替: {}年度の収益・費用を元入金へ振替", fy.label()),
            lines,
        }])
    }

    // opening_entries は既定実装（何も生成しない）のまま使う（モジュール doc
    // 「opening_entries は実装しない」、`DECISIONS.md` D-065）。
}

/// 収益・費用科目をゼロにする仕訳の側（借方・貸方）を決める。
///
/// 原則は「残高を反対側に立てる」。`balance` は `AccountType::is_debit_normal`
/// に従った符号付き残高（`kaikei_core::trial_balance` を参照）であり、通常の
/// 符号（収益・負債・純資産なら貸方残、資産・費用なら借方残）であれば単純に
/// 反対側へ計上すればよい。
///
/// 返品・値引等で残高が通常と逆の符号（例: 売上高がマイナス）になっている
/// 場合は、逆に**通常側**へ計上しないとゼロにならない。`balance.is_negative()`
/// で場合分けするのはこのため。
fn zeroing_side(account_type: AccountType, balance: &Money) -> Side {
    let debit_normal = account_type.is_debit_normal();
    match (debit_normal, balance.is_negative()) {
        (true, false) => Side::Credit,
        (true, true) => Side::Debit,
        (false, false) => Side::Debit,
        (false, true) => Side::Credit,
    }
}

/// 生成した明細の借方合計と貸方合計が一致することを検証する。
///
/// モジュール doc「貸借が一致する理由」のとおり、手順1〜3の計算が正しければ
/// 常に一致するはずだが、将来の変更で崩れた場合に誤った仕訳がそのまま
/// 提案されてしまうことを防ぐ最後の砦として置く（`CLAUDE.md` §2「会計データは
/// 間違うと実害が出る」）。
fn verify_balanced(lines: &[JournalLine]) -> Result<(), PolicyError> {
    let currency = lines
        .first()
        .expect("verify_balanced は空でない lines でのみ呼ばれる")
        .amount()
        .currency();

    let debit_total = side_total(lines, Side::Debit, currency)?;
    let credit_total = side_total(lines, Side::Credit, currency)?;

    if debit_total.minor() != credit_total.minor() {
        let diff = debit_total.sub(&credit_total)?.abs();
        return Err(CoreError::Unbalanced {
            debit: debit_total.to_display_string(),
            credit: credit_total.to_display_string(),
            diff: diff.to_display_string(),
        }
        .into());
    }
    Ok(())
}

fn side_total(lines: &[JournalLine], side: Side, currency: Currency) -> Result<Money, PolicyError> {
    let amounts = lines
        .iter()
        .filter(|l| l.side() == side)
        .map(|l| l.amount());
    Ok(sum_money(amounts)?.unwrap_or_else(|| Money::zero(currency)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::new_entry;
    use kaikei_core::{AccountDef, Currency, JournalEntry, TagSchema};
    use proptest::prelude::*;

    fn test_chart() -> ChartOfAccounts {
        ChartOfAccounts::new(vec![
            account("100", "現金", AccountType::Asset),
            account("320", "借入金", AccountType::Liability),
            account("400", "元入金", AccountType::Equity),
            account("410", "事業主貸", AccountType::Equity),
            account("420", "事業主借", AccountType::Equity),
            account("500", "売上高", AccountType::Revenue),
            account("510", "雑収入", AccountType::Revenue),
            account("600", "仕入高", AccountType::Expense),
            account("610", "地代家賃", AccountType::Expense),
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
        // `TagSchema::empty()` を使う理由はモジュール doc「tax_category タグを
        // 付与しない」を参照。closing_entries 自体のロジック（貸借計算）を
        // 実運用のタグ要件から切り離してテストする。
        TagSchema::empty()
    }

    fn policy(chart: &ChartOfAccounts) -> JpSoleProprietorClosingPolicy {
        JpSoleProprietorClosingPolicy::new(
            chart,
            AccountCode::parse("400").unwrap(),
            AccountCode::parse("410").unwrap(),
            AccountCode::parse("420").unwrap(),
        )
        .unwrap()
    }

    fn cash_entry(
        chart: &ChartOfAccounts,
        schema: &TagSchema,
        fy: &FiscalYear,
        id: u128,
        account_code: &str,
        account_side: Side,
        amount_minor: i128,
    ) -> JournalEntry {
        let account = AccountCode::parse(account_code).unwrap();
        let cash = AccountCode::parse("100").unwrap();
        let amount = Money::from_minor(amount_minor, Currency::JPY);
        let cash_side = opposite(account_side);
        let lines = vec![
            JournalLine::new(account, account_side, amount, TagSet::new(), None).unwrap(),
            JournalLine::new(cash, cash_side, amount, TagSet::new(), None).unwrap(),
        ];
        new_entry(id, id as u32, fy, chart, schema, fy.start(), "test", lines)
    }

    fn opposite(side: Side) -> Side {
        match side {
            Side::Debit => Side::Credit,
            Side::Credit => Side::Debit,
        }
    }

    fn balance_of(entry: &ProposedEntry, code: &str) -> Option<(Side, i128)> {
        entry
            .lines
            .iter()
            .find(|l| l.account().as_str() == code)
            .map(|l| (l.side(), l.amount().minor()))
    }

    fn debit_total(entry: &ProposedEntry) -> i128 {
        entry
            .lines
            .iter()
            .filter(|l| l.is_debit())
            .map(|l| l.amount().minor())
            .sum()
    }

    fn credit_total(entry: &ProposedEntry) -> i128 {
        entry
            .lines
            .iter()
            .filter(|l| !l.is_debit())
            .map(|l| l.amount().minor())
            .sum()
    }

    // ---- 手順1〜3: 具体的な数値例（手計算した期待値） ----

    /// 売上高1,000,000（貸）・仕入高400,000（借）の単純な黒字。
    /// 期待: 売上高を借方1,000,000でゼロ化、仕入高を貸方400,000でゼロ化、
    /// 元入金へ貸方600,000（所得）を計上。借方合計=貸方合計=1,000,000。
    #[test]
    fn closing_entries_reproduces_docs_section_9_example_with_profit() {
        let chart = test_chart();
        let schema = schema();
        let fy = fy();
        let entries = [
            cash_entry(&chart, &schema, &fy, 1, "500", Side::Credit, 1_000_000),
            cash_entry(&chart, &schema, &fy, 2, "600", Side::Debit, 400_000),
        ];
        let tb = TrialBalance::from_entries(entries.iter(), &chart, &schema, &[]).unwrap();

        let proposed = policy(&chart).closing_entries(&tb, &fy).unwrap();
        assert_eq!(proposed.len(), 1);
        let entry = &proposed[0];
        assert_eq!(entry.entry_date, fy.end());
        assert_eq!(entry.lines.len(), 3);

        assert_eq!(balance_of(entry, "500"), Some((Side::Debit, 1_000_000)));
        assert_eq!(balance_of(entry, "600"), Some((Side::Credit, 400_000)));
        assert_eq!(balance_of(entry, "400"), Some((Side::Credit, 600_000)));

        assert_eq!(debit_total(entry), 1_000_000);
        assert_eq!(credit_total(entry), 1_000_000);
    }

    /// 売上高300,000（貸）・仕入高500,000（借）の赤字（所得マイナス200,000）。
    /// 期待: 元入金へ借方200,000（損失分、元入金を減らす）を計上。
    #[test]
    fn closing_entries_handles_a_loss_by_debiting_capital_account() {
        let chart = test_chart();
        let schema = schema();
        let fy = fy();
        let entries = [
            cash_entry(&chart, &schema, &fy, 1, "500", Side::Credit, 300_000),
            cash_entry(&chart, &schema, &fy, 2, "600", Side::Debit, 500_000),
        ];
        let tb = TrialBalance::from_entries(entries.iter(), &chart, &schema, &[]).unwrap();

        let proposed = policy(&chart).closing_entries(&tb, &fy).unwrap();
        assert_eq!(proposed.len(), 1);
        let entry = &proposed[0];

        assert_eq!(balance_of(entry, "500"), Some((Side::Debit, 300_000)));
        assert_eq!(balance_of(entry, "600"), Some((Side::Credit, 500_000)));
        assert_eq!(balance_of(entry, "400"), Some((Side::Debit, 200_000)));

        assert_eq!(debit_total(entry), 500_000);
        assert_eq!(credit_total(entry), 500_000);
    }

    /// 返品・値引で売上高がマイナス残高（貸方< 借方）になるケース。
    /// 現金100,000（借）/売上高100,000（貸）の後、返品で売上高150,000（借）/
    /// 現金150,000（貸）。売上高の残高は 100,000 - 150,000 = -50,000。
    /// 期待: 通常と逆の借方残高なので、ゼロ化は**貸方**（通常側）に立てる。
    #[test]
    fn closing_entries_handles_negative_revenue_balance_from_returns() {
        let chart = test_chart();
        let schema = schema();
        let fy = fy();
        let entries = [
            cash_entry(&chart, &schema, &fy, 1, "500", Side::Credit, 100_000),
            // 返品: 売上高を借方に150,000（残高がマイナスになるよう多めに戻す）。
            cash_entry(&chart, &schema, &fy, 2, "500", Side::Debit, 150_000),
        ];
        let tb = TrialBalance::from_entries(entries.iter(), &chart, &schema, &[]).unwrap();
        assert_eq!(
            tb.balance_of(&AccountCode::parse("500").unwrap())
                .unwrap()
                .minor(),
            -50_000
        );

        let proposed = policy(&chart).closing_entries(&tb, &fy).unwrap();
        assert_eq!(proposed.len(), 1);
        let entry = &proposed[0];

        // 売上高: 残高マイナスなので貸方（通常側）にゼロ化。
        assert_eq!(balance_of(entry, "500"), Some((Side::Credit, 50_000)));
        // 所得 = -50,000（損失）なので元入金は借方。
        assert_eq!(balance_of(entry, "400"), Some((Side::Debit, 50_000)));

        assert_eq!(debit_total(entry), 50_000);
        assert_eq!(credit_total(entry), 50_000);
    }

    // ---- 残高0の科目に明細を作らない ----

    #[test]
    fn closing_entries_skips_zero_balance_revenue_and_expense_accounts() {
        let chart = test_chart();
        let schema = schema();
        let fy = fy();
        // 売上高・雑収入とも計上し、雑収入は貸借同額で相殺してゼロにする。
        let entries = [
            cash_entry(&chart, &schema, &fy, 1, "500", Side::Credit, 100_000),
            cash_entry(&chart, &schema, &fy, 2, "510", Side::Credit, 50_000),
            cash_entry(&chart, &schema, &fy, 3, "510", Side::Debit, 50_000),
        ];
        let tb = TrialBalance::from_entries(entries.iter(), &chart, &schema, &[]).unwrap();
        assert_eq!(
            tb.balance_of(&AccountCode::parse("510").unwrap())
                .unwrap()
                .minor(),
            0
        );

        let proposed = policy(&chart).closing_entries(&tb, &fy).unwrap();
        let entry = &proposed[0];
        assert!(
            entry.lines.iter().all(|l| l.account().as_str() != "510"),
            "残高0の雑収入(510)には明細が作られてはならない"
        );
    }

    // ---- 収益・費用が空（取引ゼロ）の年度 ----

    #[test]
    fn closing_entries_with_no_revenue_or_expense_activity_returns_empty() {
        let chart = test_chart();
        let schema = schema();
        let fy = fy();
        // 資産・負債のみの取引（借入金の受入）。収益・費用は一切動かない。
        let entries = [cash_entry(
            &chart,
            &schema,
            &fy,
            1,
            "320",
            Side::Credit,
            1_000_000,
        )];
        let tb = TrialBalance::from_entries(entries.iter(), &chart, &schema, &[]).unwrap();

        let proposed = policy(&chart).closing_entries(&tb, &fy).unwrap();
        assert!(proposed.is_empty());
    }

    #[test]
    fn closing_entries_with_completely_empty_trial_balance_returns_empty() {
        let chart = test_chart();
        let schema = schema();
        let fy = fy();
        let tb = TrialBalance::from_entries(std::iter::empty(), &chart, &schema, &[]).unwrap();

        let proposed = policy(&chart).closing_entries(&tb, &fy).unwrap();
        assert!(proposed.is_empty());
    }

    // ---- opening_entries は既定実装のまま（何も生成しない） ----

    #[test]
    fn opening_entries_uses_default_impl_and_returns_empty() {
        let chart = test_chart();
        let schema = schema();
        let fy = fy();
        let entries = [cash_entry(
            &chart,
            &schema,
            &fy,
            1,
            "500",
            Side::Credit,
            1_000_000,
        )];
        let tb = TrialBalance::from_entries(entries.iter(), &chart, &schema, &[]).unwrap();

        let opening = policy(&chart).opening_entries(&tb, &fy).unwrap();
        assert!(opening.is_empty());
    }

    // ---- 構築時の科目存在検証 ----

    #[test]
    fn new_rejects_missing_capital_account_and_names_the_code() {
        let chart = ChartOfAccounts::new(vec![
            account("410", "事業主貸", AccountType::Equity),
            account("420", "事業主借", AccountType::Equity),
        ])
        .unwrap();
        let err = JpSoleProprietorClosingPolicy::new(
            &chart,
            AccountCode::parse("400").unwrap(),
            AccountCode::parse("410").unwrap(),
            AccountCode::parse("420").unwrap(),
        )
        .unwrap_err();
        match err {
            JpError::MissingClosingAccount { role, code } => {
                assert_eq!(role, "元入金");
                assert_eq!(code, "400");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn new_rejects_missing_owner_drawings_account_and_names_the_code() {
        let chart = ChartOfAccounts::new(vec![
            account("400", "元入金", AccountType::Equity),
            account("420", "事業主借", AccountType::Equity),
        ])
        .unwrap();
        let err = JpSoleProprietorClosingPolicy::new(
            &chart,
            AccountCode::parse("400").unwrap(),
            AccountCode::parse("410").unwrap(),
            AccountCode::parse("420").unwrap(),
        )
        .unwrap_err();
        match err {
            JpError::MissingClosingAccount { role, code } => {
                assert_eq!(role, "事業主貸");
                assert_eq!(code, "410");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn new_rejects_missing_owner_contributions_account_and_names_the_code() {
        let chart = ChartOfAccounts::new(vec![
            account("400", "元入金", AccountType::Equity),
            account("410", "事業主貸", AccountType::Equity),
        ])
        .unwrap();
        let err = JpSoleProprietorClosingPolicy::new(
            &chart,
            AccountCode::parse("400").unwrap(),
            AccountCode::parse("410").unwrap(),
            AccountCode::parse("420").unwrap(),
        )
        .unwrap_err();
        match err {
            JpError::MissingClosingAccount { role, code } => {
                assert_eq!(role, "事業主借");
                assert_eq!(code, "420");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn new_succeeds_when_all_three_accounts_exist_and_holds_the_codes() {
        let chart = test_chart();
        let policy = policy(&chart);
        assert_eq!(policy.capital_account().as_str(), "400");
        assert_eq!(policy.owner_drawings_account().as_str(), "410");
        assert_eq!(policy.owner_contributions_account().as_str(), "420");
    }

    // ---- 出力順の決定性 ----

    #[test]
    fn closing_entries_output_is_deterministic_across_repeated_calls() {
        let chart = test_chart();
        let schema = schema();
        let fy = fy();
        let entries = [
            cash_entry(&chart, &schema, &fy, 1, "500", Side::Credit, 100_003),
            cash_entry(&chart, &schema, &fy, 2, "510", Side::Credit, 50_007),
            cash_entry(&chart, &schema, &fy, 3, "600", Side::Debit, 30_011),
            cash_entry(&chart, &schema, &fy, 4, "610", Side::Debit, 20_013),
        ];
        let tb = TrialBalance::from_entries(entries.iter(), &chart, &schema, &[]).unwrap();
        let policy = policy(&chart);

        let fingerprint = |proposed: &[ProposedEntry]| -> Vec<(String, i128, bool)> {
            proposed
                .iter()
                .flat_map(|e| e.lines.iter())
                .map(|l| {
                    (
                        l.account().as_str().to_string(),
                        l.amount().minor(),
                        l.is_debit(),
                    )
                })
                .collect()
        };

        let first = fingerprint(&policy.closing_entries(&tb, &fy).unwrap());
        for i in 1..20 {
            let again = fingerprint(&policy.closing_entries(&tb, &fy).unwrap());
            assert_eq!(again, first, "{i}回目の実行で明細の順序・内容が変わった");
        }
    }

    // ---- プロパティテスト ----
    //
    // `PROGRESS.md` Phase 0 の教訓（生成器は「型が表現できる範囲」ではなく
    // 「仕様が許容する範囲」に合わせる）に従い、端数・境界値（1, -1, 大きな値）と
    // 負の残高（返品・値引で実際に起こりうる）を `prop_oneof!` で明示的に含める。

    #[derive(Debug, Clone, Copy)]
    enum Target {
        Revenue1,
        Revenue2,
        Expense1,
        Expense2,
    }

    impl Target {
        fn account_code(self) -> &'static str {
            match self {
                Target::Revenue1 => "500",
                Target::Revenue2 => "510",
                Target::Expense1 => "600",
                Target::Expense2 => "610",
            }
        }

        /// 通常側（残高を増やす向き）の借方・貸方。
        fn increasing_side(self) -> Side {
            match self {
                Target::Revenue1 | Target::Revenue2 => Side::Credit,
                Target::Expense1 | Target::Expense2 => Side::Debit,
            }
        }
    }

    fn any_target() -> impl Strategy<Value = Target> {
        prop_oneof![
            Just(Target::Revenue1),
            Just(Target::Revenue2),
            Just(Target::Expense1),
            Just(Target::Expense2),
        ]
    }

    /// 残高として狙う符号付き金額（最小通貨単位）。0は除く
    /// （`JournalLine::new` が0円を拒否するため、意図して生成対象から外す）。
    fn any_signed_minor() -> impl Strategy<Value = i128> {
        prop_oneof![
            6 => 1i128..=1_000_000i128,
            6 => -1_000_000i128..=-1i128,
            1 => Just(1i128),
            1 => Just(-1i128),
            1 => Just(999_999_999i128),
            1 => Just(-999_999_999i128),
        ]
    }

    fn any_row() -> impl Strategy<Value = (Target, i128)> {
        (any_target(), any_signed_minor())
    }

    /// `rows` から、各行の科目残高が指定どおりの符号・金額になるような
    /// 2行仕訳（対象科目 / 現金）を組み立てる。
    fn build_entries(
        chart: &ChartOfAccounts,
        schema: &TagSchema,
        fy: &FiscalYear,
        rows: &[(Target, i128)],
    ) -> Vec<JournalEntry> {
        rows.iter()
            .enumerate()
            .map(|(i, (target, minor))| {
                let account_side = if *minor > 0 {
                    target.increasing_side()
                } else {
                    opposite(target.increasing_side())
                };
                cash_entry(
                    chart,
                    schema,
                    fy,
                    i as u128,
                    target.account_code(),
                    account_side,
                    minor.abs(),
                )
            })
            .collect()
    }

    proptest! {
        /// **最重要の性質1**: 任意の試算表に対して、生成された `ProposedEntry` は
        /// すべて貸借一致する。
        #[test]
        fn closing_entries_are_always_balanced(
            rows in prop::collection::vec(any_row(), 0..=8),
        ) {
            let chart = test_chart();
            let schema = schema();
            let fy = fy();
            let entries = build_entries(&chart, &schema, &fy, &rows);
            let tb = TrialBalance::from_entries(entries.iter(), &chart, &schema, &[]).unwrap();

            let proposed = policy(&chart).closing_entries(&tb, &fy).unwrap();
            for entry in &proposed {
                prop_assert_eq!(debit_total(entry), credit_total(entry));
            }
        }

        /// **最重要の性質2**: 収益・費用のゼロ化仕訳を適用した後、収益・費用の
        /// 残高がすべて0になる。
        #[test]
        fn closing_entries_zero_out_revenue_and_expense_when_applied(
            rows in prop::collection::vec(any_row(), 0..=8),
        ) {
            let chart = test_chart();
            let schema = schema();
            let fy = fy();
            let mut entries = build_entries(&chart, &schema, &fy, &rows);
            let tb_before = TrialBalance::from_entries(entries.iter(), &chart, &schema, &[]).unwrap();

            let proposed = policy(&chart).closing_entries(&tb_before, &fy).unwrap();
            for (i, p) in proposed.into_iter().enumerate() {
                let closing_entry = new_entry(
                    100_000 + i as u128,
                    100_000 + i as u32,
                    &fy,
                    &chart,
                    &schema,
                    p.entry_date,
                    &p.description,
                    p.lines,
                );
                entries.push(closing_entry);
            }

            let tb_after = TrialBalance::from_entries(entries.iter(), &chart, &schema, &[]).unwrap();
            prop_assert!(tb_after.total_by_type(AccountType::Revenue).is_zero());
            prop_assert!(tb_after.total_by_type(AccountType::Expense).is_zero());
        }
    }
}
