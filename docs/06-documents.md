# 06 — 証憑ファイル管理（kaikei-blob）

## 1. 方式：Content-Addressed Storage

ファイル名で管理せず、**内容の SHA-256 ハッシュで管理する。**

```
blobs/
├── 3f/
│   └── 3fa8b2c1d4e5...      SHA-256 をそのままファイル名に
├── 7c/
│   └── 7c91e0af23bb...
└── tmp/                     アップロード中の一時領域
```

### この方式が優れている理由

| # | 利点 |
|---|---|
| 1 | **改変が自動検出される** — 1 バイト変えればハッシュが変わり、DB の記録と一致しなくなる。真実性の担保が構造から出てくる |
| 2 | **重複排除** — 同じ PDF を 2 回取り込んでも 1 つ |
| 3 | **ファイル名の呪縛から解放** — 日本語ファイル名、macOS の NFD 正規化問題、長さ制限を全部回避 |
| 4 | **リネーム不要** — 分類は DB のメタデータで行う |

---

## 2. trait 定義

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BlobHash([u8; 32]);

impl BlobHash {
    pub fn of(bytes: &[u8]) -> Self;
    pub fn to_hex(&self) -> String;
    pub fn parse_hex(s: &str) -> Result<Self, BlobError>;
    /// "3f/3fa8b2c1..." のパス表現
    pub fn to_path(&self) -> String;
}

#[async_trait]
pub trait BlobStore: Send + Sync {
    /// 保存してハッシュを返す。既存なら何もせず同じハッシュを返す
    async fn put(&self, bytes: &[u8]) -> Result<BlobHash, BlobError>;
    async fn get(&self, hash: &BlobHash) -> Result<Vec<u8>, BlobError>;
    async fn exists(&self, hash: &BlobHash) -> Result<bool, BlobError>;
    /// 整合性検査。保存内容のハッシュを再計算して照合する
    async fn verify(&self, hash: &BlobHash) -> Result<bool, BlobError>;

    // delete は定義しない
}
```

**`delete` を trait に定義しない。**
保存義務がある以上、削除経路をコードに用意しない。

### 実装

- `LocalBlobStore` — ローカルファイルシステム（Phase 4。`ROADMAP.md` の成果物一覧を参照）
- `S3BlobStore` — S3 互換オブジェクトストレージ（Phase 5 以降）

書き込みは `tmp/` に書いてから `rename` する（原子性の確保）。

---

## 3. DB スキーマ

```sql
CREATE TABLE documents (
    id             UUID PRIMARY KEY,
    blob_hash      TEXT NOT NULL,          -- SHA-256 hex
    original_name  TEXT NOT NULL,          -- "請求書_2026年6月.pdf"
    mime_type      TEXT NOT NULL,
    byte_size      BIGINT NOT NULL,

    -- 検索要件の3項目
    doc_date       DATE NOT NULL,          -- 取引年月日
    amount_minor   BIGINT,                 -- 金額
    counterparty   TEXT,                   -- 取引先

    doc_type       TEXT NOT NULL,          -- invoice / receipt / contract / other
    received_via   TEXT NOT NULL,          -- email / download / scan / manual
    received_at    TIMESTAMPTZ NOT NULL,   -- 授受日時
    note           TEXT,
    created_at     TIMESTAMPTZ NOT NULL
);

CREATE INDEX idx_documents_date  ON documents (doc_date);
CREATE INDEX idx_documents_cp    ON documents (counterparty);
CREATE INDEX idx_documents_amt   ON documents (amount_minor);
CREATE INDEX idx_documents_hash  ON documents (blob_hash);

-- 帳簿との相互関連性
CREATE TABLE entry_documents (
    entry_id    UUID NOT NULL REFERENCES journal_entries(id),
    document_id UUID NOT NULL REFERENCES documents(id),
    PRIMARY KEY (entry_id, document_id)
);
```

`documents` は INSERT のみ（`kaikei_app` に UPDATE/DELETE を与えない）。
メタデータの訂正が必要な場合は新しい行を追加し、旧行に `superseded_by` を持たせる
（Phase 2 で必要になったら追加。最初は入れない）。

---

## 4. 検索要件

電子取引データについて、基準期間（2年前）の課税売上高が 5,000万円以下で、
税務調査時にダウンロードの求めに応じられるようにしている場合は、
検索機能の確保が不要とされる。基準期間の判定は税抜金額で行う。

つまり**多くの個人事業主は検索要件が免除される。**

### それでも検索機能は最初から作る

- 保存義務そのものは免除されない
- 真実性の確保（改変防止）は必要
- 売上が伸びたら検索要件が発生する
- **DB に入れておけば検索は自然に付いてくる**（追加コストが小さい）

免除に甘えると、売上が伸びた瞬間に作り直しになる。

### 実装する検索

```rust
pub struct DocumentQuery {
    pub date_from: Option<NaiveDate>,
    pub date_to: Option<NaiveDate>,
    pub amount_min: Option<i64>,
    pub amount_max: Option<i64>,
    pub counterparty: Option<String>,
    pub doc_type: Option<String>,
}
```

**3 項目（日付・金額・取引先）の組み合わせ検索と範囲指定**に対応する。
これが検索要件の内容に対応する。

---

## 5. 人間向けフォルダは「エクスポートで生成する」

保存構造と閲覧構造を分離する。

```rust
/// 税務調査・確定申告用にエクスポート
/// 実体はハードリンク（同一FS）またはコピー
pub async fn export_for_audit(
    fy: FiscalYear,
    out: &Path,
    blob: &dyn BlobStore,
    repo: &dyn DocumentRepository,
) -> Result<ExportSummary, ExportError>;
```

### 出力構造

```
export_2026/
├── 電子取引/
│   ├── 2026-04-15_株式会社ABC_110000_請求書.pdf
│   └── 2026-04-20_クラウドサービスXYZ_3300_領収書.pdf
├── スキャン/
│   └── ...
├── 仕訳日記帳.csv
├── 総勘定元帳.csv
├── 試算表.csv
├── index.csv               ← 検索要件の代替として提出できる一覧
└── checksums.txt           ← 各ファイルの SHA-256
```

### 命名規則の根拠

国税庁の運用では、書面を
「課税期間ごとに取引年月日順にまとめ、取引先ごとに整理する」形（またはその逆順）で
整理して提示できる場合も検索要件が不要とされている。

`{日付}_{取引先}_{金額}_{種別}.pdf` の命名は日付順ソートと取引先の識別を同時に満たす。

**保存はハッシュ、閲覧は人間が読める名前。** どちらも要件を満たす。

### ファイル名の安全化

- パス区切り文字、制御文字を除去
- 長すぎる取引先名は切り詰める（合計 200 文字以内）
- 重複したら `_2`, `_3` を付ける
- `index.csv` に元のファイル名とハッシュを必ず残す

---

## 6. 整合性検査コマンド

```
kaikei verify --year 2026
```

1. `documents` の全 `blob_hash` について `BlobStore::verify()` を実行
2. `period_snapshots.checksum` を再計算して照合
3. 帳簿の貸借一致を全期間で検証
4. 結果をレポート

**この機能が「改変されていないことを証明できる」という価値の実体。**
定期実行を推奨し、結果を保存できるようにする。

---

## 7. 真実性の確保に関する方針

要件を満たす手段は複数ある。

| 手段 | 本プロジェクトの方針 |
|---|---|
| タイムスタンプ付与 | **実装しない**（認定業者との契約が必要・有料） |
| 訂正削除履歴が残るシステム | **これで対応**（append-only + CAS） |
| 事務処理規程の備付け | ソフトの外。テンプレートを `docs/templates/` に用意する |

### 事務処理規程のテンプレート

国税庁が公開しているサンプルをベースに、本システムを使う場合の記述例を用意する。
ただし**「これで法令要件を満たす」と書かない。** 税理士確認を促す文言を添える。

---

## 8. スキャナ保存について

紙で受け取った領収書をスキャンして保存する区分。
電子取引とは別の要件（解像度、階調、タイムスタンプまたは訂正削除履歴等）がある。

**Phase 5 以降のスコープ。** 最初は「電子取引データの保存」に集中する。

ただし `documents.received_via` に `scan` の値を持たせておき、
将来区分を分けられるようにしておく。

---

## 9. テストケース

| # | ケース | 期待 |
|---|---|---|
| D-01 | 同じ内容のファイルを 2 回 put | 同じハッシュ、ファイルは 1 つ |
| D-02 | 1 バイト違うファイル | 別のハッシュ |
| D-03 | put した内容が get で完全に戻る | バイト列一致 |
| D-04 | 保存後にファイルを外部から改変 → verify | `false` |
| D-05 | 存在しないハッシュを get | エラー |
| D-06 | 日本語ファイル名 | `original_name` に保持され、blob 名には影響しない |
| D-07 | 3 項目の組み合わせ検索 | 正しく絞り込まれる |
| D-08 | 金額の範囲指定検索 | 境界値を含む |
| D-09 | export_for_audit | ファイル名が規則通り、index.csv が全件を含む |
| D-10 | ファイル名の重複 | `_2` が付く |
| D-11 | 仕訳と証憑の紐付け → 仕訳から辿れる | — |
| D-12 | 1 証憑を複数仕訳に紐付け | 許可される |
