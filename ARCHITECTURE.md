# ARCHITECTURE.md

## 1. 基本方針

**層の分離をフォルダではなく crate 境界で行う。**

| 手段 | 違反したとき |
|---|---|
| フォルダ（`domain/`, `infrastructure/`） | 動く。規約なので破れる |
| crate 境界（Cargo） | **コンパイルエラー。物理的に不可能** |

`kaikei-core` の `Cargo.toml` に `sqlx` が無い限り、ドメイン層から DB を触るコードは
**書けない**。フォルダ分割なら `use crate::infrastructure::db` と書けてしまい、
数年後には誰かが必ず書いている。

この構成はフォルダ階層版より DDD に忠実である。層の分離をコンパイラに強制させている。

---

## 2. crate 構成

```
kaikei/
├── Cargo.toml                    ワークスペース
├── crates/
│   ├── kaikei-core/              ① 不変。複式簿記エンジン
│   ├── kaikei-policy/            ②③ 可変部の抽象（traitのみ）
│   ├── kaikei-jp/                ④ 日本・個人事業主アダプタ
│   ├── kaikei-jp-data/           年度別YAML（税区分/科目/タグスキーマ）
│   ├── kaikei-store/             永続化。append-onlyをDB権限で強制
│   ├── kaikei-blob/              証憑ファイル（Content-Addressed Storage）
│   ├── kaikei-import/            CSV取込。別コンテキスト
│   ├── kaikei-import-data/       銀行別CSVプロファイルYAML
│   ├── kaikei-report/            青色申告決算書・各種CSV出力
│   ├── kaikei-app/               application層。I/Oはここだけ
│   ├── kaikei-api/               axum HTTP
│   └── kaikei-mcp/               MCPサーバー
└── .github/workflows/
    └── architecture.yml          依存方向をCIで検査
```

### DDD 層との対応

| DDD の層 | crate |
|---|---|
| domain | `kaikei-core`（これ全体が domain） |
| domain のポート | `kaikei-policy`（trait） |
| domain の可変部実装 | `kaikei-jp` |
| application | `kaikei-app` |
| infrastructure | `kaikei-store`, `kaikei-blob`, `kaikei-import` |
| presentation | `kaikei-api`, `kaikei-mcp` |

`kaikei-core` の中にさらに `domain/` を作る必要はない。**crate 全体が domain だから。**

---

## 3. 依存方向

```
kaikei-core          ← 何にも依存しない（rust_decimal, thiserror のみ）
     ↑
kaikei-policy        ← trait定義のみ
     ↑
kaikei-jp            ← policy実装 + jp-data
     ↑
kaikei-app           ← core + policy(trait)。jpは注入される
     ↑
kaikei-store / kaikei-blob / kaikei-import / kaikei-report
     ↑
kaikei-api / kaikei-mcp
```

### CI による強制

```yaml
# .github/workflows/architecture.yml
- name: kaikei-core は依存を持たない
  run: |
    DEPS=$(cargo tree -p kaikei-core --edges normal --prefix none \
           | grep -v '^kaikei-core' | grep -v '^$' \
           | sed 's/ .*//' | sort -u)
    ALLOWED="rust_decimal thiserror"
    for d in $DEPS; do
      echo "$ALLOWED" | grep -qw "$d" || { echo "禁止された依存: $d"; exit 1; }
    done
```

`skeleton/architecture-ci.yml` に完全版がある。**設計を人間の意志ではなく仕組みで守る。**

---

## 4. crate 内のフォルダ分割

### 原則：技術的役割ではなくドメイン概念で切る

```
✅ kaikei-core/src/
   ├── lib.rs
   ├── money.rs           Money, Currency, Ratio
   ├── account.rs         AccountCode, AccountType, AccountDef, ChartOfAccounts
   ├── tag.rs             TagKey, TagValue, TagSet, TagSchema
   ├── journal.rs         JournalEntry, JournalLine, Side  ★集約
   ├── period.rs          FiscalYear, AccountingDate, PeriodStatus
   ├── trial_balance.rs   TrialBalance（read model）
   ├── clock.rs           Clock trait
   └── error.rs           CoreError
```

```
❌ kaikei-core/src/
   ├── entities/
   ├── value_objects/
   ├── repositories/
   └── services/
```

**DDDのパターン名でフォルダを切るのはアンチパターン。**

- `entities/` と `value_objects/` は技術的分類であり、会計の言葉ではない（ユビキタス言語に反する）
- 仕訳を理解するのに `entities/journal_entry.rs` と `value_objects/money.rs` を往復させられる
- 「これはエンティティか値オブジェクトか」という生産性ゼロの議論が発生する

### Rust 固有の理由：可視性

```rust
// journal.rs
pub struct JournalEntry {
    lines: Vec<JournalLine>,   // private
}
```

private フィールドは**同一モジュール内からのみ**アクセスできる。
ファイルを分けると `pub(crate)` に緩めるしかなく、カプセル化が崩れる。

→ **集約は1モジュールに収める**のが Rust では自然な帰結。
Rust のモジュールシステムは集約の境界と相性が良い。

1000行を超えた場合の分割：

```
journal/
├── mod.rs        JournalEntry本体（private フィールドを触るコードはここに集約）
├── line.rs       JournalLine（独立した値）
└── validate.rs   pub(super) な検証ヘルパー
```

---

## 5. application 層は縦に切る

```
kaikei-app/src/
├── lib.rs
├── context.rs              TaxContext等の組み立て
├── ports.rs                Repository trait（domainが要求する穴）
├── error.rs                AppError
└── usecase/
    ├── post_entry.rs       仕訳を起こす
    ├── reverse_entry.rs    赤伝を起こす
    ├── import_csv.rs       CSV取込
    ├── journalize.rs       取込明細を仕訳化（★コンテキスト間の翻訳層）
    ├── attach_document.rs  証憑を紐付ける
    ├── close_period.rs     期間を締める
    └── report.rs           試算表・決算書
```

**ユースケース1つ = 1ファイル = 1関数。**
`AccountingService` のような巨大構造体を作らない（定番の崩壊パターン）。

```rust
// usecase/post_entry.rs
pub struct PostEntryInput { /* ... */ }
pub struct PostEntryOutput { pub entry_id: EntryId, pub entry_no: EntryNumber }

pub async fn execute<R, T>(
    repo: &R,
    tax: &T,
    clock: &dyn Clock,
    input: PostEntryInput,
) -> Result<PostEntryOutput, AppError>
where
    R: JournalRepository,
    T: TaxPolicy,
{
    // 1. I/O: 必要なデータをロード（policyは純関数なので事前に集める）
    // 2. policy: 消費税行を導出
    // 3. domain: JournalEntry::new で不変条件を検証
    // 4. I/O: 保存
}
```

構造体のメソッドではなく**関数**にすることで、依存が引数に全部現れる。

---

## 6. store 層

```
kaikei-store/src/
├── lib.rs
├── pool.rs
├── journal_repository.rs      JournalRepository実装
├── chart_repository.rs
├── counterparty_repository.rs
├── imported_tx_repository.rs
├── document_repository.rs
├── numbering.rs               採番（カウンタ行をFOR UPDATE）
├── query/                     ★ read model（Repositoryを通さない）
│   ├── trial_balance.rs       SQL集計 → DTO直行
│   ├── ledger.rs
│   └── search.rs
└── migrations/
```

`query/` の分離が重要。**書き込みはドメインモデル経由、読み取りは SQL 集計。**
混ぜると、試算表のために集約を全部ロードする実装に流れる。

### Repository のシグネチャ

```rust
// kaikei-app/src/ports.rs
#[async_trait]
pub trait JournalRepository: Send + Sync {
    async fn find(&self, id: &EntryId) -> Result<Option<JournalEntry>, RepoError>;
    async fn save(&self, entry: &JournalEntry) -> Result<(), RepoError>;
    // update / delete は定義しない
}
```

`Arc<dyn JournalRepository>` を `State` に入れる。
Router の State をジェネリックにすると型が爆発するため、trait object を選ぶ。

---

## 7. jp 層

```
kaikei-jp/src/
├── lib.rs
├── tax/
│   ├── category.rs       TaxCategoryCode, TaxCategoryTable
│   ├── policy.rs         TaxPolicy実装
│   ├── rounding.rs       端数処理
│   └── invoice_no.rs     InvoiceRegistrationNo（チェックデジット検証）
├── sole_proprietor/      ★ 個人事業主固有
│   ├── closing.rs        元入金振替のClosingPolicy
│   ├── household.rs      家事按分
│   └── statement.rs      青色申告決算書の様式
├── chart.rs              勘定科目テンプレート読み込み
└── tags.rs               TagSchema提供
```

`sole_proprietor/` と切ることで、将来法人対応するときに `corporation/` が並ぶだけになる。
**拡張の形が構造に見えている**状態を保つ。

---

## 8. 境界づけられたコンテキスト

この構成には**別のコンテキストが2つ**ある。

| コンテキスト | 語彙 | crate |
|---|---|---|
| 記帳 | 仕訳、勘定科目、借方/貸方、試算表 | core, policy, jp, store |
| 取引明細取込 | 取引、入金/出金、摘要、未処理 | import |

`ImportedTransaction` と `JournalEntry` は別の言語圏の住人。
「入金/出金」と「借方/貸方」は似ているが同じではない（借方は資産増加も費用発生も表す）。

→ 直接変換せず `usecase/journalize.rs` を翻訳層とする（Context Map の実装）。
**`kaikei-import` が `kaikei-core` に依存していないことが分離の証拠。**

---

## 9. 拡張性リスクと対策（設計レビュー結果）

5年運用を想定した既知のリスク。

| ID | リスク | 対策 |
|---|---|---|
| R1 | `TagSet` がゴミ箱化する（キーが50種類、typo混入） | `TagSchema` を core が検証。未登録キーを拒否 |
| R2 | 分析軸（取引先/プロジェクト/按分率）が無く後付けで集計を全書き換え | `TrialBalance::from_entries` に `group_by: &[TagKey]` を最初から持たせる |
| R3 | policy trait に async と I/O が侵入しテストが重くなる | policy は純関数固定。`TaxContext` を引数で渡す |
| R4 | `Numbering` の `&mut self` が非同期・DBと噛み合わない | store 経由。カウンタ行を `FOR UPDATE`。欠番方針を明文化 |
| R5 | core が肥大化する（「消費税もcoreにあった方が便利」） | CI で依存を機械的に検査 |
| R6 | 外貨が来る（Stripe USD入金等） | `Currency` は最初から持つ。`FxPolicy` は Phase 後半 |

R1 と R2 は一体の設計。`aggregatable: true` を宣言したキーだけが `group_by` に渡せる。

---

## 10. 技術選定

| 項目 | 選定 | 理由 |
|---|---|---|
| Web | axum | tower/hyper エコシステム。薄い |
| DB | **PostgreSQL 固定** | JSONB、GIN、テーブル単位の権限制御が必要 |
| DB アクセス | sqlx | ORM を使わない。Data Mapper を手書きする |
| Decimal | rust_decimal | 按分率・税率の計算に使用 |
| CSV | csv + **encoding_rs** | 邦銀CSVはほぼ Shift-JIS |
| エラー | thiserror（lib）/ anyhow（bin） | |
| 日付 | core は自前の `AccountingDate` | chrono を core に入れない |

### ORM を使わない理由

DDD ではドメインエンティティのフィールドが private、生成と変更はドメインのメソッド経由のみ。
ORM は「DBの行からオブジェクトを復元する」ためにカプセル化を外から破る必要がある。

C#/Java はリフレクションで解決するが、**Rust にはランタイムリフレクションが無い**。
derive macro で private フィールドに触ることは技術的に可能だが、
その瞬間ドメイン構造体にインフラ由来の属性が付き、避けたかった結合が戻る。

→ **永続化専用の Row 型を別に定義し、`TryFrom` で変換する。**

```rust
#[derive(sqlx::FromRow)]
struct JournalEntryRow { /* ... */ }     // 永続化専用DTO

impl TryFrom<(JournalEntryRow, Vec<JournalLineRow>)> for JournalEntry { /* ... */ }
```

エンティティと Row を別の型にした瞬間に ORM の制約から解放される。
sqlx なら `query_as!` でコンパイル時に SQL も検証されるので安全性も落ちない。
