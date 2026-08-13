//! 帳簿の整合性検査（[`execute`]）。
//!
//! `ROADMAP.md` Phase 5 の完了条件「整合性検査が通る」に対応する。
//!
//! # 何を検査するのか
//!
//! **2つの経路で計算した同じ数字が一致するか**を見る。`kaikei` は同じ帳簿に
//! 対して2つの集計経路を持っている（`DECISIONS.md` D-093）:
//!
//! - **read model** … SQL の `SUM`（`get_trial_balance`）
//! - **ドメインモデル** … `TrialBalance::from_entries`（決算書・決算振替）
//!
//! 両者が食い違ったら、**どちらかにバグがある**。片方だけを見ている限り
//! 気づけない——どちらも「貸借が一致した、もっともらしい試算表」を返すからで
//! ある。D-093 のトレードオフに「その突き合わせは今のところ自動では行って
//! いない」と書いた宿題をここで消化する。
//!
//! あわせて、帳簿の内部で閉じた検査も行う（赤伝の参照先、仕訳番号の重複）。
//!
//! # ハッシュ連鎖はまだ検査しない
//!
//! `docs/03-database.md` §2 の checksum（`h_i = sha256(h_{i-1} ||
//! canonical_json(entry_i))`）は、**canonical JSON の形が未定義**であり、
//! かつ記録する側（`close_period`）が未実装である（`DECISIONS.md` D-070）。
//! 検査だけ先に作ると、**検査対象の無い検査**になる。
//!
//! # 「異常なし」を返せることに意味がある
//!
//! 不整合が無ければ空の一覧を返す。**何も見つからなかったことと、検査が
//! 走らなかったことは違う**ので、検査した仕訳の件数を必ず返す。

use crate::error::AppError;
use crate::ports::{ChartRepo, JournalRepo, TrialBalanceQuery};
use crate::view::TrialBalanceView;
use kaikei_core::{AccountCode, FiscalYear, Money, TagSchema, TrialBalance};
use std::collections::{BTreeMap, BTreeSet};

/// [`execute`] への入力。
#[derive(Debug, Clone)]
pub struct VerifyInput {
    /// 検査する会計年度（暦年）。
    pub fiscal_year: i32,
}

/// 見つかった不整合1件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// 機械可読な種別。
    pub kind: FindingKind,
    /// 人が読む説明。**何が食い違ったかと、次に何を見ればよいかを書く**
    /// （`CLAUDE.md` §11）。
    pub detail: String,
}

/// 不整合の種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FindingKind {
    /// read model とドメインモデルで、科目の残高が食い違う。
    BalanceMismatch,
    /// read model とドメインモデルで、集計対象の科目の集合が食い違う。
    AccountSetMismatch,
    /// 赤伝が指している原仕訳が、この期間の帳簿に見当たらない。
    DanglingReversal,
    /// 同じ仕訳番号が複数の仕訳に付いている。
    DuplicateEntryNumber,
}

impl FindingKind {
    /// 機械可読名。
    pub fn as_code(&self) -> &'static str {
        match self {
            FindingKind::BalanceMismatch => "balance_mismatch",
            FindingKind::AccountSetMismatch => "account_set_mismatch",
            FindingKind::DanglingReversal => "dangling_reversal",
            FindingKind::DuplicateEntryNumber => "duplicate_entry_number",
        }
    }
}

/// [`execute`] の出力。
#[derive(Debug, Clone)]
pub struct VerifyOutput {
    /// 検査した仕訳の件数。
    ///
    /// **0 件でも「異常なし」が返る。** 検査が走らなかったのか、帳簿が空
    /// なのかを呼び出し側が区別できるように返す。
    pub entry_count: usize,
    /// 見つかった不整合。空なら異常なし。
    pub findings: Vec<Finding>,
}

impl VerifyOutput {
    /// 不整合が1件も無いか。
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }
}

/// 帳簿を検査する。
///
/// # Errors
///
/// 読み込みに失敗した場合は [`AppError::Repo`]、試算表の組み立てに失敗した
/// 場合は [`AppError::Core`]。**不整合が見つかったことは `Err` にしない**
/// ——検査は「走って、結果を返す」ものであり、結果が悪いことは失敗ではない。
pub async fn execute<Tx>(
    tx: &mut Tx,
    query: &dyn TrialBalanceQuery,
    tag_schema: &TagSchema,
    input: VerifyInput,
) -> Result<VerifyOutput, AppError>
where
    Tx: JournalRepo + ChartRepo + Send,
{
    let fiscal_year = FiscalYear::calendar_year(input.fiscal_year);
    let from = fiscal_year.start();
    let to = fiscal_year.end();

    let chart = tx.load_chart().await?;
    let entries = tx.list_entries_in_period(from, to).await?;
    let entry_count = entries.len();

    let mut findings = Vec::new();

    // 1. 赤伝の参照先と仕訳番号の重複（帳簿の内部で閉じた検査）。
    findings.extend(check_reversals(&entries));
    findings.extend(check_entry_numbers(&entries));

    // 2. ★2つの経路で計算した試算表を突き合わせる★
    let domain = TrialBalance::from_entries(entries.iter(), &chart, tag_schema, &[])?;
    let rows = query.trial_balance(from, to, &[]).await?;
    // 通貨は行から推論せず、ドメイン側と同じ帳簿通貨を使う（`view.rs` の doc）。
    // ここでは残高の値だけを比べるので、包むのは行を扱いやすくするため。
    let read_model = TrialBalanceView::new(rows, domain_currency(&domain));
    findings.extend(compare_balances(&domain, &read_model));

    Ok(VerifyOutput {
        entry_count,
        findings,
    })
}

/// ドメイン側の試算表から通貨を取る。
///
/// 行が無ければ判断できないので JPY にはフォールバックせず、
/// **比較に通貨を使わない**（残高の突き合わせは `Money` 同士の比較で行い、
/// `Money` は通貨を内包している）。ここで返すのは `TrialBalanceView` を
/// 構築するための形式的な値である。
fn domain_currency(domain: &TrialBalance) -> kaikei_core::Currency {
    domain
        .rows()
        .first()
        .map_or(kaikei_core::Currency::JPY, |row| row.balance.currency())
}

/// 赤伝が指している原仕訳が帳簿にあるか。
fn check_reversals(entries: &[kaikei_core::JournalEntry]) -> Vec<Finding> {
    // `EntryId` は `Ord` を実装しないので、比較には内部表現の `u128` を使う。
    let ids: BTreeSet<u128> = entries.iter().map(|e| e.id().as_u128()).collect();
    entries
        .iter()
        .filter_map(|entry| {
            let target = entry.reverses()?;
            if ids.contains(&target.as_u128()) {
                return None;
            }
            Some(Finding {
                kind: FindingKind::DanglingReversal,
                // 期間外の仕訳を訂正した赤伝はこれに当たる。**異常とは限らない**
                // ので、その可能性を文言に含める。
                detail: format!(
                    "仕訳番号 {} の赤伝が訂正している仕訳が、この期間の帳簿にありません。\
                     前年度の仕訳を訂正した場合はこれで正しい可能性があります。\
                     get_entry で訂正元を確認してください",
                    entry.entry_no().as_u32()
                ),
            })
        })
        .collect()
}

/// 同じ仕訳番号が2つ以上ないか。
///
/// DB には `UNIQUE (fiscal_year, entry_no)` があるので通常は起こらないが、
/// **制約が効いていることを帳簿の側から確かめる**意味がある（マイグレーションの
/// 適用漏れ、別経路での投入）。
fn check_entry_numbers(entries: &[kaikei_core::JournalEntry]) -> Vec<Finding> {
    let mut seen: BTreeMap<u32, usize> = BTreeMap::new();
    for entry in entries {
        *seen.entry(entry.entry_no().as_u32()).or_insert(0) += 1;
    }
    seen.into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(number, count)| Finding {
            kind: FindingKind::DuplicateEntryNumber,
            detail: format!(
                "仕訳番号 {number} が {count} 件の仕訳に付いています。\
                 会計年度ごとに一意であるべき番号です"
            ),
        })
        .collect()
}

/// ドメインモデルと read model の残高を突き合わせる。
///
/// **同じ帳簿から出た2つの数字が違うなら、どちらかにバグがある。**
/// どちらが正しいかはここでは決めない——決められないからこそ、両方の値を
/// 出して人間に返す。
fn compare_balances(domain: &TrialBalance, read_model: &TrialBalanceView) -> Vec<Finding> {
    let domain_balances: BTreeMap<AccountCode, Money> = domain
        .rows()
        .iter()
        .map(|row| (row.account.clone(), row.balance))
        .collect();
    let read_balances: BTreeMap<AccountCode, Money> = read_model
        .rows()
        .iter()
        .map(|row| (row.account.clone(), row.balance))
        .collect();

    let mut findings = Vec::new();

    // 科目の集合が違う（片方にしか現れない科目がある）。
    let domain_only: Vec<&AccountCode> = domain_balances
        .keys()
        .filter(|code| !read_balances.contains_key(*code))
        .collect();
    let read_only: Vec<&AccountCode> = read_balances
        .keys()
        .filter(|code| !domain_balances.contains_key(*code))
        .collect();
    if !domain_only.is_empty() || !read_only.is_empty() {
        findings.push(Finding {
            kind: FindingKind::AccountSetMismatch,
            detail: format!(
                "集計対象の科目が2つの経路で食い違います。\
                 仕訳から集計したときだけ現れる科目: {:?} / \
                 SQL で集計したときだけ現れる科目: {:?}",
                domain_only.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
                read_only.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
            ),
        });
    }

    // 両方にある科目の残高を比べる。
    for (code, domain_balance) in &domain_balances {
        let Some(read_balance) = read_balances.get(code) else {
            continue; // 上で報告済み
        };
        if domain_balance != read_balance {
            findings.push(Finding {
                kind: FindingKind::BalanceMismatch,
                detail: format!(
                    "科目 {} の残高が2つの経路で食い違います。\
                     仕訳から集計: {} / SQL で集計: {}。\
                     どちらが正しいかはこの検査では判定できません",
                    code.as_str(),
                    domain_balance.to_display_string(),
                    read_balance.to_display_string(),
                ),
            });
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{fixed_clock, sample_chart_with_tax_account, AllOpen};
    use kaikei_core::{
        AccountingDate, Currency, EntryId, EntryNumber, JournalEntry, JournalLine, NewEntry, Side,
        TagSet,
    };

    fn yen(amount: i128) -> Money {
        Money::from_minor(amount, Currency::JPY)
    }

    fn line(account: &str, side: Side, amount: i128) -> JournalLine {
        JournalLine::new(
            AccountCode::parse(account).unwrap(),
            side,
            yen(amount),
            TagSet::new(),
            None,
        )
        .unwrap()
    }

    fn entry(id: u128, no: u32, lines: Vec<JournalLine>) -> JournalEntry {
        JournalEntry::new(
            NewEntry {
                id: EntryId::new(id),
                entry_no: EntryNumber::new(no),
                entry_date: AccountingDate::new(2026, 6, 1).unwrap(),
                description: "テスト".to_string(),
                lines,
                document_refs: Vec::new(),
            },
            &FiscalYear::calendar_year(2026),
            &sample_chart_with_tax_account(),
            &TagSchema::empty(),
            &AllOpen,
            &fixed_clock(),
        )
        .unwrap()
    }

    fn balanced(id: u128, no: u32, amount: i128) -> JournalEntry {
        entry(
            id,
            no,
            vec![
                line("100", Side::Debit, amount),
                line("500", Side::Credit, amount),
            ],
        )
    }

    // VF-1: 同じ仕訳番号が2件あれば報告する。
    #[test]
    fn duplicate_entry_numbers_are_reported() {
        let entries = [balanced(1, 7, 1_000), balanced(2, 7, 2_000)];

        let findings = check_entry_numbers(&entries);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, FindingKind::DuplicateEntryNumber);
        assert!(findings[0].detail.contains("7"), "{}", findings[0].detail);
    }

    #[test]
    fn distinct_entry_numbers_are_clean() {
        let entries = [balanced(1, 1, 1_000), balanced(2, 2, 2_000)];
        assert!(check_entry_numbers(&entries).is_empty());
    }

    // VF-2: 赤伝の訂正元が帳簿に無ければ報告する。ただし前年度の訂正で
    //       あれば正常なので、文言でそれを伝える。
    #[test]
    fn a_reversal_pointing_outside_the_period_is_reported_without_calling_it_wrong() {
        let original = balanced(1, 1, 5_000);
        let reversal = original
            .reverse(
                EntryId::new(2),
                EntryNumber::new(2),
                AccountingDate::new(2026, 6, 2).unwrap(),
                "訂正".to_string(),
                &FiscalYear::calendar_year(2026),
                &sample_chart_with_tax_account(),
                &TagSchema::empty(),
                &AllOpen,
                &fixed_clock(),
            )
            .unwrap();

        // 原仕訳を含めなければ、赤伝の参照先が見つからない。
        let findings = check_reversals(std::slice::from_ref(&reversal));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, FindingKind::DanglingReversal);
        assert!(
            findings[0].detail.contains("前年度"),
            "異常と断定しないこと: {}",
            findings[0].detail
        );

        // 原仕訳を含めれば異常なし。
        assert!(check_reversals(&[original, reversal]).is_empty());
    }

    // VF-3: ★本命★ 2つの経路の残高が食い違えば報告する。
    #[test]
    fn a_balance_mismatch_between_the_two_paths_is_reported_with_both_values() {
        let entries = [balanced(1, 1, 110_000)];
        let chart = sample_chart_with_tax_account();
        let domain =
            TrialBalance::from_entries(entries.iter(), &chart, &TagSchema::empty(), &[]).unwrap();

        // read model 側がわざと違う値を返した状況を作る。
        let read_model = TrialBalanceView::new(
            vec![
                crate::view::BalanceRowView {
                    account: AccountCode::parse("100").unwrap(),
                    account_type: kaikei_core::AccountType::Asset,
                    group: crate::view::GroupKeyView::default(),
                    debit_total: yen(110_000),
                    credit_total: yen(0),
                    balance: yen(999), // ← 食い違い
                },
                crate::view::BalanceRowView {
                    account: AccountCode::parse("500").unwrap(),
                    account_type: kaikei_core::AccountType::Revenue,
                    group: crate::view::GroupKeyView::default(),
                    debit_total: yen(0),
                    credit_total: yen(110_000),
                    balance: yen(110_000),
                },
            ],
            Currency::JPY,
        );

        let findings = compare_balances(&domain, &read_model);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, FindingKind::BalanceMismatch);
        // 両方の値を出す（どちらが正しいかは決めない）。
        assert!(findings[0].detail.contains("999"), "{}", findings[0].detail);
        assert!(
            findings[0].detail.contains("110,000") || findings[0].detail.contains("110000"),
            "{}",
            findings[0].detail
        );
        assert!(
            findings[0].detail.contains("判定できません"),
            "どちらが正しいか断定しないこと: {}",
            findings[0].detail
        );
    }

    // VF-4: 片方にしか現れない科目も報告する。
    #[test]
    fn an_account_present_in_only_one_path_is_reported() {
        let entries = [balanced(1, 1, 1_000)];
        let chart = sample_chart_with_tax_account();
        let domain =
            TrialBalance::from_entries(entries.iter(), &chart, &TagSchema::empty(), &[]).unwrap();

        // read model 側に科目が1つしか無い。
        let read_model = TrialBalanceView::new(
            vec![crate::view::BalanceRowView {
                account: AccountCode::parse("100").unwrap(),
                account_type: kaikei_core::AccountType::Asset,
                group: crate::view::GroupKeyView::default(),
                debit_total: yen(1_000),
                credit_total: yen(0),
                balance: yen(1_000),
            }],
            Currency::JPY,
        );

        let findings = compare_balances(&domain, &read_model);

        assert!(findings
            .iter()
            .any(|f| f.kind == FindingKind::AccountSetMismatch));
        assert!(
            findings[0].detail.contains("500"),
            "食い違った科目を名指しすること: {}",
            findings[0].detail
        );
    }

    // 一致していれば何も報告しない。
    #[test]
    fn matching_paths_produce_no_findings() {
        let entries = [balanced(1, 1, 1_000)];
        let chart = sample_chart_with_tax_account();
        let domain =
            TrialBalance::from_entries(entries.iter(), &chart, &TagSchema::empty(), &[]).unwrap();

        let read_model = TrialBalanceView::new(
            vec![
                crate::view::BalanceRowView {
                    account: AccountCode::parse("100").unwrap(),
                    account_type: kaikei_core::AccountType::Asset,
                    group: crate::view::GroupKeyView::default(),
                    debit_total: yen(1_000),
                    credit_total: yen(0),
                    balance: yen(1_000),
                },
                crate::view::BalanceRowView {
                    account: AccountCode::parse("500").unwrap(),
                    account_type: kaikei_core::AccountType::Revenue,
                    group: crate::view::GroupKeyView::default(),
                    debit_total: yen(0),
                    credit_total: yen(1_000),
                    balance: yen(1_000),
                },
            ],
            Currency::JPY,
        );

        assert!(compare_balances(&domain, &read_model).is_empty());
    }

    // 種別の機械可読名が重複しない（応答で使う語彙）。
    #[test]
    fn finding_kinds_have_distinct_codes() {
        let kinds = [
            FindingKind::BalanceMismatch,
            FindingKind::AccountSetMismatch,
            FindingKind::DanglingReversal,
            FindingKind::DuplicateEntryNumber,
        ];
        let codes: BTreeSet<&str> = kinds.iter().map(FindingKind::as_code).collect();
        assert_eq!(codes.len(), kinds.len());
    }
}
