# 05 — CSV 取込（kaikei-import）

## 1. 最重要の設計判断：取込データは仕訳ではない

銀行明細 CSV の 1 行は**「仕訳」ではなく「未処理の取引記録」**。
別集約・別コンテキストとして分離する。

ここを間違えると設計全体が汚染される。

| コンテキスト | 語彙 |
|---|---|
| 記帳（core） | 仕訳、勘定科目、借方/貸方、試算表 |
| 取込（import） | 取引、入金/出金、摘要、未処理 |

「入金/出金」と「借方/貸方」は似ているが同じではない
（借方は資産増加も費用発生も表す）。直接変換せず、
`kaikei-app/usecase/journalize.rs` を翻訳層とする。

**`kaikei-import` は `kaikei-core` に依存しない。**

---

## 2. 型定義

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ImportedTxId(u128);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceId(String);       // "example_bank", "example_card"

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction { In, Out }     // 入金 / 出金

#[derive(Debug, Clone)]
pub struct ImportedTransaction {
    pub id: ImportedTxId,
    pub source: SourceId,
    pub external_key: String,      // 冪等性のキー
    pub occurred_on: NaiveDate,
    pub amount_minor: i64,         // 常に正。符号は direction で表現
    pub currency: String,
    pub direction: Direction,
    pub raw_description: String,   // "ｶ)ｻﾝﾌﾟﾙ ｼﾖｳｼﾞ"
    pub balance_after: Option<i64>,
    pub raw_row: serde_json::Value,  // 元CSV行を丸ごと保持
    pub status: ImportStatus,
    pub imported_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub enum ImportStatus {
    Pending,                              // 未仕訳
    Journalized { entry_id: u128 },        // 仕訳済み
    Ignored { reason: String },            // 事業外・重複などで無視
}
```

**このテーブルは UPDATE 可**（status が変わる）。帳簿本体とは別世界なので
append-only 制約をかけない。**この分離があるから帳簿の不変性を守れる。**

---

## 3. DB スキーマ

```sql
CREATE TABLE imported_transactions (
    id              UUID PRIMARY KEY,
    source          TEXT NOT NULL,
    external_key    TEXT NOT NULL,
    occurred_on     DATE NOT NULL,
    amount_minor    BIGINT NOT NULL CHECK (amount_minor > 0),
    currency        CHAR(3) NOT NULL DEFAULT 'JPY',
    direction       SMALLINT NOT NULL CHECK (direction IN (1, 2)),  -- 1=In, 2=Out
    raw_description TEXT NOT NULL,
    balance_after   BIGINT,
    raw_row         JSONB NOT NULL,
    status          TEXT NOT NULL,          -- pending / journalized / ignored
    entry_id        UUID REFERENCES journal_entries(id),
    ignore_reason   TEXT,
    imported_at     TIMESTAMPTZ NOT NULL,
    UNIQUE (source, external_key)
);

CREATE INDEX idx_imported_status ON imported_transactions (status, occurred_on);
CREATE INDEX idx_imported_date   ON imported_transactions (occurred_on);
```

`UNIQUE (source, external_key)` が冪等性を担保する。

---

## 4. 冪等性

同じ CSV を 2 回取り込んでも重複しないこと。必須要件。

### external_key の作り方

1. 銀行が取引 ID を持っていればそれを使う
2. 無ければ `sha256(occurred_on | amount | direction | description | balance_after)`

**`balance_after`（取引後残高）を含めるのがコツ。**
同日同額同摘要の取引（コンビニで 2 回買った等）を区別できる。

残高列が無い CSV の場合は、同一グループ内の連番を付与する。

```rust
/// 同一 (date, amount, direction, description) が複数ある場合、
/// CSV 内の出現順で連番を付けて区別する
fn build_external_key(row: &ParsedRow, occurrence: u32) -> String;
```

### 取込結果の返却

```rust
pub struct ImportResult {
    pub inserted: usize,
    pub skipped_duplicate: usize,
    pub errors: Vec<RowError>,   // 行番号と理由
}
```

**部分成功を許す。** 1 行のパースエラーで全体を失敗させない。
エラー行は理由付きで返し、ユーザーが判断できるようにする。

---

## 5. CSV プロファイル（データで持つ）

銀行ごとにフォーマットが違う地獄への対処。**コードでなくデータ。**

`crates/kaikei-import-data/profiles/*.yaml`
（例は `crates/kaikei-import-data/profiles/csv-profile-example.yaml`）

### スキーマ

```yaml
id: example_bank
name: みずほ銀行 ビジネスWEB
kind: bank                    # bank | credit_card
encoding: Shift_JIS           # ★ ほぼ全ての邦銀がこれ
delimiter: ","
skip_rows: 1
skip_trailing_rows: 0         # 合計行などを飛ばす

date:
  column: 0
  format: "%Y/%m/%d"
  era: false                  # true なら和暦（R08/04/15 等）

amount:
  mode: separate_columns      # separate_columns | signed_column
  debit_column: 2             # 出金（お支払金額）
  credit_column: 3            # 入金（お預り金額）
  thousands_separator: true
  # mode: signed_column の場合
  # column: 2
  # positive_means: In        # 正の値が入金か出金か

description:
  columns: [4, 5]             # 複数列を連結
  separator: " "
  trim: true
  normalize_kana: true        # 半角カナ → 全角（検索性のため）

balance:
  column: 6
  optional: true

external_key: [date, amount, direction, description, balance]
```

### 対応すべき現実

| 問題 | 対処 |
|---|---|
| Shift-JIS | `encoding_rs` で変換。**必須** |
| 金額のカンマ | `thousands_separator: true` |
| 和暦 | `era: true`。`R08/04/15` → 2026-04-15 |
| 入金/出金が別列 | `mode: separate_columns` |
| 符号付き 1 列 | `mode: signed_column` |
| 半角カナ | `normalize_kana` で全角化 |
| BOM | 自動除去 |
| 合計行が末尾にある | `skip_trailing_rows` |
| 空行 | 自動スキップ |

**新しい銀行への対応が YAML を 1 枚追加するだけになる。**
これは OSS としてコミュニティから PR を受け付けやすい形でもあり、成長経路になる。

### 用意すべき初期プロファイル

- みずほ / 三菱UFJ / 三井住友 / ゆうちょ / 楽天銀行 / 住信SBI / PayPay銀行
- 楽天カード / 三井住友カード / JCB / Amex
- freee / 弥生 のエクスポート形式（移行用）

**実際の CSV サンプルを `tests/fixtures/` に置いてテストする。**
個人情報を含まないダミーデータを自作すること。実データをコミットしない。

---

## 6. 仕訳化（journalize）

### ルールエンジン

```rust
pub struct JournalizeRule {
    pub id: u128,
    pub priority: i32,
    pub source: Option<SourceId>,          // 特定ソースのみに適用
    pub direction: Option<Direction>,
    pub pattern: DescriptionPattern,
    pub amount_range: Option<(i64, i64)>,
    // 生成する仕訳のテンプレート
    pub account: AccountCode,
    pub counter_account: AccountCode,      // 通常は現金/預金
    pub tax_category: Option<String>,
    pub counterparty: Option<String>,
    pub business_ratio: Option<Ratio>,     // 家事按分も自動化できる
    pub active: bool,
}

pub enum DescriptionPattern {
    Contains(String),
    StartsWith(String),
    Regex(String),
}
```

`"ｶ)ｻﾝﾌﾟﾙ"` → 消耗品費 / 課税仕入10%（適格）

`priority` の昇順で評価し、最初にマッチしたルールを採用する。

### AI（MCP）の役割

ルールで確定できるものは自動化し、**残りを MCP 経由で AI に提案させる**。

ここがこのプロダクトの差別化点。既存の会計ソフトも学習型の自動仕訳を持つが、
**「なぜその科目にしたか」を説明できない。**
AI なら「サンプル商会での購入、金額1,980円、過去の類似取引は消耗品費」と根拠を出せる。

### 設計上の鉄則

- `suggest_*` 系のツールは**提案のみ**。確定は人間
- 提案には必ず**根拠**を含める（類似する過去仕訳、マッチしたルール）
- 確信度が低い場合はそう言う。断定しない
- 1 取引 → 1 仕訳が基本だが、**1 対 N も許す**
  （まとめ入金 → 複数の売掛金消込）

### 状態遷移

```
Pending ──journalize──> Journalized { entry_id }
   │
   └──ignore──> Ignored { reason }

Journalized の取り消しは？
  → 帳簿の逆仕訳を起こし、ImportedTransaction を Pending に戻す
  → 「Journalized → Pending」の遷移は許すが、監査ログに残す
```

---

## 7. 出力側（kaikei-report）

| 出力 | 用途 | 優先度 |
|---|---|---|
| 仕訳日記帳 CSV | 汎用・確認用 | 高 |
| 総勘定元帳 CSV | 科目別確認 | 高 |
| **弥生インポート形式 CSV** | **税理士連携・乗り換え** | **最高** |
| freee インポート形式 CSV | 乗り換え | 中 |
| 全件 JSON エクスポート | 可搬性・バックアップ | 高 |

弥生形式を塞ぐと、いくら中身が良くても使われない。ここは早めに作る。

---

## 8. テスト方針

```
kaikei-import/tests/
├── fixtures/
│   ├── example_bank_sample.csv          Shift-JIS のダミーデータ
│   ├── example_card_sample.csv
│   ├── era_date_sample.csv        和暦
│   ├── signed_column_sample.csv
│   └── broken_rows.csv            エラー行を含む
└── import_test.rs
```

| # | ケース | 期待 |
|---|---|---|
| I-01 | Shift-JIS の CSV を取込 | 文字化けしない |
| I-02 | 同じ CSV を 2 回取込 | 2 回目は全件 skipped_duplicate |
| I-03 | 同日同額同摘要が 2 行 | 別の取引として 2 件登録される |
| I-04 | 和暦の日付 | 正しく西暦に変換 |
| I-05 | 符号付き 1 列 | direction が正しい |
| I-06 | 金額にカンマ | パースできる |
| I-07 | 不正な行が混在 | 正常行は登録、不正行は errors に |
| I-08 | BOM 付き UTF-8 | 除去される |
| I-09 | 空行・合計行 | スキップされる |
| I-10 | ルールにマッチして仕訳化 | 貸借一致した仕訳が生成される |
| I-11 | 家事按分ルール | 3 行の仕訳が生成される |
| I-12 | 仕訳化済みを再度仕訳化 | エラー（二重仕訳の防止） |
