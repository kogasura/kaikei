# 02 — Phase 0 テストケース一覧

**実装より先にこの一覧を全部テストとして書く**（失敗する状態でよい）。
Phase 0 の完了条件は「この全項目が通ること」。

命名は `#[test] fn 分類_期待動作()` の形式。日本語識別子は使わない。

---

## money

### 生成と表示

| # | ケース | 期待 |
|---|---|---|
| M-01 | `Money::from_minor(1000, JPY)` → `to_display_string()` | `"1,000"` |
| M-02 | `Money::from_minor(123456, USD)` → `to_display_string()` | `"1,234.56"` |
| M-03 | `Money::parse("1000", JPY)` | `minor == 1000` |
| M-04 | `Money::parse("1234.56", USD)` | `minor == 123456` |
| M-05 | `Money::parse("1000.5", JPY)` | **エラー**（JPY は小数不可） |
| M-06 | `Money::parse("1.234", USD)` | **エラー**（桁数超過） |
| M-07 | `Money::parse("abc", JPY)` | エラー |
| M-08 | `Money::parse("-500", JPY)` | `minor == -500`（負値は許容） |
| M-09 | `Money::zero(JPY).is_zero()` | `true` |
| M-10 | 3桁通貨 KWD で `parse("1.234")` | `minor == 1234` |

### 演算

| # | ケース | 期待 |
|---|---|---|
| M-20 | 同一通貨の `add` | 正常 |
| M-21 | JPY + USD の `add` | `CurrencyMismatch` |
| M-22 | 同一通貨の `sub`（結果が負） | 負値で正常 |
| M-23 | `neg`、`abs` | 符号反転 / 絶対値 |
| M-24 | `sum_money` 空イテレータ | `Ok(None)` |
| M-25 | `sum_money` 通貨混在 | `CurrencyMismatch` |
| M-26 | `mul_ratio(0.30, Floor)` で 100,000 JPY | `30,000` |
| M-27 | `mul_ratio(0.333, Floor)` で 100 JPY | `33` |
| M-28 | `mul_ratio(0.333, Ceil)` で 100 JPY | `34` |
| M-29 | `mul_ratio(0.335, HalfUp)` で 1000 JPY | `335` |
| M-30 | i128 の上限付近での加算 | オーバーフローしない（checked） |

### Ratio

| # | ケース | 期待 |
|---|---|---|
| M-40 | `parse_fraction("0.3")` | 正常 |
| M-41 | `parse_fraction("1.5")` | エラー（1 超過） |
| M-42 | `parse_fraction("-0.1")` | エラー |
| M-43 | `parse_rate("0.08")` | 正常 |

---

## account

| # | ケース | 期待 |
|---|---|---|
| A-01 | `AccountCode::parse("500")` | 正常 |
| A-02 | `AccountCode::parse("")` | エラー |
| A-03 | `AccountCode::parse("あいう")` | エラー（英数字とハイフンのみ） |
| A-04 | 33文字のコード | エラー |
| A-05 | `AccountType::Asset.is_debit_normal()` | `true` |
| A-06 | `AccountType::Expense.is_debit_normal()` | `true` |
| A-07 | `AccountType::Liability.is_debit_normal()` | `false` |
| A-08 | `AccountType::Equity.is_debit_normal()` | `false` |
| A-09 | `AccountType::Revenue.is_debit_normal()` | `false` |
| A-10 | `ChartOfAccounts::new` 親が存在しない | `InvalidChart` |
| A-11 | 循環参照（A→B→A） | `InvalidChart` |
| A-12 | 重複コード | `InvalidChart` |
| A-13 | `descendants` 3階層 | 子と孫の両方が返る |
| A-14 | `descendants` 葉ノード | 空 |

---

## tag

| # | ケース | 期待 |
|---|---|---|
| T-01 | `TagKey::parse("tax_category")` | 正常 |
| T-02 | `TagKey::parse("TaxCategory")` | エラー（snake_case のみ） |
| T-03 | `TagKey::parse("")` | エラー |
| T-04 | schema に無いキーで `validate` | `UnknownTagKey` |
| T-05 | `Code` 期待の位置に `Decimal` | `TagTypeMismatch` |
| T-06 | `required_for: [Expense]` のキーを欠いた Expense 明細 | `MissingRequiredTag` |
| T-07 | 同じキーを Asset 明細で欠く（required_for に無い） | 正常 |
| T-08 | `TagSchema::empty()` に対し空の TagSet | 正常 |
| T-09 | `TagSchema::empty()` に対し何かキーがある TagSet | `UnknownTagKey` |
| T-10 | `is_aggregatable` 宣言通りに返る | — |

---

## period

| # | ケース | 期待 |
|---|---|---|
| P-01 | `AccountingDate::new(2026, 2, 29)` | エラー（閏年でない） |
| P-02 | `AccountingDate::new(2024, 2, 29)` | 正常 |
| P-03 | `AccountingDate::new(2026, 13, 1)` | エラー |
| P-04 | `AccountingDate::new(2026, 4, 31)` | エラー |
| P-05 | `parse("2026-04-15")` | 正常 |
| P-06 | `parse("2026/04/15")` | エラー（ISO のみ） |
| P-07 | 日付の順序比較 | 正しく並ぶ |
| P-08 | `FiscalYear::calendar_year(2026)` | 2026-01-01 〜 2026-12-31 |
| P-09 | `contains` 開始日・終了日（境界） | 両端とも `true` |
| P-10 | `contains` 範囲外 | `false` |
| P-11 | `FiscalYear::new` で start > end | エラー |

---

## journal — JournalLine

| # | ケース | 期待 |
|---|---|---|
| L-01 | 正常な借方明細 | 正常 |
| L-02 | `amount` が負 | `InvalidAmount`（符号は side で表現） |
| L-03 | `amount` がゼロ | `InvalidAmount` |
| L-04 | `is_debit()` が side と一致 | — |

---

## journal — JournalEntry::new（最重要）

### 正常系

| # | ケース | 期待 |
|---|---|---|
| J-01 | 2行、借方100/貸方100 | 正常 |
| J-02 | 3行、借方100/貸方60+40 | 正常 |
| J-03 | 4行、借方70+30/貸方60+40 | 正常 |
| J-04 | 消費税ありの4行（売掛110 / 売上100 + 仮受10） | 正常 |
| J-05 | `debit_total()` / `credit_total()` が一致 | — |
| J-06 | `recorded_at` が Clock の値と一致 | — |

### 貸借不一致（このプロジェクトの核）

| # | ケース | 期待 |
|---|---|---|
| J-10 | 借方100 / 貸方90 | `Unbalanced { diff: "10" }` |
| J-11 | 借方100 / 貸方110 | `Unbalanced { diff: "10" }` |
| J-12 | エラーメッセージに借方・貸方・差額が含まれる | 文字列アサーション |
| J-13 | 全て借方（貸方ゼロ） | `Unbalanced` |

### 明細数

| # | ケース | 期待 |
|---|---|---|
| J-20 | 0行 | `TooFewLines { found: 0 }` |
| J-21 | 1行 | `TooFewLines { found: 1 }` |

### 勘定科目

| # | ケース | 期待 |
|---|---|---|
| J-30 | chart に無い科目 | `UnknownAccount` |
| J-31 | `postable: false` の科目 | `NotPostable` |
| J-32 | 同じ科目が借方と貸方に両方 | 正常（許容する） |

### 通貨

| # | ケース | 期待 |
|---|---|---|
| J-40 | JPY と USD が混在 | `CurrencyMismatch` |
| J-41 | 全て USD で貸借一致 | 正常 |
| J-42 | `currency()` が明細の通貨を返す | — |

### タグ

| # | ケース | 期待 |
|---|---|---|
| J-50 | schema 適合のタグ | 正常 |
| J-51 | 未登録キー | `UnknownTagKey` |
| J-52 | Expense 明細で必須タグ欠落 | `MissingRequiredTag` |

### 日付と期間

| # | ケース | 期待 |
|---|---|---|
| J-60 | fy 範囲外の日付 | `DateOutOfFiscalYear` |
| J-61 | fy の開始日ちょうど | 正常 |
| J-62 | fy の終了日ちょうど | 正常 |
| J-63 | `PeriodGuard` が Closed を返す日付 | `PeriodClosed` |

### 摘要

| # | ケース | 期待 |
|---|---|---|
| J-70 | 空文字 | `EmptyDescription` |
| J-71 | 空白のみ | `EmptyDescription`（trim して判定） |

### 不変性の保証（構造テスト）

| # | ケース | 期待 |
|---|---|---|
| J-80 | `JournalEntry` に `update` / `delete` / `set_*` メソッドが無い | **コンパイル時の設計テスト**。doc コメントで担保し、レビュー項目にする |
| J-81 | `lines()` が返すのは不変参照 | 外部から変更できない |

---

## journal — reverse

| # | ケース | 期待 |
|---|---|---|
| R-01 | 借方100/貸方100 の逆仕訳 | 貸方100/借方100 |
| R-02 | 4行の仕訳の逆仕訳 | 全行の side が反転 |
| R-03 | `amount` は変わらない | — |
| R-04 | `tags` が複製される | — |
| R-05 | `reverses` に元 id が入る | — |
| R-06 | `reverse_reason` が入る | — |
| R-07 | `description` が `【訂正】` 付き | — |
| R-08 | `is_reversal()` が `true` | — |
| R-09 | 逆仕訳の逆仕訳 | 許可される |
| R-10 | 元仕訳と逆仕訳を合算すると全科目ゼロ | **重要**。TrialBalance で検証 |
| R-11 | 締められた期間への逆仕訳 | `PeriodClosed` |
| R-12 | 元仕訳が前年度、逆仕訳は今年度の日付 | 逆仕訳は今年度に属する |

---

## trial_balance

### 基本

| # | ケース | 期待 |
|---|---|---|
| B-01 | 仕訳0件 | 空。`is_balanced() == true` |
| B-02 | 仕訳1件（借方100/貸方100） | 2行。totals が一致 |
| B-03 | 同一科目に複数回記帳 | 集約されて1行 |
| B-04 | `is_balanced()` が常に true | 全ケースで |

### 残高の向き（DOMAIN.md §2 との一致）

| # | ケース | 期待 |
|---|---|---|
| B-10 | 資産科目に借方100 | `balance == +100` |
| B-11 | 資産科目に貸方100 | `balance == -100` |
| B-12 | 負債科目に貸方100 | `balance == +100` |
| B-13 | 負債科目に借方100 | `balance == -100` |
| B-14 | 収益科目に貸方100 | `balance == +100` |
| B-15 | 費用科目に借方100 | `balance == +100` |
| B-16 | 純資産科目に貸方100 | `balance == +100` |

### group_by

| # | ケース | 期待 |
|---|---|---|
| B-20 | `group_by` 空 | 科目のみで集計 |
| B-21 | `group_by: [counterparty]` | 科目×取引先で分割 |
| B-22 | `group_by` 2キー | 組み合わせで分割 |
| B-23 | タグが無い明細を group_by | 空の GroupKey に集約される |
| B-24 | `aggregatable: false` のキーを指定 | `NotAggregatable` |
| B-25 | group_by しても totals は一致 | — |

### 検算シナリオ（統合テスト）

| # | ケース | 期待 |
|---|---|---|
B-30 | 売上計上 → 入金 → 経費支払 の3仕訳 | 現金・売掛金・売上・経費の残高が手計算と一致 |
B-31 | 家事按分仕訳（地代家賃 615: 30,000 / 事業主貸 410: 70,000 / 現金 100,000） | 貸借一致し、各残高が正しい |
B-32 | 誤った仕訳 → 逆仕訳 → 正しい仕訳 | 最終残高が「正しい仕訳のみ」の場合と一致 |
B-33 | 100件の仕訳をランダム生成して集計 | 常に `is_balanced()` |

---

## プロパティテスト（proptest 推奨）

| # | 性質 |
|---|---|
| PT-01 | 任意の貸借一致明細で `JournalEntry::new` が成功する |
| PT-02 | 任意の仕訳集合で `TrialBalance::is_balanced()` が true |
| PT-03 | 任意の仕訳とその逆仕訳の合算で全科目残高がゼロ |
| PT-04 | `Money::parse(m.to_display_string())` が元に戻る（ラウンドトリップ） |
| PT-05 | `mul_ratio` の結果が元金額を超えない（ratio ≤ 1 のとき） |

---

## テスト補助（`tests/common/mod.rs`）

```rust
/// テスト用の最小勘定科目表
pub fn test_chart() -> ChartOfAccounts {
    // 科目コードは kaikei-jp-data/chart/sole_proprietor.yaml と一致させる
    // 100 現金           (Asset)
    // 135 売掛金         (Asset)
    // 180 仮払消費税等   (Asset)
    // 310 買掛金         (Liability)
    // 330 仮受消費税等   (Liability)
    // 400 元入金         (Equity)
    // 410 事業主貸       (Equity)
    // 420 事業主借       (Equity)
    // 500 売上高         (Revenue)
    // 609 消耗品費       (Expense)
    // 615 地代家賃       (Expense)
    // 999 見出し         (Expense, postable = false)
}

pub fn test_schema() -> TagSchema {
    // tax_category: Code, aggregatable, required_for [Revenue, Expense]
    // counterparty:  Code, aggregatable
    // business_ratio: Decimal, not aggregatable
}

pub fn open_guard() -> impl PeriodGuard;      // 常に Open
pub fn closed_guard() -> impl PeriodGuard;    // 常に Closed
pub fn fixed_clock() -> FixedClock;
```

---

## 完了条件

- 上記全ケースが通る
- `cargo clippy -- -D warnings` が通る
- `kaikei-core` の依存が `rust_decimal`, `thiserror` のみ（CI が検査）
- `#![forbid(unsafe_code)]` が有効
- 公開 API に doc コメントがある（`#![warn(missing_docs)]`）
