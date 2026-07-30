# 03 — データベース設計

DB は **PostgreSQL 固定**。JSONB、GIN インデックス、テーブル単位の権限制御が必要なため。

---

## 1. 原則：append-only を DB 権限で物理的に強制する

アプリのコードだけで守るのは不十分。

```sql
-- ロール作成
CREATE ROLE kaikei_migrator LOGIN PASSWORD '...';   -- マイグレーション専用
CREATE ROLE kaikei_app      LOGIN PASSWORD '...';   -- アプリ実行用

-- 帳簿本体は INSERT と SELECT のみ
GRANT SELECT, INSERT ON journal_entries, journal_lines, entry_documents TO kaikei_app;
REVOKE UPDATE, DELETE ON journal_entries, journal_lines, entry_documents FROM kaikei_app;

-- 可変テーブルは通常通り
GRANT SELECT, INSERT, UPDATE, DELETE ON imported_transactions, journalize_rules TO kaikei_app;
GRANT SELECT, INSERT, UPDATE ON accounts, counterparties, entry_counters TO kaikei_app;
GRANT SELECT, INSERT ON documents, period_snapshots TO kaikei_app;
```

これで**バグでも AI でも帳簿を書き換えられない**。
電子帳簿保存法における真実性の確保について、タイムスタンプ付与か
訂正削除履歴が残るシステムのいずれかが求められるが、この構造は後者に対応する。

### 補助的にトリガでも防ぐ（多層防御）

```sql
CREATE OR REPLACE FUNCTION reject_mutation() RETURNS trigger AS $$
BEGIN
  RAISE EXCEPTION 'append-only table: % は変更できません（訂正は逆仕訳で行ってください）',
    TG_TABLE_NAME;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER no_update_journal_entries
  BEFORE UPDATE OR DELETE ON journal_entries
  FOR EACH ROW EXECUTE FUNCTION reject_mutation();

CREATE TRIGGER no_update_journal_lines
  BEFORE UPDATE OR DELETE ON journal_lines
  FOR EACH ROW EXECUTE FUNCTION reject_mutation();
```

---

## 2. スキーマ

### 帳簿本体（append-only）

```sql
CREATE TABLE journal_entries (
    id             UUID PRIMARY KEY,
    fiscal_year    INTEGER NOT NULL,
    entry_no       INTEGER NOT NULL,
    entry_date     DATE NOT NULL,           -- 取引日。TZ を持たせない
    description    TEXT NOT NULL CHECK (btrim(description) <> ''),
    reverses       UUID REFERENCES journal_entries(id),
    reverse_reason TEXT,
    recorded_at    TIMESTAMPTZ NOT NULL,    -- 記帳時刻。UTC
    UNIQUE (fiscal_year, entry_no),
    CHECK ((reverses IS NULL) = (reverse_reason IS NULL))
);

CREATE INDEX idx_entries_date ON journal_entries (entry_date);
CREATE INDEX idx_entries_fy   ON journal_entries (fiscal_year, entry_no);
CREATE INDEX idx_entries_rev  ON journal_entries (reverses) WHERE reverses IS NOT NULL;

CREATE TABLE journal_lines (
    entry_id     UUID    NOT NULL REFERENCES journal_entries(id),
    line_no      SMALLINT NOT NULL,
    account_code TEXT    NOT NULL,
    side         SMALLINT NOT NULL CHECK (side IN (1, 2)),  -- 1=借方, 2=貸方
    amount_minor BIGINT  NOT NULL CHECK (amount_minor > 0),
    currency     CHAR(3) NOT NULL DEFAULT 'JPY',
    tags         JSONB   NOT NULL DEFAULT '{}',
    memo         TEXT,
    PRIMARY KEY (entry_id, line_no)
);

CREATE INDEX idx_lines_account ON journal_lines (account_code);
CREATE INDEX idx_lines_tags    ON journal_lines USING GIN (tags);
```

**`entry_date` は `DATE`、`recorded_at` は `TIMESTAMPTZ`。**
取引日にタイムゾーンを持たせると年度跨ぎで必ず事故る。ここは絶対に混ぜない。

`amount_minor` は `BIGINT`。core は `i128` だが、実際の金額で 2^63 を超えることはない。
保存時に範囲チェックしてエラーにする。

### tags の JSONB 形式

```json
{
  "tax_category":   { "t": "code",    "v": "PURCHASE_10_QUALIFIED" },
  "counterparty":   { "t": "code",    "v": "CP0012" },
  "business_ratio": { "t": "decimal", "v": "0.30" }
}
```

型タグ（`t`）を持たせることで `TagValue` に復元できる。
GIN インデックスで `tags @> '{"counterparty": {"t":"code","v":"CP0012"}}'` の検索が効く。

### マスタ（可変）

```sql
CREATE TABLE accounts (
    code         TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    account_type SMALLINT NOT NULL,   -- 1=Asset .. 5=Expense
    parent_code  TEXT REFERENCES accounts(code),
    postable     BOOLEAN NOT NULL DEFAULT TRUE,
    sort_order   INTEGER NOT NULL DEFAULT 0,
    active       BOOLEAN NOT NULL DEFAULT TRUE   -- 削除ではなく無効化
);

CREATE TABLE counterparties (
    code            TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    invoice_reg_no  TEXT,             -- T + 13桁
    is_qualified    BOOLEAN,          -- 適格請求書発行事業者か
    verified_at     DATE,
    note            TEXT,
    active          BOOLEAN NOT NULL DEFAULT TRUE
);

CREATE TABLE entry_counters (
    fiscal_year INTEGER PRIMARY KEY,
    next_no     INTEGER NOT NULL,
    skipped     JSONB NOT NULL DEFAULT '[]'  -- 欠番とその理由
);
```

**マスタは物理削除しない**（`active` フラグ）。過去の仕訳が参照しているため。

### 締めスナップショット

```sql
CREATE TABLE period_snapshots (
    fiscal_year  INTEGER NOT NULL,
    period_end   DATE    NOT NULL,
    closed_at    TIMESTAMPTZ NOT NULL,
    balances     JSONB   NOT NULL,   -- 締め時点の全科目残高
    entry_count  INTEGER NOT NULL,
    last_entry_no INTEGER NOT NULL,
    checksum     TEXT    NOT NULL,   -- 対象仕訳のハッシュ連鎖
    PRIMARY KEY (fiscal_year, period_end)
);
```

性能のためではなく**意味のため**に作る。
`checksum` があると、後から帳簿が改変されていないことを証明できる
（append-only なので改変は起きないが、証明できることに別の価値がある）。
確定申告後の年度に対してこれを残すのは実務上効く。

### checksum の計算方法

```
h_0 = sha256("")
h_i = sha256(h_{i-1} || canonical_json(entry_i))
checksum = hex(h_n)
```

`entry_no` の昇順で連鎖させる。`canonical_json` はキー順ソート・空白なし。

---

## 3. 残高計算：まず素直に SUM する

年間数千件の規模。**マテリアライズドビューや残高テーブルは作らない。**

```sql
-- query/trial_balance.rs
SELECT
    l.account_code,
    a.account_type,
    SUM(CASE WHEN l.side = 1 THEN l.amount_minor ELSE 0 END) AS debit_total,
    SUM(CASE WHEN l.side = 2 THEN l.amount_minor ELSE 0 END) AS credit_total
FROM journal_lines l
JOIN journal_entries e ON e.id = l.entry_id
JOIN accounts a        ON a.code = l.account_code
WHERE e.entry_date BETWEEN $1 AND $2
GROUP BY l.account_code, a.account_type
ORDER BY l.account_code;
```

### 最適化の判断閾値

| 仕訳件数 | 方式 |
|---|---|
| 〜10万件 | 都度 SUM（現状） |
| 10万件〜 | 月次残高スナップショット + 差分 |

個人事業主なら前者から出ることはまずない。
**「後で必要になったら」で間に合う類の最適化。先にやらない。**

### group_by 付きの集計

```sql
SELECT
    l.account_code,
    a.account_type,
    l.tags -> 'counterparty' ->> 'v' AS counterparty,
    SUM(CASE WHEN l.side = 1 THEN l.amount_minor ELSE 0 END) AS debit_total,
    SUM(CASE WHEN l.side = 2 THEN l.amount_minor ELSE 0 END) AS credit_total
FROM journal_lines l
JOIN journal_entries e ON e.id = l.entry_id
JOIN accounts a        ON a.code = l.account_code
WHERE e.entry_date BETWEEN $1 AND $2
GROUP BY 1, 2, 3;
```

group_by するキーは `TagSchema` で `aggregatable: true` のものだけ。
**アプリ層で検証してから SQL を組む**（SQL インジェクション防止も兼ねる）。
タグキーはホワイトリスト照合してから文字列連結する。

---

## 4. 採番（R4 への対策）

連続番号（欠番なし）はトランザクションのロールバックと原理的に衝突する
（Postgres のシーケンスは欠番が出る）。

```sql
-- カウンタ行をロックして取得
BEGIN;
SELECT next_no FROM entry_counters WHERE fiscal_year = $1 FOR UPDATE;
UPDATE entry_counters SET next_no = next_no + 1 WHERE fiscal_year = $1;
-- 仕訳を INSERT
COMMIT;
```

単一ユーザー前提なので競合しない。

### 方針の明文化（README に書く）

> 仕訳番号は会計年度ごとの連番とする。
> トランザクションが失敗した場合、その番号は使用されず欠番となる。
> 欠番は `entry_counters.skipped` に理由とともに記録し、監査時に説明可能な状態を保つ。

曖昧にしておくと税務調査に耐えられない実装になる。ここは決めておく。

---

## 5. マイグレーションの掟

append-only の帳簿には通常のマイグレーション常識が通用しない。

1. **既存の仕訳行を書き換えるマイグレーションを書かない。**
   一度でも書くと、この仕組みの根拠が崩れる
2. カラム追加は NULL 許容のみ
3. 開発中の壊れたデータは DB を丸ごと作り直す（`sqlx database reset`）。
   本番では逆仕訳で直す
4. タグの意味が変わる場合、新キーを作って旧キーは残す
   （`tax_category` → `tax_category_v2`）
5. マイグレーションは `kaikei_migrator` ロールで実行。`kaikei_app` では実行できない

3 が心理的に一番きつい。「1件だけ UPDATE すれば直る」という誘惑が何度も来る。
**DB 権限で塞いであるのはそのため。**

### ファイル命名

```
migrations/
├── 0001_roles_and_grants.sql
├── 0002_accounts.sql
├── 0003_journal.sql
├── 0004_append_only_triggers.sql
├── 0005_counterparties.sql
├── 0006_entry_counters.sql
├── 0007_period_snapshots.sql
├── 0008_documents.sql
└── 0009_imported_transactions.sql
```

---

## 6. Repository の実装方針

ORM を使わず Data Mapper を手書きする（理由は `ARCHITECTURE.md` §10）。

```rust
// 永続化専用の Row 型。ドメイン型とは別
#[derive(sqlx::FromRow)]
struct JournalEntryRow {
    id: uuid::Uuid,
    fiscal_year: i32,
    entry_no: i32,
    entry_date: chrono::NaiveDate,
    description: String,
    reverses: Option<uuid::Uuid>,
    reverse_reason: Option<String>,
    recorded_at: chrono::DateTime<chrono::Utc>,
}

#[derive(sqlx::FromRow)]
struct JournalLineRow {
    entry_id: uuid::Uuid,
    line_no: i16,
    account_code: String,
    side: i16,
    amount_minor: i64,
    currency: String,
    tags: serde_json::Value,
    memo: Option<String>,
}

impl TryFrom<(JournalEntryRow, Vec<JournalLineRow>)> for JournalEntry {
    type Error = RepoError;
    fn try_from(...) -> Result<Self, Self::Error> {
        // JournalEntry::rehydrate を使う（検証を再実行しない）
    }
}
```

### 取得は 2 クエリに分ける

N+1 ではなく固定 2 クエリ。集約 1 つのロードならこれで十分。

```rust
async fn find(&self, id: &EntryId) -> Result<Option<JournalEntry>, RepoError> {
    let entry = sqlx::query_as!(JournalEntryRow,
        "SELECT * FROM journal_entries WHERE id = $1", uuid)
        .fetch_optional(&self.pool).await?;
    let Some(entry) = entry else { return Ok(None) };

    let lines = sqlx::query_as!(JournalLineRow,
        "SELECT * FROM journal_lines WHERE entry_id = $1 ORDER BY line_no", uuid)
        .fetch_all(&self.pool).await?;

    Ok(Some((entry, lines).try_into()?))
}
```

一覧取得で子も必要な場合は、Postgres の `json_agg` で 1 クエリにまとめる選択肢もある。
**どの方式かは Repository 実装の内部に閉じる**ので、後から変えられる。

---

## 7. トランザクション境界

application 層が握る。Repository のシグネチャに `&mut Transaction` を含めるか否かは
以下の方針で統一する。

```rust
// ports.rs — トランザクションは Unit of Work として渡す
#[async_trait]
pub trait UnitOfWork {
    async fn begin(&self) -> Result<Box<dyn Tx>, RepoError>;
}

#[async_trait]
pub trait Tx: Send {
    fn journal(&mut self) -> &mut dyn JournalRepository;
    fn documents(&mut self) -> &mut dyn DocumentRepository;
    async fn commit(self: Box<Self>) -> Result<(), RepoError>;
}
```

Rust の借用チェッカと相性が悪い領域なので、**Phase 1 で実装したら早めに使ってみて、
苦痛なら設計を見直す**こと。ここは理論より実感を優先してよい。

---

## 8. バックアップと可搬性

会計データは 7 年保存が必要（法人税法上の帳簿書類の保存期間）。

- `pg_dump` による論理バックアップを日次
- **さらに JSON 全件エクスポート機能を作る**（`kaikei-report`）。
  DB のバージョンや本プロジェクトの存続に依存しない形でデータを残せるようにする
- これは OSS プロジェクトとしての信頼性に直結する。
  「このソフトが消えてもデータは残る」と言えることが採用の条件になる
