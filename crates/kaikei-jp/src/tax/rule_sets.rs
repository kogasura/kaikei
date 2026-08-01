//! 適用期間の異なる複数の税区分マスタの集合（[`TaxRuleSets`]）。取引日で
//! どのマスタを使うか選ぶ（`CLAUDE.md` §7「年度別データの選択は取引日で行う」）。

use crate::error::JpError;
use crate::tax::table::TaxCategoryTable;
use kaikei_core::AccountingDate;

/// 適用期間の異なる複数のマスタを保持し、取引日で引ける形にしたもの。
///
/// 構築時に適用期間の重なりを検証する（`DECISIONS.md` D-054）。構築に
/// 成功した `TaxRuleSets` に対する [`TaxRuleSets::for_date`] は純粋な参照
/// だけで完結し、I/O を行わない（`CLAUDE.md` §3 / `docs/04-jp-tax.md` §2）。
#[derive(Debug, Clone)]
pub struct TaxRuleSets {
    tables: Vec<TaxCategoryTable>,
}

impl TaxRuleSets {
    /// `kaikei-jp-data` の埋め込みマスタ全件
    /// （[`kaikei_jp_data::TAX_CATEGORY_SOURCES`]）から構築する。
    pub fn from_embedded() -> Result<Self, JpError> {
        let tables = kaikei_jp_data::TAX_CATEGORY_SOURCES
            .iter()
            .map(|source| TaxCategoryTable::from_embedded(*source))
            .collect::<Result<Vec<_>, _>>()?;
        Self::new(tables)
    }

    /// 任意のマスタ群から構築する（ユーザーが自分の YAML に差し替える経路。
    /// `TaxCategoryTable::from_path` 等で読み込んだ値を渡す）。
    ///
    /// 適用期間が重なるマスタの組が1つでもあれば `JpError::OverlappingTaxPeriods`
    /// を返す。重なりをどちらか一方の「勝ち」で無言に解決すると、後から
    /// マスタを追加した人が意図せず既存の取引日の解釈を変えてしまいうる
    /// ため、ロード時点で必ず人間に修正させる（`DECISIONS.md` D-054）。
    pub fn new(tables: Vec<TaxCategoryTable>) -> Result<Self, JpError> {
        for i in 0..tables.len() {
            for j in (i + 1)..tables.len() {
                if tables[i].overlaps(&tables[j]) {
                    return Err(JpError::OverlappingTaxPeriods {
                        first_label: tables[i].label().to_string(),
                        first_range: tables[i].range_display(),
                        second_label: tables[j].label().to_string(),
                        second_range: tables[j].range_display(),
                    });
                }
            }
        }
        Ok(TaxRuleSets { tables })
    }

    /// 取引日に適用されるマスタを引く。
    ///
    /// どのマスタの適用期間にも入らない取引日は `None`（正常な戻り値。
    /// `DECISIONS.md` D-055）。呼び出し側（`JpTaxPolicy`）が
    /// `kaikei_policy::PolicyError::NoApplicableRuleSet` に写像することを想定する。
    pub fn for_date(&self, date: AccountingDate) -> Option<&TaxCategoryTable> {
        self.tables.iter().find(|table| table.contains(date))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tax::settings::TaxSettingsDefaults;
    use crate::tax::{RoundingUnit, TaxMode};
    use kaikei_core::RoundMode;
    use proptest::prelude::*;

    fn defaults() -> TaxSettingsDefaults {
        TaxSettingsDefaults {
            tax_mode: TaxMode::Exclusive,
            rounding: RoundMode::Floor,
            rounding_unit: RoundingUnit::Line,
        }
    }

    fn date(year: i32, month: u8, day: u8) -> AccountingDate {
        AccountingDate::new(year, month, day).unwrap()
    }

    fn table(
        label: &str,
        applies_from: AccountingDate,
        applies_to: Option<AccountingDate>,
    ) -> TaxCategoryTable {
        TaxCategoryTable::new(
            label.to_string(),
            applies_from,
            applies_to,
            defaults(),
            vec![],
        )
        .unwrap()
    }

    #[test]
    fn from_embedded_loads_the_bundled_master_without_error() {
        let rule_sets = TaxRuleSets::from_embedded().unwrap();
        // 2026.yaml が適用開始日として持つ日付（実データ）を引ける。
        assert!(rule_sets.for_date(date(2026, 4, 1)).is_some());
    }

    #[test]
    fn for_date_returns_none_when_no_table_applies() {
        let rule_sets = TaxRuleSets::new(vec![table(
            "2026",
            date(2026, 1, 1),
            Some(date(2026, 12, 31)),
        )])
        .unwrap();

        assert!(rule_sets.for_date(date(2025, 12, 31)).is_none());
        assert!(rule_sets.for_date(date(2027, 1, 1)).is_none());
    }

    #[test]
    fn for_date_picks_the_open_ended_table_beyond_the_finite_one() {
        let finite = table("2025", date(2025, 1, 1), Some(date(2025, 12, 31)));
        let open_ended = table("2026-", date(2026, 1, 1), None);
        let rule_sets = TaxRuleSets::new(vec![finite, open_ended]).unwrap();

        assert_eq!(
            rule_sets.for_date(date(2025, 6, 1)).unwrap().label(),
            "2025"
        );
        assert_eq!(
            rule_sets.for_date(date(2026, 1, 1)).unwrap().label(),
            "2026-"
        );
        assert_eq!(
            rule_sets.for_date(date(2099, 1, 1)).unwrap().label(),
            "2026-"
        );
    }

    #[test]
    fn new_rejects_overlapping_finite_periods() {
        let a = table("A", date(2026, 1, 1), Some(date(2026, 6, 30)));
        let b = table("B", date(2026, 6, 1), Some(date(2026, 12, 31)));

        let err = TaxRuleSets::new(vec![a, b]).unwrap_err();
        match err {
            JpError::OverlappingTaxPeriods {
                first_label,
                second_label,
                ..
            } => {
                assert_eq!(first_label, "A");
                assert_eq!(second_label, "B");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn new_rejects_open_ended_period_overlapping_a_later_finite_period() {
        let open_ended = table("open", date(2020, 1, 1), None);
        let later = table("later", date(2026, 1, 1), Some(date(2026, 12, 31)));

        assert!(TaxRuleSets::new(vec![open_ended, later]).is_err());
    }

    #[test]
    fn new_accepts_adjacent_non_overlapping_periods() {
        let a = table("A", date(2026, 1, 1), Some(date(2026, 3, 31)));
        let b = table("B", date(2026, 4, 1), Some(date(2026, 12, 31)));
        assert!(TaxRuleSets::new(vec![a, b]).is_ok());
    }

    /// 3件以上あるとき、**隣接していないペア**の重なりも検出すること。
    ///
    /// 総当たり（`for i, for j in i+1..`）でなく隣接ペアだけを見る実装に
    /// 退化したら、このテストが落ちる（レビュー指摘）。
    #[test]
    fn new_rejects_overlap_between_non_adjacent_tables() {
        let a = table("A", date(2024, 1, 1), Some(date(2024, 12, 31)));
        let b = table("B", date(2025, 1, 1), Some(date(2025, 12, 31))); // A/C と重ならない
        let c = table("C", date(2024, 6, 1), Some(date(2024, 12, 31))); // A とのみ重なる

        let err = TaxRuleSets::new(vec![a, b, c])
            .expect_err("A と C の重なりを検出すべき（隣接していないペア）");
        let message = err.to_string();
        assert!(
            message.contains("\"A\"") && message.contains("\"C\""),
            "重なっている当事者（A と C）を名指しすること: {message}"
        );
    }

    /// `applies_to: null`（無期限）のマスタが2つあれば必ず重なる。
    ///
    /// 「無期限 × 無期限」は、どちらも終端を持たない以上どこかで必ず重なる。
    /// 片方だけ無期限のケース（`new_rejects_open_ended_period_overlapping_a_later_finite_period`）
    /// とは別に、両方無期限の組み合わせを直接押さえる（レビュー指摘）。
    #[test]
    fn new_rejects_two_open_ended_periods() {
        let a = table("A", date(2026, 1, 1), None);
        let b = table("B", date(2030, 1, 1), None);
        assert!(
            TaxRuleSets::new(vec![a, b]).is_err(),
            "無期限のマスタが2つあれば必ず重なるので拒否すべき"
        );
    }

    #[test]
    fn overlap_error_message_names_both_labels_and_ranges() {
        let a = table("kaikei-jp-data/tax/jp/2026a.yaml", date(2026, 1, 1), None);
        let b = table(
            "kaikei-jp-data/tax/jp/2026b.yaml",
            date(2026, 6, 1),
            Some(date(2026, 12, 31)),
        );
        let message = TaxRuleSets::new(vec![a, b]).unwrap_err().to_string();

        assert!(message.contains("2026a.yaml"), "message = {message}");
        assert!(message.contains("2026b.yaml"), "message = {message}");
        assert!(message.contains("2026-01-01"), "message = {message}");
        assert!(message.contains("2026-06-01"), "message = {message}");
    }

    // ---- プロパティテスト ----
    //
    // `PROGRESS.md` Phase 0 の教訓（生成器は「仕様が許容する範囲」に合わせる）
    // に従い、日付の生成器は 2026 年周辺だけでなく、うるう日・年跨ぎ・
    // マスタの境界日の前後を `prop_oneof!` で明示的に含める。

    /// 固定の3マスタ（重ならない）: T1=2024年、T2=2025年上半期、T3=2025-07-01〜無期限。
    fn fixture_rule_sets() -> (TaxRuleSets, AccountingDate, AccountingDate, AccountingDate) {
        let t1_from = date(2024, 1, 1);
        let t1_to = date(2024, 12, 31);
        let t2_from = date(2025, 1, 1);
        let t2_to = date(2025, 6, 30);
        let t3_from = date(2025, 7, 1);

        let rule_sets = TaxRuleSets::new(vec![
            table("T1", t1_from, Some(t1_to)),
            table("T2", t2_from, Some(t2_to)),
            table("T3", t3_from, None),
        ])
        .unwrap();
        (rule_sets, t1_to, t2_from, t3_from)
    }

    /// 期待される割当先ラベル。どのマスタにも属さない場合は `None`。
    fn expected_label(d: AccountingDate) -> Option<&'static str> {
        if d >= date(2024, 1, 1) && d <= date(2024, 12, 31) {
            Some("T1")
        } else if d >= date(2025, 1, 1) && d <= date(2025, 6, 30) {
            Some("T2")
        } else if d >= date(2025, 7, 1) {
            Some("T3")
        } else {
            None
        }
    }

    fn any_probe_date() -> impl Strategy<Value = AccountingDate> {
        prop_oneof![
            // 各マスタの境界（当日・前日・翌日）を明示的に含める。
            Just(date(2023, 12, 31)),
            Just(date(2024, 1, 1)),
            Just(date(2024, 1, 2)),
            Just(date(2024, 12, 30)),
            Just(date(2024, 12, 31)),
            Just(date(2025, 1, 1)),
            Just(date(2025, 1, 2)),
            Just(date(2025, 6, 29)),
            Just(date(2025, 6, 30)),
            Just(date(2025, 7, 1)),
            Just(date(2025, 7, 2)),
            // 閏日（2024年はうるう年）。
            Just(date(2024, 2, 29)),
            // 年跨ぎ。
            Just(date(2024, 12, 31)),
            Just(date(2025, 1, 1)),
            // 無期限マスタの遠い将来の日付。
            Just(date(2999, 12, 31)),
            // 全マスタの適用期間より前の遠い過去。
            Just(date(1900, 1, 1)),
            // 各マスタ内のランダムな日（月・日はどのマスタでも有効な範囲に収める）。
            (2024i32..=2025i32, 1u8..=12u8, 1u8..=28u8)
                .prop_map(|(y, m, d)| AccountingDate::new(y, m, d).unwrap()),
        ]
    }

    proptest! {
        /// 重ならないマスタ群に対して、for_date が返すマスタは高々1つであり、
        /// かつ手計算した期待ラベルと一致する。
        #[test]
        fn for_date_matches_expected_table_for_non_overlapping_rule_sets(d in any_probe_date()) {
            let (rule_sets, _, _, _) = fixture_rule_sets();
            let found = rule_sets.for_date(d).map(|t| t.label());
            prop_assert_eq!(found, expected_label(d));

            // 「高々1つ」であることの直接検証: 内部のマスタのうち
            // contains(d) が真になるものは0件または1件のみ。
            let matching_count = rule_sets.tables.iter().filter(|t| t.contains(d)).count();
            prop_assert!(matching_count <= 1);
        }

        /// applies_from <= date <= applies_to の範囲内なら、必ずそのマスタが返る
        /// （無期限マスタも含む）。
        #[test]
        fn for_date_always_returns_the_table_when_date_is_within_its_range(d in any_probe_date()) {
            let (rule_sets, t1_to, t2_from, t3_from) = fixture_rule_sets();
            let t1_from = date(2024, 1, 1);
            let t2_to = date(2025, 6, 30);

            if d >= t1_from && d <= t1_to {
                prop_assert_eq!(rule_sets.for_date(d).map(|t| t.label()), Some("T1"));
            }
            if d >= t2_from && d <= t2_to {
                prop_assert_eq!(rule_sets.for_date(d).map(|t| t.label()), Some("T2"));
            }
            if d >= t3_from {
                prop_assert_eq!(rule_sets.for_date(d).map(|t| t.label()), Some("T3"));
            }
        }

        /// 一方の適用開始日がもう一方の適用期間内に入るよう構成した2マスタは、
        /// 必ず重なりとして拒否される。
        #[test]
        fn new_rejects_randomly_constructed_overlapping_pairs(
            a_from_offset in 0i32..24,
            a_len_months in 1i32..24,
            b_start_offset_into_a in 0i32..24,
        ) {
            // A: 2024-01-01 を起点に a_from_offset ヶ月後から a_len_months ヶ月間。
            let a_from = add_months(date(2024, 1, 1), a_from_offset);
            let a_to = add_months(a_from, a_len_months);
            // B の開始日は A の期間内に収まるよう構成する（必ず重なる）。
            let b_from = add_months(a_from, b_start_offset_into_a.min(a_len_months));
            let b_to = add_months(b_from, 1);

            let a = table("A", a_from, Some(a_to));
            let b = table("B", b_from, Some(b_to));

            prop_assert!(TaxRuleSets::new(vec![a, b]).is_err());
        }
    }

    /// テスト専用の月加算（`kaikei-core` は日付演算 API を持たないため、
    /// 生成器の都合でここに閉じたローカル実装を置く。年またぎのみ扱えれば
    /// 十分なので、常に月末日ではなく1日に丸めて安全側に倒す）。
    fn add_months(d: AccountingDate, months: i32) -> AccountingDate {
        let total = (d.year() * 12 + (d.month() as i32 - 1)) + months;
        let year = total.div_euclid(12);
        let month = (total.rem_euclid(12) + 1) as u8;
        AccountingDate::new(year, month, 1).unwrap()
    }
}
