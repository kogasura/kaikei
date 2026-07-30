# 04 — 日本税制アダプタ（kaikei-jp）

## 1. 原則：税率も控除割合もコードに書かない

年度別 YAML として `kaikei-jp-data` に置き、`kaikei-jp` はそれを解釈するだけ。
改正が来たら **YAML を追加するだけ**で `kaikei-core` も `kaikei-jp` のロジックも触らない。

年度別データの選択は**取引日**で行う。記帳日ではない。

---

## 2. policy trait（kaikei-policy）

```rust
/// 税額計算と税区分の妥当性
pub trait TaxPolicy {
    fn validate_tag(&self, ctx: &TaxContext<'_>, tags: &TagSet, account: &AccountDef)
        -> Result<(), PolicyError>;

    /// 税抜経理での消費税行を自動生成する
    fn derive_tax_lines(&self, ctx: &TaxContext<'_>, lines: &[JournalLine])
        -> Result<Vec<JournalLine>, PolicyError>;

    fn round(&self, raw: Money) -> Money;
}

/// 決算振替仕訳の生成
pub trait ClosingPolicy {
    fn closing_entries(&self, tb: &TrialBalance, fy: &FiscalYear)
        -> Result<Vec<NewEntry>, PolicyError>;
}

/// 財務諸表の様式
pub trait StatementPolicy {
    fn balance_sheet(&self, tb: &TrialBalance) -> Statement;
    fn income_statement(&self, tb: &TrialBalance) -> Statement;
}

/// 追加検証
pub trait EntryValidator {
    fn validate(&self, ctx: &TaxContext<'_>, entry: &JournalEntry)
        -> Result<(), PolicyError>;
}

/// 採番
pub trait Numbering {
    fn peek(&self, fy: &FiscalYear) -> EntryNumber;
}
```

**この 5 つが「変わる部分」の全リスト。**
ここに現れないものは不変層に置いてよい。新しい国を追加するときはこれを実装するだけ。

### 全て純関数（R3 対策）

I/O が必要なデータは呼び出し側が `TaxContext` に詰めて渡す。

```rust
pub struct TaxContext<'a> {
    pub as_of: AccountingDate,
    pub categories: &'a TaxCategoryTable,        // 年度別YAMLから
    pub counterparties: &'a CounterpartyIndex,   // DBから引いたスナップショット
    pub settings: &'a JpSettings,
}

pub struct JpSettings {
    pub tax_mode: TaxMode,               // Exclusive（税抜） / Inclusive（税込）
    pub rounding: RoundMode,             // 端数処理
    pub is_taxable_business: bool,       // 課税事業者か免税事業者か
    pub simplified_taxation: bool,       // 簡易課税か
}
```

---

## 3. 税区分マスタ（年度別 YAML）

`kaikei-jp-data/tax/jp/2026.yaml` の例は `skeleton/data/tax-jp-2026.yaml` を参照。

### スキーマ

```yaml
version: 1
applies_from: 2026-04-01      # この日以降の取引に適用
applies_to: null              # null なら無期限（次の年度ファイルが上書き）

categories:
  - code: SALES_10            # 一意。仕訳の tags.tax_category に入る値
    label: "課税売上 10%"
    direction: sales          # sales | purchase | none
    rate: "0.10"              # 文字列で書く（浮動小数点誤差を避ける）
    deductible: null          # purchase のみ意味を持つ
    deduction_ratio: null     # 非適格の経過措置。null なら 1.0
    requires_qualified_invoice: false
    tax_account: "330"        # 仮受消費税の科目コード
```

### 重要な設計判断

- `rate` は**文字列**で書き、`rust_decimal` でパースする。YAML の float を使わない
- `tax_account` を YAML に持たせる。科目コード体系はユーザーが変更できるため
- `direction: none` は非課税・不課税・対象外（税額計算をしない）

---

## 4. タグスキーマ

`kaikei-jp-data/tags.yaml`（`skeleton/data/tags.yaml` を参照）

新しいタグキーを使う前に必ずここへ登録する。core が未登録キーを拒否する。

---

## 5. 勘定科目テンプレート

`kaikei-jp-data/chart/sole_proprietor.yaml`（`skeleton/data/chart-sole-proprietor.yaml`）

個人事業主（青色申告）向けの標準的な科目体系。ユーザーは複製して自由に編集できる。

### 個人事業主固有の科目

| コード | 科目 | 種別 | 備考 |
|---|---|---|---|
| 400 | 元入金 | Equity | 年に1回だけ動く |
| 410 | 事業主貸 | Equity | 期首でゼロにリセット |
| 420 | 事業主借 | Equity | 期首でゼロにリセット |

---

## 6. インボイス登録番号

```rust
/// T + 13桁（法人番号または個人事業主用の13桁）
pub struct InvoiceRegistrationNo(String);

impl InvoiceRegistrationNo {
    pub fn parse(s: &str) -> Result<Self, JpError> {
        // 1. 先頭が 'T'
        // 2. 続く13桁が数字
        // 3. チェックデジット検証（法人番号のアルゴリズム）
    }
    pub fn as_str(&self) -> &str;
    pub fn corporate_number(&self) -> &str;   // T を除いた13桁
}
```

### チェックデジットの計算（法人番号）

```
検査用数字 = 9 - (Σ(P_n × Q_n) mod 9)
  P_n : 1桁目（最下位）から12桁目までの数字
  Q_n : n が奇数のとき 1、偶数のとき 2
```

先頭1桁が検査用数字、残り12桁が基礎番号。

### 注意

- **形式検証のみ行う。** 実在確認や適格事業者かの判定は国税庁の公表サイト/API の領域
- 適格事業者かは `counterparties.is_qualified` にユーザーが記録する
- 自動照会機能は Phase 5 以降の検討事項。外部 API 依存を早期に入れない

---

## 7. 消費税行の自動生成（derive_tax_lines）

税抜経理での動作。

### 入力（ユーザーが書く仕訳）

```
売掛金 110,000 / 売上高 100,000 (tags: tax_category=SALES_10)
```

これは貸借不一致なので、そのままでは core に通らない。

### 処理

1. `direction: sales` かつ `rate` を持つ明細を抽出
2. `税額 = round(本体 × rate)`
3. 反対側（貸方）に `tax_account` の明細を追加

### 出力

```
売掛金 110,000 / 売上高      100,000 (tax_category=SALES_10)
                 仮受消費税   10,000 (tax_category=SALES_10)
```

### 実装の注意

- **端数処理は明細ごとか合計かを選べるようにする。**
  事業者が選択できるため `JpSettings` に持たせる
- 免税事業者（`is_taxable_business: false`）の場合、税行を生成しない（税込経理）
- 生成した税行にも元の `tax_category` を付ける（集計のため）
- 非適格の経過措置がある場合、控除できない部分の扱いは
  「仮払消費税を減らして本体に足す」方式にする（要 税理士確認）

---

## 8. 家事按分

```rust
pub struct HouseholdSplitInput {
    pub total: Money,
    pub business_ratio: Ratio,
    pub expense_account: AccountCode,   // 地代家賃など
    pub owner_account: AccountCode,     // 事業主貸
    pub payment_account: AccountCode,   // 現金・預金
    pub tax_category: Option<TaxCategoryCode>,
}

/// 按分後の明細を生成する。按分率は tags に残す
pub fn household_split(
    input: HouseholdSplitInput,
    settings: &JpSettings,
) -> Result<Vec<JournalLine>, JpError>;
```

### 出力例（家賃 100,000 / 事業割合 30%）

```
地代家賃(615)  30,000 (tags: business_ratio=0.30, tax_category=...) / 現金(100) 100,000
事業主貸(410)  70,000                                                  /
```

按分率をタグに残すのは、税務調査で根拠を問われるため。

**core は「ただの3行仕訳」として受け取るだけ。**

---

## 9. 決算処理（個人事業主）

法人とは別物。`JpSoleProprietorClosingPolicy` として実装。

### 手順

1. **収益・費用を集計して所得を算出**
   ```
   所得 = 収益合計 − 費用合計
   ```
2. **収益・費用の各科目をゼロにする振替仕訳を生成**
3. **元入金への振替**
   ```
   翌年期首の元入金 = 前年元入金 + 前年所得 + 事業主借 − 事業主貸
   ```
4. 事業主貸・事業主借は期首でゼロにリセット

### 実装上の注意

- 青色申告特別控除（65万/55万/10万）は**帳簿科目ではない**。申告書上の控除
  → `kaikei-report` の決算書出力で扱う。仕訳を作らない
- 減価償却費、家事按分の年次調整、棚卸は Phase 5 の検討事項

---

## 10. 決算書出力（kaikei-report）

| 出力 | 形式 | 優先度 |
|---|---|---|
| 仕訳日記帳 | CSV | 高 |
| 総勘定元帳 | CSV | 高 |
| 試算表 | CSV / JSON | 高 |
| **弥生インポート形式** | CSV | **高（税理士連携）** |
| 青色申告決算書（4ページ相当のデータ） | JSON → 帳票 | 中 |
| freee インポート形式 | CSV | 中 |
| 全件 JSON エクスポート | JSON | 高（可搬性） |
| e-Tax 連携 | — | スコープ外（Phase 5 以降） |

**弥生形式への対応は優先度が高い。**
税理士は弥生を使っている確率が高く、「税理士に渡せる」が採用の実質条件になる。

---

## 11. 免責の徹底

`kaikei-jp` の README とコード doc に以下を明記する。

> 本 crate は日本の税制に対応した処理を提供しますが、税務上の正しさを保証しません。
> 税区分の判定、経費性の判断、控除の適用可否は税理士等の専門家に確認してください。
> 本 crate は電子帳簿保存法の機能要件を意識して設計されていますが、
> JIIMA 認証を取得しておらず、運用要件（事務処理規程の備付け等）は利用者の責任です。

エラーメッセージで税務判断を断定しない。

```
❌ "この経費は損金になりません"
✅ "税区分 XXX は控除割合 80% が設定されています（経過措置）。
    適用可否は税理士にご確認ください"
```
