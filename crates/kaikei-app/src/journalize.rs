//! 取り込んだ明細から仕訳を組み立てるルール（`docs/05-csv-import.md` §6）。
//!
//! **純関数。** I/O をしない。ルールの読み込み（YAML）も保存も外側が行い、
//! ここは「この明細にどのルールが当たるか」「当たったら何という仕訳になるか」
//! だけを決める。
//!
//! # 提案であって確定ではない
//!
//! ここが返すのは [`ProposedEntry`] であり、帳簿に入る仕訳ではない。確定は
//! 人が行う（§6「設計上の鉄則」）。仕訳番号の採番も貸借の最終検証も、
//! 呼び出し側が [`kaikei_core::JournalEntry::new`] を通して行う。
//!
//! # 入金/出金と借方/貸方を混ぜない
//!
//! ルールが持つのは「科目」と「相手科目（口座）」であって、借方でも貸方でも
//! ない。**どちらの側に立つかは明細の向きが決める**（[`sides_for`]）。ここを
//! 取り違えると収入と経費が丸ごと入れ替わる。

use kaikei_core::{
    AccountCode, AccountingDate, CoreError, Currency, JournalLine, Money, Side, TagKey, TagSet,
    TagValue,
};
use kaikei_policy::ProposedEntry;

use crate::ports::ImportDirection;

/// 摘要の照合の仕方。
///
/// **正規表現は持たない。** ルールを書くのは会計をする人であって、書き間違えた
/// 正規表現は「当たらない」ではなく「別の明細に当たる」形で失敗する。まずは
/// 説明のいらない3つだけにする。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DescriptionPattern {
    /// 摘要にこの文字列を含む。
    Contains(String),
    /// 摘要がこの文字列で始まる。
    StartsWith(String),
    /// 摘要がこの文字列と等しい。
    Equals(String),
}

impl DescriptionPattern {
    /// 摘要に当たるか。
    ///
    /// **大文字小文字を区別しない。** 銀行の摘要は表記が揺れる（`Sample` と
    /// `SAMPLE` が同じ明細で混ざる）。区別すると、同じ店なのに片方だけ
    /// 自動化されないという分かりにくい状態になる。
    pub fn matches(&self, description: &str) -> bool {
        let haystack = description.to_lowercase();
        match self {
            DescriptionPattern::Contains(needle) => haystack.contains(&needle.to_lowercase()),
            DescriptionPattern::StartsWith(needle) => haystack.starts_with(&needle.to_lowercase()),
            DescriptionPattern::Equals(needle) => haystack == needle.to_lowercase(),
        }
    }
}

/// 仕訳化のルール1件（`docs/05-csv-import.md` §6）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalizeRule {
    /// ルールの名前（どのルールが当たったかを人に見せるために使う）。
    pub id: String,
    /// 評価の順。**小さいほど先**。
    pub priority: i32,
    /// この取り込み元にだけ適用する。`None` なら全て。
    pub source: Option<String>,
    /// この向きにだけ適用する。`None` なら入金・出金の両方。
    pub direction: Option<ImportDirection>,
    /// 摘要の条件。
    pub pattern: DescriptionPattern,
    /// 金額の下限（この額以上）。
    pub amount_min: Option<i64>,
    /// 金額の上限（この額以下）。
    pub amount_max: Option<i64>,
    /// 立てる科目（消耗品費・売上など）。
    pub account: AccountCode,
    /// 相手科目（普通預金・現金など）。
    pub counter_account: AccountCode,
    /// 税区分。線上の値をそのままタグに入れる。
    pub tax_category: Option<String>,
    /// 取引先**コード**（`CP0001` など。`kaikei_policy::CounterpartyIndex` と
    /// 対応する）。**表示名ではない**——名前を入れると、適格請求書発行事業者
    /// かどうかの判定が引けなくなる。
    pub counterparty: Option<String>,
    /// 使うかどうか。**消さずに止められる**ようにする——過去にどのルールで
    /// 記帳したかを追えなくなるため。
    pub active: bool,
}

/// 照合に使う明細の中身。
///
/// [`crate::view::ImportedTxView`] をそのまま取らないのは、**保存されていない
/// 明細も試せるようにする**ためである。取り込む前に「このルールが当たるか」を
/// 確かめられないと、ルールを書くたびに本番の明細を汚すことになる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchTarget<'a> {
    /// 取り込み元。
    pub source: &'a str,
    /// 取引年月日。
    pub occurred_on: AccountingDate,
    /// 金額（常に正）。
    pub amount_minor: i64,
    /// 入金か出金か。
    pub direction: ImportDirection,
    /// 摘要。
    pub raw_description: &'a str,
}

/// ルールが当たった結果。
#[derive(Debug, Clone)]
pub struct JournalizeMatch {
    /// 当たったルールの名前。**必ず返す**——提案には根拠が要る（§6）。
    pub rule_id: String,
    /// 提案する仕訳。
    pub entry: ProposedEntry,
}

/// 明細に当たる最初のルールを選ぶ。
///
/// `priority` の昇順で見て、**最初に当たったものを採る**。同じ `priority` が
/// 並んだときは `id` の昇順で決める——並び方を決めておかないと、ルールを
/// 足しただけで過去と違う科目が提案されうる。
///
/// 止めてあるルール（`active == false`）は見ない。
pub fn first_matching<'r>(
    rules: &'r [JournalizeRule],
    target: &MatchTarget<'_>,
) -> Option<&'r JournalizeRule> {
    let mut candidates: Vec<&JournalizeRule> = rules
        .iter()
        .filter(|rule| rule.active && matches_rule(rule, target))
        .collect();
    candidates.sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.id.cmp(&b.id)));
    candidates.into_iter().next()
}

/// ルールが明細に当たるか。
fn matches_rule(rule: &JournalizeRule, target: &MatchTarget<'_>) -> bool {
    if let Some(source) = &rule.source {
        if source != target.source {
            return false;
        }
    }
    if let Some(direction) = rule.direction {
        if direction != target.direction {
            return false;
        }
    }
    // 範囲は両端を含む。「1000円以上」と書いた人が 1000 円を外されると驚く。
    if let Some(min) = rule.amount_min {
        if target.amount_minor < min {
            return false;
        }
    }
    if let Some(max) = rule.amount_max {
        if target.amount_minor > max {
            return false;
        }
    }
    rule.pattern.matches(target.raw_description)
}

/// 明細の向きから、科目と相手科目が立つ側を決める。
///
/// **出金**は「費用が増えて口座が減る」——科目が借方、口座が貸方。
/// **入金**はその逆。ここを取り違えると収入と経費が丸ごと入れ替わる。
///
/// 返り値は `(科目の側, 相手科目の側)`。
pub fn sides_for(direction: ImportDirection) -> (Side, Side) {
    match direction {
        ImportDirection::Out => (Side::Debit, Side::Credit),
        ImportDirection::In => (Side::Credit, Side::Debit),
    }
}

/// ルールから仕訳を組み立てる。
///
/// # Errors
///
/// 金額が仕訳明細として成り立たない（0 以下）場合は [`CoreError`]。
///
/// タグのキーが不正な場合も [`CoreError`]。**税区分の値そのものは検査しない**
/// ——何が有効な税区分かは `kaikei-policy` の領分であり、ここで二重に持つと
/// 片方だけ直したときに食い違う。
pub fn build_entry(
    rule: &JournalizeRule,
    target: &MatchTarget<'_>,
    currency: Currency,
) -> Result<JournalizeMatch, CoreError> {
    let amount = Money::from_minor(i128::from(target.amount_minor), currency);
    let (account_side, counter_side) = sides_for(target.direction);

    // 税区分と取引先は**科目の側にだけ付ける。** 口座（普通預金）に消費税の
    // 区分は無く、両方に付けると集計が二重になる。
    let mut tags = TagSet::new();
    if let Some(tax) = &rule.tax_category {
        tags.insert(TagKey::parse("tax_category")?, TagValue::Code(tax.clone()));
    }
    if let Some(counterparty) = &rule.counterparty {
        tags.insert(
            TagKey::parse("counterparty")?,
            TagValue::Code(counterparty.clone()),
        );
    }

    let lines = vec![
        JournalLine::new(rule.account.clone(), account_side, amount, tags, None)?,
        JournalLine::new(
            rule.counter_account.clone(),
            counter_side,
            amount,
            TagSet::new(),
            None,
        )?,
    ];

    Ok(JournalizeMatch {
        rule_id: rule.id.clone(),
        entry: ProposedEntry {
            entry_date: target.occurred_on,
            // **摘要は明細のものを使う。** ルールの名前に置き換えると、
            // 帳簿を見たときに元の取引が何だったか分からなくなる。
            description: target.raw_description.to_string(),
            lines,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(text: &str) -> AccountCode {
        AccountCode::parse(text).unwrap()
    }

    fn rule(id: &str, pattern: DescriptionPattern) -> JournalizeRule {
        JournalizeRule {
            id: id.to_string(),
            priority: 100,
            source: None,
            direction: None,
            pattern,
            amount_min: None,
            amount_max: None,
            account: code("500"),
            counter_account: code("100"),
            tax_category: None,
            counterparty: None,
            active: true,
        }
    }

    fn target<'a>(
        description: &'a str,
        amount: i64,
        direction: ImportDirection,
    ) -> MatchTarget<'a> {
        MatchTarget {
            source: "example_bank",
            occurred_on: AccountingDate::new(2026, 6, 15).unwrap(),
            amount_minor: amount,
            direction,
            raw_description: description,
        }
    }

    // ─── 照合 ────────────────────────────────────────────

    #[test]
    fn contains_starts_with_and_equals_each_do_what_they_say() {
        let text = "カ)サンプル シヨウジ";
        assert!(DescriptionPattern::Contains("サンプル".into()).matches(text));
        assert!(DescriptionPattern::StartsWith("カ)".into()).matches(text));
        assert!(DescriptionPattern::Equals(text.into()).matches(text));

        assert!(!DescriptionPattern::StartsWith("サンプル".into()).matches(text));
        assert!(!DescriptionPattern::Equals("サンプル".into()).matches(text));
    }

    /// 表記の揺れで当たらなくならない。
    ///
    /// 銀行の摘要は `Sample` と `SAMPLE` が同じ口座で混ざる。区別すると
    /// 「同じ店なのに片方だけ自動化されない」という分かりにくい状態になる。
    #[test]
    fn matching_ignores_letter_case() {
        let pattern = DescriptionPattern::Contains("sample".into());
        assert!(pattern.matches("SAMPLE.CO.JP"));
        assert!(pattern.matches("Sample Shokai"));
    }

    /// **本命。** 優先度の小さいものが勝つ。
    #[test]
    fn the_lowest_priority_number_wins() {
        let mut specific = rule(
            "sample_books",
            DescriptionPattern::Contains("サンプル".into()),
        );
        specific.priority = 10;
        specific.account = code("501");
        let general = rule("everything", DescriptionPattern::Contains("".into()));

        let rules = vec![general, specific];
        let chosen = first_matching(&rules, &target("サンプル", 1980, ImportDirection::Out))
            .expect("当たること");

        assert_eq!(chosen.id, "sample_books");
    }

    /// 優先度が同じなら名前の順で決まる。
    ///
    /// 決めておかないと、ルールを足しただけで過去と違う科目が提案されうる。
    #[test]
    fn a_tie_is_broken_by_name_so_the_result_does_not_drift() {
        let rules = vec![
            rule("zebra", DescriptionPattern::Contains("サンプル".into())),
            rule("alpha", DescriptionPattern::Contains("サンプル".into())),
        ];

        let chosen =
            first_matching(&rules, &target("サンプル", 1980, ImportDirection::Out)).unwrap();

        assert_eq!(chosen.id, "alpha");
    }

    /// 止めたルールは当たらない。
    #[test]
    fn an_inactive_rule_is_skipped() {
        let mut stopped = rule("stopped", DescriptionPattern::Contains("サンプル".into()));
        stopped.active = false;

        assert!(
            first_matching(&[stopped], &target("サンプル", 1980, ImportDirection::Out)).is_none()
        );
    }

    /// 向き・取り込み元・金額で絞れる。
    #[test]
    fn a_rule_can_be_narrowed_by_direction_source_and_amount() {
        let mut narrow = rule("narrow", DescriptionPattern::Contains("ｺﾝﾋﾞﾆ".into()));
        narrow.direction = Some(ImportDirection::Out);
        narrow.source = Some("example_bank".to_string());
        narrow.amount_min = Some(1_000);
        narrow.amount_max = Some(5_000);
        let rules = vec![narrow];

        assert!(first_matching(&rules, &target("ｺﾝﾋﾞﾆ", 2_000, ImportDirection::Out)).is_some());
        // 向きが違う。
        assert!(first_matching(&rules, &target("ｺﾝﾋﾞﾆ", 2_000, ImportDirection::In)).is_none());
        // 金額が範囲の外。
        assert!(first_matching(&rules, &target("ｺﾝﾋﾞﾆ", 500, ImportDirection::Out)).is_none());
        assert!(first_matching(&rules, &target("ｺﾝﾋﾞﾆ", 9_000, ImportDirection::Out)).is_none());

        // 取り込み元が違う。
        let other = MatchTarget {
            source: "example_card",
            ..target("ｺﾝﾋﾞﾆ", 2_000, ImportDirection::Out)
        };
        assert!(first_matching(&rules, &other).is_none());
    }

    /// 金額の範囲は両端を含む。
    ///
    /// 「1000円以上」と書いた人が 1000 円を外されると驚く。
    #[test]
    fn the_amount_range_includes_both_ends() {
        let mut bounded = rule("bounded", DescriptionPattern::Contains("".into()));
        bounded.amount_min = Some(1_000);
        bounded.amount_max = Some(2_000);
        let rules = vec![bounded];

        assert!(first_matching(&rules, &target("x", 1_000, ImportDirection::Out)).is_some());
        assert!(first_matching(&rules, &target("x", 2_000, ImportDirection::Out)).is_some());
    }

    #[test]
    fn no_rule_matches_is_not_an_error() {
        let rules = vec![rule(
            "sample",
            DescriptionPattern::Contains("サンプル".into()),
        )];
        assert!(first_matching(&rules, &target("ｽｰﾊﾟｰ", 500, ImportDirection::Out)).is_none());
    }

    // ─── 仕訳の組み立て ──────────────────────────────────

    /// **本命。** 出金は費用が借方、口座が貸方。
    ///
    /// 入れ替わると経費が収入になる。
    #[test]
    fn a_payment_puts_the_expense_on_the_debit_side() {
        let rule = rule("sample", DescriptionPattern::Contains("サンプル".into()));

        let built = build_entry(
            &rule,
            &target("カ)サンプル", 1_980, ImportDirection::Out),
            Currency::JPY,
        )
        .unwrap();

        assert_eq!(built.entry.lines[0].account(), &code("500"));
        assert_eq!(built.entry.lines[0].side(), Side::Debit, "費用は借方");
        assert_eq!(built.entry.lines[1].account(), &code("100"));
        assert_eq!(built.entry.lines[1].side(), Side::Credit, "口座は貸方");
    }

    /// **本命。** 入金は口座が借方、収益が貸方。
    #[test]
    fn a_deposit_puts_the_account_on_the_debit_side() {
        let mut sales = rule("sales", DescriptionPattern::Contains("ﾋﾞｰﾃﾂｸ".into()));
        sales.account = code("400");

        let built = build_entry(
            &sales,
            &target("ﾋﾞｰﾃﾂｸ(ｶ", 550_000, ImportDirection::In),
            Currency::JPY,
        )
        .unwrap();

        assert_eq!(built.entry.lines[0].account(), &code("400"));
        assert_eq!(built.entry.lines[0].side(), Side::Credit, "収益は貸方");
        assert_eq!(built.entry.lines[1].account(), &code("100"));
        assert_eq!(built.entry.lines[1].side(), Side::Debit, "口座は借方");
    }

    /// 貸借が一致する。
    #[test]
    fn the_two_lines_balance() {
        let built = build_entry(
            &rule("x", DescriptionPattern::Contains("".into())),
            &target("なにか", 1_980, ImportDirection::Out),
            Currency::JPY,
        )
        .unwrap();

        assert_eq!(built.entry.lines.len(), 2);
        assert_eq!(built.entry.lines[0].amount(), built.entry.lines[1].amount());
    }

    /// **本命。** 摘要は明細のものを使う。
    ///
    /// ルールの名前に置き換えると、帳簿を見たときに元の取引が何だったか
    /// 分からなくなる。
    #[test]
    fn the_description_comes_from_the_statement_not_the_rule() {
        let built = build_entry(
            &rule("sample_shohin", DescriptionPattern::Contains("".into())),
            &target("カ)サンプル シヨウジ", 1_980, ImportDirection::Out),
            Currency::JPY,
        )
        .unwrap();

        assert_eq!(built.entry.description, "カ)サンプル シヨウジ");
        // どのルールで作ったかは別に返す（提案には根拠が要る）。
        assert_eq!(built.rule_id, "sample_shohin");
    }

    /// 税区分と取引先は科目の側にだけ付く。
    ///
    /// 口座（普通預金）に消費税の区分は無く、両方に付けると集計が二重になる。
    #[test]
    fn the_tax_category_goes_only_on_the_account_line() {
        let mut taxed = rule("taxed", DescriptionPattern::Contains("".into()));
        taxed.tax_category = Some("TAXABLE_PURCHASE_10_QUALIFIED".to_string());
        taxed.counterparty = Some("CP0001".to_string());

        let built = build_entry(
            &taxed,
            &target("なにか", 1_980, ImportDirection::Out),
            Currency::JPY,
        )
        .unwrap();

        let tax_key = TagKey::parse("tax_category").unwrap();
        assert!(
            built.entry.lines[0].tags().get(&tax_key).is_some(),
            "科目側"
        );
        assert!(
            built.entry.lines[1].tags().get(&tax_key).is_none(),
            "口座側には付かない"
        );
    }

    /// 取引日は明細の日付になる。
    #[test]
    fn the_entry_date_comes_from_the_statement() {
        let built = build_entry(
            &rule("x", DescriptionPattern::Contains("".into())),
            &target("なにか", 100, ImportDirection::Out),
            Currency::JPY,
        )
        .unwrap();

        assert_eq!(
            built.entry.entry_date,
            AccountingDate::new(2026, 6, 15).unwrap()
        );
    }
}
