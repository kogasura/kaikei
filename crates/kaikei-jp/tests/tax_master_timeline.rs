//! 同梱の消費税区分マスタ（`kaikei_jp_data::TAX_CATEGORY_SOURCES`）が
//! **時間軸として連続していること**を検査する。
//!
//! # なぜ必要か
//!
//! 消費税の改正は暦年の途中に施行される。免税事業者等からの課税仕入れに
//! 係る経過措置は 2026-10-01（令和8年10月1日）に控除割合が 80% → 70% へ
//! 変わり、2026年には2つのマスタが要る（`DECISIONS.md` D-092）。
//!
//! マスタをファイルで分ける方式には、**分けたときに穴を空ける**という
//! 固有の失敗がある。`applies_to` を入れ忘れれば期間が重なり（これは
//! `TaxRuleSets::new` が D-054 で弾く）、逆に1日ずらせば**どのマスタにも
//! 属さない日**ができる。後者は構築時には何も起きず、その日の取引を
//! 記帳しようとして初めて `NoApplicableRuleSet` で落ちる。
//! 「マスタを足した日に、DB 無しで、手元で落ちる」検査をここに置く。
//!
//! # 期待値を手で書き写さない方針（`DECISIONS.md` D-047 / D-051）
//!
//! 「同梱マスタの一覧」も「税区分コードの一覧」もこのファイルには書かない。
//! 前者は `TAX_CATEGORY_SOURCES` から、後者は**マスタ同士を突き合わせて**
//! 導く。手で維持する一覧を増やすと、それ自体が腐って「乖離を検出する
//! ための仕組みが乖離する」状態になる。
//!
//! 唯一の例外が [`the_transitional_measure_ratio_changes_on_the_day_it_changed`]
//! で、ここだけは施行日と控除割合を直に書く。**それが検査したい制度の
//! 事実そのもの**であり、YAML から取った値を YAML と比べても何も守れない
//! ためである（出典は `docs/09-tax-research.md`）。

use kaikei_core::{AccountingDate, Ratio};
use kaikei_jp::tax::TaxRuleSets;
use std::collections::BTreeSet;

/// 同梱マスタを適用開始日の昇順に並べる。
fn embedded_sorted() -> Vec<(String, AccountingDate, Option<AccountingDate>)> {
    let rule_sets = TaxRuleSets::from_embedded().expect("同梱マスタは構築できるはず");
    let mut ranges: Vec<(String, AccountingDate, Option<AccountingDate>)> = rule_sets
        .iter()
        .map(|table| {
            (
                table.label().to_string(),
                table.applies_from(),
                table.applies_to(),
            )
        })
        .collect();
    ranges.sort_by_key(|(_, from, _)| *from);
    ranges
}

/// 期間を閉じてよいのは最後のマスタ以外。最後だけが無期限（`applies_to: null`）。
///
/// 途中のマスタが無期限だと、そこから後ろのマスタと必ず重なる
/// （`TaxRuleSets::new` が弾くので実際には構築に失敗する）。逆に最後のマスタを
/// 閉じてしまうと、その翌日以降に**どのマスタも無い**状態になる。
#[test]
fn only_the_last_master_is_open_ended() {
    let ranges = embedded_sorted();
    assert!(!ranges.is_empty(), "同梱マスタが1件もありません");

    let last_index = ranges.len() - 1;
    for (index, (label, _, applies_to)) in ranges.iter().enumerate() {
        if index == last_index {
            assert!(
                applies_to.is_none(),
                "最後のマスタ {label} に applies_to が入っています。\
                 その翌日以降の取引日でマスタが引けなくなります。\
                 後続のマスタを追加したなら、そちらを無期限にしてください"
            );
        } else {
            assert!(
                applies_to.is_some(),
                "{label} は最後のマスタではないのに applies_to が null です。\
                 新しいマスタを足したとき、直前のマスタに applies_to を入れる\
                 のを忘れています（DECISIONS.md D-092）"
            );
        }
    }
}

/// 同梱マスタが覆う期間に穴が無い。
///
/// 「前のマスタの終了日の翌日 == 次のマスタの開始日」を直接検査したいが、
/// `AccountingDate` に翌日を求める操作は無い（`kaikei-core` は `chrono` に
/// 依存しない。`CLAUDE.md` §1）。そこで**最初のマスタの開始日から十分先まで
/// を1日ずつ全部引く**。標本ではなく全数なので、1日でも穴があれば必ず落ちる。
/// 月末（9/30 など）にずれた穴は、日を絞った標本検査では踏み損ねる。
#[test]
fn the_covered_period_has_no_gaps() {
    let rule_sets = TaxRuleSets::from_embedded().expect("同梱マスタは構築できるはず");
    let ranges = embedded_sorted();
    let first_from = ranges.first().expect("同梱マスタが1件もありません").1;

    // 最後のマスタは無期限なので上限は任意。最初の開始日から10年先まで見る。
    let upper_year = first_from.year() + 10;

    let mut checked = 0usize;
    for year in first_from.year()..=upper_year {
        for month in 1u8..=12 {
            for day in 1u8..=31 {
                // 存在しない日付（2月30日など）は AccountingDate が拒否する。
                let Ok(date) = AccountingDate::new(year, month, day) else {
                    continue;
                };
                if date < first_from {
                    continue;
                }
                assert!(
                    rule_sets.for_date(date).is_some(),
                    "取引日 {} に適用されるマスタがありません\
                     （同梱マスタの期間に穴があります）。有効な期間: {}",
                    date.to_iso_string(),
                    rule_sets.available_ranges_display(),
                );
                checked += 1;
            }
        }
    }

    // 「1日も検査していないのに緑」を防ぐ（ループの条件を壊した場合の保険）。
    assert!(checked > 3_000, "検査した日数が少なすぎます: {checked}");
}

/// 各マスタの適用期間の**両端ちょうど**で、そのマスタが引ける。
///
/// 期間は閉区間（両端を含む）である。`contains` の比較が片側だけ `<` に
/// 変わるような退行は、範囲の内側を見ているだけでは気づけない。
#[test]
fn both_ends_of_every_range_resolve_to_that_master() {
    let rule_sets = TaxRuleSets::from_embedded().expect("同梱マスタは構築できるはず");

    for table in rule_sets.iter() {
        let from = table.applies_from();
        let resolved = rule_sets.for_date(from).unwrap_or_else(|| {
            panic!(
                "{} の開始日 {} でマスタが引けません",
                table.label(),
                from.to_iso_string()
            )
        });
        assert_eq!(
            resolved.label(),
            table.label(),
            "開始日 {} は {} に属するはずですが {} が引かれました",
            from.to_iso_string(),
            table.label(),
            resolved.label()
        );

        let Some(to) = table.applies_to() else {
            continue;
        };
        let resolved = rule_sets.for_date(to).unwrap_or_else(|| {
            panic!(
                "{} の終了日 {} でマスタが引けません",
                table.label(),
                to.to_iso_string()
            )
        });
        assert_eq!(
            resolved.label(),
            table.label(),
            "終了日 {} は {} に属するはずですが {} が引かれました",
            to.to_iso_string(),
            table.label(),
            resolved.label()
        );
    }
}

/// すべてのマスタが同じ税区分コードの集合を持つ。
///
/// マスタを分けたときに、片方にだけ区分を足す・片方から落とすという事故が
/// 起きる（実際に `PURCHASE_8_REDUCED_NON_QUALIFIED` が長く欠落していた）。
/// 集合が食い違うと、**同じ記帳が取引日によって成功したり失敗したりする**。
///
/// 期待するコード一覧はここに書かない。マスタ同士を突き合わせるだけなので、
/// 区分を増やすときにこのテストを直す必要は無い（両方に足せば通る）。
///
/// 制度上、ある区分が特定の期間にしか存在しないことは起こりうる
/// （例: 少額特例は令和11年9月30日で終わる）。そのときは**この検査を
/// 期間つきの表に置き換える**こと。単に緩めると、意図しない欠落が
/// 二度と検出できなくなる。
#[test]
fn every_master_declares_the_same_set_of_category_codes() {
    let rule_sets = TaxRuleSets::from_embedded().expect("同梱マスタは構築できるはず");
    let mut baseline: Option<(String, BTreeSet<String>)> = None;

    for table in rule_sets.iter() {
        let codes: BTreeSet<String> = table.categories().map(|c| c.code.clone()).collect();
        match &baseline {
            None => baseline = Some((table.label().to_string(), codes)),
            Some((first_label, first_codes)) => {
                let missing: Vec<&String> = first_codes.difference(&codes).collect();
                let extra: Vec<&String> = codes.difference(first_codes).collect();
                assert!(
                    missing.is_empty() && extra.is_empty(),
                    "{} と {} で税区分コードの集合が違います。\
                     {} に無い: {:?} / {} にだけある: {:?}。\
                     同じ記帳が取引日によって通ったり通らなかったりします",
                    first_label,
                    table.label(),
                    table.label(),
                    missing,
                    table.label(),
                    extra,
                );
            }
        }
    }
}

/// 経過措置の控除割合が、施行日を境に実際に変わる。
///
/// **このテストだけは施行日と割合を直に書く。** YAML から取った値を YAML と
/// 比べても、値が書き換わったことを検出できないためである。
///
/// 出典（`docs/09-tax-research.md` に引用を収録）:
/// 国税庁 インボイスQ&A 問113（令和8年4月改訂）。令和8年度税制改正により
/// 経過措置は2年延長され、控除割合は 2026-10-01 から 70% になった。
/// 割合の判定は**課税仕入れを行った日**で行う（同 問113-3）。
#[test]
fn the_transitional_measure_ratio_changes_on_the_day_it_changed() {
    let rule_sets = TaxRuleSets::from_embedded().expect("同梱マスタは構築できるはず");

    // 経過措置の対象は 10% と軽減 8% の両方に及ぶ（問113）。
    let codes = [
        "PURCHASE_10_NON_QUALIFIED",
        "PURCHASE_8_REDUCED_NON_QUALIFIED",
    ];
    let cases = [
        (AccountingDate::new(2026, 9, 30).unwrap(), "0.80"),
        (AccountingDate::new(2026, 10, 1).unwrap(), "0.70"),
    ];

    for (date, expected) in cases {
        let expected_ratio = Ratio::parse_fraction(expected).expect("期待値は割合として妥当");
        let table = rule_sets
            .for_date(date)
            .unwrap_or_else(|| panic!("{} でマスタが引けません", date.to_iso_string()));
        for code in codes {
            let category = table
                .category(code)
                .unwrap_or_else(|e| panic!("{} に区分 {code} がありません: {e}", table.label()));
            let ratio = category.deduction_ratio.unwrap_or_else(|| {
                panic!("{code}（{}）に deduction_ratio がありません", table.label())
            });
            assert_eq!(
                ratio.as_decimal(),
                expected_ratio.as_decimal(),
                "取引日 {} の {code} の控除割合は {expected} のはずです（{}）。\
                 国税庁インボイスQ&A 問113 / docs/09-tax-research.md",
                date.to_iso_string(),
                table.label(),
            );
        }
    }
}
