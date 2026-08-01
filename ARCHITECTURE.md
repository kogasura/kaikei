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

これは**最終形**。Phase 2 完了時点で実在するのは `kaikei-core` / `kaikei-policy` /
`kaikei-app` / `kaikei-store` / `kaikei-jp` / `kaikei-jp-data`（進捗は `README.md`）。

このほか、`kaikei-store` と `kaikei-jp` が互いを知れないという依存方向の制約
（本節末尾の図）を守ったまま両者を実際に PostgreSQL に繋いで検証するための
テスト専用 crate `kaikei-e2e` が Phase 2 で追加されている。**この最終形の図には
含めない**（本番の合成ルートではなく、テストの置き場として存在する。
`DECISIONS.md` D-068）。

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

`(P2+)` は Phase 2 以降で追加するもの。それ以外は Phase 1 完了時点で存在する。

```
kaikei-app/src/
├── lib.rs                  kaikei-policy 型の再エクスポート（D-047）
├── context.rs              BookSettings / 会計年度の決定 / PostingContext の組み立て
├── ports.rs                Store / TxOps / 各 Repo trait（domainが要求する穴）
├── tx.rs                   with_tx（commit/rollback の取りこぼしを型で防ぐ）
├── error.rs                AppError / RepoError
├── clock.rs                SystemClock（AppClock: Clock + Send + Sync）
├── id.rs                   UuidV7IdGenerator
├── currency.rs             帳簿通貨の決定
├── period_guard.rs         ClosedPeriodGuard（core の PeriodGuard 実装）
├── view.rs                 read model の DTO（BalanceRowView / TrialBalanceView）
├── testing.rs              InMemoryStore 等の fake（testing feature 配下）
└── usecase/
    ├── post_entry.rs       仕訳を起こす
    ├── reverse_entry.rs    赤伝を起こす
    ├── report.rs           試算表
    ├── import_csv.rs       CSV取込                        (P2+)
    ├── journalize.rs       取込明細を仕訳化（★翻訳層）    (P2+)
    ├── attach_document.rs  証憑を紐付ける                 (P2+)
    └── close_period.rs     期間を締める                   (P2+)
```

**ユースケース1つ = 1ファイル = 1関数。**
`AccountingService` のような巨大構造体を作らない（定番の崩壊パターン）。

```rust
// usecase/post_entry.rs（実物）
pub struct PostEntryInput {
    pub entry_date: AccountingDate,
    pub description: String,
    pub lines: Vec<JournalLine>,
    pub auto_tax_lines: bool,
}

pub async fn execute<Tx>(
    tx: &mut Tx,
    tax: &dyn TaxPolicy,
    tag_schema: &TagSchema,
    id_gen: &dyn IdGenerator,
    clock: &dyn AppClock,
    settings: &BookSettings,
    input: PostEntryInput,
) -> Result<JournalEntry, AppError>
where
    Tx: TxOps,
{
    // 1. I/O   : 会計年度・勘定科目表・締め状態・取引先索引をロード
    // 2. 純関数: タグ検証（税額行の導出より前、元の明細に対して）
    // 3. 純関数: 消費税行を導出（1回だけ。戻り値は確定後の明細一覧）
    // 4. I/O   : 採番（失敗しうる検証を全て終えた直後・INSERT の直前）
    // 5. domain: JournalEntry::new で不変条件を検証（これ以降 lines に触らない）
    // 6. I/O   : 保存
}
```

構造体のメソッドではなく**関数**にすることで、依存が引数に全部現れる。
依存を `PostEntryDeps` のような構造体にまとめない理由は `DECISIONS.md` D-045。

I/O とドメイン検証の**順序**が重要。採番（4）を検証（2・3・5）より後に置くことで、
検証失敗が採番を消費しない（同一トランザクションなので巻き戻る）。
`JournalEntry::new`（5）より後で `lines` に触ると貸借一致の検証を迂回できてしまう。

---

## 6. store 層

Phase 1 完了時点の実物（`ledger.rs` / `search.rs` / 取込・証憑の Repository は
Phase 2 以降）:

```
kaikei-store/src/
├── lib.rs
├── pool.rs                    PgStore（Store 実装）と接続確立ヘルパ
├── store.rs                   PgTx（TxScope / TxOps 実装）
├── journal/                   ★ 集約1つ = 1モジュール（CLAUDE.md §6）
│   ├── mod.rs                 JournalRepo 実装（insert_entry / find_entry）
│   ├── row.rs                 永続化専用の Row 型
│   └── mapper.rs              Row → JournalEntry（rehydrate を呼ぶ唯一の場所）
├── chart.rs                   ChartRepo 実装
├── period.rs                  PeriodRepo 実装（締めスナップショット）
├── numbering.rs               NumberingRepo 実装（採番）
├── convert.rs                 core の値オブジェクト ⇄ DB 表現
├── tags.rs                    TagSet ⇄ JSONB
├── sqlstate.rs                SQLSTATE → RepoError（DB 接続なしでテスト可能）
├── error.rs                   sqlx::Error → RepoError の入口
├── bin/kaikei-migrate.rs      マイグレーション実行バイナリ
├── query/                     ★ read model（Repositoryを通さない）
│   └── trial_balance.rs       SQL集計 → DTO直行
└── ../migrations/
```

`query/` の分離が重要。**書き込みはドメインモデル経由、読み取りは SQL 集計。**
混ぜると、試算表のために集約を全部ロードする実装に流れる。

### Repository のシグネチャ

トランザクションは `&mut Tx` として**引数で引き回す**（`DECISIONS.md` D-029）。
Repository を `Arc<dyn Repo>` として持ち回り、その内側でトランザクションを
開始する形は採らない。「複数の Repository をまたぐ操作を1つのトランザクションに
収める」ことがシグネチャから読めなくなるため。

```rust
// kaikei-app/src/ports.rs（実物）
#[async_trait]
pub trait JournalRepo: Send {
    async fn find_entry(&mut self, id: EntryId) -> Result<Option<JournalEntry>, RepoError>;
    async fn find_reversal_of(&mut self, id: EntryId)
        -> Result<Option<(EntryId, EntryNumber)>, RepoError>;
    async fn insert_entry(&mut self, entry: &JournalEntry) -> Result<(), RepoError>;
    // update / delete は定義しない（CLAUDE.md §2）
}

/// 1つのトランザクションで使える操作の総体。ユースケースはこれを `&mut Tx` で受け取る。
pub trait TxOps: JournalRepo + ChartRepo + PeriodRepo + NumberingRepo + Send {}
```

`&self` ではなく `&mut self` なのは、`PgTx` が
`sqlx::Transaction` を排他的に借りるため（同一トランザクション上で2つの
リポジトリ操作を同時に走らせることは原理的にできない）。この制約を
`Arc<Mutex<..>>` で隠さず型に出すことで、借用チェッカが
「トランザクションを跨いだ並行アクセス」をコンパイル時に禁じる。

### 合成ルート（axum の `State`）

`Arc<dyn Store<Tx = ..>>` のような trait object ではなく、**具象型
`Arc<PgStore>`** を `State` に積む。`Store` は関連型 `Tx` を持つため、
trait object 化するには `Tx` を dyn 化の時点で具象型に固定する必要があり、
その時点で「実装を差し替えられる」という抽象化の利点がほとんど残らない
（本番で使う `Store` 実装は `kaikei-store::PgStore` の1つだけ）。
一方 `with_tx<S: Store>` はジェネリックのまま使えるため、具象型を渡しても
呼び出し側が実装の詳細を意識する必要はない。詳細は `DECISIONS.md` D-029。

---

## 7. jp 層

Phase 2 完了時点の実物（当初案の `sole_proprietor/` サブフォルダは採らなかった。
理由は下記）:

```
kaikei-jp/src/
├── lib.rs
├── yaml.rs                YAML文字列 → T: DeserializeOwned の共通ローダ
├── error.rs                JpError
├── invoice.rs               InvoiceRegistrationNo（チェックデジット検証）
├── chart.rs                 勘定科目テンプレート読み込み → ChartOfAccounts
├── tags.rs                  タグスキーマ読み込み → TagSchema
├── household_split.rs       家事按分（TaxPolicyの実装ではなく独立関数。D-064）
├── closing.rs                JpSoleProprietorClosingPolicy（ClosingPolicy実装）
├── statement.rs             JpStatementPolicy（StatementPolicy実装）
├── account_type.rs          AccountType の文字列パース共通処理
├── test_support.rs          #[cfg(test)] 専用の共通ヘルパ
└── tax/
    ├── mod.rs
    ├── category.rs          TaxCategory, TaxDirection（1区分ぶん）
    ├── table.rs              TaxCategoryTable（1適用期間ぶんのマスタ集合）
    ├── rule_sets.rs          TaxRuleSets（複数マスタ・取引日による選択）
    ├── settings.rs           TaxSettingsDefaults, TaxMode, RoundingUnit
    └── policy.rs             JpTaxPolicy（TaxPolicy実装）, JpSettings
```

**当初案の `sole_proprietor/` サブフォルダは採らなかった。**
個人事業主固有のファイル（`closing.rs` / `household_split.rs` / `statement.rs`）
は `kaikei-jp` 自体が個人事業主専用の crate（`DECISIONS.md` D-017）であるため、
crate 内でさらにサブフォルダへ切っても新たな情報が増えなかった。将来法人対応が
具体化した段階で `kaikei-jp/src/corporation/` を追加する形で拡張する
（`DECISIONS.md` D-017 の「拡張の余地」）。

**`TaxCategoryCode` という専用型も作らなかった。** 税区分コードは
`TaxCategory::code`（`String`）としてマスタが保持し、`TagValue::Code` に
そのまま格納する。`household_split` はマスタ（`TaxRuleSets`）を保持しないため
文字列の実在確認をその場ではできず、newtype 化しても検証を前倒しできない
（`kaikei-jp/src/household_split.rs` モジュール doc「`tax_category` を独自の
型にしない」を参照）。実在確認は記帳時に `TaxPolicy::validate_tag` が行う。

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
