//! core の値オブジェクトと DB 表現との相互変換。
//!
//! PR-5 本体と PR-6（read model）の両方が参照する共有基盤（`DECISIONS.md`
//! D-034）。
//!
//! # R7: 型の境界での無言のデータ破損を防ぐ
//!
//! `amount_minor` は core が `i128`・DB が `BIGINT`（`i64`）、`entry_no` は
//! core が `u32`・DB が `INTEGER`（`i32`）で、それぞれ幅が異なる。
//! **`as` によるキャストは一切使わない。** 必ず `i64::try_from` /
//! `i32::try_from` 等を経由し、失敗は [`RepoError::OutOfRange`] を返す
//! （phase1計画 R7。`DECISIONS.md` D-020 で修正済みの `Money::neg`/
//! `Money::abs`（release ビルドで無言に切り詰まる欠陥）と同じ欠陥クラスを
//! store 層で再発させない）。

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use kaikei_app::error::RepoError;
use kaikei_core::{AccountType, AccountingDate, Currency, EntryNumber, Money, Side, Timestamp};

// ---- AccountingDate ⇄ chrono::NaiveDate ----

/// `AccountingDate`（取引日）を `entry_date`（`DATE` 列）に対応する
/// `chrono::NaiveDate` に変換する。
///
/// `AccountingDate` の年は `i32` の全域を取りうるが、`chrono::NaiveDate` が
/// 表現できる範囲（西暦およそ ±26万年）は遥かに狭い。範囲外は panic せず
/// [`RepoError::OutOfRange`] を返す。
pub fn accounting_date_to_naive_date(date: AccountingDate) -> Result<NaiveDate, RepoError> {
    NaiveDate::from_ymd_opt(date.year(), u32::from(date.month()), u32::from(date.day())).ok_or_else(
        || RepoError::OutOfRange {
            reason: format!(
                "取引日 {} は日付として保存できる範囲を超えています",
                date.to_iso_string()
            ),
        },
    )
}

/// `entry_date`（`DATE` 列）から読み出した `chrono::NaiveDate` を
/// `AccountingDate` に変換する。
///
/// `NaiveDate` は常に暦として有効な年月日を保持するため、この変換が失敗する
/// ことは通常無い。それでも失敗した場合は panic せず [`RepoError::Corrupt`]
/// （復元直前の再検証で検出した不整合）を返す。
pub fn naive_date_to_accounting_date(date: NaiveDate) -> Result<AccountingDate, RepoError> {
    let corrupt = || RepoError::Corrupt {
        reason: format!("保存されている日付を復元できません: {date}"),
    };
    let month = u8::try_from(date.month()).map_err(|_| corrupt())?;
    let day = u8::try_from(date.day()).map_err(|_| corrupt())?;
    AccountingDate::new(date.year(), month, day).map_err(|_| corrupt())
}

// ---- Timestamp ⇄ chrono::DateTime<Utc> ----

/// `Timestamp`（記帳時刻、ナノ秒精度）を `recorded_at`（`TIMESTAMPTZ` 列、
/// マイクロ秒精度）に対応する `chrono::DateTime<Utc>` に変換する。
///
/// # マイクロ秒への丸めについて（phase1計画 R8 / `DECISIONS.md` D-030 / D-036）
///
/// **ここでは丸めない。** `kaikei-app::clock::SystemClock` が記帳時刻を
/// 生成する時点で既にマイクロ秒粒度に丸めているため（D-030）、通常の記帳
/// 経路でこの関数に渡される `Timestamp` は常にマイクロ秒境界に揃っている。
/// この関数自身はナノ秒精度をそのまま保持して変換するので、仮にマイクロ秒に
/// 揃っていない `Timestamp`（テスト用の `FixedClock` 等）を渡しても、ここで
/// 値を破壊することはない。実際にマイクロ秒未満の端数が失われるのは `sqlx`
/// が `chrono::DateTime<Utc>` を `TIMESTAMPTZ` の実際のワイヤ形式へエンコード
/// する段階（DB 列の型そのものが持つ精度）であり、そこは store の変換コードの
/// 責務の外にある。往復同値性を検証する proptest（PR-5 本体
/// `tests/round_trip.rs`）はマイクロ秒に揃った値のみを生成対象にすること
/// （`SystemClock` が生成する値の性質を反映するため。D-036 参照）。
///
/// `chrono::DateTime::timestamp_nanos_opt()` は 2262 年で `None` を返す
/// （内部で `i64` ナノ秒に一本化するため）ため使わず、秒とナノ秒端数に
/// 分解してから `DateTime::from_timestamp` に渡す。
///
/// # Errors
///
/// 秒部分が `i64` に収まらない、または `chrono` の表現可能範囲を超える場合は
/// [`RepoError::OutOfRange`]。
pub fn timestamp_to_datetime(ts: Timestamp) -> Result<DateTime<Utc>, RepoError> {
    let nanos = ts.as_unix_nanos();
    let out_of_range = || RepoError::OutOfRange {
        reason: format!("記帳時刻（{nanos}ns）は保存できる範囲を超えています"),
    };

    let secs = nanos.div_euclid(1_000_000_000);
    let subsec_nanos = nanos.rem_euclid(1_000_000_000);

    let secs = i64::try_from(secs).map_err(|_| out_of_range())?;
    // `rem_euclid(1_000_000_000)` の結果は必ず 0..1_000_000_000 に収まるため
    // 理論上失敗しないが、`as` によるキャストを避けるために `try_from` を経由する
    // （R7 と同じ規律）。
    let subsec_nanos = u32::try_from(subsec_nanos).map_err(|_| out_of_range())?;

    DateTime::<Utc>::from_timestamp(secs, subsec_nanos).ok_or_else(out_of_range)
}

/// `recorded_at`（`TIMESTAMPTZ` 列）から読み出した `chrono::DateTime<Utc>` を
/// `Timestamp` に変換する。
///
/// `chrono::DateTime<Utc>` が表現できる範囲は `i128` のナノ秒に対して遥かに
/// 狭いため、この変換が桁あふれすることは無い（`i128::from` による安全な
/// 拡大変換のみで完結する）。
pub fn datetime_to_timestamp(dt: DateTime<Utc>) -> Timestamp {
    let secs = i128::from(dt.timestamp());
    let subsec_nanos = i128::from(dt.timestamp_subsec_nanos());
    Timestamp::from_unix_nanos(secs * 1_000_000_000 + subsec_nanos)
}

// ---- Money ⇄ (amount_minor, currency, currency_minor_unit) ----

/// `Money` を `journal_lines` の3列（`amount_minor BIGINT`, `currency CHAR(3)`,
/// `currency_minor_unit SMALLINT`）に対応するタプルに変換する。
///
/// `Money` の内部表現は `i128` だが `amount_minor` 列は `BIGINT`（`i64`）。
/// 幅が異なるため `i64::try_from` を経由し、収まらない場合は
/// [`RepoError::OutOfRange`]（`as i64` を使うと release ビルドで無言に
/// 切り詰まる。phase1計画 R7）。
pub fn money_to_columns(money: &Money) -> Result<(i64, String, i16), RepoError> {
    let amount_minor = i64::try_from(money.minor()).map_err(|_| RepoError::OutOfRange {
        reason: format!(
            "金額 {} は保存できる範囲（BIGINTの上限）を超えています",
            money.to_display_string()
        ),
    })?;
    let currency = money.currency().code().to_string();
    let currency_minor_unit = i16::from(money.currency().minor_unit());
    Ok((amount_minor, currency, currency_minor_unit))
}

/// `journal_lines` の3列から読み出した値を `Money` に変換する。
///
/// `currency_minor_unit` が `u8`（`Currency::new` の引数）に収まらない、
/// または `currency` が不正なコード形式の場合は [`RepoError::Corrupt`]
/// （`CHECK (currency_minor_unit BETWEEN 0 AND 18)` により通常は起こらないが、
/// 復元直前の再検証として防御する）。
pub fn money_from_columns(
    amount_minor: i64,
    currency: &str,
    currency_minor_unit: i16,
) -> Result<Money, RepoError> {
    let minor_unit = u8::try_from(currency_minor_unit).map_err(|_| RepoError::Corrupt {
        reason: format!("保存されている通貨の小数桁数が不正です: {currency_minor_unit}"),
    })?;
    let currency = Currency::new(currency, minor_unit).map_err(|e| RepoError::Corrupt {
        reason: format!("保存されている通貨情報を復元できません: {e}"),
    })?;
    Ok(Money::from_minor(i128::from(amount_minor), currency))
}

// ---- Side ⇄ i16 ----

/// `Side` を `journal_lines.side`（`SMALLINT CHECK (side IN (1, 2))`）に
/// 対応する `i16` に変換する（1=借方, 2=貸方。`migrations/0003_journal.sql`
/// のコメントと一致させる）。
pub fn side_to_i16(side: Side) -> i16 {
    match side {
        Side::Debit => 1,
        Side::Credit => 2,
    }
}

/// `journal_lines.side` から読み出した `i16` を `Side` に変換する。
///
/// `1`/`2` 以外は [`RepoError::Corrupt`]（DB の `CHECK (side IN (1, 2))` に
/// より通常は起こらないが、復元直前の再検証として防御する）。
pub fn side_from_i16(value: i16) -> Result<Side, RepoError> {
    match value {
        1 => Ok(Side::Debit),
        2 => Ok(Side::Credit),
        other => Err(RepoError::Corrupt {
            reason: format!("side の値が不正です（1=借方, 2=貸方 以外）: {other}"),
        }),
    }
}

// ---- AccountType ⇄ i16 ----

/// `AccountType` を `accounts.account_type`（`SMALLINT`。コメント
/// `1=Asset .. 5=Expense`）に対応する `i16` に変換する。core の宣言順
/// （Asset, Liability, Equity, Revenue, Expense）と一致させる。
pub fn account_type_to_i16(account_type: AccountType) -> i16 {
    match account_type {
        AccountType::Asset => 1,
        AccountType::Liability => 2,
        AccountType::Equity => 3,
        AccountType::Revenue => 4,
        AccountType::Expense => 5,
    }
}

/// `accounts.account_type` から読み出した `i16` を `AccountType` に変換する。
///
/// `1`〜`5` 以外は [`RepoError::Corrupt`]（`accounts.account_type` に
/// `CHECK` 制約は無いため、ここでの再検証が唯一の防御になる）。
pub fn account_type_from_i16(value: i16) -> Result<AccountType, RepoError> {
    match value {
        1 => Ok(AccountType::Asset),
        2 => Ok(AccountType::Liability),
        3 => Ok(AccountType::Equity),
        4 => Ok(AccountType::Revenue),
        5 => Ok(AccountType::Expense),
        other => Err(RepoError::Corrupt {
            reason: format!("account_type の値が不正です（1〜5の範囲外）: {other}"),
        }),
    }
}

// ---- EntryNumber ⇄ i32 ----

/// `EntryNumber`（`u32`）を `journal_entries.entry_no`（`INTEGER` = `i32`）に
/// 変換する。`u32` は `i32` より広いため無条件には収まらない（R7）。
pub fn entry_no_to_i32(entry_no: EntryNumber) -> Result<i32, RepoError> {
    i32::try_from(entry_no.as_u32()).map_err(|_| RepoError::OutOfRange {
        reason: format!(
            "仕訳番号 {} は保存できる範囲（INTEGERの上限 {}）を超えています",
            entry_no.as_u32(),
            i32::MAX
        ),
    })
}

/// `journal_entries.entry_no` から読み出した `i32` を `EntryNumber` に変換する。
///
/// 負値は [`RepoError::Corrupt`]（`journal_entries.entry_no` に `CHECK`
/// 制約は無いため、ここでの再検証が唯一の防御になる）。
pub fn entry_no_from_i32(value: i32) -> Result<EntryNumber, RepoError> {
    let raw = u32::try_from(value).map_err(|_| RepoError::Corrupt {
        reason: format!("保存されている仕訳番号が負の値です: {value}"),
    })?;
    Ok(EntryNumber::new(raw))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // ---- AccountingDate ⇄ NaiveDate ----

    #[test]
    fn accounting_date_round_trips_through_naive_date() {
        let date = AccountingDate::new(2026, 4, 15).unwrap();
        let naive = accounting_date_to_naive_date(date).unwrap();
        assert_eq!(naive_date_to_accounting_date(naive).unwrap(), date);
    }

    #[test]
    fn accounting_date_leap_day_round_trips() {
        let date = AccountingDate::new(2024, 2, 29).unwrap();
        let naive = accounting_date_to_naive_date(date).unwrap();
        assert_eq!(naive_date_to_accounting_date(naive).unwrap(), date);
    }

    // R7: chronoの表現範囲(西暦およそ±26万年)を超えるAccountingDateは
    // 無言のデータ破損ではなくOutOfRangeを返す。
    #[test]
    fn accounting_date_year_beyond_chrono_range_is_out_of_range() {
        let date = AccountingDate::new(i32::MAX, 1, 1).unwrap();
        assert!(matches!(
            accounting_date_to_naive_date(date),
            Err(RepoError::OutOfRange { .. })
        ));
    }

    #[test]
    fn accounting_date_negative_year_beyond_chrono_range_is_out_of_range() {
        let date = AccountingDate::new(i32::MIN, 1, 1).unwrap();
        assert!(matches!(
            accounting_date_to_naive_date(date),
            Err(RepoError::OutOfRange { .. })
        ));
    }

    // ---- Timestamp ⇄ DateTime<Utc> ----

    #[test]
    fn timestamp_round_trips_through_datetime_at_microsecond_boundary() {
        let ts = Timestamp::from_unix_nanos(1_700_000_000_123_000);
        let dt = timestamp_to_datetime(ts).unwrap();
        assert_eq!(datetime_to_timestamp(dt), ts);
    }

    // R8: 変換自体はナノ秒未満を切り捨てない（丸めはSystemClock側の責務。D-036）。
    #[test]
    fn timestamp_round_trips_even_with_sub_microsecond_remainder() {
        let ts = Timestamp::from_unix_nanos(1_700_000_000_123_456);
        let dt = timestamp_to_datetime(ts).unwrap();
        assert_eq!(datetime_to_timestamp(dt), ts);
    }

    #[test]
    fn timestamp_before_epoch_round_trips() {
        let ts = Timestamp::from_unix_nanos(-1_000_000_000);
        let dt = timestamp_to_datetime(ts).unwrap();
        assert_eq!(datetime_to_timestamp(dt), ts);
    }

    #[test]
    fn timestamp_zero_round_trips() {
        let ts = Timestamp::from_unix_nanos(0);
        let dt = timestamp_to_datetime(ts).unwrap();
        assert_eq!(datetime_to_timestamp(dt), ts);
    }

    // R7: i128::MAX/MIN ナノ秒はchronoの表現範囲を大きく超えるためOutOfRange。
    #[test]
    fn timestamp_i128_max_is_out_of_range() {
        let ts = Timestamp::from_unix_nanos(i128::MAX);
        assert!(matches!(
            timestamp_to_datetime(ts),
            Err(RepoError::OutOfRange { .. })
        ));
    }

    #[test]
    fn timestamp_i128_min_is_out_of_range() {
        let ts = Timestamp::from_unix_nanos(i128::MIN);
        assert!(matches!(
            timestamp_to_datetime(ts),
            Err(RepoError::OutOfRange { .. })
        ));
    }

    // ---- Money ⇄ (i64, String, i16) ----

    #[test]
    fn money_round_trips_through_columns() {
        let money = Money::from_minor(123_456, Currency::USD);
        let (amount_minor, currency, minor_unit) = money_to_columns(&money).unwrap();
        assert_eq!(amount_minor, 123_456);
        assert_eq!(currency, "USD");
        assert_eq!(minor_unit, 2);
        assert_eq!(
            money_from_columns(amount_minor, &currency, minor_unit).unwrap(),
            money
        );
    }

    #[test]
    fn money_negative_amount_round_trips() {
        let money = Money::from_minor(-500, Currency::JPY);
        let (amount_minor, currency, minor_unit) = money_to_columns(&money).unwrap();
        assert_eq!(
            money_from_columns(amount_minor, &currency, minor_unit).unwrap(),
            money
        );
    }

    // R7: i64::MAXを超えるMoney（i128の範囲）は `as i64` による無言の切り詰め
    // ではなくOutOfRangeを返す。
    #[test]
    fn money_beyond_i64_max_is_out_of_range() {
        let money = Money::from_minor(i128::from(i64::MAX) + 1, Currency::JPY);
        assert!(matches!(
            money_to_columns(&money),
            Err(RepoError::OutOfRange { .. })
        ));
    }

    #[test]
    fn money_beyond_i64_min_is_out_of_range() {
        let money = Money::from_minor(i128::from(i64::MIN) - 1, Currency::JPY);
        assert!(matches!(
            money_to_columns(&money),
            Err(RepoError::OutOfRange { .. })
        ));
    }

    #[test]
    fn money_at_i64_boundaries_succeeds() {
        let max = Money::from_minor(i128::from(i64::MAX), Currency::JPY);
        assert!(money_to_columns(&max).is_ok());
        let min = Money::from_minor(i128::from(i64::MIN), Currency::JPY);
        assert!(money_to_columns(&min).is_ok());
    }

    #[test]
    fn money_from_columns_rejects_minor_unit_beyond_u8_range() {
        // currency_minor_unit は SMALLINT（i16）だが Currency::new は u8 を要求する。
        // i16 の値域は u8 よりはるかに広いため、256以上は u8 に収まらずCorruptになる。
        let result = money_from_columns(1000, "JPY", 256);
        assert!(matches!(result, Err(RepoError::Corrupt { .. })));
    }

    // ---- Side ⇄ i16 ----

    #[test]
    fn side_round_trips() {
        assert_eq!(
            side_from_i16(side_to_i16(Side::Debit)).unwrap(),
            Side::Debit
        );
        assert_eq!(
            side_from_i16(side_to_i16(Side::Credit)).unwrap(),
            Side::Credit
        );
    }

    #[test]
    fn side_matches_check_constraint_values() {
        assert_eq!(side_to_i16(Side::Debit), 1);
        assert_eq!(side_to_i16(Side::Credit), 2);
    }

    #[test]
    fn side_from_invalid_value_is_corrupt() {
        for invalid in [0i16, 3, -1, i16::MAX, i16::MIN] {
            assert!(matches!(
                side_from_i16(invalid),
                Err(RepoError::Corrupt { .. })
            ));
        }
    }

    // ---- AccountType ⇄ i16 ----

    #[test]
    fn account_type_round_trips() {
        for at in [
            AccountType::Asset,
            AccountType::Liability,
            AccountType::Equity,
            AccountType::Revenue,
            AccountType::Expense,
        ] {
            assert_eq!(account_type_from_i16(account_type_to_i16(at)).unwrap(), at);
        }
    }

    #[test]
    fn account_type_matches_comment_in_migration() {
        assert_eq!(account_type_to_i16(AccountType::Asset), 1);
        assert_eq!(account_type_to_i16(AccountType::Expense), 5);
    }

    #[test]
    fn account_type_from_invalid_value_is_corrupt() {
        for invalid in [0i16, 6, -1, i16::MAX, i16::MIN] {
            assert!(matches!(
                account_type_from_i16(invalid),
                Err(RepoError::Corrupt { .. })
            ));
        }
    }

    // ---- EntryNumber ⇄ i32 ----

    #[test]
    fn entry_no_round_trips() {
        let entry_no = EntryNumber::new(42);
        let value = entry_no_to_i32(entry_no).unwrap();
        assert_eq!(entry_no_from_i32(value).unwrap(), entry_no);
    }

    // R7: u32::MAXはi32::MAXを超えるため、`as i32` による無言の切り詰めではなく
    // OutOfRangeを返す。
    #[test]
    fn entry_no_u32_max_is_out_of_range() {
        let entry_no = EntryNumber::new(u32::MAX);
        assert!(matches!(
            entry_no_to_i32(entry_no),
            Err(RepoError::OutOfRange { .. })
        ));
    }

    #[test]
    fn entry_no_at_i32_max_succeeds() {
        let entry_no = EntryNumber::new(u32::try_from(i32::MAX).unwrap());
        assert!(entry_no_to_i32(entry_no).is_ok());
    }

    #[test]
    fn entry_no_just_beyond_i32_max_is_out_of_range() {
        let entry_no = EntryNumber::new(u32::try_from(i32::MAX).unwrap() + 1);
        assert!(matches!(
            entry_no_to_i32(entry_no),
            Err(RepoError::OutOfRange { .. })
        ));
    }

    #[test]
    fn entry_no_from_negative_i32_is_corrupt() {
        assert!(matches!(
            entry_no_from_i32(-1),
            Err(RepoError::Corrupt { .. })
        ));
    }

    // ---- プロパティテスト（Phase 0の教訓: 境界値をprop_oneof!で明示的に含める）----

    fn any_money_minor() -> impl Strategy<Value = i128> {
        prop_oneof![
            8 => -1_000_000_000_000i128..=1_000_000_000_000i128,
            1 => Just(i128::from(i64::MAX)),
            1 => Just(i128::from(i64::MIN)),
            1 => Just(i128::from(i64::MAX) + 1),
            1 => Just(i128::from(i64::MIN) - 1),
            1 => Just(i128::MAX),
            1 => Just(i128::MIN),
            1 => Just(0i128),
        ]
    }

    fn any_entry_no() -> impl Strategy<Value = u32> {
        let i32_max_as_u32 = u32::try_from(i32::MAX).expect("i32::MAX は u32 に収まる");
        prop_oneof![
            8 => 0u32..=1_000_000u32,
            1 => Just(i32_max_as_u32),
            1 => Just(i32_max_as_u32 + 1),
            1 => Just(u32::MAX),
            1 => Just(0u32),
        ]
    }

    proptest! {
        #[test]
        fn money_to_columns_never_silently_truncates(minor in any_money_minor()) {
            let money = Money::from_minor(minor, Currency::JPY);
            match money_to_columns(&money) {
                Ok((amount_minor, _, _)) => prop_assert_eq!(i128::from(amount_minor), minor),
                Err(RepoError::OutOfRange { .. }) => {
                    prop_assert!(minor > i128::from(i64::MAX) || minor < i128::from(i64::MIN));
                }
                Err(other) => prop_assert!(false, "予期しないエラー: {other:?}"),
            }
        }

        #[test]
        fn entry_no_to_i32_never_silently_truncates(raw in any_entry_no()) {
            let entry_no = EntryNumber::new(raw);
            match entry_no_to_i32(entry_no) {
                Ok(value) => prop_assert_eq!(u32::try_from(value).unwrap(), raw),
                Err(RepoError::OutOfRange { .. }) => {
                    prop_assert!(raw > u32::try_from(i32::MAX).unwrap());
                }
                Err(other) => prop_assert!(false, "予期しないエラー: {other:?}"),
            }
        }
    }
}
