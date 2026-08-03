# 03 — データベース設計

DB は **PostgreSQL 固定**。JSONB、GIN インデックス、テーブル単位の権限制御が必要なため。

---

## 1. 原則：append-only を DB 権限で物理的に強制する

アプリのコードだけで守るのは不十分。

> **Phase 1 での実装範囲の注記（F-1、人間承認済み）**: `documents` / `entry_documents`
> は Phase 1 では作らない（Phase 4 の `kaikei-blob` と同時に設計する）。
> 以下の例からも除外している。

```sql
-- ロール作成・パスワード設定は docker/postgres/init/01-roles.sql に集約する
-- （docker-compose と CI の両方から同一ファイルを流す。真実の点を1つにする）。
-- kaikei_migrator: マイグレーション実行用ロール（テーブル/スキーマの所有者）。
-- kaikei_app: アプリ実行用ロール。

-- 帳簿本体は INSERT と SELECT のみ。TRUNCATE も明示的に禁止する
-- （TRUNCATE は既定では非所有者に付与されない権限だが、意図を明示するため
-- 明示的に REVOKE しておく）。
GRANT SELECT, INSERT ON journal_entries, journal_lines TO kaikei_app;
REVOKE UPDATE, DELETE, TRUNCATE ON journal_entries, journal_lines FROM kaikei_app;

-- 可変テーブルは通常通り（DELETE は許可しない。物理削除ではなく active フラグで無効化する）
GRANT SELECT, INSERT, UPDATE ON accounts, counterparties, entry_counters TO kaikei_app;
GRANT SELECT, INSERT ON period_snapshots TO kaikei_app;

-- 監査ログ（Phase 3 で追加。docs/07-mcp-server.md §9）も帳簿本体と同じ扱いにする。
-- 専用のトリガ関数と専用 ERRCODE を割り当てること（reject_mutation() を流用すると
-- 「訂正は逆仕訳で行ってください」という的外れな案内になる。D-038 と同じ誤診クラス）。
GRANT SELECT, INSERT ON audit_log TO kaikei_app;
REVOKE UPDATE, DELETE, TRUNCATE ON audit_log FROM kaikei_app;
```

これで**バグでも AI でも帳簿を書き換えられない**。

訂正削除の履歴が構造的に残る形を目指した設計だが、**法令要件を満たすと
主張するものではない**（`CLAUDE.md` §10。書いてよいのは「〜の機能要件を
意識した設計」まで。`docs/08-compliance.md` §1 を参照）。

### 補助的にトリガでも防ぐ（多層防御）

`kaikei_migrator`（テーブル所有者）は REVOKE をバイパスできるため、
トリガが所有者に対する最後の防御線になる。

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

-- TRUNCATE は行トリガ（FOR EACH ROW）を起動しない（空テーブルでも通ってしまう）。
-- STATEMENT トリガを別途張る必要がある。
CREATE TRIGGER no_truncate_journal_entries
  BEFORE TRUNCATE ON journal_entries
  FOR EACH STATEMENT EXECUTE FUNCTION reject_mutation();

CREATE TRIGGER no_truncate_journal_lines
  BEFORE TRUNCATE ON journal_lines
  FOR EACH STATEMENT EXECUTE FUNCTION reject_mutation();
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
    entry_id            UUID     NOT NULL REFERENCES journal_entries(id),
    line_no             SMALLINT NOT NULL,
    account_code        TEXT     NOT NULL,
    side                SMALLINT NOT NULL CHECK (side IN (1, 2)),  -- 1=借方, 2=貸方
    amount_minor        BIGINT   NOT NULL CHECK (amount_minor > 0),
    currency            CHAR(3)  NOT NULL,
    currency_minor_unit SMALLINT NOT NULL CHECK (currency_minor_unit BETWEEN 0 AND 18),
    tags                JSONB    NOT NULL DEFAULT '{}',
    memo                TEXT,
    PRIMARY KEY (entry_id, line_no)
);

CREATE INDEX idx_lines_account ON journal_lines (account_code);
CREATE INDEX idx_lines_tags    ON journal_lines USING GIN (tags);
```

**`entry_date` は `DATE`、`recorded_at` は `TIMESTAMPTZ`。**
取引日にタイムゾーンを持たせると年度跨ぎで必ず事故る。ここは絶対に混ぜない。

`amount_minor` は `BIGINT`。core は `i128` だが、実際の金額で 2^63 を超えることはない。
保存時に範囲チェックしてエラーにする。

**`currency` / `currency_minor_unit` に `DEFAULT` を付けない（人間承認済みの決定）。**
`currency` だけ指定して `currency_minor_unit` が既定で 0 になると、
本来 2 桁の通貨（USD 等）の金額が **100 倍ズレて保存されても CHECK に引っかからない**。
上限 18 は `kaikei-core` の `Currency::MAX_MINOR_UNIT`（`DECISIONS.md` D-020）と一致させる。

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
    fiscal_year         INTEGER NOT NULL,
    period_end          DATE    NOT NULL,
    closed_at           TIMESTAMPTZ NOT NULL,
    balances            JSONB   NOT NULL,   -- 締め時点の全科目残高
    currency            CHAR(3) NOT NULL,
    currency_minor_unit SMALLINT NOT NULL CHECK (currency_minor_unit BETWEEN 0 AND 18),
    entry_count         INTEGER NOT NULL,
    last_entry_no       INTEGER NOT NULL,
    checksum            TEXT    NOT NULL,   -- 対象仕訳のハッシュ連鎖
    PRIMARY KEY (fiscal_year, period_end)
);
```

`currency` / `currency_minor_unit` は `journal_lines` と同じ理由で `DEFAULT` を
付けない。`balances` の金額をどの通貨・何桁の最小単位で解釈するかを明示する。

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

## 4. 採番

Postgres の `SEQUENCE` は（トランザクションのロールバックとは独立に値を払い出すため）
ロールバックで欠番が出る。`entry_counters` を明細のカウンタ行として使い、
**採番を仕訳の INSERT と同一トランザクションに置く**ことでこれを避ける。

```sql
-- カウンタ行をロックして取得（同一トランザクション内）
BEGIN;
SELECT next_no FROM entry_counters WHERE fiscal_year = $1 FOR UPDATE;
UPDATE entry_counters SET next_no = next_no + 1 WHERE fiscal_year = $1;
-- 仕訳（journal_entries / journal_lines）を INSERT
COMMIT;
```

単一ユーザー前提なので競合しない。

### 方針の明文化（`README.md`「仕訳番号と欠番」に記載済み。人間承認済み・G-1）

> 仕訳番号は会計年度ごとの連番とする。
> 採番（`entry_counters` の更新）は仕訳の INSERT と同一トランザクションで行うため、
> 検証失敗時はカウンタの増分も一緒に巻き戻り、**通常は欠番が発生しない**
> （欠番が出るのは Postgres の `SEQUENCE` を別トランザクションで払い出す場合）。
> `entry_counters.skipped` は、それでも**意図的に**番号を飛ばした場合の理由を
> 記録するための専用フィールドであり、Phase 1 では書き込みを実装しない。

過去の版（採番を別トランザクションで行う実装を前提とした記述）は
「トランザクションが失敗した場合、その番号は使用されず欠番となる」としていたが、
同一トランザクション採番の実装とは整合しないため、上記の通り改訂した。

---

## 5. マイグレーションの掟

append-only の帳簿には通常のマイグレーション常識が通用しない。

1. **既存の仕訳行を書き換えるマイグレーションを書かない。**
   一度でも書くと、この仕組みの根拠が崩れる
2. カラム追加は NULL 許容のみ
3. 開発中の壊れたデータは DB を丸ごと作り直す（`docker compose down -v` で
   named volume ごと作り直す。README 参照）。本番では逆仕訳で直す。
   **`sqlx database reset`（`DROP DATABASE`）は使わない**: `DROP DATABASE` には
   対象データベースの所有者権限が必要だが、`kaikei` データベースの所有者は
   コンテナ初期化用スーパーユーザー（`kaikei_root`）であり、マイグレーション実行用の
   `kaikei_migrator` は所有者ではない（`kaikei_migrator` が所有者なのはテーブルと
   `public` スキーマのみ）
4. タグの意味が変わる場合、新キーを作って旧キーは残す
   （`tax_category` → `tax_category_v2`）
5. マイグレーションは `kaikei_migrator` ロールで実行。`kaikei_app` では実行できない

3 が心理的に一番きつい。「1件だけ UPDATE すれば直る」という誘惑が何度も来る。
**DB 権限で塞いであるのはそのため。**

### ファイル命名

```
migrations/
├── 0001_baseline_privileges.sql   -- ロール作成は行わない。前提条件の検証のみ
├── 0002_accounts.sql
├── 0003_journal.sql
├── 0004_append_only_triggers.sql
├── 0005_counterparties.sql
├── 0006_entry_counters.sql
└── 0007_period_snapshots.sql
```

ロールの作成・パスワード設定は `docker/postgres/init/01-roles.sql` に集約する
（マイグレーションには書かない）。ロールはクラスタ単位のオブジェクトであり、
`#[sqlx::test]` はテストのたびに新しいデータベースを作成してマイグレーションを
再実行するため、ロール作成をマイグレーションに書くと2件目以降のテストが
全て失敗する。

`0008_documents.sql` / `0009_imported_transactions.sql`（`documents` /
`entry_documents` / `imported_transactions`）は Phase 1 では作らない
（人間承認済み・F-1。Phase 4 の `kaikei-blob` / `kaikei-import` と同時に設計する）。

---

## 6. Repository の実装方針

ORM を使わず Data Mapper を手書きする（理由は `ARCHITECTURE.md` §10）。

```rust
// crates/kaikei-store/src/journal/row.rs — 永続化専用の Row 型。ドメイン型とは別。
// いずれも pub(crate)。DB 行の生表現は永続化層の内部実装詳細。
#[derive(sqlx::FromRow)]
pub(crate) struct JournalEntryRow {
    pub id: uuid::Uuid,
    pub fiscal_year: i32,
    pub entry_no: i32,
    pub entry_date: chrono::NaiveDate,
    pub description: String,
    pub reverses: Option<uuid::Uuid>,
    pub reverse_reason: Option<String>,
    pub recorded_at: chrono::DateTime<chrono::Utc>,
}

#[derive(sqlx::FromRow)]
pub(crate) struct JournalLineRow {
    // entry_id は SELECT しない（呼び出し側が WHERE entry_id = $1 で
    // どの仕訳の明細かを既に把握しているため）
    pub line_no: i16,
    pub account_code: String,
    pub side: i16,
    pub amount_minor: i64,
    pub currency: String,
    pub currency_minor_unit: i16,   // 通貨ごとの小数桁（CLAUDE.md §8）
    pub tags: serde_json::Value,
    pub memo: Option<String>,
}

/// ★ 孤児則（E0117）の回避用ローカル型
pub(crate) struct EntryRows {
    pub entry: JournalEntryRow,
    pub lines: Vec<JournalLineRow>,
}

// crates/kaikei-store/src/journal/mapper.rs
impl TryFrom<EntryRows> for JournalEntry {
    type Error = RepoError;
    fn try_from(rows: EntryRows) -> Result<Self, Self::Error> {
        // JournalEntry::rehydrate を使う（検証を再実行しない）。
        // rehydrate を呼んでよいのはこのファイルだけ（CI が検査する）。
    }
}
```

**`impl TryFrom<(JournalEntryRow, Vec<JournalLineRow>)> for JournalEntry` は書けない。**
`Self`（`JournalEntry`）もタプル構築子も外部型（`kaikei-core` の型）であり、
孤児則の言う「最初のローカル型」が引数リストに現れないため E0117 になる。
`EntryRows` というローカルな包み型を1つ挟むことで
`impl TryFrom<EntryRows> for JournalEntry`（最初の型引数がローカル型）として実装できる。
Phase 1 の実装時に実測で確認済み。

### 取得は 2 クエリに分ける

N+1 ではなく固定 2 クエリ。集約 1 つのロードならこれで十分。

```rust
async fn find_entry(&mut self, id: EntryId) -> Result<Option<JournalEntry>, RepoError> {
    let uuid = entry_id_to_uuid(id);

    let entry_row: Option<JournalEntryRow> = sqlx::query_as(
        "SELECT id, fiscal_year, entry_no, entry_date, description, reverses, \
                reverse_reason, recorded_at \
         FROM journal_entries WHERE id = $1",
    )
    .bind(uuid)
    .fetch_optional(self.conn())
    .await
    .map_err(from_sqlx_error)?;

    let Some(entry_row) = entry_row else { return Ok(None) };

    let line_rows: Vec<JournalLineRow> = sqlx::query_as(
        "SELECT line_no, account_code, side, amount_minor, currency, \
                currency_minor_unit, tags, memo \
         FROM journal_lines WHERE entry_id = $1 ORDER BY line_no",
    )
    .bind(uuid)
    .fetch_all(self.conn())
    .await
    .map_err(from_sqlx_error)?;

    Ok(Some(JournalEntry::try_from(EntryRows { entry: entry_row, lines: line_rows })?))
}
```

`SELECT *` ではなく列を明示する。append-only の帳簿は列の追加（NULL 許容のみ）が
起こりうるため、`*` だと Row 型との対応が静かにズレる。

`self.conn()` は `PgTx` が保持する `sqlx::Transaction` への排他借用。プールから
新しい接続を取るのではなく、**呼び出し側が開いたトランザクションの中で実行する**
（§7 を参照）。

一覧取得で子も必要な場合は、Postgres の `json_agg` で 1 クエリにまとめる選択肢もある。
**どの方式かは Repository 実装の内部に閉じる**ので、後から変えられる。

---

## 7. トランザクション境界

application 層が握る。トランザクションは**ユースケース関数の引数
`tx: &mut Tx` として引き回す**（`DECISIONS.md` D-029）。

```rust
// kaikei-app/src/ports.rs
pub trait Store: Send + Sync + 'static {
    type Tx: TxOps + TxScope;
    async fn begin(&self) -> Result<Self::Tx, RepoError>;
}

pub trait TxScope: Send + Sized {
    async fn commit(self) -> Result<(), RepoError>;
    async fn rollback(self) -> Result<(), RepoError>;
}

/// 1トランザクションで使える操作の総体。
pub trait TxOps: JournalRepo + ChartRepo + PeriodRepo + NumberingRepo + Send {}

// kaikei-app/src/tx.rs — commit / rollback の取りこぼしを型で防ぐ
pub async fn with_tx<S, T, F>(store: &S, f: F) -> Result<T, AppError>
where
    S: Store,
    T: Send,
    F: for<'a> FnOnce(&'a mut S::Tx) -> BoxFut<'a, Result<T, AppError>> + Send;
```

ユースケース側は `post_entry::execute(tx, tax, tag_schema, id_gen, clock, settings, input)`
のように `&mut Tx` を第1引数で受け取る。**採番と仕訳の INSERT が同一トランザクションに
乗ること（§4）が、シグネチャから読める**のがこの形の要点。

### 借用チェッカとの相性（Phase 1 で実装しての評価）

当初案の `UnitOfWork` + `Box<dyn Tx>`（`fn journal(&mut self) -> &mut dyn JournalRepository`）は
**採用しなかった**。ROADMAP Phase 1 の完了条件「実際に使ってみて、苦痛なら設計を
見直す」に対する結論として、実装前の比較検討で `&mut Tx` を選び、実装後もそのまま
維持できている（`DECISIONS.md` D-029）。実際に踏んだ痛点は2つだけで、いずれも
回避策が確立している:

1. **`match f(&mut tx).await { .. }` が E0505 になる。** match のスクルティニーが
   match 式全体のスコープで生存し、分岐内の `tx.commit()`（`self` を消費する）と
   `&mut tx` の借用が衝突する。`let result = f(&mut tx).await;` で受けて借用を
   先に終わらせれば解決する（`tx.rs` にコメントで残してある）。
2. **`with_tx` のクロージャは HRTB（`for<'a>`）のため `'static` でない借用を
   キャプチャできない。** 呼び出し側は必要な値を `clone()` してから
   `Box::pin(async move { .. })` に渡す。E2E テスト・ユースケーステストの
   すべてがこの形で書けている。

`Box<dyn Tx>` 案ならこの2点は避けられたが、代わりに
「リポジトリごとに `&mut dyn` を取り出す」段階でトランザクションの排他借用が
実行時（`RefCell` 相当）に押し出され、**同一トランザクション上の2つのリポジトリ操作を
同時に走らせるコードがコンパイルを通ってしまう**。型で禁じられる方を選んだ。

---

## 8. バックアップと可搬性

会計データは 7 年保存が必要（法人税法上の帳簿書類の保存期間）。

- `pg_dump` による論理バックアップを日次
- **さらに JSON 全件エクスポート機能を作る**（`kaikei-report`）。
  DB のバージョンや本プロジェクトの存続に依存しない形でデータを残せるようにする
- これは OSS プロジェクトとしての信頼性に直結する。
  「このソフトが消えてもデータは残る」と言えることが採用の条件になる
