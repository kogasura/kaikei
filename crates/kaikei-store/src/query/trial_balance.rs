//! `kaikei_app::ports::TrialBalanceQuery`（試算表）の SQL 集計による実装。
//!
//! `CLAUDE.md` §6「read model は物理的に分離する」の原則どおり、`Store`/`PgTx`
//! （PR-5 本体）を一切経由せず、`sqlx::query!` で SQL から直接
//! `kaikei_app::view::BalanceRowView` を組み立てる。残高テーブルは持たず、
//! 毎回 `SUM` で集計する（`DECISIONS.md` D-007）。
//!
//! # SQLインジェクション対策（`group_by` の可変長）
//!
//! `group_by` はキー数が可変だが、SQL文そのものは固定である（キーごとに
//! 文字列連結でSQLを組み立てない）。`unnest($3::text[])` で配列パラメータを
//! バインドし、行として展開してから `jsonb_object_agg` で1つのJSONB
//! オブジェクトに戻す。`docs/03-database.md` §3 が示す「ホワイトリスト照合
//! してから文字列連結する」という素朴な対策を、**文字列連結そのものをやめる**
//! ことでより強く置き換えている。`group_by` の要素が
//! `TagSchema::is_aggregatable` を満たすかどうかの検証はユースケース側
//! （app層、PR-7）の責務であり、この実装はSQL集計に徹する
//! （`kaikei_app::ports::TrialBalanceQuery::trial_balance` の doc を参照）。
//!
//! # `SUM` の桁あふれ（`DECISIONS.md` D-033）
//!
//! `SUM(amount_minor)` はPostgreSQL上は `NUMERIC` を返すが、ワークスペースの
//! sqlx featureには `NUMERIC` のdecode先が無いため、SQL側で明示的に
//! `::BIGINT` へキャストしてから `i64` として受け取る。桁あふれで発生する
//! SQLSTATE `22003` は [`crate::error::from_sqlx_error`]（内部で
//! [`crate::sqlstate::map_sqlstate`] に委譲）が `RepoError::OutOfRange` に
//! 自動的に写像するため、ここで個別に処理する必要はない。
//!
//! # 通貨が混在した場合の扱い（`DECISIONS.md` D-042）
//!
//! `journal_lines` は行ごとに `currency`/`currency_minor_unit` を持つため、
//! 理論上は同一期間・同一科目に複数通貨の明細が混在しうる。
//! `kaikei_core::TrialBalance::from_entries` は対象の仕訳集合**全体**で通貨が
//! 単一であることを要求し（`CoreError::CurrencyMismatch`）、これは科目単位
//! ではなく集計対象全体での判定である。この実装も同じ粒度で判定する:
//! 集計結果に2種類以上の `(currency, currency_minor_unit)` の組が現れた場合、
//! [`kaikei_app::error::RepoError::Unsupported`] を返す（Phase 1の read model
//! は複数通貨の試算表表示をサポートしない）。
//!
//! # 科目が見つからない場合の扱い
//!
//! `journal_lines.account_code` に `accounts.code` への外部キー制約は無い
//! （`docs/03-database.md` §2、`crates/kaikei-store/tests/common/mod.rs` の
//! コメント）。このため `accounts` へは `LEFT JOIN` し、対応する科目が
//! 見つからない（`account_type` が `NULL`）行があれば、黙って集計から除外
//! せず [`kaikei_app::error::RepoError::Corrupt`] を返す（phase1計画 R4
//! 「無検証APIの危険面積を減らす」と同じ、「保存できないものを静かに
//! 落とさない」という規律）。

use async_trait::async_trait;
use kaikei_app::error::RepoError;
use kaikei_app::ports::TrialBalanceQuery;
use kaikei_app::view::{BalanceRowView, GroupKeyView};
use kaikei_core::{AccountCode, AccountingDate, TagKey};
use sqlx::PgPool;

use crate::convert::{account_type_from_i16, accounting_date_to_naive_date, money_from_columns};
use crate::error::from_sqlx_error;

/// [`TrialBalanceQuery`] のPostgreSQL実装。
///
/// `Store`/`PgTx`（書き込み側、PR-5本体）とは独立に自前の `PgPool` を持つ
/// （`CLAUDE.md` §6。read modelはRepositoryを経由しない）。
pub struct PgTrialBalanceQuery {
    pool: PgPool,
}

impl PgTrialBalanceQuery {
    /// 接続プールからread modelクエリを作る。
    pub fn new(pool: PgPool) -> Self {
        PgTrialBalanceQuery { pool }
    }
}

#[async_trait]
impl TrialBalanceQuery for PgTrialBalanceQuery {
    async fn trial_balance(
        &self,
        from: AccountingDate,
        to: AccountingDate,
        group_by: &[TagKey],
    ) -> Result<Vec<BalanceRowView>, RepoError> {
        let from_date = accounting_date_to_naive_date(from)?;
        let to_date = accounting_date_to_naive_date(to)?;
        let keys: Vec<String> = group_by
            .iter()
            .map(|key| key.as_str().to_string())
            .collect();

        let records = sqlx::query!(
            r#"
            SELECT
                l.account_code                                                   AS "account_code!",
                a.account_type                                                   AS "account_type?",
                l.currency                                                       AS "currency!",
                l.currency_minor_unit                                            AS "currency_minor_unit!",
                (SELECT jsonb_object_agg(k, l.tags -> k ->> 'v')
                   FROM unnest($3::text[]) AS k
                  WHERE l.tags ? k)                                              AS "group_keys: serde_json::Value",
                SUM(CASE WHEN l.side = 1 THEN l.amount_minor ELSE 0 END)::BIGINT AS "debit_total!: i64",
                SUM(CASE WHEN l.side = 2 THEN l.amount_minor ELSE 0 END)::BIGINT AS "credit_total!: i64"
            FROM journal_lines l
            JOIN journal_entries e ON e.id = l.entry_id
            LEFT JOIN accounts a   ON a.code = l.account_code
            WHERE e.entry_date BETWEEN $1 AND $2
            GROUP BY 1, 2, 3, 4, 5
            ORDER BY 1, 5
            "#,
            from_date,
            to_date,
            &keys[..],
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx_error)?;

        ensure_single_currency(
            records
                .iter()
                .map(|record| (record.currency.as_str(), record.currency_minor_unit)),
        )?;

        let mut rows = Vec::with_capacity(records.len());
        for record in records {
            let account =
                AccountCode::parse(&record.account_code).map_err(|e| RepoError::Corrupt {
                    reason: format!("保存されている勘定科目コードを復元できません: {e}"),
                })?;
            let raw_account_type = record.account_type.ok_or_else(|| RepoError::Corrupt {
                reason: format!(
                    "勘定科目コード {} が accounts テーブルに存在しません\
                     （試算表の集計元データが破損しています）",
                    record.account_code
                ),
            })?;
            let account_type = account_type_from_i16(raw_account_type)?;
            let group = group_keys_from_json(record.group_keys)?;
            let debit_total = money_from_columns(
                record.debit_total,
                &record.currency,
                record.currency_minor_unit,
            )?;
            let credit_total = money_from_columns(
                record.credit_total,
                &record.currency,
                record.currency_minor_unit,
            )?;
            let balance = if account_type.is_debit_normal() {
                debit_total.sub(&credit_total)
            } else {
                credit_total.sub(&debit_total)
            }
            .map_err(|e| RepoError::OutOfRange {
                reason: format!("残高の計算が範囲を超えました: {e}"),
            })?;

            rows.push(BalanceRowView {
                account,
                account_type,
                group,
                debit_total,
                credit_total,
                balance,
            });
        }

        Ok(rows)
    }
}

/// 集計結果の `group_keys`（JSONBオブジェクト。該当キーが無ければ `NULL`）を
/// [`GroupKeyView`] に変換する。
fn group_keys_from_json(value: Option<serde_json::Value>) -> Result<GroupKeyView, RepoError> {
    let Some(value) = value else {
        return Ok(GroupKeyView::new());
    };
    let obj = value.as_object().ok_or_else(|| RepoError::Corrupt {
        reason: format!("group_by の集計結果がJSONオブジェクトではありません: {value}"),
    })?;

    let mut group = GroupKeyView::new();
    for (key, v) in obj {
        let value_str = v.as_str().ok_or_else(|| RepoError::Corrupt {
            reason: format!("group_by キー \"{key}\" の値が文字列ではありません: {v}"),
        })?;
        group.insert(key.clone(), value_str.to_string());
    }
    Ok(group)
}

/// 集計結果全体で通貨が単一であることを確認する。
///
/// `kaikei_core::TrialBalance::from_entries` は対象の仕訳集合全体で通貨が
/// 単一であることを要求するため、それと同じ粒度で検証する（モジュールdocの
/// 「通貨が混在した場合の扱い」を参照）。
fn ensure_single_currency<'a>(
    mut pairs: impl Iterator<Item = (&'a str, i16)>,
) -> Result<(), RepoError> {
    let Some(first) = pairs.next() else {
        return Ok(());
    };
    for pair in pairs {
        if pair != first {
            return Err(RepoError::Unsupported {
                reason: format!(
                    "試算表の集計対象期間に複数の通貨が混在しています（{} と {}）。\
                     Phase 1のread modelは単一通貨のみをサポートします。\
                     期間や科目を絞り込んで再実行してください",
                    first.0, pair.0
                ),
            });
        }
    }
    Ok(())
}
