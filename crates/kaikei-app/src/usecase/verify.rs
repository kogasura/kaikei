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
    /// 取引日・摘要・明細がまったく同じ仕訳が複数ある（二重計上の疑い）。
    ///
    /// **誤りとは言わない。** 同じ日に同額の交通費が2件ある、といった正当な
    /// 重複は普通にある。一方で、同じ取引を2回取り込むと帳簿が静かに膨らみ、
    /// 決算まで気づけない。疑いとして知らせ、判断は人間に返す。
    SuspectedDuplicate,
    /// 毎月同じ日に同じ額で繰り返される支出が、月によって違う科目に
    /// 入っている（科目の当て間違いの疑い）。
    ///
    /// **所得は変わらないが、決算書の欄が変わる。** 同じサブスクリプションが
    /// ある月は通信費、ある月は新聞図書費、という状態は帳簿の質の問題であり、
    /// 税務調査で理由を問われうる。
    ///
    /// **誤りとは言わない。** 同じ額の別の取引が偶然並ぶことはある。
    InconsistentAccount,
    /// 同じ費用科目の中で、消費税の区分が割れている。
    ///
    /// **こちらは消費税額に効く。** 科目の割れ（[`Self::InconsistentAccount`]）
    /// は所得を動かさないが、税区分の割れは仕入税額控除の額を変える。
    ///
    /// **誤りとは言わない。** 同じ科目に課税と非課税が混ざるのは正当にある
    /// （国内と海外の旅費、課税と非課税の手数料など）。
    InconsistentTaxCategory,
}

impl FindingKind {
    /// 機械可読名。
    pub fn as_code(&self) -> &'static str {
        match self {
            FindingKind::BalanceMismatch => "balance_mismatch",
            FindingKind::InconsistentAccount => "inconsistent_account",
            FindingKind::InconsistentTaxCategory => "inconsistent_tax_category",
            FindingKind::AccountSetMismatch => "account_set_mismatch",
            FindingKind::DanglingReversal => "dangling_reversal",
            FindingKind::DuplicateEntryNumber => "duplicate_entry_number",
            FindingKind::SuspectedDuplicate => "suspected_duplicate",
        }
    }

    /// 「誤り」ではなく「確認する価値がある」だけの種別か。
    ///
    /// **この区別が無いと検査が使い物にならない。** 正当な重複（同じ日に
    /// 同額の交通費が2件、など）は普通にあるので、疑いを不整合と同じ扱いに
    /// すると、正しい帳簿でも検査が失敗する。失敗が当たり前になると、
    /// 本当の不整合を見落とす。
    ///
    /// # 種別を足したら、ここも直す
    ///
    /// **実際に忘れた。** `InconsistentAccount` と `InconsistentTaxCategory`
    /// を足したとき（D-120 / D-121）にここを更新せず、**実帳簿の `verify` が
    /// 終了コード1で失敗するようになっていた**——どちらの doc にも
    /// 「誤りとは限らない」と書いておきながらである。
    ///
    /// 網羅的な `match` にして、種別を足したらコンパイルが通らないようにした。
    /// **`matches!` は足し忘れを黙って通す。**
    pub fn is_suspicion(&self) -> bool {
        match self {
            // 帳簿が内部で食い違っているもの。**直すまで申告に進めない。**
            FindingKind::BalanceMismatch
            | FindingKind::AccountSetMismatch
            | FindingKind::DanglingReversal
            | FindingKind::DuplicateEntryNumber => false,
            // 誤りとは限らないもの。**正当な形が普通にある。**
            FindingKind::SuspectedDuplicate
            | FindingKind::InconsistentAccount
            | FindingKind::InconsistentTaxCategory => true,
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
    /// 重複の疑いを科目ごとにまとめたもの。
    ///
    /// **件数だけでは、所得に効くものと効かないものが混ざる。**
    /// 詳しくは [`DuplicateSummary`] を参照。
    pub duplicate_summary: DuplicateSummary,
}

impl VerifyOutput {
    /// 不整合が1件も無いか。
    pub fn is_clean(&self) -> bool {
        self.findings
            .iter()
            .all(|finding| finding.kind.is_suspicion())
    }

    /// 定かな不整合（帳簿が内部で食い違っているもの）。
    pub fn inconsistencies(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|finding| !finding.kind.is_suspicion())
    }

    /// 疑い（誤りとは限らないが、確認する価値があるもの）。
    pub fn suspicions(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|finding| finding.kind.is_suspicion())
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
    findings.extend(check_suspected_duplicates(&entries));
    let duplicate_summary = summarize_suspected_duplicates(&entries, &chart);
    findings.extend(check_inconsistent_accounts(&entries, &chart));
    findings.extend(check_inconsistent_tax_categories(&entries, &chart));

    // 2. ★2つの経路で計算した試算表を突き合わせる★
    let domain = TrialBalance::from_entries(entries.iter(), &chart, tag_schema, &[])?;
    let rows = query.trial_balance(from, to, &[]).await?;
    // 通貨は行から推論せず、ドメイン側と同じ帳簿通貨を使う（`view.rs` の doc）。
    // ここでは残高の値だけを比べるので、包むのは行を扱いやすくするため。
    let read_model = TrialBalanceView::new(rows, domain_currency(&domain));
    findings.extend(compare_balances(&domain, &read_model));

    Ok(VerifyOutput {
        duplicate_summary,
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

/// 二重計上の疑いを拾う。
///
/// 取引日・摘要・明細（科目・貸借・金額）がまったく同じ仕訳が複数あれば、
/// 同じ取引を2回入れた疑いがある。**外部データを繰り返し取り込む運用では
/// 実際に起きる**（この帳簿でも一度起きた）。
///
/// # 誤りとは言わない
///
/// 同じ日に同額の交通費が2件、といった正当な重複はある。**断定すると、
/// 正しい帳簿に「誤り」と言うことになる。** 疑いとして件数と内容を出し、
/// 判断は人間に返す。
///
/// # 赤伝とその原仕訳は重複ではない
///
/// 逆仕訳は貸借が逆なので明細の並びが一致せず、この検査には引っかからない。
/// 念のため、訂正（`is_reversal`）は比較から外す。
/// 繰り返しと見なすのに要る回数。
///
/// 2回では偶然と区別できない。3回同じ額・同じ日に出るなら定期的な支出である
/// 可能性が高い。
const RECURRING_TIMES: usize = 3;

/// 「同じ日」と見なす幅（日にちの種類の数）。
///
/// 引落日は土日祝でずれるので1日に固定しない。ただし3種類以上になると
/// 「たまたま同額」を拾い始める（実帳簿で試したところ、この幅を広げると
/// 1,000円 の交通費・新聞図書費・会議費が混ざった）。
const RECURRING_DAY_KINDS: usize = 2;

/// 毎月同じ日に同じ額で繰り返される支出が、違う科目に入っていないか。
///
/// # 実際に見つけたもの
///
/// 実帳簿（2026年）で2件見つかった。
///
/// | 取引 | 額 | 割れ方 |
/// |---|---|---|
/// | YouTube Premium（毎月2日） | 2,280 | 1・2・3・5月は通信費、4・6・7・8月は新聞図書費 |
/// | noteプレミアム（毎月1日） | 1,980 | 3月は通信費、2・4月は新聞図書費 |
///
/// **手で SQL を書かないと見つからなかった。**
///
/// # 所得は変わらない
///
/// どちらも経費なので所得は動かない。変わるのは**決算書のどの欄に載るか**
/// である。同じサブスクリプションが月によって違う欄に載る状態は帳簿の質の
/// 問題であり、税務調査で理由を問われうる。
///
/// # 条件を絞る理由
///
/// 「同じ額で科目が違う」だけでは足りない。実帳簿で試したところ、
/// 1,000円 の支出が旅費交通費・新聞図書費・会議費の3科目に分かれて拾われた
/// ——**別々の取引が偶然同じ額だっただけ**である。
///
/// 「**毎月・ほぼ同じ日**」を足すと、この誤検出が消えて狙った2件だけが
/// 残った。定期的な支出（サブスクリプション・家賃・保険料）は日付が揃う。
///
/// # 費用の科目だけを見る
///
/// 資産・負債・資本の科目は対象外。同じ額の入出金が違う科目に入るのは
/// 普通である（振替・立替・カードの引落しなど）。**絞る前は 2,280円 の
/// 指摘に 325 未払金（カードの引落し）が混ざっていた。**
fn check_inconsistent_accounts(
    entries: &[kaikei_core::JournalEntry],
    chart: &kaikei_core::ChartOfAccounts,
) -> Vec<Finding> {
    use std::collections::{BTreeMap, BTreeSet};

    struct Group {
        times: usize,
        months: BTreeSet<String>,
        days: BTreeSet<u8>,
        accounts: BTreeSet<String>,
    }

    let mut by_amount: BTreeMap<i128, Group> = BTreeMap::new();
    for entry in entries {
        for line in entry.lines() {
            if line.side() != kaikei_core::Side::Debit {
                continue;
            }
            // **費用の科目だけを見る。** 資産・負債・資本は対象外——同じ額の
            // 入出金が違う科目に入るのは普通である（振替・立替・カードの
            // 引落しなど）。実際、絞る前は 2,280円 の指摘に 325 未払金
            // （カード引落し）が混ざっていた。
            let is_expense = chart
                .get(line.account())
                .is_some_and(|def| def.account_type == kaikei_core::AccountType::Expense);
            if !is_expense {
                continue;
            }
            let group = by_amount.entry(line.amount().minor()).or_insert(Group {
                times: 0,
                months: BTreeSet::new(),
                days: BTreeSet::new(),
                accounts: BTreeSet::new(),
            });
            group.times += 1;
            group.months.insert(format!(
                "{:04}-{:02}",
                entry.entry_date().year(),
                entry.entry_date().month()
            ));
            group.days.insert(entry.entry_date().day());
            group.accounts.insert(line.account().as_str().to_string());
        }
    }

    let mut findings: Vec<(i128, Finding)> = by_amount
        .into_iter()
        .filter(|(_, group)| {
            group.times >= RECURRING_TIMES
                && group.months.len() >= RECURRING_TIMES
                && group.days.len() <= RECURRING_DAY_KINDS
                && group.accounts.len() > 1
        })
        .map(|(amount, group)| {
            (
                amount * group.times as i128,
                Finding {
                    kind: FindingKind::InconsistentAccount,
                    detail: format!(
                        concat!(
                            "毎月ほぼ同じ日に {} 円の支出が {} 件ありますが、",
                            "科目が {} に分かれています（{}）。",
                            "同じ取引なら科目を揃えてください。",
                            "別々の取引が偶然同じ額なら、そのままで構いません"
                        ),
                        amount,
                        group.times,
                        group.accounts.len(),
                        group
                            .accounts
                            .iter()
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(" / ")
                    ),
                },
            )
        })
        .collect();

    // 金額の大きい順（`check_suspected_duplicates` と同じ理由）。
    findings.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.detail.cmp(&b.1.detail)));
    findings.into_iter().map(|(_, finding)| finding).collect()
}

/// 同じ費用科目の中で、消費税の区分が割れていないか。
///
/// # 科目の割れより重い
///
/// [`check_inconsistent_accounts`] が見るのは所得を動かさない誤りだが、
/// **税区分の割れは仕入税額控除の額を変える。**
///
/// # 実際に見つけたもの
///
/// 実帳簿（2026年）の地代家賃6件のうち、**5件が課税仕入10%、1件が非課税**
/// だった。住宅の貸付けは非課税（消費税法別表第二13号）なので、自宅の家賃を
/// 課税仕入として控除していれば控除のとりすぎになる。逆に事務所であれば
/// 課税が正しく、非課税の1件が誤りになる。**どちらが正しいかはこのソフトには
/// 分からない**——契約の中身を知らないからである。
///
/// # 少数派を「誤り」と呼ばない
///
/// 上の例では、5件と1件のどちらが誤りかが決まらない。**多数決で決めない。**
/// 件数を並べて、判断は人間に返す。
///
/// # 閾値を足していない
///
/// 「少数派が全体の N% 未満なら」のような絞りを入れたくなるが、**実帳簿で
/// 誤検出が出ていない**（割れている科目は1つだけ）ので、確かめようのない
/// 条件を先回りで足さない。旅費交通費のように課税と対象外が正当に混ざる
/// 科目が出てきたら、そのとき実物を見て決めること。
fn check_inconsistent_tax_categories(
    entries: &[kaikei_core::JournalEntry],
    chart: &kaikei_core::ChartOfAccounts,
) -> Vec<Finding> {
    use std::collections::BTreeMap;

    let Ok(tax_key) = kaikei_core::TagKey::parse("tax_category") else {
        return Vec::new();
    };

    // 科目 → 税区分 → 件数
    let mut by_account: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    for entry in entries {
        for line in entry.lines() {
            if line.side() != kaikei_core::Side::Debit {
                continue;
            }
            let is_expense = chart
                .get(line.account())
                .is_some_and(|def| def.account_type == kaikei_core::AccountType::Expense);
            if !is_expense {
                continue;
            }
            let Some(kaikei_core::TagValue::Code(code)) = line.tags().get(&tax_key) else {
                continue;
            };
            *by_account
                .entry(line.account().as_str().to_string())
                .or_default()
                .entry(code.clone())
                .or_insert(0) += 1;
        }
    }

    by_account
        .into_iter()
        .filter(|(_, categories)| {
            categories.len() > 1 && categories.values().sum::<usize>() >= RECURRING_TIMES
        })
        .map(|(account, categories)| Finding {
            kind: FindingKind::InconsistentTaxCategory,
            detail: format!(
                concat!(
                    "科目 {} の消費税の区分が {} 種類に分かれています（{}）。",
                    "**仕入税額控除の額が変わります。** ",
                    "同じ性質の取引なら区分を揃えてください。",
                    "課税と非課税が正当に混ざる科目であれば、そのままで構いません"
                ),
                account,
                categories.len(),
                categories
                    .iter()
                    .map(|(code, times)| format!("{code} {times}件"))
                    .collect::<Vec<_>>()
                    .join(" / ")
            ),
        })
        .collect()
}

/// 重複を見分けるための指紋。取引日・摘要・明細（科目・貸借・金額）で作る。
///
/// **明細は並び順が違っても同じ仕訳なので、揃えてから比べる。**
fn duplicate_fingerprint(entry: &kaikei_core::JournalEntry) -> String {
    let mut lines: Vec<String> = entry
        .lines()
        .iter()
        .map(|line| {
            format!(
                "{}/{:?}/{}",
                line.account().as_str(),
                line.side(),
                line.amount().minor()
            )
        })
        .collect();
    lines.sort_unstable();
    format!(
        "{}|{}|{}",
        entry.entry_date().to_iso_string(),
        entry.description(),
        lines.join(",")
    )
}

/// 重複の疑いを科目ごとにまとめたもの。
///
/// # なぜ件数だけでは足りないのか
///
/// 実帳簿で 62 件と言われても手の付けようがない。**しかも件数は、所得に
/// 効くものと効かないものを混ぜている。** 実際の内訳はこうだった。
///
/// | 科目 | 余分な額 | 所得に効くか |
/// |---|---:|---|
/// | 事業主貸 | 1,030,000円 | **効かない**（引出し） |
/// | 旅費交通費 | 12,238円 | 効く |
/// | 支払手数料 | 725円 | 効く |
///
/// 目立つのは 103万円だが、**全部が誤りでも所得は1円も動かない**。逆に
/// 所得に効くのは 12,963円 しかない。この差は件数を見ても分からない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateSummary {
    /// 科目ごとの内訳。余分な額の大きい順。
    pub by_account: Vec<DuplicateGroup>,
}

impl DuplicateSummary {
    /// 全部が誤りだったときに所得が動きうる額（費用・収益のぶんだけ）。
    #[must_use]
    pub fn at_risk_affecting_income(&self) -> i128 {
        self.by_account
            .iter()
            .filter(|group| group.affects_income)
            .map(|group| group.at_risk_minor)
            .sum()
    }

    /// 内訳が1件も無いか。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_account.is_empty()
    }
}

/// [`DuplicateSummary`] の1科目分。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateGroup {
    /// 科目コード。
    pub account: String,
    /// 科目名。
    pub name: String,
    /// 費用または収益か（＝所得に効くか）。
    pub affects_income: bool,
    /// この科目に触れている重複の組数。
    pub groups: usize,
    /// 余分な分（2件なら1件、3件なら2件）の合計。
    pub at_risk_minor: i128,
}

/// 重複の疑いを科目ごとにまとめる。
///
/// **金額は明細ごとに割り当てる。** 1つの仕訳に借方が複数あるとき、
/// 全額をどれか1つの科目に寄せると内訳が嘘になる。
fn summarize_suspected_duplicates(
    entries: &[kaikei_core::JournalEntry],
    chart: &kaikei_core::ChartOfAccounts,
) -> DuplicateSummary {
    use std::collections::{BTreeMap, BTreeSet};

    let mut seen: BTreeMap<String, Vec<&kaikei_core::JournalEntry>> = BTreeMap::new();
    for entry in entries.iter().filter(|entry| !entry.is_reversal()) {
        seen.entry(duplicate_fingerprint(entry))
            .or_default()
            .push(entry);
    }

    struct Acc {
        groups: BTreeSet<String>,
        at_risk: i128,
    }
    let mut by_account: BTreeMap<String, Acc> = BTreeMap::new();
    for (key, group) in &seen {
        if group.len() < 2 {
            continue;
        }
        // 余分な分は (件数 − 1) 組ぶん。
        let extra = group.len() as i128 - 1;
        for line in group[0].lines() {
            if line.side() != kaikei_core::Side::Debit {
                continue;
            }
            let slot = by_account
                .entry(line.account().as_str().to_string())
                .or_insert(Acc {
                    groups: BTreeSet::new(),
                    at_risk: 0,
                });
            slot.groups.insert(key.clone());
            slot.at_risk += line.amount().minor() * extra;
        }
    }

    let mut rows: Vec<DuplicateGroup> = by_account
        .into_iter()
        .map(|(code, acc)| {
            let def = kaikei_core::AccountCode::parse(&code)
                .ok()
                .and_then(|parsed| chart.get(&parsed).cloned());
            DuplicateGroup {
                name: def
                    .as_ref()
                    .map_or_else(|| code.clone(), |def| def.name.clone()),
                affects_income: def.as_ref().is_some_and(|def| {
                    matches!(
                        def.account_type,
                        kaikei_core::AccountType::Expense | kaikei_core::AccountType::Revenue
                    )
                }),
                account: code,
                groups: acc.groups.len(),
                at_risk_minor: acc.at_risk,
            }
        })
        .collect();
    // 余分な額の大きい順。同額なら科目コード順で安定させる。
    rows.sort_by(|a, b| {
        b.at_risk_minor
            .cmp(&a.at_risk_minor)
            .then_with(|| a.account.cmp(&b.account))
    });
    DuplicateSummary { by_account: rows }
}

fn check_suspected_duplicates(entries: &[kaikei_core::JournalEntry]) -> Vec<Finding> {
    let fingerprint = duplicate_fingerprint;

    // 金額も持つ。**金額が無いと 145円 の重複と 300,000円 の重複が
    // 同じに見える。** 実際に freee 側で見つかった二重計上は
    // 事業主貸 300,000円 のような大きな額で、同じ日に同額が並ぶ交通費
    // （正当な重複）に埋もれると気づけない。
    let mut seen: BTreeMap<String, (Vec<u32>, i128)> = BTreeMap::new();
    for entry in entries.iter().filter(|entry| !entry.is_reversal()) {
        let amount = entry.debit_total().minor();
        let slot = seen
            .entry(fingerprint(entry))
            .or_insert((Vec::new(), amount));
        slot.0.push(entry.entry_no().as_u32());
    }

    let mut findings: Vec<(i128, Finding)> = seen
        .into_iter()
        .filter(|(_, (numbers, _))| numbers.len() > 1)
        .map(|(key, (numbers, amount))| {
            let date = key.split('|').next().unwrap_or("");
            let description = key.split('|').nth(1).unwrap_or("");
            // 余分な分（2件なら1件、3件なら2件）が、疑わしい金額である。
            let at_risk = amount * (numbers.len() as i128 - 1);
            (
                at_risk,
                Finding {
                    kind: FindingKind::SuspectedDuplicate,
                    detail: format!(
                        concat!(
                            "取引日・摘要・明細が同じ仕訳が {} 件あります",
                            "（{}「{}」/ 1件あたり {} 円 / 仕訳番号 {}）。",
                            "同じ取引を2回入れていないか確認してください。",
                            "正当な重複であればそのままで構いません"
                        ),
                        numbers.len(),
                        date,
                        description,
                        amount,
                        numbers
                            .iter()
                            .map(|n| n.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                },
            )
        })
        .collect();

    // **金額の大きい順に並べる。** 呼び出し側は先頭の数件しか出さないので、
    // 並び順がそのまま「何を見せるか」になる。日付順だと 145円 の重複が
    // 先に出て、300,000円 の重複が「ほか N 件」に隠れる。
    findings.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.detail.cmp(&b.1.detail)));
    findings.into_iter().map(|(_, finding)| finding).collect()
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
    /// 摘要と日付を指定して仕訳を作る（重複の検査に使う）。
    fn entry_on(
        id: u128,
        no: u32,
        day: u8,
        description: &str,
        lines: Vec<JournalLine>,
    ) -> JournalEntry {
        JournalEntry::new(
            NewEntry {
                id: EntryId::new(id),
                entry_no: EntryNumber::new(no),
                entry_date: AccountingDate::new(2026, 6, day).unwrap(),
                description: description.to_string(),
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

    fn pair(amount: i128) -> Vec<JournalLine> {
        vec![
            line("100", Side::Debit, amount),
            line("500", Side::Credit, amount),
        ]
    }

    // **本命。** 同じ取引を2回入れたら疑いとして知らせる。
    //
    // この帳簿で実際に起きた（外部データを取り込む前に手で入れた仕訳が、
    // 取り込んだ分と重複した）。金額も貸借も正しいので、残高の突き合わせ
    // では見つからない。
    #[test]
    fn the_same_transaction_entered_twice_is_reported_as_suspected() {
        let entries = vec![
            entry_on(1, 1, 15, "ビーテック 5月分 請求", pair(550_000)),
            entry_on(2, 2, 15, "ビーテック 5月分 請求", pair(550_000)),
        ];

        let findings = check_suspected_duplicates(&entries);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, FindingKind::SuspectedDuplicate);
        assert!(
            findings[0].detail.contains("2 件"),
            "{}",
            findings[0].detail
        );
        assert!(
            findings[0].detail.contains("1, 2"),
            "どの仕訳かを言うこと: {}",
            findings[0].detail
        );
        // **誤りと断定しない。** 正当な重複もある。
        assert!(
            findings[0]
                .detail
                .contains("正当な重複であればそのままで構いません"),
            "{}",
            findings[0].detail
        );
    }

    // **本命。** 金額の大きい順に並べる。
    //
    // 呼び出し側は先頭の数件しか出さないので、並び順がそのまま
    // 「何を見せるか」になる。日付順だと 145円 の重複が先に出て、
    // 300,000円 の重複が「ほか N 件」に隠れる。実際に freee 側で見つかった
    // 二重計上は 事業主貸 300,000円 のような大きな額だった。
    #[test]
    fn the_biggest_amounts_come_first() {
        let entries = vec![
            // 1月: 145円 の重複（正当な手数料の重複でも起こりうる）
            entry_on(1, 1, 10, "振込手数料", pair(145)),
            entry_on(2, 1, 10, "振込手数料", pair(145)),
            // 3月: 300,000円 の重複（これが見たい）
            entry_on(3, 3, 24, "事業主貸", pair(300_000)),
            entry_on(4, 3, 24, "事業主貸", pair(300_000)),
            // 2月: 5,000円 の重複
            entry_on(5, 2, 1, "消耗品費", pair(5_000)),
            entry_on(6, 2, 1, "消耗品費", pair(5_000)),
        ];

        let findings = check_suspected_duplicates(&entries);

        assert_eq!(findings.len(), 3);
        assert!(
            findings[0].detail.contains("事業主貸"),
            "300,000円 が先頭に来ること: {}",
            findings[0].detail
        );
        assert!(
            findings[1].detail.contains("消耗品費"),
            "{}",
            findings[1].detail
        );
        assert!(
            findings[2].detail.contains("振込手数料"),
            "{}",
            findings[2].detail
        );
    }

    // **本命。** 金額を出す。
    //
    // 金額が無いと 145円 の重複と 300,000円 の重複が同じに見える。
    #[test]
    fn the_amount_is_stated() {
        let entries = vec![
            entry_on(1, 3, 24, "事業主貸", pair(300_000)),
            entry_on(2, 3, 24, "事業主貸", pair(300_000)),
        ];

        let findings = check_suspected_duplicates(&entries);

        assert!(
            findings[0].detail.contains("300000"),
            "1件あたりの金額を出すこと: {}",
            findings[0].detail
        );
    }

    // 3件以上あれば、余分な分で重み付けする。
    //
    // 1,000円が3件（余分2件＝2,000円）は、1,500円が2件（余分1件＝1,500円）より
    // 疑わしい金額が大きい。
    #[test]
    fn more_copies_weigh_more() {
        let entries = vec![
            entry_on(1, 1, 10, "A", pair(1_000)),
            entry_on(2, 1, 10, "A", pair(1_000)),
            entry_on(3, 1, 10, "A", pair(1_000)),
            entry_on(4, 2, 10, "B", pair(1_500)),
            entry_on(5, 2, 10, "B", pair(1_500)),
        ];

        let findings = check_suspected_duplicates(&entries);

        assert_eq!(findings.len(), 2);
        assert!(
            findings[0].detail.contains("「A」"),
            "余分2件×1,000円 が先: {}",
            findings[0].detail
        );
    }

    // 日付・摘要・金額のどれかが違えば疑わない。
    #[test]
    fn entries_that_differ_are_not_suspected() {
        let entries = vec![
            entry_on(1, 1, 15, "A", pair(1_000)),
            entry_on(2, 2, 16, "A", pair(1_000)), // 日付が違う
            entry_on(3, 3, 15, "B", pair(1_000)), // 摘要が違う
            entry_on(4, 4, 15, "A", pair(2_000)), // 金額が違う
        ];

        assert!(check_suspected_duplicates(&entries).is_empty());
    }

    // 明細の並び順が違うだけなら同じ仕訳として扱う（並べてから比べる）。
    #[test]
    fn the_order_of_lines_does_not_hide_a_duplicate() {
        let entries = vec![
            entry_on(
                1,
                1,
                15,
                "A",
                vec![
                    line("100", Side::Debit, 1_000),
                    line("500", Side::Credit, 1_000),
                ],
            ),
            entry_on(
                2,
                2,
                15,
                "A",
                vec![
                    line("500", Side::Credit, 1_000),
                    line("100", Side::Debit, 1_000),
                ],
            ),
        ];

        assert_eq!(check_suspected_duplicates(&entries).len(), 1);
    }

    // **疑いだけなら検査は失敗しない。** 正当な重複がある帳簿で検査が
    // 落ちるようになると、失敗が当たり前になって本当の不整合を見落とす。
    #[test]
    fn suspicions_alone_do_not_make_the_check_fail() {
        let output = VerifyOutput {
            duplicate_summary: DuplicateSummary {
                by_account: Vec::new(),
            },
            entry_count: 2,
            findings: vec![Finding {
                kind: FindingKind::SuspectedDuplicate,
                detail: "同じ日に同額の交通費".to_string(),
            }],
        };

        assert!(output.is_clean(), "疑いだけなら不整合とはしない");
        assert_eq!(output.suspicions().count(), 1);
        assert_eq!(output.inconsistencies().count(), 0);
    }

    // 定かな不整合が1件でもあれば失敗する。
    #[test]
    fn a_real_inconsistency_makes_the_check_fail() {
        let output = VerifyOutput {
            duplicate_summary: DuplicateSummary {
                by_account: Vec::new(),
            },
            entry_count: 2,
            findings: vec![
                Finding {
                    kind: FindingKind::SuspectedDuplicate,
                    detail: "疑い".to_string(),
                },
                Finding {
                    kind: FindingKind::DuplicateEntryNumber,
                    detail: "仕訳番号の重複".to_string(),
                },
            ],
        };

        assert!(!output.is_clean());
        assert_eq!(output.inconsistencies().count(), 1);
    }

    #[test]
    fn finding_kinds_have_distinct_codes() {
        // **種別を足したらここにも足す。** この一覧が網羅されていないと、
        // 新しい種別のコードが他と重複していても気づけない。
        let kinds = [
            FindingKind::BalanceMismatch,
            FindingKind::AccountSetMismatch,
            FindingKind::DanglingReversal,
            FindingKind::DuplicateEntryNumber,
            FindingKind::SuspectedDuplicate,
        ];
        let codes: BTreeSet<&str> = kinds.iter().map(FindingKind::as_code).collect();
        assert_eq!(codes.len(), kinds.len(), "コードが重複している");

        // 一覧が網羅されていることを、変換の全分岐で確かめる。**足し忘れると
        // 上の重複検査がすり抜ける。**
        for kind in &kinds {
            assert!(!kind.as_code().is_empty());
        }
        assert_eq!(
            kinds.len(),
            5,
            "FindingKind に種別を足したら、この一覧と件数も更新すること"
        );
    }

    // ---- 科目の一貫性（check_inconsistent_accounts） ----

    /// 費用の明細を持つ仕訳を、月と日を指定して作る。
    fn expense_on(
        id: u128,
        no: u32,
        month: u8,
        day: u8,
        account: &str,
        amount: i128,
    ) -> JournalEntry {
        JournalEntry::new(
            NewEntry {
                id: EntryId::new(id),
                entry_no: EntryNumber::new(no),
                entry_date: AccountingDate::new(2026, month, day).unwrap(),
                description: "サブスクリプション".to_string(),
                lines: vec![
                    line(account, Side::Debit, amount),
                    line("100", Side::Credit, amount),
                ],
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

    /// **本命。** 毎月同じ日の同額が違う科目に入っていれば拾う。
    ///
    /// 実帳簿の YouTube Premium がこれ（2,280円・毎月2日・通信費と新聞図書費に
    /// 4件ずつ）。**手で SQL を書かないと見つからなかった。**
    #[test]
    fn a_subscription_split_across_accounts_is_reported() {
        let entries = vec![
            expense_on(1, 1, 1, 2, "604", 2_280),
            expense_on(2, 2, 2, 2, "604", 2_280),
            expense_on(3, 3, 3, 2, "621", 2_280),
        ];

        let findings = check_inconsistent_accounts(&entries, &sample_chart_with_tax_account());

        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].kind, FindingKind::InconsistentAccount);
        assert!(findings[0].detail.contains("2280"), "{findings:?}");
        assert!(findings[0].detail.contains("604"), "{findings:?}");
        assert!(findings[0].detail.contains("621"), "{findings:?}");
    }

    /// 科目が揃っていれば言わない。
    #[test]
    fn a_subscription_on_one_account_is_quiet() {
        let entries = vec![
            expense_on(1, 1, 1, 2, "604", 2_280),
            expense_on(2, 2, 2, 2, "604", 2_280),
            expense_on(3, 3, 3, 2, "604", 2_280),
        ];
        assert!(check_inconsistent_accounts(&entries, &sample_chart_with_tax_account()).is_empty());
    }

    /// **本命。** 日付が散らばっていれば拾わない。
    ///
    /// 「同じ額で科目が違う」だけでは足りない。実帳簿で試したところ、
    /// 1,000円 の支出が旅費交通費・新聞図書費・会議費の3科目に分かれて拾われた
    /// ——**別々の取引が偶然同じ額だっただけ**である。
    #[test]
    fn the_same_amount_on_scattered_days_is_not_reported() {
        let entries = vec![
            expense_on(1, 1, 1, 5, "604", 1_000),
            expense_on(2, 2, 2, 17, "621", 1_000),
            expense_on(3, 3, 3, 28, "604", 1_000),
        ];
        assert!(
            check_inconsistent_accounts(&entries, &sample_chart_with_tax_account()).is_empty(),
            "日が3種類あるので定期的な支出ではない"
        );
    }

    /// 引落日は土日でずれるので、2種類までは同じ日と見なす。
    #[test]
    fn two_kinds_of_day_still_count_as_the_same_day() {
        let entries = vec![
            expense_on(1, 1, 1, 2, "604", 2_280),
            expense_on(2, 2, 2, 3, "604", 2_280),
            expense_on(3, 3, 3, 2, "621", 2_280),
        ];
        assert_eq!(
            check_inconsistent_accounts(&entries, &sample_chart_with_tax_account()).len(),
            1,
            "土日でずれた分を見逃さない"
        );
    }

    /// 2回では拾わない（偶然と区別できない）。
    #[test]
    fn twice_is_not_enough() {
        let entries = vec![
            expense_on(1, 1, 1, 2, "604", 2_280),
            expense_on(2, 2, 2, 2, "621", 2_280),
        ];
        assert!(check_inconsistent_accounts(&entries, &sample_chart_with_tax_account()).is_empty());
    }

    /// 同じ月に3回でも拾わない（毎月の支出ではない）。
    #[test]
    fn three_times_in_one_month_is_not_a_subscription() {
        let entries = vec![
            expense_on(1, 1, 6, 2, "604", 2_280),
            expense_on(2, 2, 6, 2, "621", 2_280),
            expense_on(3, 3, 6, 2, "604", 2_280),
        ];
        assert!(check_inconsistent_accounts(&entries, &sample_chart_with_tax_account()).is_empty());
    }

    /// **本命。** 費用でない科目は見ない。
    ///
    /// 絞る前は 2,280円 の指摘に 325 未払金（カードの引落し）が混ざっていた。
    /// 同じ額の入出金が違う科目に入るのは普通である。
    #[test]
    fn accounts_that_are_not_expenses_are_ignored() {
        // 100 は資産（現金）。借方に立つが費用ではない。
        let entries = vec![
            expense_on(1, 1, 1, 2, "100", 2_280),
            expense_on(2, 2, 2, 2, "100", 2_280),
            expense_on(3, 3, 3, 2, "604", 2_280),
        ];
        assert!(
            check_inconsistent_accounts(&entries, &sample_chart_with_tax_account()).is_empty(),
            "費用の科目は1つしか無い"
        );
    }

    /// 科目表に無いコードは見ない（落ちない）。
    #[test]
    fn an_unknown_account_code_is_skipped() {
        let entries = vec![
            expense_on(1, 1, 1, 2, "604", 2_280),
            expense_on(2, 2, 2, 2, "604", 2_280),
            expense_on(3, 3, 3, 2, "604", 2_280),
        ];
        let empty = kaikei_core::ChartOfAccounts::new(vec![]).unwrap();
        assert!(check_inconsistent_accounts(&entries, &empty).is_empty());
    }

    // ---- 税区分の一貫性（check_inconsistent_tax_categories） ----

    /// `tax_category` を受け付けるスキーマ。
    ///
    /// **`TagSchema::empty()` はタグを一切拒む。** 税区分の検査は税区分タグが
    /// 付いた明細を見るので、空のスキーマでは仕訳を作る時点で落ちる。
    fn schema_with_tax_category() -> TagSchema {
        TagSchema::new(vec![(
            kaikei_core::TagKey::parse("tax_category").unwrap(),
            kaikei_core::TagDef {
                value_type: kaikei_core::TagValueType::Code,
                aggregatable: true,
                required_for: Vec::new(),
            },
        )])
    }

    fn taxed_line(account: &str, amount: i128, tax_category: &str) -> JournalLine {
        let mut tags = TagSet::new();
        tags.insert(
            kaikei_core::TagKey::parse("tax_category").unwrap(),
            kaikei_core::TagValue::Code(tax_category.to_string()),
        );
        JournalLine::new(
            AccountCode::parse(account).unwrap(),
            Side::Debit,
            yen(amount),
            tags,
            None,
        )
        .unwrap()
    }

    fn taxed_entry(id: u128, no: u32, day: u8, account: &str, tax_category: &str) -> JournalEntry {
        JournalEntry::new(
            NewEntry {
                id: EntryId::new(id),
                entry_no: EntryNumber::new(no),
                entry_date: AccountingDate::new(2026, 6, day).unwrap(),
                description: "家賃".to_string(),
                lines: vec![
                    taxed_line(account, 100_000, tax_category),
                    line("100", Side::Credit, 100_000),
                ],
                document_refs: Vec::new(),
            },
            &FiscalYear::calendar_year(2026),
            &sample_chart_with_tax_account(),
            &schema_with_tax_category(),
            &AllOpen,
            &fixed_clock(),
        )
        .unwrap()
    }

    /// **本命。** 同じ費用科目の中で税区分が割れていれば知らせる。
    ///
    /// 実帳簿の地代家賃がこれ（6件のうち5件が課税仕入10%、1件が非課税）。
    /// **仕入税額控除の額が変わる。**
    #[test]
    fn a_split_tax_category_within_one_account_is_reported() {
        let entries = vec![
            taxed_entry(1, 1, 1, "604", "PURCHASE_10_QUALIFIED"),
            taxed_entry(2, 2, 2, "604", "PURCHASE_10_QUALIFIED"),
            taxed_entry(3, 3, 3, "604", "TAX_FREE"),
        ];

        let findings =
            check_inconsistent_tax_categories(&entries, &sample_chart_with_tax_account());

        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].kind, FindingKind::InconsistentTaxCategory);
        assert!(findings[0].detail.contains("604"), "{findings:?}");
        // **件数を両方出す。** どちらが誤りかは決められないので、
        // 多数派を「正しい」と読ませない。
        assert!(
            findings[0].detail.contains("PURCHASE_10_QUALIFIED 2件"),
            "{findings:?}"
        );
        assert!(findings[0].detail.contains("TAX_FREE 1件"), "{findings:?}");
    }

    /// 揃っていれば言わない。
    #[test]
    fn one_tax_category_per_account_is_quiet() {
        let entries = vec![
            taxed_entry(1, 1, 1, "604", "PURCHASE_10_QUALIFIED"),
            taxed_entry(2, 2, 2, "604", "PURCHASE_10_QUALIFIED"),
            taxed_entry(3, 3, 3, "604", "PURCHASE_10_QUALIFIED"),
        ];
        assert!(
            check_inconsistent_tax_categories(&entries, &sample_chart_with_tax_account())
                .is_empty()
        );
    }

    /// **本命。** 科目が違えば別々に見る（混ぜない）。
    #[test]
    fn different_accounts_are_judged_separately() {
        let entries = vec![
            taxed_entry(1, 1, 1, "604", "PURCHASE_10_QUALIFIED"),
            taxed_entry(2, 2, 2, "604", "PURCHASE_10_QUALIFIED"),
            taxed_entry(3, 3, 3, "604", "PURCHASE_10_QUALIFIED"),
            taxed_entry(4, 4, 4, "621", "TAX_FREE"),
            taxed_entry(5, 5, 5, "621", "TAX_FREE"),
            taxed_entry(6, 6, 6, "621", "TAX_FREE"),
        ];
        assert!(
            check_inconsistent_tax_categories(&entries, &sample_chart_with_tax_account())
                .is_empty(),
            "科目ごとには揃っている"
        );
    }

    /// 2件では拾わない（`check_inconsistent_accounts` と同じ閾値）。
    #[test]
    fn two_entries_are_not_enough_for_a_tax_split() {
        let entries = vec![
            taxed_entry(1, 1, 1, "604", "PURCHASE_10_QUALIFIED"),
            taxed_entry(2, 2, 2, "604", "TAX_FREE"),
        ];
        assert!(
            check_inconsistent_tax_categories(&entries, &sample_chart_with_tax_account())
                .is_empty()
        );
    }

    /// **本命。** 費用でない科目は見ない。
    #[test]
    fn non_expense_accounts_are_ignored_for_tax_categories() {
        let entries = vec![
            taxed_entry(1, 1, 1, "100", "PURCHASE_10_QUALIFIED"),
            taxed_entry(2, 2, 2, "100", "PURCHASE_10_QUALIFIED"),
            taxed_entry(3, 3, 3, "100", "TAX_FREE"),
        ];
        assert!(
            check_inconsistent_tax_categories(&entries, &sample_chart_with_tax_account())
                .is_empty()
        );
    }

    /// 税区分が付いていない明細は数えない。
    #[test]
    fn lines_without_a_tax_category_are_not_counted() {
        let entries = vec![
            taxed_entry(1, 1, 1, "604", "PURCHASE_10_QUALIFIED"),
            taxed_entry(2, 2, 2, "604", "TAX_FREE"),
            // タグ無しの明細を持つ仕訳。数に入れば3件になって拾われる。
            entry_on(
                3,
                3,
                3,
                "タグ無し",
                vec![
                    line("604", Side::Debit, 100_000),
                    line("100", Side::Credit, 100_000),
                ],
            ),
        ];
        assert!(
            check_inconsistent_tax_categories(&entries, &sample_chart_with_tax_account())
                .is_empty(),
            "タグ無しを数えると2件が3件になってしまう"
        );
    }

    // ---- 種別の分類（is_suspicion） ----

    /// **本命。** 「誤りとは限らない」種別は検査を失敗させない。
    ///
    /// **実際に忘れた。** `InconsistentAccount` と `InconsistentTaxCategory` を
    /// 足したときに `is_suspicion` を更新せず、実帳簿の `verify` が終了コード1で
    /// 失敗するようになっていた——どちらの doc にも「誤りとは限らない」と
    /// 書いておきながらである。
    ///
    /// **この形の抜けは、正しい帳簿でしか露見しない。** 不整合が別にある帳簿で
    /// 試すと、どちらにせよ失敗するので気づけない。
    #[test]
    fn advisory_findings_do_not_fail_the_check() {
        for kind in [
            FindingKind::SuspectedDuplicate,
            FindingKind::InconsistentAccount,
            FindingKind::InconsistentTaxCategory,
        ] {
            assert!(kind.is_suspicion(), "{kind:?} は疑いであるべき");
            let output = VerifyOutput {
                duplicate_summary: DuplicateSummary {
                    by_account: Vec::new(),
                },
                entry_count: 1,
                findings: vec![Finding {
                    kind,
                    detail: String::new(),
                }],
            };
            assert!(output.is_clean(), "{kind:?} だけで失敗しないこと");
        }
    }

    /// 帳簿が内部で食い違っている種別は検査を失敗させる。
    #[test]
    fn real_inconsistencies_fail_the_check() {
        for kind in [
            FindingKind::BalanceMismatch,
            FindingKind::AccountSetMismatch,
            FindingKind::DanglingReversal,
            FindingKind::DuplicateEntryNumber,
        ] {
            assert!(!kind.is_suspicion(), "{kind:?} は不整合であるべき");
            let output = VerifyOutput {
                duplicate_summary: DuplicateSummary {
                    by_account: Vec::new(),
                },
                entry_count: 1,
                findings: vec![Finding {
                    kind,
                    detail: String::new(),
                }],
            };
            assert!(!output.is_clean(), "{kind:?} で失敗すること");
        }
    }

    /// **本命。** すべての種別が、どちらかに分類されている。
    ///
    /// 上の2つのテストが数えた種別の合計が、`as_code` が知っている種別の数と
    /// 一致することを見る。**種別を足してテストに書き忘れたら、ここで落ちる。**
    #[test]
    fn every_kind_is_classified_in_the_tests_above() {
        let advisory = [
            FindingKind::SuspectedDuplicate,
            FindingKind::InconsistentAccount,
            FindingKind::InconsistentTaxCategory,
        ];
        let hard = [
            FindingKind::BalanceMismatch,
            FindingKind::AccountSetMismatch,
            FindingKind::DanglingReversal,
            FindingKind::DuplicateEntryNumber,
        ];
        let mut codes: Vec<&str> = advisory
            .iter()
            .chain(hard.iter())
            .map(|kind| kind.as_code())
            .collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(
            codes.len(),
            advisory.len() + hard.len(),
            "同じ種別を2度数えている"
        );
        // **種別を足したら `is_suspicion` の match が落ちる**（網羅的なので）。
        // ここは「テストにも足したか」を見る。
        assert_eq!(
            codes.len(),
            7,
            "種別を足したら、上の2つのテストにも足すこと"
        );
    }
    // ─── 重複の内訳（DuplicateSummary） ─────────────────

    fn expense(id: u128, no: u32, amount: i128) -> JournalEntry {
        entry(
            id,
            no,
            vec![
                line("604", Side::Debit, amount),
                line("100", Side::Credit, amount),
            ],
        )
    }

    // **本命。** 費用の重複は所得に効く。
    #[test]
    fn duplicated_expense_counts_toward_income() {
        let entries = vec![expense(1, 1, 1_000), expense(2, 2, 1_000)];

        let summary = summarize_suspected_duplicates(&entries, &sample_chart_with_tax_account());

        assert_eq!(summary.by_account.len(), 1);
        let group = &summary.by_account[0];
        assert_eq!(group.account, "604");
        assert_eq!(group.name, "通信費");
        assert!(group.affects_income);
        assert_eq!(group.groups, 1);
        // 2件なら余分は1件ぶん。
        assert_eq!(group.at_risk_minor, 1_000);
        assert_eq!(summary.at_risk_affecting_income(), 1_000);
    }

    // **本命。** 資産の重複は所得に効かない。
    //
    // 実帳簿では事業主貸の 1,030,000円 がこれだった。額はいちばん大きいが、
    // 全部が誤りでも所得は1円も動かない。**混ぜて数えると読み違える。**
    #[test]
    fn duplicated_asset_does_not_count_toward_income() {
        let entries = vec![balanced(1, 1, 500_000), balanced(2, 2, 500_000)];

        let summary = summarize_suspected_duplicates(&entries, &sample_chart_with_tax_account());

        assert_eq!(summary.by_account.len(), 1);
        assert!(
            !summary.by_account[0].affects_income,
            "資産は所得に効かない"
        );
        assert_eq!(summary.by_account[0].at_risk_minor, 500_000);
        assert_eq!(
            summary.at_risk_affecting_income(),
            0,
            "額は大きいが所得は動かない"
        );
    }

    // **本命。** 3件なら余分は2件ぶん。
    #[test]
    fn three_copies_put_two_at_risk() {
        let entries = vec![
            expense(1, 1, 1_000),
            expense(2, 2, 1_000),
            expense(3, 3, 1_000),
        ];

        let summary = summarize_suspected_duplicates(&entries, &sample_chart_with_tax_account());

        assert_eq!(summary.by_account[0].at_risk_minor, 2_000);
        assert_eq!(summary.by_account[0].groups, 1, "組は1つ");
    }

    // **本命。** 借方が複数なら、明細ごとに割り当てる。
    //
    // 全額をどれか1つの科目に寄せると内訳が嘘になる。
    #[test]
    fn multiple_debit_lines_are_split_by_line() {
        let two_debits = |id: u128, no: u32| {
            entry(
                id,
                no,
                vec![
                    line("604", Side::Debit, 300),
                    line("621", Side::Debit, 700),
                    line("100", Side::Credit, 1_000),
                ],
            )
        };
        let entries = vec![two_debits(1, 1), two_debits(2, 2)];

        let summary = summarize_suspected_duplicates(&entries, &sample_chart_with_tax_account());

        assert_eq!(summary.by_account.len(), 2);
        // 大きい順なので新聞図書費が先。
        assert_eq!(summary.by_account[0].account, "621");
        assert_eq!(summary.by_account[0].at_risk_minor, 700);
        assert_eq!(summary.by_account[1].account, "604");
        assert_eq!(summary.by_account[1].at_risk_minor, 300);
        assert_eq!(summary.at_risk_affecting_income(), 1_000);
    }

    // **本命。** 余分な額の大きい順に並べる。
    #[test]
    fn groups_are_sorted_by_amount() {
        let entries = vec![
            expense(1, 1, 100),
            expense(2, 2, 100),
            balanced(3, 3, 900),
            balanced(4, 4, 900),
        ];

        let summary = summarize_suspected_duplicates(&entries, &sample_chart_with_tax_account());

        assert_eq!(summary.by_account[0].account, "100", "大きいほうが先");
        assert_eq!(summary.by_account[1].account, "604");
    }

    // 重複が無ければ空。
    #[test]
    fn no_duplicates_means_empty_summary() {
        let entries = vec![expense(1, 1, 100), expense(2, 2, 200)];

        let summary = summarize_suspected_duplicates(&entries, &sample_chart_with_tax_account());

        assert!(summary.is_empty());
        assert_eq!(summary.at_risk_affecting_income(), 0);
    }

    // **本命。** 逆仕訳は数えない。
    //
    // 危ないのは「重複を見つけて、2件とも逆仕訳した」ときである。逆仕訳
    // どうしも互いに同じ形になるので、**除外しないと訂正した瞬間に新しい
    // 重複として挙がる**。訂正するほど指摘が増えるなら使い物にならない。
    //
    // 逆仕訳を1件だけ入れても効かない（借貸が入れ替わって指紋が変わるので、
    // 元の仕訳とは組にならず、1件では重複にならない）。**2件入れて初めて
    // この除外が効いているかを確かめられる。**
    #[test]
    fn reversals_are_not_counted() {
        let mut entries = vec![expense(1, 1, 1_000), expense(2, 2, 1_000)];
        for (index, id) in [3_u128, 4].into_iter().enumerate() {
            entries.push(
                entries[index]
                    .reverse(
                        EntryId::new(id),
                        EntryNumber::new(id as u32),
                        AccountingDate::new(2026, 6, 1).unwrap(),
                        "訂正".to_string(),
                        &FiscalYear::calendar_year(2026),
                        &sample_chart_with_tax_account(),
                        &TagSchema::empty(),
                        &AllOpen,
                        &fixed_clock(),
                    )
                    .unwrap(),
            );
        }

        let summary = summarize_suspected_duplicates(&entries, &sample_chart_with_tax_account());

        assert_eq!(summary.by_account.len(), 1, "逆仕訳の組を作らないこと");
        assert_eq!(
            summary.by_account[0].at_risk_minor, 1_000,
            "訂正しても指摘が増えないこと"
        );
    }
}
