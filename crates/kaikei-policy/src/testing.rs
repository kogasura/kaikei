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
    sum_money, AccountDef, AccountType, BalanceRow, Currency, EntryNumber, FiscalYear,
    JournalEntry, JournalLine, Money, RoundMode, Side, TagSet, TrialBalance,
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

/// 側（借方・貸方）ごとの明細合計に対して一律に `rate` を掛けた税額行を、
/// 同じ側に最大1行追加する `TaxPolicy`。
///
/// 税区分ごとの判定（`direction` / `deductible` / 適格請求書の要否等）は
/// 一切行わない最小実装であり、実際の税制ロジックの代替にはならない。
/// 税額が 0 になる側については税額行を追加しない。
///
/// **税額は明細1行ごとではなく、側ごとの合計に対して1回だけ計算する。**
/// 明細ごとに計算すると、行ごとの丸め誤差の積み上げが入力の貸借一致から
/// ズレて出力全体の貸借が崩れることがある（例:
/// 借方 33,333円+66,667円・貸方100,000円・rate=10%・Floor のとき、
/// 行ごとに丸めると 3,333+6,666=9,999 円になり貸方の 10,000 円と一致しない）。
/// 同じ base（側ごとの合計）に同じ rate/round を適用する限り、
/// 「入力が貸借一致なら出力も貸借一致する」ことが保証される
/// （回帰テスト: `flat_rate_tax_policy_keeps_balance_when_debit_side_is_split_into_multiple_lines`）。
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
        for side in [Side::Debit, Side::Credit] {
            let amounts = lines
                .iter()
                .filter(|l| l.side() == side)
                .map(|l| l.amount());
            let Some(total) = sum_money(amounts)? else {
                continue;
            };
            let tax_amount = self.apply_ratio(ctx, total, self.rate)?;
            if tax_amount.is_zero() {
                continue;
            }
            result.push(JournalLine::new(
                tax_account.clone(),
                side,
                tax_amount,
                TagSet::new(),
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
        let next = match issued {
            None => 1,
            // `+ 1` を無検証で行うと `EntryNumber(u32::MAX)` のとき debug では
            // panic、release では無言に `0` へラップする（`0` は「未払い出し」を
            // 表す `None` と実質衝突する値になる）。D-018/D-020 と同じ欠陥クラス
            // なので `checked_add` を使う。
            Some(n) => n
                .as_u32()
                .checked_add(1)
                .ok_or_else(|| PolicyError::InvalidPolicyData {
                    reason: "仕訳番号が u32 の上限に達しました".to_string(),
                })?,
        };
        Ok(EntryNumber::new(next))
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

    // `tb.rows()` を1回だけ走査し、科目種別ごとに行を振り分ける（区分数だけ
    // `tb.rows()` を繰り返し走査しないため）。`AccountType` は `Ord`/`Hash` を
    // 導出していない（`kaikei-core` の変更が必要で本 crate の変更対象外）ため
    // `BTreeMap`/`HashMap` ではなく `PartialEq` の線形探索で束ねる。
    // 実際の科目種別は5種類のみなので、線形探索であっても実質 O(行数) で収まる。
    let mut by_type: Vec<(AccountType, Vec<&BalanceRow>)> = Vec::new();
    for row in tb.rows() {
        match by_type.iter_mut().find(|(t, _)| *t == row.account_type) {
            Some((_, rows)) => rows.push(row),
            None => by_type.push((row.account_type, vec![row])),
        }
    }

    let mut sections = Vec::with_capacity(groups.len());
    let mut total = Money::zero(currency);
    for (account_type, section_title) in groups {
        let rows: &[&BalanceRow] = by_type
            .iter()
            .find(|(t, _)| t == account_type)
            .map(|(_, rows)| rows.as_slice())
            .unwrap_or(&[]);
        let lines: Vec<StatementLine> = rows
            .iter()
            .map(|row| StatementLine {
                account: row.account.clone(),
                label: row.account.as_str().to_string(),
                amount: row.balance,
            })
            .collect();
        let subtotal = sum_money(rows.iter().map(|row| &row.balance))
            .expect("同一 TrialBalance 内の残高は同一通貨であるため、合算は失敗しない")
            .unwrap_or_else(|| Money::zero(currency));
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

    // 回帰テスト: 借方が複数行に分割されていると、行ごとに税額を計算した場合
    // 丸め誤差が積み上がって出力全体の貸借が崩れる不具合があった
    // （借方 33,333円+66,667円・貸方100,000円・rate=10%・Floor のとき、
    // 旧実装は行ごとに floor(33,333×0.1)=3,333 / floor(66,667×0.1)=6,666 を
    // 計算し、合計 9,999 円が貸方側の 10,000 円と食い違っていた）。
    // 側ごとの合計に対して1回だけ税額を計算する現在の実装では、
    // 「入力が貸借一致なら出力も貸借一致する」ことを確認する。
    #[test]
    fn flat_rate_tax_policy_keeps_balance_when_debit_side_is_split_into_multiple_lines() {
        let policy = FlatRateTaxPolicy {
            rate: kaikei_core::Ratio::parse_rate("0.10").unwrap(),
            tax_account: "330",
        };
        let chart = ChartOfAccounts::new(vec![]).unwrap();
        let schema = TagSchema::empty();
        let counterparties = crate::counterparty::CounterpartyIndex::empty();
        let ctx = sample_context(&chart, &schema, &counterparties);

        let lines = vec![
            JournalLine::new(
                AccountCode::parse("100").unwrap(),
                Side::Debit,
                Money::from_minor(33_333, Currency::JPY),
                TagSet::new(),
                None,
            )
            .unwrap(),
            JournalLine::new(
                AccountCode::parse("100").unwrap(),
                Side::Debit,
                Money::from_minor(66_667, Currency::JPY),
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

        let derivation = policy.derive_tax_lines(&ctx, &lines).unwrap();
        let debit_total: i128 = derivation
            .lines
            .iter()
            .filter(|l| l.is_debit())
            .map(|l| l.amount().minor())
            .sum();
        let credit_total: i128 = derivation
            .lines
            .iter()
            .filter(|l| !l.is_debit())
            .map(|l| l.amount().minor())
            .sum();
        assert_eq!(
            debit_total, credit_total,
            "行ごとの端数計算では 109,999 / 110,000 と貸借が崩れていたバグの回帰テスト"
        );
        // 借方合計 100,000 の 10% は 10,000（側ごとの合計に対する丸め）。
        assert_eq!(debit_total, 110_000);
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

    // 回帰テスト: `EntryNumber(u32::MAX)` の次を求めると、無検証の `+ 1` では
    // debug で panic・release で無言に `0`（＝「未払い出し」を表す `None` と
    // 実質衝突する値）へラップしていた（D-018/D-020 と同じ欠陥クラス）。
    // `checked_add` により `Err` を返すことを確認する。
    #[test]
    fn sequential_numbering_at_u32_max_returns_error_instead_of_wrapping() {
        let numbering = SequentialNumbering;
        let fy = FiscalYear::calendar_year(2026);
        let result = numbering.peek(&fy, Some(EntryNumber::new(u32::MAX)));
        assert!(
            matches!(result, Err(PolicyError::InvalidPolicyData { .. })),
            "u32::MAX の次はエラーになるべき（無言のラップは禁止）: {result:?}"
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

    // ---- プロパティテスト: FlatRateTaxPolicy の貸借一致（修正1の回帰防止） ----
    //
    // 明細1行ごとに税額を計算する実装は、行ごとの丸め誤差が積み上がって
    // 出力の貸借を崩すことがある（上記の回帰テスト参照）。ここでは
    // 「借方・貸方それぞれ任意の行数に任意に分割されていても、入力が
    // 貸借一致していれば出力も貸借一致する」という不変条件を、
    // Phase 0 の教訓（生成器のレンジを型が構築可能な全域に近づける）を
    // 踏まえてランダムな行数・金額・税率で検証する。
    mod balance_invariant {
        use super::*;
        use proptest::prelude::*;
        use proptest::strategy::BoxedStrategy;

        /// `total`（`k` 個の正の整数へ分割可能、すなわち `total >= k`）を、
        /// ランダムな正の整数 `k` 個に分割する。
        fn positive_partition(total: i128, k: usize) -> BoxedStrategy<Vec<i128>> {
            if k <= 1 {
                return Just(vec![total]).boxed();
            }
            // 最初の1個を 1..=(total - (k-1)) から選び、残りを再帰的に分割する
            // （残り k-1 個に少なくとも 1 円ずつ残すための上限）。
            (1i128..=(total - (k as i128 - 1)))
                .prop_flat_map(move |first| {
                    positive_partition(total - first, k - 1).prop_map(move |mut rest| {
                        let mut amounts = vec![first];
                        amounts.append(&mut rest);
                        amounts
                    })
                })
                .boxed()
        }

        /// 借方・貸方それぞれが同じ `total` に分割された、行数も端数の出方も
        /// 様々な明細の組を生成する。`total` は「実務的にありそうな金額」では
        /// なく、1円のような極小値から百万円台までを広く踏む
        /// （Phase 0 の教訓: 生成器のレンジが仕様の許容範囲より狭いと
        /// 実バグを見逃す）。
        fn balanced_split_strategy() -> impl Strategy<Value = (Vec<i128>, Vec<i128>)> {
            let total_strategy = prop_oneof![
                3 => 1i128..=3i128,
                7 => 4i128..=1_000_000i128,
            ];
            total_strategy
                .prop_flat_map(|total| {
                    let max_k = total.min(4) as u8;
                    (Just(total), 1u8..=max_k, 1u8..=max_k)
                })
                .prop_flat_map(|(total, k_debit, k_credit)| {
                    (
                        positive_partition(total, k_debit as usize),
                        positive_partition(total, k_credit as usize),
                    )
                })
        }

        proptest! {
            #[test]
            fn flat_rate_tax_policy_preserves_balance_for_arbitrary_splits(
                (debit_amounts, credit_amounts) in balanced_split_strategy(),
                rate_str in prop_oneof![
                    Just("0.00".to_string()),
                    Just("1.00".to_string()),
                    "0\\.[0-9]{1,3}",
                    "[1-9][0-9]{0,1}\\.[0-9]{1,3}",
                ],
            ) {
                // 生成器自体が入力の貸借一致を保証していることの自己検証。
                let input_debit: i128 = debit_amounts.iter().sum();
                let input_credit: i128 = credit_amounts.iter().sum();
                prop_assert_eq!(input_debit, input_credit);

                let rate = kaikei_core::Ratio::parse_rate(&rate_str).unwrap();
                let policy = FlatRateTaxPolicy { rate, tax_account: "330" };
                let chart = ChartOfAccounts::new(vec![]).unwrap();
                let schema = TagSchema::empty();
                let counterparties = crate::counterparty::CounterpartyIndex::empty();
                let ctx = sample_context(&chart, &schema, &counterparties);

                let debit_account = AccountCode::parse("100").unwrap();
                let credit_account = AccountCode::parse("500").unwrap();
                let mut lines = Vec::new();
                for amount in &debit_amounts {
                    lines.push(
                        JournalLine::new(
                            debit_account.clone(),
                            Side::Debit,
                            Money::from_minor(*amount, Currency::JPY),
                            TagSet::new(),
                            None,
                        )
                        .unwrap(),
                    );
                }
                for amount in &credit_amounts {
                    lines.push(
                        JournalLine::new(
                            credit_account.clone(),
                            Side::Credit,
                            Money::from_minor(*amount, Currency::JPY),
                            TagSet::new(),
                            None,
                        )
                        .unwrap(),
                    );
                }

                let derivation = policy.derive_tax_lines(&ctx, &lines).unwrap();
                let output_debit: i128 = derivation
                    .lines
                    .iter()
                    .filter(|l| l.is_debit())
                    .map(|l| l.amount().minor())
                    .sum();
                let output_credit: i128 = derivation
                    .lines
                    .iter()
                    .filter(|l| !l.is_debit())
                    .map(|l| l.amount().minor())
                    .sum();
                prop_assert_eq!(output_debit, output_credit);
            }
        }
    }
}
