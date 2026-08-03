# 04 — 日本税制アダプタ（kaikei-jp）

## 1. 原則：税率も控除割合もコードに書かない

年度別 YAML として `kaikei-jp-data` に置き、`kaikei-jp` はそれを解釈するだけ。
改正が来たら **YAML を追加するだけ**で `kaikei-core` も `kaikei-jp` のロジックも触らない。

年度別データの選択は**取引日**で行う。記帳日ではない。

---

## 2. policy trait（kaikei-policy）

> **`TaxContext` は国非依存の4項目に限定する（`DECISIONS.md` D-025）。**
> 年度別税区分マスタ（`TaxCategoryTable`）や事業者設定（`JpSettings`）は
> `kaikei-jp` の型であり、`kaikei-policy` には一切含めない。そのまま含めると
> policy → jp の循環依存になり、`kaikei-app` も jp の型を知る必要が生じて
> `CLAUDE.md` §1 の依存方向が崩れる（`ARCHITECTURE.md` §7 参照）。

```rust
/// 税額計算と税区分の妥当性
pub trait TaxPolicy: Send + Sync {
    fn validate_tag(&self, ctx: &TaxContext<'_>, tags: &TagSet, account: &AccountDef)
        -> Result<(), PolicyError>;

    /// 税抜経理での消費税行を導出する。戻り値は**確定後の明細一覧**
    /// （入力＋税額行）であり、追加行だけではない。税額 0 の行は生成しない
    /// （`JournalLine::new` が 0 円を拒否するため）。
    fn derive_tax_lines(&self, ctx: &TaxContext<'_>, lines: &[JournalLine])
        -> Result<TaxDerivation, PolicyError>;

    fn round_mode(&self, ctx: &TaxContext<'_>) -> RoundMode;

    /// 按分・税額計算に使う。既定実装は
    /// `base.mul_ratio(ratio, self.round_mode(ctx))?`。`Money` は最小通貨単位の
    /// 整数（`i128`）で端数を保持できないため、`round(Money) -> Money` という
    /// 形は事実上の恒等関数にしかならず、この形に変えた（`DECISIONS.md` D-026）。
    fn apply_ratio(&self, ctx: &TaxContext<'_>, base: Money, ratio: Ratio)
        -> Result<Money, PolicyError> { /* 既定実装あり */ }
}

/// [`TaxPolicy::derive_tax_lines`] の戻り値。確定後の明細一覧に加え、
/// 断定を避けた補足情報（`PolicyNote`）を持てる（`CLAUDE.md` §10）。
pub struct TaxDerivation {
    pub lines: Vec<JournalLine>,
    pub notes: Vec<PolicyNote>,
}

/// 決算振替仕訳の生成
pub trait ClosingPolicy: Send + Sync {
    /// `EntryId` / `EntryNumber` の採番は store の I/O のため、ここでは
    /// 仕訳を提案する `ProposedEntry`（未採番）を返す（`DECISIONS.md` D-027）。
    fn closing_entries(&self, tb: &TrialBalance, fy: &FiscalYear)
        -> Result<Vec<ProposedEntry>, PolicyError>;

    /// 期首の振替仕訳（前年度繰越等）。個人事業主の元入金振替を当年度末と
    /// 翌年期首のどちらに計上するかは未確定（§9、税理士確認事項）なので、
    /// 既定実装では何も生成しない。
    fn opening_entries(&self, tb: &TrialBalance, fy: &FiscalYear)
        -> Result<Vec<ProposedEntry>, PolicyError> { Ok(Vec::new()) }
}

/// 財務諸表の様式
pub trait StatementPolicy: Send + Sync {
    fn balance_sheet(&self, tb: &TrialBalance) -> Statement;
    fn income_statement(&self, tb: &TrialBalance) -> Statement;
}

/// 追加検証
pub trait EntryValidator: Send + Sync {
    fn validate(&self, ctx: &TaxContext<'_>, entry: &JournalEntry)
        -> Result<(), PolicyError>;
}

/// 採番
pub trait Numbering: Send + Sync {
    /// `issued` は直近で払い出し済みの番号（未払い出しなら `None`）。
    fn peek(&self, fy: &FiscalYear, issued: Option<EntryNumber>)
        -> Result<EntryNumber, PolicyError>;
}
```

**この 5 つが「変わる部分」の全リスト。**
ここに現れないものは不変層に置いてよい。新しい国を追加するときはこれを実装するだけ。

### 全て純関数（R3 対策）

I/O が必要なデータは呼び出し側が `TaxContext` に詰めて渡す。

```rust
// kaikei-policy::TaxContext。国非依存の4項目のみ（DECISIONS.md D-025）。
pub struct TaxContext<'a> {
    pub as_of: AccountingDate,
    pub chart: &'a ChartOfAccounts,
    pub tag_schema: &'a TagSchema,
    pub counterparties: &'a CounterpartyIndex,   // DBから引いたスナップショット
}
```

税区分マスタ（`TaxCategoryTable`）と事業者設定（`JpSettings`）は
`kaikei-jp` 側の型であり、`TaxContext` には含めない。`JpTaxPolicy`
（`kaikei-jp` の `TaxPolicy` 実装）が**構築時**に保持し、マスタの選択は
`TaxContext::as_of`（取引日）で行う。

```rust
// kaikei-jp::tax 内部の状態。TaxContext には現れない。
pub struct JpTaxPolicy {
    rule_sets: TaxRuleSets,   // 適用期間の異なるマスタ群。取引日で引く
    settings: JpSettings,
}

pub struct JpSettings {
    pub tax_mode: TaxMode,               // Exclusive（税抜） / Inclusive（税込）
    pub rounding: RoundMode,             // 端数処理の方式
    pub rounding_unit: RoundingUnit,     // 端数処理の単位（Line / Document。§7）
    pub is_taxable_business: bool,       // 課税事業者か免税事業者か
    pub simplified_taxation: bool,       // 簡易課税か
}
```

**マスタの保持は「暦年 → マスタ1件」の写像にしない**（`DECISIONS.md` D-050）。
日本の消費税率改正は年度途中に起きるのが通例で（2019年10月の軽減税率導入など）、
暦年会計の個人事業主にとっては**1つの暦年に2つのマスタが適用される期間が
実際に生じる**ため、暦年をキーにすると表現できなくなる。

`TaxRuleSets`（実装済み。`crates/kaikei-jp/src/tax/rule_sets.rs`）が
各マスタの `applies_from` / `applies_to` を見て取引日から選ぶ。適用期間が
重なるマスタ群は構築時に拒否される（D-054）。該当するマスタが無い取引日に
対しては `for_date` が `None` を返すので、`JpTaxPolicy` 側で
`PolicyError::NoApplicableRuleSet { as_of }` に写像する（D-055）。

`JpSettings` はマスタ側の `settings_defaults`（`TaxSettingsDefaults`）を
既定値として、事業者ごとの設定で上書きする形になる。両者の合成規則は
`JpTaxPolicy` の実装時に決める。

YAML の読み込みは合成ルートの起動時 I/O であり、`TaxPolicy` の各メソッド自体は
引き続き同期の純関数のままになる。設定変更（税率改定・事業者区分の変更等）を
反映するには `Arc<dyn TaxPolicy>` を作り直す（単一ユーザー・自己ホスト前提
なのでプロセス再起動で足りる）。

---

## 3. 税区分マスタ（年度別 YAML）

実物は `crates/kaikei-jp-data/tax/jp/2026.yaml`。

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

実物は `crates/kaikei-jp-data/tags.yaml`。

新しいタグキーを使う前に必ずここへ登録する。core が未登録キーを拒否する。

`kaikei-jp::tags`（PR-5）が YAML → `kaikei_core::TagSchema` のロードを行う。
埋め込みデータ（`kaikei_jp_data::TAGS`）と任意パスからの差し替えの両方に対応する。
各タグ定義が持つ `description` は `kaikei_core::TagDef` に対応するフィールドが
無いため、ロード時に破棄する（`DECISIONS.md` D-062）。

---

## 5. 勘定科目テンプレート

実物は `crates/kaikei-jp-data/chart/sole_proprietor.yaml`。

個人事業主（青色申告）向けの標準的な科目体系。ユーザーは複製して自由に編集できる。

### 個人事業主固有の科目

| コード | 科目 | 種別 | 備考 |
|---|---|---|---|
| 400 | 元入金 | Equity | 年に1回だけ動く |
| 410 | 事業主貸 | Equity | 期首でゼロにリセット |
| 420 | 事業主借 | Equity | 期首でゼロにリセット |

### ローダ（PR-5）

`kaikei-jp::chart` が YAML → `kaikei_core::ChartOfAccounts` のロードを行う。
埋め込みデータ（`kaikei_jp_data::CHART_SOLE_PROPRIETOR`）と任意パスからの
差し替えの両方に対応する。`postable`・`parent` は省略可能（省略時はそれぞれ
`true`・`None`）。`sort` は `kaikei_core::AccountDef` に対応するフィールドが
無いためロード時に破棄する（`DECISIONS.md` D-061）。

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
2. `税額 = apply_ratio(本体, rate)`（端数処理込み。`TaxPolicy::apply_ratio` 相当）
3. 反対側（貸方）に `tax_account` の明細を追加

### 出力

```
売掛金 110,000 / 売上高      100,000 (tax_category=SALES_10)
                 仮受消費税   10,000 (tax_category=SALES_10)
```

### 実装の注意

- **端数処理は明細ごとか合計かを選べるようにする。**
  事業者が選択できるため `JpSettings` に持たせる（`rounding_unit`）。
  合計（`Document`）の場合は、同じ（税区分・側・税額科目）の本体を**先に合算してから
  1回だけ丸める**。明細ごとに丸めた値を足し直す形にすると合計がずれる
- 免税事業者（`is_taxable_business: false`）の場合、税行を生成しない（税込経理）
- 生成した税行にも元の `tax_category` を付ける（集計のため）
- 税額 0 の行は生成しない（`JournalLine::new` が 0 円を拒否する）

### 非適格の経過措置（`deduction_ratio` < 1）の扱い

**控除できない部分の帳簿処理は実装しない**（`DECISIONS.md` D-059）。

当初は「仮払消費税を減らして本体に足す」方式を想定していたが、これは
**税務判断そのもの**であり、`docs/08-compliance.md` §9-1 の税理士確認事項として
未解決のままである。確認が済む前に実装すると、`CLAUDE.md` §10「税務判断を
断定するメッセージを出さない」に反する形で帳簿の金額を確定させてしまう。

現在の `JpTaxPolicy` は `deduction_ratio` を**税額計算に反映せず**、
`rate` だけで税額を計算する。代わりに `TaxDerivation.notes` に
`PolicyNote`（`Warning`）を添えて、控除割合が 1 未満であること・控除できない
部分の処理が未実装であること・判断は税理士に確認すべきことを伝える。

同じ理由で、**簡易課税（`simplified_taxation`）のみなし仕入率による計算も
実装していない**。設定は保持するが `derive_tax_lines` の挙動は変えず、
`PolicyNote` を添えるのみ。

---

## 8. 家事按分

```rust
// crates/kaikei-jp/src/household_split.rs（実装済み）
pub struct HouseholdSplitInput {
    pub total: Money,
    pub business_ratio: Ratio,
    pub expense_account: AccountCode,   // 地代家賃など
    pub owner_account: AccountCode,     // 事業主貸
    pub payment_account: AccountCode,   // 現金・預金
    pub tax_category: Option<String>,   // 税区分コード。実在確認はここではしない
}

/// 按分後の明細を生成する。按分率は tags に残す
pub fn household_split(
    input: HouseholdSplitInput,
    settings: &JpSettings,
) -> Result<Vec<JournalLine>, JpError>;
```

`tax_category` は `TaxCategoryCode` のような専用型にしない。この関数は
税区分マスタ（`TaxRuleSets`）も取引日も持たないため、渡された文字列が実在する
区分かを検証できない。実在確認は記帳時に `TaxPolicy::validate_tag` が行う
（`DECISIONS.md` D-064）。

**`tax_category` は型の上では `Option` だが、実質的にはほぼ必須。**
`tags.yaml` の `tax_category` は `required_for: [Revenue, Expense]` であり、
`expense_account` に費用科目（地代家賃など）を指定する限り、`None` のまま
`JournalEntry::new` に渡すと `MissingRequiredTag` で弾かれる。
`household_split` 自身が弾かないのは、`ChartOfAccounts` を持たず科目種別を
判定できないため（ユーザーが差し替えた科目表では分類が違いうる）。

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
- `ClosingPolicy::closing_entries` / `opening_entries` は `kaikei-policy::TaxContext`
  を引数に取らない（`kaikei-policy` の trait 定義を参照）。元入金・事業主貸・
  事業主借の**科目コードは `JpSoleProprietorClosingPolicy` が構築時に保持する**。
  年度別税区分マスタ・事業者設定を `JpTaxPolicy` が構築時に保持するのと同じ
  パターン（`DECISIONS.md` D-025）であり、科目コード体系が変わった場合は
  実装を作り直す（再起動する）ことで追従する

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
