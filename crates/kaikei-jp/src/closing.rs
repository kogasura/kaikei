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
//! # `tax_category` タグの扱い（`DECISIONS.md` D-066）
//!
//! `kaikei-jp-data/tags.yaml`（同梱の既定タグスキーマ）は `tax_category` を
//! `required_for: [Revenue, Expense]` としている。`closing_entries` が生成する
//! 収益・費用のゼロ化明細もこの制約を受けるため、`tax_category` タグを
//! 付けないまま `kaikei_core::JournalEntry::new` に通すと
//! `CoreError::MissingRequiredTag` で拒否される（実際に踏んだ不具合）。
//!
//! `kaikei-policy::ClosingPolicy::closing_entries` は `TagSchema` も
//! `TaxCategoryTable` も引数に取らない trait シグネチャのため（凍結層のため
//! 変更しない）、`JpSoleProprietorClosingPolicy` は**構築時**に
//!
//! - `tax_category: Option<String>` — 収益・費用のゼロ化明細に付与する
//!   消費税区分コード。**どの区分コードを使うかはここにハードコードしない**
//!   （`CLAUDE.md` §1・§10）。同梱の税区分マスタ（`kaikei-jp-data/tax/jp/2026.yaml`）
//!   の `NOT_APPLICABLE`（「対象外」。注記に「資産・負債の振替など、消費税と
//!   無関係な取引に使う」とある）が候補になりうるが、それを選ぶかどうかは
//!   呼び出し側（合成ルート）の判断に委ねる
//! - `schema: &TagSchema` — 実際に使われるタグスキーマ。構築時に、
//!   `tax_category` の有無によって収益・費用の明細が本当にこのスキーマへ
//!   適合するかを検証する
//!
//! を受け取る。元入金（Equity）の明細には `tax_category` を付けない
//! （`required_for` の対象外のため）。
//!
//! ## 構築時検証の実装方法（`kaikei-core` に手を加えない）
//!
//! `kaikei_core::TagSchema` は `required_for` を読み出す専用の getter を
//! 公開していない（`defs` は非公開、`is_aggregatable` は無関係）。
//! `kaikei-core` は凍結層であり本 PR では変更しない。そこで、`required_for`
//! を読み出す代わりに、`closing_entries` が実際に生成するのと**全く同じ
//! `TagSet`**（`tax_category` が `Some` ならその1タグのみ、`None` なら空）を
//! 構築時に組み立て、`TagSchema::validate`（既存の公開 API）に
//! `AccountType::Revenue` / `AccountType::Expense` の両方で通す。
//! これは「`required_for` に含まれるか」を間接的に問い合わせるより強い
//! チェックになる（未登録キー・型不一致も同時に検出でき、`closing_entries`
//! が生成する明細が実際にスキーマを満たすことを直接保証する）。
//!
//! # 前提: 集計軸なし（`group_by = []`）の試算表を渡すこと
//!
//! `closing_entries` は `TrialBalance` の各行を1科目1残高として扱う。
//! `TrialBalance::from_entries` に `group_by` を与えて作った試算表を渡すと、
//! **同一科目がグループごとに複数行に分かれ、その科目に対して複数本の
//! ゼロ化明細が生成される**。貸借は数学的に必ず一致するので壊れはしないが、
//! グループを識別していた元のタグは引き継がれないため、意図した集計単位が
//! 失われる。決算処理には集計軸なしの試算表を渡すこと。
//!
//! # 貸借が一致する理由
//!
//! 収益・費用の各行を「反対側」に立ててゼロにすると、生成される明細群だけでは
//! 貸借が一致しない（収益側の合計と費用側の合計が異なるため）。その差額は
//! 定義上ちょうど所得（収益合計 − 費用合計）に等しく、元入金への1行
//! （所得が正なら貸方・負なら借方）で過不足なく埋め合わされる。
//! `verify_balanced` はこれを実行時に検算する最後の砦。

use kaikei_core::{
    sum_money, AccountCode, AccountDef, AccountType, ChartOfAccounts, CoreError, Currency,
    FiscalYear, JournalLine, Money, Side, TagKey, TagSchema, TagSet, TagValue, TrialBalance,
};
use kaikei_policy::{ClosingPolicy, PolicyError, ProposedEntry};
use std::sync::OnceLock;

use crate::error::JpError;

/// 決算処理に使う3科目の科目コード。
///
/// **位置引数で並べて渡さない。** どれも `AccountCode` 型なので、順序を
/// 取り違えても型検査を通ってしまい、所得が事業主貸に振り替えられる
/// といった会計上の誤りが無言で成立する（レビューで実際に再現された）。
/// 構造体リテラルはフィールド名を必須にするため、この形なら取り違えが起きない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosingAccounts {
    /// 元入金。所得の振替先。
    pub capital: AccountCode,
    /// 事業主貸。現時点の `closing_entries` では使わないが、
    /// `opening_entries` を実装する際に必要（`DECISIONS.md` D-065/D-066）。
    pub owner_drawings: AccountCode,
    /// 事業主借。同上。
    pub owner_contributions: AccountCode,
}

/// 個人事業主（青色申告）向けの決算振替仕訳を生成する `ClosingPolicy` 実装。
///
/// 元入金・事業主貸・事業主借の科目コードは構築時に保持する
/// （`JpTaxPolicy` がマスタを構築時に持つのと同じパターン。
/// `docs/04-jp-tax.md` §9「実装上の注意」）。事業主貸・事業主借は現時点の
/// `closing_entries`（手順1・2・3の当年度末分）では使わないが、
/// [`ClosingPolicy::opening_entries`] を将来実装する際に必要になるため
/// 構築時にまとめて検証・保持する（`DECISIONS.md` D-066）。
///
/// `tax_category` は収益・費用のゼロ化明細に付与する消費税区分コード
/// （モジュール doc「`tax_category` タグの扱い」を参照。`DECISIONS.md` D-066）。
#[derive(Debug, Clone)]
pub struct JpSoleProprietorClosingPolicy {
    capital_account: AccountCode,
    owner_drawings_account: AccountCode,
    owner_contributions_account: AccountCode,
    tax_category: Option<String>,
}

impl JpSoleProprietorClosingPolicy {
    /// 決算科目（元入金・事業主貸・事業主借）の科目コード、`tax_category` に
    /// 使う消費税区分コード、勘定科目表、タグスキーマから構築する。
    ///
    /// 次の2つを構築時に検証する。実行時（決算処理の最中）ではなく構築時に
    /// 失敗させることで、記帳作業の途中で決算処理だけが失敗する事態を避ける。
    ///
    /// 1. 決算科目3つ（元入金・事業主貸・事業主借）が `chart` に存在すること。
    ///    存在しなければ `JpError::MissingClosingAccount`
    ///    （見つからなかった科目コードを含む）
    /// 2. `closing_entries` が実際に生成する収益・費用の明細のタグ
    ///    （`tax_category` が `Some` ならその1タグ、`None` なら空）が
    ///    `schema` に適合すること。適合しなければ
    ///    `JpError::ClosingTagSchemaMismatch`
    pub fn new(
        chart: &ChartOfAccounts,
        schema: &TagSchema,
        accounts: ClosingAccounts,
        tax_category: Option<String>,
    ) -> Result<Self, JpError> {
        let ClosingAccounts {
            capital: capital_account,
            owner_drawings: owner_drawings_account,
            owner_contributions: owner_contributions_account,
        } = accounts;

        let capital_def = require_account(chart, &capital_account, "元入金")?;
        require_account(chart, &owner_drawings_account, "事業主貸")?;
        require_account(chart, &owner_contributions_account, "事業主借")?;

        // 3科目は定義上すべて別の科目。同じコードを渡すのは設定ミスであり、
        // 放置すると決算振替が意図しない科目に載る。
        require_distinct(
            &capital_account,
            &owner_drawings_account,
            "元入金",
            "事業主貸",
        )?;
        require_distinct(
            &capital_account,
            &owner_contributions_account,
            "元入金",
            "事業主借",
        )?;
        require_distinct(
            &owner_drawings_account,
            &owner_contributions_account,
            "事業主貸",
            "事業主借",
        )?;

        // 生成する明細と**同じ形の `TagSet`** をスキーマに通して、構築時に
        // 「決算処理を走らせたら記帳できない」状態を検出する。
        //
        // `required_for` を問い合わせる形にしないのは、`TagSchema` がそれを
        // 読む API を公開していない（`kaikei-core` は不変層で追加できない）ことに加え、
        // 実際に渡す `TagSet` をそのまま検証する方が未登録キー・型不一致も
        // 同時に拾えて強いため。
        //
        // 収益・費用のゼロ化明細と、元入金の明細では**付けるタグが違う**
        // （後者は常に空）ので、両方を対応する科目種別で検証する。同梱の
        // `tags.yaml` は純資産に必須タグを課していないが、ユーザーが差し替えた
        // スキーマで課していた場合、ここで検証しないと決算処理の実行時まで発覚しない。
        //
        // 元入金の科目種別は `AccountType::Equity` を決め打ちせず、**勘定科目表に
        // 実際に登録されている種別**（`capital_def.account_type`）を使う。
        // 記帳時に `JournalEntry::new` がタグを検証するときに使うのはそちらであり、
        // 決め打ちすると「構築時は通ったのに記帳時に落ちる」食い違いが生まれる
        // （レビューで実際に再現された）。
        let zeroing_tags = build_zeroing_tags(tax_category.as_deref());
        let capital_tags = TagSet::new();
        for (account_type, tags) in [
            (AccountType::Revenue, &zeroing_tags),
            (AccountType::Expense, &zeroing_tags),
            (capital_def.account_type, &capital_tags),
        ] {
            schema.validate(tags, account_type).map_err(|source| {
                JpError::ClosingTagSchemaMismatch {
                    account_type_label: account_type.label_ja().to_string(),
                    reason: source.to_string(),
                }
            })?;
        }

        Ok(JpSoleProprietorClosingPolicy {
            capital_account,
            owner_drawings_account,
            owner_contributions_account,
            tax_category,
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

    /// 収益・費用のゼロ化明細に付与する消費税区分コードを返す。
    pub fn tax_category(&self) -> Option<&str> {
        self.tax_category.as_deref()
    }
}

/// 決算科目が「存在し」「記帳可能」であることを構築時に検証し、
/// 実際に登録されている定義を返す。
///
/// `postable` を見るのは、見出し科目（`postable: false`）を指定されると
/// `closing_entries` は明細を作れてしまう一方、それを
/// `kaikei_core::JournalEntry::new` に通した瞬間に `CoreError::NotPostable` で
/// 拒否されるため。決算処理の実行時ではなく構築時に落とす（`DECISIONS.md` D-066）。
///
/// 戻り値の `AccountDef` は、タグスキーマ検証で**実際に登録されている科目種別**を
/// 使うために必要（決め打ちすると、記帳時に使われる種別と食い違いうる）。
fn require_account<'a>(
    chart: &'a ChartOfAccounts,
    code: &AccountCode,
    role: &str,
) -> Result<&'a AccountDef, JpError> {
    let def = chart
        .get(code)
        .ok_or_else(|| JpError::MissingClosingAccount {
            role: role.to_string(),
            code: code.as_str().to_string(),
        })?;

    if !def.postable {
        return Err(JpError::NotPostableClosingAccount {
            role: role.to_string(),
            code: code.as_str().to_string(),
        });
    }

    Ok(def)
}

/// 決算科目どうしが別の科目コードであることを検証する。
fn require_distinct(
    a: &AccountCode,
    b: &AccountCode,
    role_a: &str,
    role_b: &str,
) -> Result<(), JpError> {
    if a == b {
        return Err(JpError::DuplicateClosingAccount {
            role_a: role_a.to_string(),
            role_b: role_b.to_string(),
            code: a.as_str().to_string(),
        });
    }
    Ok(())
}

/// `tax_category_key()` は固定文字列であり、呼び出しのたびにパース・
/// アロケーションし直す必要はない（`crate::tax::policy` の
/// `tax_category_key` と同じ意図。モジュールが異なるため個別に保持する）。
fn tax_category_key() -> &'static TagKey {
    static KEY: OnceLock<TagKey> = OnceLock::new();
    KEY.get_or_init(|| {
        TagKey::parse("tax_category")
            .expect("\"tax_category\" は tags.yaml に登録された既知のタグキー")
    })
}

/// 収益・費用のゼロ化明細に付けるタグを組み立てる。`tax_category` が
/// `Some` ならその1タグのみを持つ `TagSet`、`None` なら空の `TagSet`。
///
/// `closing_entries`（実際の明細生成）と `new`（構築時の `TagSchema::validate`
/// による検証）の両方から呼ばれる。**両者が同じタグを組み立てることが
/// 検証の前提**であり、片方だけを変更すると検証が実際の生成結果と
/// 食い違ってしまう。
fn build_zeroing_tags(tax_category: Option<&str>) -> TagSet {
    let mut tags = TagSet::new();
    if let Some(code) = tax_category {
        tags.insert(tax_category_key().clone(), TagValue::Code(code.to_string()));
    }
    tags
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
                build_zeroing_tags(self.tax_category.as_deref()),
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
        // `tax_category` を含め何も必須にしないスキーマ。closing_entries
        // 自体のロジック（貸借計算）を実運用のタグ要件から切り離してテストする
        // （`tax_category` の要件そのものを検証するテストは別に用意する）。
        TagSchema::empty()
    }

    fn policy(chart: &ChartOfAccounts) -> JpSoleProprietorClosingPolicy {
        JpSoleProprietorClosingPolicy::new(
            chart,
            &schema(),
            ClosingAccounts {
                capital: AccountCode::parse("400").unwrap(),
                owner_drawings: AccountCode::parse("410").unwrap(),
                owner_contributions: AccountCode::parse("420").unwrap(),
            },
            None,
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
            &schema(),
            ClosingAccounts {
                capital: AccountCode::parse("400").unwrap(),
                owner_drawings: AccountCode::parse("410").unwrap(),
                owner_contributions: AccountCode::parse("420").unwrap(),
            },
            None,
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
            &schema(),
            ClosingAccounts {
                capital: AccountCode::parse("400").unwrap(),
                owner_drawings: AccountCode::parse("410").unwrap(),
                owner_contributions: AccountCode::parse("420").unwrap(),
            },
            None,
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
            &schema(),
            ClosingAccounts {
                capital: AccountCode::parse("400").unwrap(),
                owner_drawings: AccountCode::parse("410").unwrap(),
                owner_contributions: AccountCode::parse("420").unwrap(),
            },
            None,
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

    // ---- tax_category タグの扱い ----
    //
    // これらのテストが、実際に踏んだ不具合（同梱 tags.yaml のままでは
    // closing_entries が生成する ProposedEntry を JournalEntry::new に
    // 通せない）の再現・修正確認になる。

    /// `tax_category` を `Revenue`/`Expense` に必須とするスキーマ。
    fn schema_requiring_tax_category_for_revenue_and_expense() -> TagSchema {
        TagSchema::new(vec![(
            TagKey::parse("tax_category").unwrap(),
            kaikei_core::TagDef {
                value_type: kaikei_core::TagValueType::Code,
                aggregatable: true,
                required_for: vec![AccountType::Revenue, AccountType::Expense],
            },
        )])
    }

    /// 決算科目に同じ科目コードを渡したら構築時に弾かれること。
    ///
    /// 3科目は定義上すべて別の科目。同じコードを許すと決算振替が意図しない
    /// 科目に載る（`opening_entries` を実装したときに顕在化する）。
    #[test]
    fn new_rejects_duplicate_closing_account_codes() {
        let chart = test_chart();

        let result = JpSoleProprietorClosingPolicy::new(
            &chart,
            &TagSchema::empty(),
            ClosingAccounts {
                capital: AccountCode::parse("400").unwrap(),
                owner_drawings: AccountCode::parse("400").unwrap(),
                owner_contributions: AccountCode::parse("420").unwrap(),
            },
            None,
        );

        match result {
            Err(JpError::DuplicateClosingAccount {
                role_a,
                role_b,
                code,
            }) => {
                assert_eq!(role_a, "元入金");
                assert_eq!(role_b, "事業主貸");
                assert_eq!(code, "400");
            }
            other => panic!("同じ科目コードの重複は構築時に弾くべき: {other:?}"),
        }
    }

    /// 決算科目が見出し科目（`postable: false`）なら構築時に弾かれること。
    ///
    /// 見出し科目には記帳できないため、構築を許すと `closing_entries` は
    /// 明細を作れる一方、それを `JournalEntry::new` に通した瞬間に
    /// `CoreError::NotPostable` で落ちる。tax_category と同じ
    /// 「構築時は通るが記帳時に落ちる」穴（レビューで再現された）。
    #[test]
    fn new_rejects_a_closing_account_that_is_not_postable() {
        let chart = ChartOfAccounts::new(vec![
            AccountDef {
                code: AccountCode::parse("400").unwrap(),
                name: "元入金（見出し）".to_string(),
                account_type: AccountType::Equity,
                parent: None,
                postable: false,
            },
            account("410", "事業主貸", AccountType::Equity),
            account("420", "事業主借", AccountType::Equity),
            account("500", "売上高", AccountType::Revenue),
            account("600", "仕入高", AccountType::Expense),
        ])
        .unwrap();

        let result = JpSoleProprietorClosingPolicy::new(
            &chart,
            &TagSchema::empty(),
            ClosingAccounts {
                capital: AccountCode::parse("400").unwrap(),
                owner_drawings: AccountCode::parse("410").unwrap(),
                owner_contributions: AccountCode::parse("420").unwrap(),
            },
            None,
        );

        match result {
            Err(JpError::NotPostableClosingAccount { role, code }) => {
                assert_eq!(role, "元入金");
                assert_eq!(code, "400");
            }
            other => panic!("見出し科目は構築時に弾くべき: {other:?}"),
        }
    }

    /// 元入金が `Equity` 以外として登録されていても、**実際の種別**で
    /// タグ検証されること。
    ///
    /// `AccountType::Equity` を決め打ちすると、記帳時に使われる種別
    /// （勘定科目表の登録内容）と食い違い、「構築時は通ったのに記帳時に
    /// 落ちる」状態になる（レビューで再現された）。
    #[test]
    fn new_validates_capital_line_tags_against_the_registered_account_type() {
        // 元入金を誤って Asset として登録した勘定科目表。
        let chart = ChartOfAccounts::new(vec![
            account("400", "元入金", AccountType::Asset),
            account("410", "事業主貸", AccountType::Equity),
            account("420", "事業主借", AccountType::Equity),
            account("500", "売上高", AccountType::Revenue),
            account("600", "仕入高", AccountType::Expense),
        ])
        .unwrap();

        // Asset に必須タグを課すスキーマ。元入金の明細は常にタグ無しなので、
        // 実際の種別（Asset）で検証すればここで弾ける。
        let schema = TagSchema::new(vec![
            (
                TagKey::parse("tax_category").unwrap(),
                kaikei_core::TagDef {
                    value_type: kaikei_core::TagValueType::Code,
                    aggregatable: true,
                    required_for: vec![],
                },
            ),
            (
                TagKey::parse("project").unwrap(),
                kaikei_core::TagDef {
                    value_type: kaikei_core::TagValueType::Code,
                    aggregatable: true,
                    required_for: vec![AccountType::Asset],
                },
            ),
        ]);

        let result = JpSoleProprietorClosingPolicy::new(
            &chart,
            &schema,
            ClosingAccounts {
                capital: AccountCode::parse("400").unwrap(),
                owner_drawings: AccountCode::parse("410").unwrap(),
                owner_contributions: AccountCode::parse("420").unwrap(),
            },
            Some("NOT_APPLICABLE".to_string()),
        );

        match result {
            Err(JpError::ClosingTagSchemaMismatch {
                account_type_label,
                reason,
            }) => {
                assert_eq!(
                    account_type_label, "資産",
                    "決め打ちの「純資産」ではなく、登録されている種別で検証すること"
                );
                assert!(reason.contains("project"), "reason = {reason}");
            }
            other => panic!("登録種別（資産）で検証して弾くべき: {other:?}"),
        }
    }

    /// `Equity` に必須タグを課すスキーマ（ユーザーが差し替えた場合を模す）。
    ///
    /// 収益・費用側は通るようにしておく（`tax_category` は登録するが必須にしない）。
    /// そうしないと収益の検証で先に落ち、Equity の検証まで到達したかが分からない。
    fn schema_requiring_a_tag_for_equity() -> TagSchema {
        TagSchema::new(vec![
            (
                TagKey::parse("tax_category").unwrap(),
                kaikei_core::TagDef {
                    value_type: kaikei_core::TagValueType::Code,
                    aggregatable: true,
                    required_for: vec![],
                },
            ),
            (
                TagKey::parse("project").unwrap(),
                kaikei_core::TagDef {
                    value_type: kaikei_core::TagValueType::Code,
                    aggregatable: true,
                    required_for: vec![AccountType::Equity],
                },
            ),
        ])
    }

    /// 元入金（Equity）の明細は常にタグ無しなので、`Equity` に必須タグを課す
    /// スキーマでは**構築時に**弾かれること。
    ///
    /// 収益・費用だけを検証していると、この経路は決算処理を実行するまで
    /// 発覚しない（レビュー前の実装がそうだった）。同梱の `tags.yaml` は
    /// Equity に必須タグを課していないため実害は出ていなかったが、
    /// ユーザーが自分のスキーマに差し替える経路がある以上、塞いでおく。
    #[test]
    fn new_rejects_a_schema_that_requires_tags_on_equity_lines() {
        let chart = test_chart();
        let schema = schema_requiring_a_tag_for_equity();

        let result = JpSoleProprietorClosingPolicy::new(
            &chart,
            &schema,
            ClosingAccounts {
                capital: AccountCode::parse("400").unwrap(),
                owner_drawings: AccountCode::parse("410").unwrap(),
                owner_contributions: AccountCode::parse("420").unwrap(),
            },
            Some("NOT_APPLICABLE".to_string()),
        );

        match result {
            Err(JpError::ClosingTagSchemaMismatch {
                account_type_label,
                reason,
            }) => {
                assert_eq!(
                    account_type_label, "純資産",
                    "どの科目種別で適合しなかったかが分かること"
                );
                assert!(
                    reason.contains("project"),
                    "不足しているタグキーが分かること: {reason}"
                );
            }
            other => panic!("Equity に必須タグを課すスキーマは構築時に弾くべき: {other:?}"),
        }
    }

    #[test]
    fn new_with_tax_category_attaches_tag_to_revenue_and_expense_lines_but_not_capital() {
        let chart = test_chart();
        let schema = schema_requiring_tax_category_for_revenue_and_expense();
        let policy = JpSoleProprietorClosingPolicy::new(
            &chart,
            &schema,
            ClosingAccounts {
                capital: AccountCode::parse("400").unwrap(),
                owner_drawings: AccountCode::parse("410").unwrap(),
                owner_contributions: AccountCode::parse("420").unwrap(),
            },
            Some("NOT_APPLICABLE".to_string()),
        )
        .unwrap();
        assert_eq!(policy.tax_category(), Some("NOT_APPLICABLE"));

        // 取引明細（試算表を作るための元仕訳）自体は `tax_category` を必須と
        // しない空スキーマで組み立てる。ここでの目的は `closing_entries` が
        // 「生成」する明細に `tax_category` が付くかどうかであり、取引明細を
        // 実運用のスキーマで記帳できるかは別の関心事
        // （下の `closing_entries_output_is_postable_against_bundled_chart_and_tags`
        // で実データを使って別途検証する）。
        let empty_schema = TagSchema::empty();
        let entries = [
            cash_entry(
                &chart,
                &empty_schema,
                &fy(),
                1,
                "500",
                Side::Credit,
                1_000_000,
            ),
            cash_entry(&chart, &empty_schema, &fy(), 2, "600", Side::Debit, 400_000),
        ];
        let tb = TrialBalance::from_entries(entries.iter(), &chart, &empty_schema, &[]).unwrap();

        let proposed = policy.closing_entries(&tb, &fy()).unwrap();
        let entry = &proposed[0];

        let category_key = TagKey::parse("tax_category").unwrap();
        let revenue_line = entry
            .lines
            .iter()
            .find(|l| l.account().as_str() == "500")
            .unwrap();
        assert_eq!(
            revenue_line.tags().get(&category_key),
            Some(&TagValue::Code("NOT_APPLICABLE".to_string()))
        );
        let expense_line = entry
            .lines
            .iter()
            .find(|l| l.account().as_str() == "600")
            .unwrap();
        assert_eq!(
            expense_line.tags().get(&category_key),
            Some(&TagValue::Code("NOT_APPLICABLE".to_string()))
        );
        let capital_line = entry
            .lines
            .iter()
            .find(|l| l.account().as_str() == "400")
            .unwrap();
        assert!(
            capital_line.tags().is_empty(),
            "元入金の明細には tax_category を含むいかなるタグも付かないはず"
        );
    }

    #[test]
    fn new_rejects_none_tax_category_when_schema_requires_it_for_revenue_or_expense() {
        let chart = test_chart();
        let schema = schema_requiring_tax_category_for_revenue_and_expense();
        let err = JpSoleProprietorClosingPolicy::new(
            &chart,
            &schema,
            ClosingAccounts {
                capital: AccountCode::parse("400").unwrap(),
                owner_drawings: AccountCode::parse("410").unwrap(),
                owner_contributions: AccountCode::parse("420").unwrap(),
            },
            None,
        )
        .unwrap_err();
        match err {
            JpError::ClosingTagSchemaMismatch {
                account_type_label,
                reason,
            } => {
                // 収益・費用のどちらを先に検証するかは実装の走査順に依存するが、
                // どちらであっても account_type_label は "収益" か "費用"。
                assert!(
                    account_type_label == "収益" || account_type_label == "費用",
                    "account_type_label = {account_type_label}"
                );
                assert!(reason.contains("tax_category"), "reason = {reason}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn new_succeeds_with_none_tax_category_when_schema_does_not_require_it() {
        let chart = test_chart();
        // tax_category キー自体は登録されているが、required_for が空。
        let schema = TagSchema::new(vec![(
            TagKey::parse("tax_category").unwrap(),
            kaikei_core::TagDef {
                value_type: kaikei_core::TagValueType::Code,
                aggregatable: true,
                required_for: vec![],
            },
        )]);
        let policy = JpSoleProprietorClosingPolicy::new(
            &chart,
            &schema,
            ClosingAccounts {
                capital: AccountCode::parse("400").unwrap(),
                owner_drawings: AccountCode::parse("410").unwrap(),
                owner_contributions: AccountCode::parse("420").unwrap(),
            },
            None,
        )
        .unwrap();
        assert_eq!(policy.tax_category(), None);
    }

    #[test]
    fn new_rejects_tax_category_when_schema_does_not_register_the_key() {
        let chart = test_chart();
        // `tax_category` キー自体が登録されていないスキーマ。
        let schema = TagSchema::empty();
        let err = JpSoleProprietorClosingPolicy::new(
            &chart,
            &schema,
            ClosingAccounts {
                capital: AccountCode::parse("400").unwrap(),
                owner_drawings: AccountCode::parse("410").unwrap(),
                owner_contributions: AccountCode::parse("420").unwrap(),
            },
            Some("NOT_APPLICABLE".to_string()),
        )
        .unwrap_err();
        assert!(matches!(err, JpError::ClosingTagSchemaMismatch { .. }));
    }

    /// **本丸**: 同梱の実データ（`kaikei_jp_data::CHART_SOLE_PROPRIETOR` /
    /// `kaikei_jp_data::TAGS`）で構築した `ChartOfAccounts` / `TagSchema` に対し、
    /// `closing_entries` が生成する `ProposedEntry` が実際に
    /// `JournalEntry::new` を通ること（コーディネーターが再現した不具合の
    /// 逆再現）。`tags.yaml` は `tax_category` を `required_for: [Revenue,
    /// Expense]` としているため、`tax_category` を指定せずに構築しようとすると
    /// この時点で `ClosingTagSchemaMismatch` になる（構築時検証そのものの確認）。
    #[test]
    fn closing_entries_output_is_postable_against_bundled_chart_and_tags() {
        let chart = crate::chart::load_embedded(kaikei_jp_data::CHART_SOLE_PROPRIETOR).unwrap();
        let schema = crate::tags::load_embedded(kaikei_jp_data::TAGS).unwrap();
        let fy = fy();

        // tax_category を指定しない構築は、同梱スキーマでは失敗する
        // （実際に踏んだ不具合の再現）。
        let err = JpSoleProprietorClosingPolicy::new(
            &chart,
            &schema,
            ClosingAccounts {
                capital: AccountCode::parse("400").unwrap(),
                owner_drawings: AccountCode::parse("410").unwrap(),
                owner_contributions: AccountCode::parse("420").unwrap(),
            },
            None,
        )
        .unwrap_err();
        assert!(matches!(err, JpError::ClosingTagSchemaMismatch { .. }));

        // 候補として挙げられている "NOT_APPLICABLE"（対象外。docs/04-jp-tax.md
        // §9 のモジュール doc、`kaikei-jp-data/tax/jp/2026.yaml` の注記を参照）を
        // 指定すると構築できる。どの区分を使うかは利用者の判断であり、この値の
        // 選択自体をこの実装が断定しているわけではない（テストの都合上の選択）。
        let policy = JpSoleProprietorClosingPolicy::new(
            &chart,
            &schema,
            ClosingAccounts {
                capital: AccountCode::parse("400").unwrap(),
                owner_drawings: AccountCode::parse("410").unwrap(),
                owner_contributions: AccountCode::parse("420").unwrap(),
            },
            Some("NOT_APPLICABLE".to_string()),
        )
        .unwrap();

        // 通常の取引仕訳（売上・仕入）を実データのスキーマで記帳する。
        let category_key = TagKey::parse("tax_category").unwrap();
        let mut sales_tags = TagSet::new();
        sales_tags.insert(category_key.clone(), TagValue::Code("SALES_10".to_string()));
        let mut purchase_tags = TagSet::new();
        purchase_tags.insert(
            category_key.clone(),
            TagValue::Code("PURCHASE_10_QUALIFIED".to_string()),
        );

        let sales_lines = vec![
            JournalLine::new(
                AccountCode::parse("100").unwrap(),
                Side::Debit,
                Money::from_minor(1_000_000, Currency::JPY),
                TagSet::new(),
                None,
            )
            .unwrap(),
            JournalLine::new(
                AccountCode::parse("500").unwrap(),
                Side::Credit,
                Money::from_minor(1_000_000, Currency::JPY),
                sales_tags,
                None,
            )
            .unwrap(),
        ];
        let purchase_lines = vec![
            JournalLine::new(
                AccountCode::parse("555").unwrap(),
                Side::Debit,
                Money::from_minor(400_000, Currency::JPY),
                purchase_tags,
                None,
            )
            .unwrap(),
            JournalLine::new(
                AccountCode::parse("100").unwrap(),
                Side::Credit,
                Money::from_minor(400_000, Currency::JPY),
                TagSet::new(),
                None,
            )
            .unwrap(),
        ];
        let entries = [
            new_entry(1, 1, &fy, &chart, &schema, fy.start(), "売上", sales_lines),
            new_entry(
                2,
                2,
                &fy,
                &chart,
                &schema,
                fy.start(),
                "仕入",
                purchase_lines,
            ),
        ];
        let tb = TrialBalance::from_entries(entries.iter(), &chart, &schema, &[]).unwrap();

        let proposed = policy.closing_entries(&tb, &fy).unwrap();
        assert_eq!(proposed.len(), 1);

        // 本丸: 生成された明細一式が、実データのスキーマで JournalEntry::new を
        // 通ること。`new_entry` 内部の `.expect(...)` が失敗すればテストが
        // 落ちる（コーディネーターが再現したのと同じ拒否が再発していないか）。
        for (i, p) in proposed.into_iter().enumerate() {
            let closing_entry = new_entry(
                1_000 + i as u128,
                1_000 + i as u32,
                &fy,
                &chart,
                &schema,
                p.entry_date,
                &p.description,
                p.lines,
            );
            // 元入金（Equity）の明細にはタグが付かないことも合わせて確認する。
            let capital_line = closing_entry
                .lines()
                .iter()
                .find(|l| l.account().as_str() == "400")
                .unwrap();
            assert!(capital_line.tags().is_empty());
        }
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
