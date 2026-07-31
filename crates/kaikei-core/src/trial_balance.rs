//! 試算表（`TrialBalance`）。仕訳の集合から科目別残高を集計する read model。
//!
//! `CLAUDE.md` §6「read model は物理的に分離する」の原則どおり、このモジュールは
//! `JournalEntry` の集合を読み取って集計するだけで、`journal.rs` の集約
//! （`JournalEntry` / `JournalLine`）の private フィールドには一切触れない。
//! `JournalEntry::lines()` などの公開ゲッターのみを使って構築する。
//!
//! 残高計算の向きは `DOMAIN.md` §2 の記録メカニズムそのもの（借方合計・貸方合計の
//! どちらから引くかは科目種別で決まる）に従う。ここを間違えると全ての残高が符号反転する。

use crate::account::{AccountCode, AccountType, ChartOfAccounts};
use crate::error::CoreError;
use crate::journal::{JournalEntry, Side};
use crate::money::{sum_money, Currency, Money};
use crate::tag::{TagKey, TagSchema, TagSet, TagValue};
use std::collections::BTreeMap;

/// `group_by` の結果キー。指定したタグキーの値の組。空なら「全体」（グルーピングなし）を表す。
///
/// 明細が `group_by` に指定されたキーの一部または全部を持たない場合、そのキーは
/// キーの組から単純に除外される（値を持たないキーを別の特別な値で埋めることはしない）。
/// `group_by` が空、またはどのキーも持たない明細は、要素数0の `GroupKey` に集約される
/// （`docs/02-test-cases.md` B-23）。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct GroupKey(Vec<(TagKey, String)>);

/// 試算表の1行。ある科目（と `group_by` を指定した場合はグループ）の借方・貸方合計と残高。
#[derive(Debug, Clone)]
pub struct BalanceRow {
    /// 勘定科目コード。
    pub account: AccountCode,
    /// 科目種別。残高の符号を決めるために保持する。
    pub account_type: AccountType,
    /// `group_by` によるグループキー。`group_by` を指定しなければ常に空。
    pub group: GroupKey,
    /// 借方合計。
    pub debit_total: Money,
    /// 貸方合計。
    pub credit_total: Money,
    /// `account_type.is_debit_normal()` に従った符号付き残高。
    ///
    /// 借方残の科目（資産・費用）は `借方合計 - 貸方合計`、
    /// 貸方残の科目（負債・純資産・収益）は `貸方合計 - 借方合計`（`DOMAIN.md` §2）。
    pub balance: Money,
}

/// 試算表。`JournalEntry` の集合から [`TrialBalance::from_entries`] で構築する read model。
#[derive(Debug, Clone)]
pub struct TrialBalance {
    rows: Vec<BalanceRow>,
    currency: Currency,
}

/// タグ値を `GroupKey` の要素として使うための文字列表現に変換する。
///
/// `group_by` に使えるのは `TagSchema::is_aggregatable` が `true` を返すキーのみに
/// 限定されているため、ここでは値の「同一性」を文字列として比較できれば十分であり、
/// 元の型（`Code` / `Text` / `Decimal` / `Date`）へ戻す必要はない。
fn tag_value_to_group_string(value: &TagValue) -> String {
    match value {
        TagValue::Code(s) => s.clone(),
        TagValue::Text(s) => s.clone(),
        TagValue::Decimal(d) => d.to_string(),
        TagValue::Date(d) => d.to_iso_string(),
    }
}

/// 明細のタグから `group_by` に対応する `GroupKey` を組み立てる。
///
/// `group_by` の順序どおりにキーを走査するため、同じ `group_by` を使う限り
/// 生成される `GroupKey` の要素順序は常に一貫する（`Ord` 比較や `PartialEq` 比較が
/// キーの並び方に依存して破綻することはない）。
fn build_group_key(tags: &TagSet, group_by: &[TagKey]) -> GroupKey {
    let mut pairs = Vec::with_capacity(group_by.len());
    for key in group_by {
        if let Some(value) = tags.get(key) {
            pairs.push((key.clone(), tag_value_to_group_string(value)));
        }
    }
    GroupKey(pairs)
}

/// `from_entries` の集計中に使う、科目×グループ単位の借方・貸方の積み上げ。
///
/// 位置タプル（`(AccountType, Money, Money)`）ではなく名前付きフィールドにすることで、
/// 「借方は index 1、貸方は index 2」のようなマジックインデックスに起因する
/// 貸借取り違えのリスクを排除する。
struct Bucket {
    account_type: AccountType,
    debit_total: Money,
    credit_total: Money,
}

impl TrialBalance {
    /// 仕訳の集合から試算表を構築する。
    ///
    /// - `group_by` が空なら科目のみで集計する
    /// - `group_by` に `schema.is_aggregatable()` が `false` のキーが含まれていたら
    ///   `CoreError::NotAggregatable`
    /// - `group_by` に同じキーを複数回渡してもエラーにはならず、クラッシュもしない。
    ///   同じ値が `GroupKey` の中に重複して入るか（値を持つ明細の場合）、
    ///   両方とも欠落する（値を持たない明細の場合）だけで、行の分割（集計結果）は
    ///   重複を取り除いた場合と変わらない。ただし冗長なので、呼び出し側で
    ///   事前に重複を取り除くことを推奨する
    /// - 同一科目（・同一グループ）への記帳は集約されて1行になる（`docs/02-test-cases.md` B-03）
    /// - `chart` に存在しない科目を参照する明細があれば `CoreError::UnknownAccount`
    ///   （`entries` は本来この `chart` で検証済みのはずだが、呼び出し側が異なる
    ///   `chart` を渡した場合に備えて防御的に検証する）
    /// - `entries` が異なる通貨を混在させていたら `CoreError::CurrencyMismatch`
    /// - `entries` が空の場合、内部通貨は `Currency::JPY` にフォールバックする
    ///   （`rows()` も空になるため、この場合の残高計算そのものには影響しない）。
    ///   これは外貨換算が未実装であることによる暫定措置であり（`DECISIONS.md` D-016）、
    ///   外貨のみを扱う帳簿に対応する際に再検討する
    pub fn from_entries<'a>(
        entries: impl Iterator<Item = &'a JournalEntry>,
        chart: &ChartOfAccounts,
        schema: &TagSchema,
        group_by: &[TagKey],
    ) -> Result<Self, CoreError> {
        for key in group_by {
            if !schema.is_aggregatable(key) {
                return Err(CoreError::NotAggregatable {
                    key: key.as_str().to_string(),
                });
            }
        }

        let mut currency: Option<Currency> = None;
        let mut buckets: BTreeMap<(AccountCode, GroupKey), Bucket> = BTreeMap::new();

        for entry in entries {
            let entry_currency = entry.currency();
            match currency {
                None => currency = Some(entry_currency),
                Some(established) if established != entry_currency => {
                    return Err(CoreError::CurrencyMismatch {
                        a: established.code().to_string(),
                        b: entry_currency.code().to_string(),
                    });
                }
                _ => {}
            }

            for line in entry.lines() {
                let def = chart
                    .get(line.account())
                    .ok_or_else(|| CoreError::UnknownAccount {
                        code: line.account().as_str().to_string(),
                    })?;
                let group = build_group_key(line.tags(), group_by);
                // `AccountCode` の clone と `GroupKey` の Vec 構築が、キーが既に
                // `buckets` に存在する場合でも毎回発生する。想定規模（個人事業主の
                // 年間仕訳数）では無視できるコストだが、将来ここがホットパスになった
                // 場合は「既存キーを先に検索し、無いときだけ構築する」形に変更する余地がある。
                let bucket_key = (line.account().clone(), group);
                let bucket = buckets.entry(bucket_key).or_insert_with(|| Bucket {
                    account_type: def.account_type,
                    debit_total: Money::zero(entry_currency),
                    credit_total: Money::zero(entry_currency),
                });
                match line.side() {
                    Side::Debit => bucket.debit_total = bucket.debit_total.add(line.amount())?,
                    Side::Credit => bucket.credit_total = bucket.credit_total.add(line.amount())?,
                }
            }
        }

        let currency = currency.unwrap_or(Currency::JPY);

        let mut rows = Vec::with_capacity(buckets.len());
        for ((account, group), bucket) in buckets {
            let Bucket {
                account_type,
                debit_total,
                credit_total,
            } = bucket;
            let balance = if account_type.is_debit_normal() {
                debit_total.sub(&credit_total)?
            } else {
                credit_total.sub(&debit_total)?
            };
            rows.push(BalanceRow {
                account,
                account_type,
                group,
                debit_total,
                credit_total,
                balance,
            });
        }

        Ok(TrialBalance { rows, currency })
    }

    /// 全行を返す。
    pub fn rows(&self) -> &[BalanceRow] {
        &self.rows
    }

    /// 指定した科目の残高を返す。`group_by` を指定して構築した試算表では、
    /// 同一科目の全グループの残高を合算して返す。該当する行が無ければ `None`。
    ///
    /// # Panics
    ///
    /// 通貨不一致による panic は起こらない（同一 `TrialBalance` 内の全行は
    /// [`TrialBalance::from_entries`] の構築時に同一通貨であることが保証されている）。
    /// ただし合算対象の残高がオーバーフローするほど極端に大きい場合は panic する
    /// （`journal.rs` の `debit_total`/`credit_total` と同じ制約）。
    pub fn balance_of(&self, account: &AccountCode) -> Option<Money> {
        let balances = self
            .rows
            .iter()
            .filter(|row| &row.account == account)
            .map(|row| &row.balance);
        sum_money(balances).expect(
            "同一 TrialBalance 内の残高は同一通貨であるため、\
             合算は（オーバーフローしない限り）失敗しない",
        )
    }

    /// 借方合計と貸方合計を返す。複式簿記の恒等式により、正しく構築された
    /// 試算表では必ず一致する（不一致なら実装のバグ）。
    ///
    /// # Panics
    ///
    /// 通貨不一致による panic は起こらない（同一 `TrialBalance` 内の全行は
    /// [`TrialBalance::from_entries`] の構築時に同一通貨であることが保証されている）。
    /// ただし合算対象の金額がオーバーフローするほど極端に大きい場合は panic する
    /// （`journal.rs` の `debit_total`/`credit_total` と同じ制約）。
    pub fn totals(&self) -> (Money, Money) {
        let debit_total = sum_money(self.rows.iter().map(|row| &row.debit_total))
            .expect(
                "同一 TrialBalance 内の debit_total は同一通貨であるため、\
                 合算は（オーバーフローしない限り）失敗しない",
            )
            .unwrap_or_else(|| Money::zero(self.currency));
        let credit_total = sum_money(self.rows.iter().map(|row| &row.credit_total))
            .expect(
                "同一 TrialBalance 内の credit_total は同一通貨であるため、\
                 合算は（オーバーフローしない限り）失敗しない",
            )
            .unwrap_or_else(|| Money::zero(self.currency));
        (debit_total, credit_total)
    }

    /// 検算。[`TrialBalance::totals`] の借方合計と貸方合計が一致するかどうかを返す。
    /// `false` になることは正しく構築された試算表では起こらない（実装のバグを示す）。
    pub fn is_balanced(&self) -> bool {
        let (debit_total, credit_total) = self.totals();
        debit_total == credit_total
    }

    /// 指定した科目種別に属する全行の残高（符号付き）の合計を返す。`group_by` を
    /// 指定して構築した試算表でも、同一科目種別に属する全グループの残高を合算する。
    ///
    /// # Panics
    ///
    /// 通貨不一致による panic は起こらない（同一 `TrialBalance` 内の全行は
    /// [`TrialBalance::from_entries`] の構築時に同一通貨であることが保証されている）。
    /// ただし合算対象の残高がオーバーフローするほど極端に大きい場合は panic する
    /// （`journal.rs` の `debit_total`/`credit_total` と同じ制約）。
    pub fn total_by_type(&self, t: AccountType) -> Money {
        let balances = self
            .rows
            .iter()
            .filter(|row| row.account_type == t)
            .map(|row| &row.balance);
        sum_money(balances)
            .expect(
                "同一 TrialBalance 内の残高は同一通貨であるため、\
                 合算は（オーバーフローしない限り）失敗しない",
            )
            .unwrap_or_else(|| Money::zero(self.currency))
    }
}
