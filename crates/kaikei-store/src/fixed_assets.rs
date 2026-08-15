//! 固定資産台帳（`0012_fixed_assets.sql`、`DECISIONS.md` D-103）。
//!
//! **DELETE は実装しない。** 資産を帳簿から外すのは除却（`disposed_on` を
//! 埋める）であって、台帳から消すことではない。消せると、過去の年度の
//! 償却費がどの資産のものだったか辿れなくなる。DB 権限でも与えていない。

use crate::convert::{accounting_date_to_naive_date, naive_date_to_accounting_date};
use crate::error::from_sqlx_error;
use crate::store::PgTx;
use async_trait::async_trait;
use chrono::NaiveDate;
use kaikei_app::error::RepoError;
use kaikei_app::ports::{FixedAssetRepo, FixedAssetRow};
use kaikei_core::{AccountCode, Currency, Money};

/// DB の1行。
type Row = (
    String,            // id
    String,            // name
    String,            // account_code
    NaiveDate,         // acquired_on
    i64,               // acquisition_cost
    String,            // currency
    i16,               // currency_minor_unit
    i16,               // method
    Option<i16>,       // useful_life_years
    Option<String>,    // business_ratio（NUMERIC を text にして受ける）
    Option<NaiveDate>, // disposed_on
    Option<String>,    // note
);

#[async_trait]
impl FixedAssetRepo for PgTx<'_> {
    async fn list_fixed_assets(&mut self) -> Result<Vec<FixedAssetRow>, RepoError> {
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT id::text, name, account_code, acquired_on, acquisition_cost, currency, currency_minor_unit, \
                    method, useful_life_years, business_ratio::text, disposed_on, note \
             FROM fixed_assets ORDER BY acquired_on, id",
        )
        .fetch_all(self.conn())
        .await
        .map_err(from_sqlx_error)?;

        rows.into_iter().map(to_row).collect()
    }

    /// # 既存行は触らない
    ///
    /// `ON CONFLICT (id) DO NOTHING`。台帳の編集は別の操作であって、
    /// 投入経路が黙って書き換えてよいものではない
    /// （`ChartWriteRepo` / `CounterpartyWriteRepo` と同じ規律）。
    async fn insert_fixed_assets(&mut self, list: &[FixedAssetRow]) -> Result<usize, RepoError> {
        if list.is_empty() {
            return Ok(0);
        }

        let mut inserted = 0usize;
        for asset in list {
            // 1件ずつ入れる。台帳は年に数件しか増えないので、`UNNEST` で
            // まとめる価値がない（明細の一括 INSERT とは規模が違う）。
            let result = sqlx::query(
                "INSERT INTO fixed_assets \
                 (id, name, account_code, acquired_on, acquisition_cost, currency, currency_minor_unit, \
                  method, useful_life_years, business_ratio, disposed_on, note) \
                 VALUES ($1::uuid, $2, $3, $4, $5, $6, $7, $8, $9, $10::numeric, $11, $12) \
                 ON CONFLICT (id) DO NOTHING",
            )
            .bind(&asset.id)
            .bind(&asset.name)
            .bind(asset.account.as_str())
            .bind(accounting_date_to_naive_date(asset.acquired_on)?)
            .bind(i64::try_from(asset.acquisition_cost.minor()).map_err(|_| {
                RepoError::Corrupt {
                    reason: format!(
                        "取得価額が大きすぎます: {}",
                        asset.acquisition_cost.to_display_string()
                    ),
                }
            })?)
            .bind(asset.acquisition_cost.currency().code())
            .bind(i16::from(asset.acquisition_cost.currency().minor_unit()))
            .bind(asset.method)
            .bind(asset.useful_life_years)
            .bind(asset.business_ratio.as_deref())
            .bind(
                asset
                    .disposed_on
                    .map(accounting_date_to_naive_date)
                    .transpose()?,
            )
            .bind(&asset.note)
            .execute(self.conn())
            .await
            .map_err(from_sqlx_error)?;
            inserted += result.rows_affected() as usize;
        }
        Ok(inserted)
    }
}

fn to_row(r: Row) -> Result<FixedAssetRow, RepoError> {
    let (id, name, account, acquired, cost, code, minor_unit, method, life, ratio, disposed, note) =
        r;
    let minor_unit = u8::try_from(minor_unit).map_err(|_| RepoError::Corrupt {
        reason: format!("保存されている通貨の最小単位が不正です: {minor_unit}"),
    })?;
    let currency = Currency::new(&code, minor_unit).map_err(|e| RepoError::Corrupt {
        reason: format!("保存されている通貨コードが不正です: {e}"),
    })?;
    Ok(FixedAssetRow {
        id,
        name,
        account: AccountCode::parse(&account).map_err(|e| RepoError::Corrupt {
            reason: format!("保存されている科目コードが不正です: {e}"),
        })?,
        acquired_on: naive_date_to_accounting_date(acquired)?,
        acquisition_cost: Money::from_minor(i128::from(cost), currency),
        method,
        useful_life_years: life,
        // 表示用の10進文字列で返す（`kaikei-app` は Decimal を公開しない）。
        // `NUMERIC(5,4)` なので "0.8000" のように末尾の0が付く。
        // `Ratio::parse_fraction` はこれをそのまま解釈できる。
        business_ratio: ratio,
        disposed_on: disposed.map(naive_date_to_accounting_date).transpose()?,
        note,
    })
}
