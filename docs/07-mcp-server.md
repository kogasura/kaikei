# 07 — MCP サーバー（kaikei-mcp）

**このプロジェクトの差別化の本体。**
AI エージェントが会計操作を安全に行うための標準インタフェース。

> **この文書の版**: Phase 3 着手時点（`DECISIONS.md` D-071 まで）を反映している。
> D-072 以降の決定でここに書かれた内容が覆った場合、**決定を入れた PR の中で
> この文書も直すこと**。設計書は一度書いたら終わりではなく、直さないと
> 「却下済みの設計を後続の実装者に指示する文書」に劣化する
> （`PROGRESS.md` Phase 2 の教訓1。同じ事故が Phase 2 で3回起きた）。

---

## 1. 設計の 4 原則

### ① 削除ツールを作らない

**削除ツールを作らないのは4層の防御のうち最も外側の1層にすぎない。**
「API に無いから安全」ではなく、次の4つが重なっている。

| 層 | 実体 | 根拠 |
|---|---|---|
| (1) MCP にツールを登録しない | `delete_journal_entry` / `update_journal_entry` / `execute_sql` を実装しない | D-014 |
| (2) DB ロール権限 | `REVOKE UPDATE, DELETE, TRUNCATE ON journal_entries, journal_lines FROM kaikei_app`（`crates/kaikei-store/migrations/0003_journal.sql`） | D-006 |
| (3) トリガ | 行トリガ + TRUNCATE 文トリガ（`0004_append_only_triggers.sql` の `reject_mutation()`）。テーブル所有者 `kaikei_migrator` にも効く唯一の防御 | D-006 |
| (4) app 層のポート | `JournalRepo` は `find_entry` / `find_reversal_of` / `insert_entry` のみ。更新系メソッドを定義しない（`crates/kaikei-app/src/ports.rs`） | `CLAUDE.md` §2 |

訂正は `reverse_journal_entry`（逆仕訳）のみ。

**append-only の対象は `journal_entries` / `journal_lines` のみ。**
マスタ（`accounts` / `counterparties` / `entry_counters`）は `UPDATE` が許可されており
（`0002` / `0005` / `0006` の各 `GRANT SELECT, INSERT, UPDATE`）、
`upsert_*` 系ツールが存在しうるのはそちら側だけである。この線引きを崩さない。

### ② 提案と確定を分ける

`suggest_*` 系は候補と根拠を返すだけで帳簿を変更しない。
帳簿を変更するのは `post_journal_entry` / `reverse_journal_entry` の2つだけであり、
提案ツールがそこへ自動的に連鎖することはない（提案の採否は呼び出し側の
**明示的な別呼び出し**）。

**Phase 3 の提案系・検証系は `suggest_tax_category` / `validate_invoice_number` の2件。**

`CLAUDE.md` §10 は「提案系の機能は候補と根拠を返し、確定は人間に残す」と定めている。
サーバが保証するのは**分離**（提案が帳簿を触らないこと）までであり、
記帳の承認そのものは MCP クライアント側のツール承認 UI の責務である
（Phase 3 の完了条件は「Claude Code から記帳できる」＝`post_journal_entry` を呼ぶのは
AI である。サーバ側に確認用の引数を足すという意味ではない）。

### ③ エラーは自己修正可能な形で返す

```
❌ "Unbalanced entry"
✅ "貸借不一致: 借方 110,000 / 貸方 100,000（差額 10,000）。"
```

**次の手が分かる文言にする。** これが MCP サーバーの品質を左右する。

**返し方も原則の一部。**
エラーは MCP の**ツール結果エラー**（`isError: true` + テキスト/構造化コンテンツ）として
返す。JSON-RPC のプロトコルエラー（`error` オブジェクト）は使わない。
クライアントが `error.data` をモデルに見せる保証が無く、
**AI が読めないエラーは自己修正に使えない**ため（D-071）。
プロトコルエラーを使うのは、ツール呼び出しに到達できない異常
（未知のツール名など）に限る。詳細は §6。

### ④ 不可逆操作は確認を要求する

**Phase 3 に、確認（`confirm`）を要求するツールは無い。**

- `close_period` は Phase 4 以降（§2 / 付録 A）。
- `confirm` を課すのは「**逆仕訳でも取り消せない操作**」に限る。
  `post_journal_entry` / `reverse_journal_entry` は append-only により記帳自体を
  取り消せないが、誤りは逆仕訳（赤伝）で訂正でき、その過程は帳簿と audit_log に
  残る。したがって `confirm` は課さない。ここに `confirm` を足すと
  「Claude Code から記帳できる」（`ROADMAP.md` Phase 3 の完了条件）を著しく損なう。
- Phase 3 で不可逆性に対して置いている歯止めは、`reverse_journal_entry` の
  **訂正理由の必須化**と**二重訂正の既定拒否**（`allow_double_reversal: false`）である。

`confirm` を要求するツールを新たに追加する場合は、この原則に照らした判断を
`DECISIONS.md` に記録すること。

---

## 2. ツール定義

**Phase 3 で MCP に登録するのは 11 ツールだけ。**
「Phase 4 以降」と書かれたツールは**名前を予約しているだけ**で、Phase 3 では
MCP サーバーに登録しない（登録しないツールは AI からは存在しないのと同じ）。
延期したものも設計意図を失わないよう表に残し、延期理由を添えてある。

### 読み取り系

| ツール | Phase | 説明 |
|---|---|---|
| `list_accounts` | **Phase 3** | 勘定科目一覧。科目コード・名称・5要素分類（`account_type`）・親科目（`parent`）・**記帳可否（`postable`）** を返す。`postable: false` は見出し科目で、記帳に使うと `NotPostable` になるため必ず返す。表示順（`sort`）は保持していないので返さない（並びは科目コード昇順。D-061） |
| `get_entry` | **Phase 3** | 仕訳 1 件の詳細（明細・タグ）。証憑リンクは Phase 4 |
| `get_trial_balance` | **Phase 3** | 試算表。集計期間（`from`/`to`、取引日ベース・両端含む）は**必須**。`group_by` には `aggregatable: true` のタグキーのみ指定可（それ以外は `NotAggregatable`。`CLAUDE.md` §4）。`from > to` は空の試算表ではなく**エラー**（入力ミスを「0件の空の試算表」として静かに成功させない）。集計対象の通貨が単一であることを要求する（D-042） |
| `search_entries` | **Phase 3** | 日付・金額・科目・取引先・摘要で仕訳検索。read model の新設が要る（下記） |
| `get_ledger` | **Phase 3** | 総勘定元帳（科目別の明細）。read model の新設が要る（下記） |
| `list_tax_categories` | **Phase 3** | 有効な税区分一覧（指定日時点）。該当する年度マスタが存在しない日付では**空配列ではなくエラー**を返し、有効期間を示す（例:「2026-01-01〜2026-12-31 のマスタのみ同梱されています。取引日を確認してください」）。`TaxRuleSets::for_date` は該当なしで `None` を返す（D-055）。**空マスタと未収録は意味が違う**——空配列を返すと AI が「この日は税区分が1つも無い」と誤解して税区分なしで記帳しようとする |
| `get_settings` | **Phase 3** | 経理方式（`tax_mode`）／端数処理方式（`rounding`）／端数処理単位（`rounding_unit`）／課税事業者か（`is_taxable_business`）／簡易課税か（`simplified_taxation`）と、会計年度の区切り規則・帳簿通貨を返す。**日付引数を取らない**（事業者設定は起動時に一度だけ合成され、取引日に応じて変わらない。D-057）。設定が未指定ならサーバは起動に失敗するので、このツールが既定値を返すことはない（§7） |
| `get_statements` | Phase 4 以降 | B/S・P/L。**延期理由: D-031。** `TrialBalance` / `BalanceRow` は `kaikei-core` の外から構築できず（`GroupKey` に公開コンストラクタが無い）、DTO 経由で組み立て直す設計が要る |
| `list_pending_transactions` | Phase 4 以降 | 未仕訳の取込明細。**延期理由: `kaikei-import` 未着手**（crate もテーブルも存在しない） |
| `search_documents` | Phase 4 以降 | 証憑検索（日付・金額・取引先）。**延期理由: `kaikei-blob` 未着手**（`documents` / `entry_documents` は Phase 4 で設計する。`docs/03-database.md` §1 の注記） |

`search_entries` / `get_ledger` / `get_entry` は read model が未実装。
`crates/kaikei-store/src/query/` に `ledger.rs` / `search.rs` / `entry_detail.rs` を新設し、
`kaikei-app::view` の DTO へ直行する（書き込み側 `Store`/`PgTx` を経由しない。
`CLAUDE.md` §6・D-031）。実装方針は §4。

### 書き込み系

| ツール | Phase | 説明 | 備考 |
|---|---|---|---|
| `post_journal_entry` | **Phase 3** | 仕訳を起こす | 貸借不一致は必ずエラー（`JournalEntry::new` と DB の遅延制約トリガの二重で防ぐ）。成功時は確定後の明細に加えて **`PolicyNote` の一覧を返す**（非適格の経過措置など、税額に反映されず注記にしか出ない情報がある。D-059）。`PolicyNote` は audit_log の `output` にも残す（D-070） |
| `reverse_journal_entry` | **Phase 3** | 赤伝を起こす | 理由（`reason`）が必須。**空文字・空白のみは MCP 層で拒否する**（core/app 側に非空検証は無い）。既に赤伝済みの仕訳を再訂正する場合は `allow_double_reversal: true` が必要（既定は拒否） |
| `journalize_transaction` | Phase 4 以降 | 取込明細を仕訳化 | **延期理由: `kaikei-import` 未着手** |
| `ignore_transaction` | Phase 4 以降 | 取込明細を無視 | **延期理由: `kaikei-import` 未着手** |
| `attach_document` | Phase 4 以降 | 証憑を仕訳に紐付け | **延期理由: `kaikei-blob` 未着手。** 現状 `PgTx::insert_entry` は `document_refs` が非空なら `RepoError::Unsupported` を返す（D-041） |
| `upsert_counterparty` | Phase 4 以降 | 取引先マスタ更新 | **延期理由: ポートに書き込みメソッドが無い**（`ChartRepo` は `load_chart` / `load_counterparties` のみ）。DB 権限（`GRANT ... UPDATE ON counterparties`）は既にある |
| `upsert_journalize_rule` | Phase 4 以降 | 仕訳化ルール更新 | **延期理由: `kaikei-import` 未着手** |
| `close_period` | Phase 4 以降 | 期間を締める | **延期理由: checksum の計算式が未確定。** 締めスナップショットの checksum は Phase 5 の `kaikei verify`（`ROADMAP.md` Phase 5）と**同一の計算式**である必要があるが、その式はどこにも定義されていない（`0007_period_snapshots.sql` は `checksum TEXT NOT NULL -- 対象仕訳のハッシュ連鎖` とコメントするのみ）。仕様が固まる前に不可逆操作を実装しない（D-070）。`confirm: true` 必須・不可逆という要件は実装時に維持する。仕様案は付録 A |

**勘定科目マスタの投入は MCP ツールではない。**
`kaikei-app` に専用のユースケースを新設して行う（D-070）。
`kaikei_jp::compose` が返す `chart` は埋め込みテンプレート由来であり、
`ChartRepo::load_chart` が読む DB の `accounts` とは別物である点に注意。

### 提案系・検証系（帳簿を変更しない）

| ツール | Phase | 説明 |
|---|---|---|
| `suggest_tax_category` | **Phase 3** | 取引内容から税区分を提案。根拠付き。`TaxRuleSets::for_date(date)` → `TaxCategoryTable::categories()` を根拠に候補と根拠を返し、確定は人間に残す（`CLAUDE.md` §10）。帳簿は一切変更しない |
| `validate_invoice_number` | **Phase 3** | 登録番号の形式検証（国税庁への実在確認・適格性判定はしない）。`kaikei_jp::invoice::InvoiceRegistrationNo::parse` をそのまま呼ぶ薄いツール。**前後の空白をトリムしない**（貼り付け由来の空白混入を検出するため。D-052）。検証は先頭文字 `T` → 桁数 → 文字種 → チェックデジットの順に固定で、最初に失敗した観点をエラーとして返す（D-053） |
| `suggest_journal_entry` | Phase 4 以降 | 取込明細から仕訳案を生成。根拠付き。**延期理由: `kaikei-import` 未着手**（`imported_tx_id` の解決経路が無い）。仕様案は付録 A |
| `explain_balance` | Phase 4 以降 | ある科目の残高の内訳を説明。**延期理由: D-031**（`get_statements` と同じ） |

### 存在させないツール

```
delete_journal_entry   ← D-014。訂正は reverse_journal_entry のみ
update_journal_entry   ← D-014
execute_sql            ← D-014
reopen_period          ← 締めの取り消しを行う経路は現時点で提供しない
```

これは「Phase 4 以降に回す」ではなく、**将来も作らない**という意味である
（D-014）。テストで機械的に不在を検査する（§9 MC-10）。

`reopen_period` について: 締め（`close_period`）自体が Phase 4 以降であり、
締めの取り消し手段は**CLI を含めて存在しない**（リポジトリのバイナリは
`crates/kaikei-store/src/bin/kaikei-migrate.rs` のみ。`kaikei-cli` は存在しない）。
`period_snapshots` は `kaikei_app` に `SELECT, INSERT` しか付与されていないため
（`0007_period_snapshots.sql`）、仮に CLI を作っても `kaikei_app` 接続では取り消せず、
DB 所有者権限での手動操作以外に手段は無い。取り消し手段を設けるかどうかは
`close_period` を実装する Phase で改めて決める。

---

## 3. 主要ツールの入出力

**この節に載せるのは Phase 3 で実装するツールだけ。**
`suggest_journal_entry` / `close_period` の入出力仕様案は付録 A に移してある。

> **未確定事項がある。** 以下の3点は PR-B（`kaikei-mcp` の骨格）で確定させる。
> 決まるまで、この節の例は「確定した契約」ではなく作業対象として読むこと。
>
> 1. **タグ値の線上形式**（後述）
> 2. **通貨の指定方法**（§5）
> 3. **金額の出力文字列形式**（§5。実装が現在返せるのは3桁区切り付きの
>    `"110,000"` のみで、区切り無しの文字列化 API は `kaikei-core` に無い）

### post_journal_entry

```json
{
  "entry_date": "2026-04-15",
  "description": "A社への請求",
  "lines": [
    { "account": "135", "side": "debit",  "amount": "110000" },
    { "account": "500", "side": "credit", "amount": "100000",
      "memo": "4月分",
      "tags": { "tax_category": "SALES_10", "counterparty": "CP0001" } }
  ],
  "auto_tax_lines": true
}
```

- `entry_date` は ISO 形式（`AccountingDate::parse` が受理する形）。**取引日**であって
  記帳日ではない（`CLAUDE.md` §7）。
- `memo` は明細ごとの省略可の備考（`JournalLine::memo`。DB の `memo` 列に永続化される）。
  NUL 文字を含む文字列は store が `RepoError::Corrupt` で拒否する（D-041）。
- **`document_ids` は無い。** 証憑の紐付けは Phase 4 の `attach_document` で行う。
  post 時には指定できず、`PgTx::insert_entry` が `document_refs` 非空を
  `RepoError::Unsupported` で拒否する（D-041）。
- `tags` の**値の型付けは未確定**。`TagSet` の値は型付き（`TagValue::Code/Text/Decimal/Date`）
  だが、平文文字列から `TagValue` を作るにはキーごとの `value_type` が要り、
  `TagSchema` は `defs` が private で公開 API は `validate` / `is_aggregatable` のみ。
  PR-B で次のいずれかに決める:
  (a) `kaikei-jp::tags::load_*` が `TagSchema` と併せて `Vec<(TagKey, TagDef)>` を返す
  （`kaikei-core` を変更せずに済む最小案）、
  (b) `kaikei-core` に `TagSchema::value_type(&TagKey) -> Option<TagValueType>` を追加する
  （**core の変更は人間の承認が必要**。`CLAUDE.md` §1）、
  (c) D-035 の `{"t":..,"v":..}` 形式を線上形式にも使う（AI には冗長）。
  決めたうえで `business_ratio`（Decimal）や日付型タグの文字列表現もこの節に明記する。

`auto_tax_lines: true` なら `TaxPolicy::derive_tax_lines` を通す。
上記の例で仮受消費税 10,000 が自動追加されて貸借が一致するのは
**税抜経理（`tax_mode: exclusive`）かつ課税事業者（`is_taxable_business: true`）の
設定の場合**に限る。税込経理または免税事業者の設定では
`JpTaxPolicy::derive_tax_lines` が入力明細をそのまま返すため、同じリクエストが
貸借不一致（借方 110,000 / 貸方 100,000）になる。同梱マスタの既定は
`tax_mode: exclusive` だが、`is_taxable_business` は既定を持たず起動時設定で決まる（§7）。

成功時：

```json
{
  "entry_id": "0192a7b3-xxxx-7xxx-xxxx-xxxxxxxxxxxx",
  "entry_no": 42,
  "fiscal_year": 2026,
  "lines": [ /* 税行を含む確定後の明細 */ ],
  "debit_total": "110000",
  "credit_total": "110000",
  "policy_notes": [
    { "severity": "warning", "message": "..." }
  ]
}
```

- `entry_id` は **UUID の正準表記**（ハイフン付き36文字。生成は UUID v7）。
  `EntryId` の内部表現は `u128` だが、`as_u128()` の10進表記では返さない。
  `reverse_journal_entry` の `original_id` も同じ表記。
- `entry_no` / `fiscal_year` は JSON number（金額ではないため文字列にしない。§5）。
- `policy_notes` は `kaikei_policy::PolicyNote`（`severity`: `info` / `warning`、`message`）。
  非適格の経過措置や簡易課税のように**税額計算に反映されず注記にしか現れない情報**の
  唯一の伝達経路であり、落とすと AI も監査ログも「控除割合の制限があった」ことを
  知る手段が無くなる（D-059 / D-070）。文言は `kaikei-policy` が組み立てたものを
  そのまま素通しする（税務判断を断定する言い換えをしない。`CLAUDE.md` §10）。
- **PR-B の契約拡張**: 現在の `post_entry::execute` は
  `tax.derive_tax_lines(...)?.lines` として `notes` を捨てており、戻り値も
  `JournalEntry` 単体なので `policy_notes` を返せない。
  `PostEntryOutput { entry, notes }` のような戻り値への拡張が Phase 3 のスコープに入る
  （Phase 2 の申し送り「`PolicyNote` が永続化されない」への回答）。

**確定後の明細を必ず返す。** AI が「何が記録されたか」を確認できるようにする。

失敗時（ツール結果エラー。`isError: true` の本文としてこの JSON を返す。
JSON-RPC のエラー応答は使わない。D-071）：

```json
{
  "error": "unbalanced",
  "message": "貸借不一致: 借方 110,000 / 貸方 100,000（差額 10,000）",
  "debit_total": "110,000",
  "credit_total": "100,000",
  "difference": "10,000"
}
```

- `error` はエラーコード（§6 の対応表）。`message` は `CoreError` / `AppError` の
  `Display` を写像したもの。
- **現在の実装が返せるのはここまで。** `CoreError::Unbalanced` が持つのは
  `debit` / `credit` / `diff` の3つの表示用文字列だけで、
  「`tax_category` が `SALES_10` の明細に対する税額行がありません」のような
  税区分に踏み込んだ文言を組み立てる経路は存在しない。
  生成経路の無い文言を確定仕様として書かないこと。
- 金額欄が3桁区切り付き（`"110,000"`）なのは `Money::to_display_string()` の出力を
  そのまま載せているため。区切り無しに揃えるかどうかは §5 の未確定事項。

`hint`（修正案）は **PR-B で足す**。AI の自己修正を一段速くする効果は大きいが、
「税区分 → 税額科目」の対応を知るのは `kaikei-jp` の `TaxCategoryTable` だけで、
`TaxPolicy` trait には引く API が無く（`validate_tag` / `derive_tax_lines` /
`round_mode` / `apply_ratio` の4つのみ）、MCP 層でこの推論を書くのは
「MCP はユースケースを呼ぶ薄い層」（§4）と `CLAUDE.md` §1 に反する。
実現可能な形は次のとおり:

- `auto_tax_lines: false` で貸借不一致になった場合に限り、`kaikei-app` に新設する
  **dry-run ユースケース**（入力明細に `derive_tax_lines` を1回かけ、貸借が一致するなら
  生成された行を返す。既存の policy API だけで書ける）の結果を
  `hint.suggested_lines` に載せる。
- それ以外の場合は `hint` を返さない。
- 存在しない科目コードに対する `hint`（候補の科目）は MCP 層が `ChartOfAccounts` から
  組み立てる。`CoreError::UnknownAccount` のメッセージは
  「勘定科目が見つかりません: {code}」だけであり、**core に候補一覧を持たせない**
  （core の変更は人間の承認事項。`CLAUDE.md` §1・§9）。候補は全件ではなく
  絞って返し、件数の上限を決める。

### reverse_journal_entry

```json
{
  "original_id": "0192a7b3-xxxx-7xxx-xxxx-xxxxxxxxxxxx",
  "reverse_date": "2026-05-01",
  "reason": "請求金額の誤り（税率の適用誤り）",
  "allow_double_reversal": false
}
```

- `reason` は必須。**空文字・空白のみは MCP 層で拒否する。**
  下位層はいずれも空文字を通す:
  `JournalEntry::reverse` は `reverse_reason = Some(reason)` と代入するだけ、
  `ReverseEntryInput.reason` は素の `String` でユースケースも検証しない、
  DB の `CHECK ((reverses IS NULL) = (reverse_reason IS NULL))` は NULL の一致しか見ない
  （摘要には `CHECK (btrim(description) <> '')` があるのに `reverse_reason` には無い、
  という非対称が実在する）。
- `allow_double_reversal` の既定は `false`。既に赤伝済みの仕訳を再訂正しようとすると
  `AppError::AlreadyReversed { entry_no, reversal_no }` になる。
- **会計年度は `reverse_date` で決まる**（元仕訳が別年度でも、逆仕訳は
  `reverse_date` の年度で採番される）。
- 摘要は `「【訂正】{元の摘要}」` に固定され、`document_refs` は複製されない。

成功時：

```json
{
  "entry_id": "0192b1c4-xxxx-7xxx-xxxx-xxxxxxxxxxxx",
  "entry_no": 43,
  "fiscal_year": 2026,
  "reverses": "0192a7b3-xxxx-7xxx-xxxx-xxxxxxxxxxxx",
  "description": "【訂正】A社への請求",
  "lines": [ /* 借方・貸方を入れ替えた明細 */ ]
}
```

失敗時（二重訂正。ツール結果エラー）：

```json
{
  "error": "already_reversed",
  "message": "この仕訳は既に訂正済みです（元仕訳 No.42 / 逆仕訳 No.43）。同じ仕訳を再度訂正する場合は allow_double_reversal: true を指定してください。"
}
```

---

## 4. 実装方針

- **MCP サーバーは薄い層にする。** ビジネスロジックを MCP 層に書かない。
- ただし「薄い」の中身は経路によって3種類ある（下記）。
- ツール定義は `kaikei-mcp/src/tools/*.rs` に **1 ツール 1 ファイル**。
  ファイル名は **MCP のツール名と1対1**にする（`post_journal_entry.rs` /
  `get_trial_balance.rs` …）。`post_entry.rs` のような名前にすると
  `kaikei-app/src/usecase/post_entry.rs` と同名になり、grep でどちらの層の話か
  区別できなくなる。

### MCP SDK とトランスポート

| 項目 | 選定 |
|---|---|
| SDK | **`rmcp` 3.x**（`default-features = false`, `features = ["server", "macros", "transport-io", "schemars"]`） |
| トランスポート | **stdio**（MCP クライアントが子プロセスとして起動する） |
| 却下 | 手書き JSON-RPC / 第三者 SDK（D-071） |

- 入力は `Parameters<T>` で受け、`T` に `#[derive(Deserialize, JsonSchema)]` を付けると
  input_schema が自動生成される（`schemars` feature がこれを有効にする）。
- `#[tool]` はツール説明文を**関数の doc コメント**から採る。したがって
  `CLAUDE.md` §11（次の手が分かる文言）と §10（税務判断を断定しない）の規律は
  doc コメントにも及ぶ。§5 の「金額は文字列」もツールの doc コメントに書く。
- 1ツール1ファイルを rmcp で実現するには、各ファイルに
  `#[tool_router(router = <ツール名>_router, vis = "pub")]` を置き、`server.rs` で
  `tool_router_a() + tool_router_b()` と `+` で合成する。
  この機構を使わないと「単一 `impl` ブロックに全ツールを書く」しかなくなる。
- `crates/kaikei-mcp` を workspace の `members` に**追加する**
  （現在はコメントアウトされている。追加しないと CI の
  `cargo test --workspace` / `cargo clippy --workspace` の対象にすらならない）。
  `rmcp` は `[workspace.dependencies]` に追加する。
- `crates/kaikei-mcp/Cargo.toml` は **`rust-version = "1.88"` を個別に宣言する**
  （workspace 既定の `1.80` を継承しない。D-071）。

### DTO は kaikei-mcp が自前で持つ

**`kaikei-core` / `kaikei-app` の型は serde を実装しない。**
`kaikei-core` への serde 参照は CI（`.github/workflows/architecture.yml`）が
機械的に禁じており、`kaikei-app` の `Cargo.toml` にも serde は無い。
したがって `Money`（`BalanceRowView.debit_total` 等）も `JournalEntry` も
直接シリアライズできない。

→ **MCP の入出力 DTO は `kaikei-mcp` が自前で持ち、詰め替える。**
`Money` → 文字列（§5）、`AccountCode` → 文字列、`EntryId` → UUID 文字列。
上位層の都合で下位 crate に serde を足さないこと（依存方向の CI はこれを弾かない
ため、そのままマージされうる）。

### 3つの呼び出し経路

| 経路 | 対象ツール | 呼び方 |
|---|---|---|
| (a) 書き込み | `post_journal_entry` / `reverse_journal_entry` | `usecase::post_entry::execute` / `reverse_entry::execute` を `tx::with_tx` で包んで呼ぶ |
| (b) 読み取り（read model 直行） | `get_trial_balance` / `search_entries` / `get_ledger` / `get_entry` | `Tx` を通さず read model クエリを呼ぶ（`usecase::report::execute` は `ports::TrialBalanceQuery` を受け取る。`CLAUDE.md` §6「Repository を通さず SQL から DTO へ直行」） |
| (c) 税制の問い合わせ | `list_tax_categories` / `get_settings` / `suggest_tax_category` / `validate_invoice_number` | `kaikei-app` を経由せず、合成ルートが保持する `kaikei-jp` の値（`TaxRuleSets::for_date` / `JpSettings` / `InvoiceRegistrationNo::parse`）から直接組み立てる |

`list_accounts` は DB の `accounts`（`ChartRepo::load_chart`）を読む。

### Phase 3 で新設が必要なもの

- `crates/kaikei-store/src/query/{search,ledger,entry_detail}.rs`（read model）。
  対応するクエリ trait を `kaikei-app/src/ports.rs` に `TrialBalanceQuery` と同型で追加し、
  DTO は `kaikei-app/src/view.rs` に置く（core の型は外から構築できない。D-031）。
  `.sqlx` オフラインキャッシュの再生成が要る
  （`.github/workflows/database.yml` の `cargo sqlx prepare --workspace --check`）。
- `kaikei-app` の**勘定科目マスタ投入ユースケース**（D-070）。
- **audit_log 書き込みポート**（§8）。
- `post_entry::execute` の戻り値拡張（`PolicyNote` を返す。§3）。

### ディレクトリ構成

```
kaikei-mcp/src/
├── main.rs               起動（設定ロード → 合成 → stdio サーバ）
├── startup.rs            合成ルート。kaikei_jp::compose::compose + PgStore の結線
├── config.rs             事業者設定の読み込みと必須検証（欠けていたら起動失敗。§7）
├── audit.rs              audit_log。別コネクション・2回書き・fail-closed/fail-open（§8）
├── amount.rs             §5 の金額文字列 ⇄ Money 変換を1箇所に閉じる
├── server.rs             tool_router の合成、rmcp の ServerHandler 実装
├── error.rs              AppError → CallToolResult::error（isError: true）への変換（§6）
└── tools/
    ├── list_accounts.rs
    ├── get_entry.rs
    ├── get_trial_balance.rs
    ├── search_entries.rs
    ├── get_ledger.rs
    ├── list_tax_categories.rs
    ├── get_settings.rs
    ├── post_journal_entry.rs
    ├── reverse_journal_entry.rs
    ├── suggest_tax_category.rs
    └── validate_invoice_number.rs
```

`tools/` というフォルダ名は `CLAUDE.md` §6 に反しない。§6 が禁じるのは
`entities/` / `value_objects/` のような DDD のパターン名（技術的分類）であり、
"tool" は MCP プロトコル自身の語彙＝この層のユビキタス言語である
（`kaikei-app/src/usecase/`、`kaikei-store/src/query/` と同型）。

### 起動時の組み立て（合成ルート）

`kaikei-mcp` は**本番で最初の合成ルート**になる。

- 組み立ての入口は **`kaikei_jp::compose::compose(ComposeOptions { .. })` ただ1つ**。
  YAML ロードを自前で再実装しない（同じ組み立てが複製されると腐る。D-047 / D-068）。
- `Composition` に **`JpStatementPolicy` を含めない**。決算書を組み立てる直前に
  その時点の `chart` を読み直して都度 `JpStatementPolicy::new(chart)` する（D-069）。
- `Store` は trait object にせず**具象型 `Arc<PgStore>`** をそのまま持つ（D-029）。
- DB 接続は `connect_app(APP_DATABASE_URL)`＝**`kaikei_app` ロール**を使う
  （D-048。理由は §7）。
- **`kaikei-e2e` のコードを流用・依存してはならない。** `kaikei-e2e` は
  「他のどの crate からも依存されない」制約下にあり（D-068、
  `architecture.yml` の該当ステップ）、依存した瞬間にその crate の存在意義が壊れる。
  必要なのは `kaikei-jp::compose` であって `kaikei-e2e` ではない。

### ログ出力（stdio 固有の制約）

**stdout は JSON-RPC 専用チャネル。**
`println!` や `tracing` の既定出力が stdout に1行でも混ざるとプロトコルが壊れ、
接続ごと落ちる。既存コードには `tracing::warn!`（D-039 の commit 忘れ警告など）が
実在し、購読者の設定次第で stdout に出る。

→ ログ・診断出力は必ず **stderr** に出す（`tracing_subscriber` の writer を
stderr に固定する）。`println!` を書かない。

---

## 5. 金額の受け渡し

**JSON では金額を文字列で扱う。**

```json
{ "amount": "110000" }     ✅
{ "amount": 110000 }       ❌ JSON の number は倍精度浮動小数点
```

JSON の number は IEEE 754 倍精度なので、大きな整数や小数で誤差が出る可能性がある。
会計データでこれは許容できない。**入出力とも文字列で統一。**

**文字列にするのは金額と日付だけ。** 件数・会計年度・仕訳番号（`entry_no`）は
JSON number のままでよい。

### number は常にエラー

**整数であっても受理しない**（D-013 の初版が残していた「整数なら警告付きで受理する余地」は
Phase 3 で却下した。D-013 の訂正注記を参照）。
サーバに届いた時点では、クライアント側で既に倍精度に丸められたかどうかを
**サーバから検出できない**。受理すれば「警告付きで壊れた金額を記帳する」ことになる。

実装形:

- 金額フィールドは専用の newtype（例 `AmountStr`）にし、`Deserialize` を**手書き**して
  number / bool / null を受けたら `de::Error::custom` で日本語のメッセージを返す。
  例:「金額は文字列で渡してください（例: `"110000"`）。JSON の number は
  倍精度浮動小数点のため、会計金額には使えません」。
  素の `String` にしておくと AI には `invalid type: integer 110000, expected a string`
  という英語の型エラーしか届かず、`CLAUDE.md` §11 を満たさない
  （D-019 が `{:?}` の英語バリアント名を禁じたのと同型の問題）。
- `JsonSchema` は `#[schemars(with = "String")]` 等で `"type": "string"` を出す。
  スキーマ上 number も許されているように見えると、AI が number を送る動機を作る。
- **`as_f64()` を書かない。** `.github/workflows/architecture.yml` の
  「f64 が金額に使われていない」ステップ（コメント行以外の `f64` を全て落とす）が
  必ず赤になる。
- rmcp は `Parameters<T>` のデシリアライズ失敗を `CallToolResult::error`
  （`isError: true`）に変換するため、この経路は §6・D-071 と整合する。

### 通貨の決め方（PR-B で確定）

`Money::parse(s, currency)` は `Currency` を**必須の引数**として要求し、
`Currency::new(code, minor_unit)` は小数桁数まで必須である。
つまり `{"amount": "110000"}` だけでは `Money` を構築できない。推奨形:

1. **帳簿通貨（コード＋小数桁）を事業者設定の必須項目にする**（§7 と同じ扱い。
   既定で JPY にフォールバックしない）。
2. 明細で `currency` を省略した場合は帳簿通貨を使う。
3. 明示された場合は `kaikei_app::currency::currency_from_code` で解決する。
   **未知のコードは桁数を推測せずエラー**（推測すると金額が100倍ズレて記帳される。
   `CLAUDE.md` §8）。
4. 1仕訳内で通貨が混在すると `JournalEntry::new` が `CurrencyMismatch` を返す。
   この制約をツールの説明文に書く。
5. 小数桁は通貨ごとに検証される（JPY に `"1000.5"` を渡すとエラー、USD は2桁まで）。

現状「帳簿通貨」を保持する場所はどこにも無い（`BookSettings` は `fiscal_year_rule`
のみ、`JpSettings` にも通貨フィールドは無い）。この追加も PR-B の作業項目。

### 出力の文字列形式（PR-B で確定）

`kaikei-core` が持つ唯一の文字列化は `Money::to_display_string()` で、
これは**3桁区切りカンマ付き**（`"110,000"`、USD なら `"1,234.56"`）を返す。
区切り無しの文字列化 API は core に無い（`minor()` は最小通貨単位の整数であり、
`minor_unit > 0` の通貨では金額そのものではない）。

推奨は次の分離:

- 機械可読フィールド（`amount` / `debit_total` 等）は**区切り無し**
  （`"110000"` / USD は `"1234.56"`）に固定する。
- 区切り付きの表記は `message` の文中でのみ使う（`Money::to_display_string()`）。

区切り無しを採る場合、整形手段が要る。`kaikei-core` に `to_plain_string()` を足す
（**core の変更は人間の承認が必要**）か、`kaikei-mcp/src/amount.rs` の DTO 変換に
閉じるかを PR-B で決める。`amount.rs` には `Money::parse` とのラウンドトリップテストを
置くこと（`Money::parse` は正しい3桁区切り付きも受理するので、入力側の互換は保たれる）。

### 桁の上限

文字列で渡しても、最小通貨単位で `i64` の範囲（`journal_lines.amount_minor` は
`BIGINT`）を超える金額は記帳時に `RepoError::OutOfRange` になる。
`Money` は `i128` 全域を保持できるため、`Money::parse` は通っても
記帳の直前で落ちる金額が存在する。

---

## 6. エラーの返し方

### 返し方

**ドメインのエラーは全てツール結果エラー（`isError: true`）で返す。**
JSON-RPC のプロトコルエラー（rmcp では `Err(ErrorData)`）は使わない。
クライアントがプロトコルエラーを不透明に描画する（例:
"Tool result missing due to internal error"）と、呼び出し元のモデルに
メッセージが届かず、`CLAUDE.md` §11「AI が自己修正できる文言」が空文になる（D-071）。

プロトコルエラーを使ってよいのは、ツール呼び出しに到達できない異常
（未知のツール名など）に限る。

### コードの写像

`AppError` → `error` コードの対応表を1箇所に持つ。写像元は次の3つ:

- `AppError`（`Repo` / `Policy` / `Core` / `AlreadyReversed` / `Inconsistent` / `Rejected`）
- `CoreError`（16 バリアント）
- `PolicyError`（8 バリアント）

`AppError` は `#[non_exhaustive]` なので、**網羅 `match` ではなく `_ =>` の受け皿を
必ず持つ**。受け皿の既定コードを決めておかないと、実装者が場当たりのコードを発明する。

| 例 | コード |
|---|---|
| `CoreError::Unbalanced` | `unbalanced` |
| `CoreError::UnknownAccount` | `unknown_account` |
| `CoreError::NotPostable` | `not_postable` |
| `CoreError::NotAggregatable` | `not_aggregatable` |
| `AppError::AlreadyReversed` | `already_reversed` |
| `RepoError::AppendOnlyViolation` | `append_only_violation` |
| 上記以外・未知バリアント | `internal` |

**対応表の確定は PR-B。** 上の表は書き方の例であり、全バリアントを埋めた版を
PR-B でこの節に置く。

---

## 7. 起動と設定

**事業者設定は明示必須。1つでも欠けたら起動を中止する。既定値で走らない。**

`JpSettingsOverrides` の `is_taxable_business` / `simplified_taxation` は
`Option` ではない素の `bool` で、`Default` も**意図的に実装されていない**
（「指定し忘れたら免税事業者扱いになる」事故を避けるため。D-057）。
設定ファイルで省略して `Default` に落ちる実装にすると、
**無言で免税事業者として税額計算される**。

| 設定 | 内容 |
|---|---|
| `tax_mode` / `rounding` / `rounding_unit` | 上書きの有無（省略時はマスタの `settings_defaults`） |
| `is_taxable_business` | 課税事業者か（**必須**） |
| `simplified_taxation` | 簡易課税か（**必須**） |
| 決算3科目 | 元入金・事業主貸・事業主借（`ClosingAccounts`。構築時に実在検証される。D-066） |
| `closing_tax_category` | 決算振替のゼロ化明細に付ける税区分コード |
| 帳簿通貨 | コード＋小数桁（§5。既定で JPY にフォールバックしない） |
| `APP_DATABASE_URL` | `kaikei_app` ロールの接続文字列（§8） |

- 検証は `config.rs` に閉じ、`main.rs` は「読めなければ起動しない」だけにする。
- `kaikei_jp::compose` が返す `ComposeError` の日本語メッセージは、
  そのまま起動失敗の理由として stderr に出す（言い換えない）。
- `get_settings` は起動時に合成した `JpSettings` をそのまま返すツールであり、
  **未設定時に既定値を返すことはない**（そもそも起動していない）。

---

## 8. 認証と信頼境界

### kaikei-mcp（Phase 3、stdio）

**認証機構を持たせない。これは延期ではなく、トランスポートの性質上そもそも
認証する対象が無いためである。**

`kaikei-mcp` は MCP クライアント（Claude Code 等）が子プロセスとして spawn する
プロセスであり、**ソケットを一切開かない**。listen アドレス・ポートの設定項目を
作らないこと。呼び出し元は OS のプロセス起動権限で既に決まっており、
トークンを検証しても、それを持ち込めるのは同じ OS ユーザーだけである。

信頼境界は2つだけ:

1. **このプロセスを起動できる OS ユーザー**
2. **接続に使う DB ロール**

Phase 3 で認証用のフィールド・設定項目を先取りで用意しないこと（YAGNI）。

### DB ロールが実効的な権限境界

認証を持たない以上、実効的な権限境界は DB ロールだけになる。

**`kaikei-mcp` は必ず `APP_DATABASE_URL`（`kaikei_app` ロール）で接続する。**
`DATABASE_URL` / `MIGRATOR_DATABASE_URL`（`kaikei_migrator`）で接続してはならない
（D-048 の変数分離）。`kaikei_migrator` はテーブル所有者なので
`0003_journal.sql` の `REVOKE` をバイパスし、append-only の DB 権限防御（D-006）が
丸ごと消えてトリガ（`0004`）だけが残る。**接続先の環境変数を1つ間違えるだけで
本プロジェクトの中核防御が1層失われる。**

起動時に接続ロールを検査し、`kaikei_migrator` なら起動を中止するのが望ましい
（§7 の「未設定は起動失敗」と同じ思想）。

### 設定ファイルの取り扱い

MCP クライアントの設定ファイル（例: Claude Code の MCP 設定）に
`APP_DATABASE_URL` を書く場合、**DB パスワードが平文で置かれる**ことになる。
ファイル権限に注意する。

### ネットワーク越しの公開は Phase 4 の論点

- **PostgreSQL**: `docker-compose.yml` が `127.0.0.1:5432:5432` として
  ループバックにのみポートを公開している（実装済み。D-015 の実効部分はこれ）。
- **`kaikei-api`（Phase 4、axum / HTTP）**: ここで初めてバインドアドレスの既定値、
  外部公開時の注意、トークン認証（D-015 の留保）が論点になる。
  外部公開の注意書きの宛先はこちらであって `kaikei-mcp` ではない。

---

## 9. 監査ログ

MCP 経由の操作は**読み取り系も含めて全て**記録する
（`ROADMAP.md` Phase 3 の完了条件「全操作が audit_log に記録される」）。

### 1リクエスト＝2行

**開始レコードと結果レコードを、操作用トランザクションとは別コネクションで
2回書く**（D-070）。

同一トランザクションで1回だけ書く実装にすると、**失敗した操作の記録が rollback で
一緒に消える**。「AI が何をしようとしたか」が最も知りたいのは失敗したときである。

```
開始レコード（status='started'）を書く   ← 別コネクション
  ↓ 失敗したら fail-closed（操作しない）
with_tx(...) で操作を実行
  ↓
結果レコード（status='ok' | 'error'）を書く  ← 別コネクション
  ↓ 失敗したら fail-open（操作は成功として返し、警告を添える）
```

- **開始レコードが書けなければツールを実行しない（fail-closed）。**
  `isError: true` のツール結果で
  「監査ログに記録できなかったため操作を実行していません。**帳簿は変更されていません**」と、
  帳簿が無変更であることまで含めて返す（`CLAUDE.md` §11）。
- **結果レコードが書けなかった場合は操作を成功として返す（fail-open）。**
  結果に警告を添える（「記帳は完了しましたが監査ログの結果記録に失敗しました。
  request_id=... の行を確認してください」）。**再実行を促す文言にしない**
  （二重計上を招く）。
- 開始レコードだけが残り結果レコードが無い行は「**結果不明**」として読む。

### 書き込み経路

素直に書くと同一トランザクションになるので、経路を明示する。

- リポジトリはすべて `&mut Tx` 経由（`TxOps`。D-029）で、`with_tx` が
  commit/rollback を握っている。**`TxOps` に audit 用メソッドを生やすと必ず
  同一トランザクションになり、上の目的が消える。**
- `kaikei-app/src/ports.rs` に `TxOps` とは独立した監査ログ用ポート
  （例 `AuditSink`。`&self` で `PgPool` から別コネクションを acquire する）を新設し、
  実装は `kaikei-store` に置く（§4「ビジネスロジックを MCP 層に書かない」と整合）。
- 呼ぶのは `with_tx` の**外側**（開始レコード → `with_tx(...)` → 結果レコード）。
- 接続プールの枯渇に注意（`connect_app` の `max_connections` は 10）。

### スキーマ

```sql
CREATE TABLE audit_log (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    request_id  UUID NOT NULL,        -- ツール呼び出しごとにサーバが採番
    occurred_at TIMESTAMPTZ NOT NULL, -- Clock から取得した値を明示的に渡す
    actor       TEXT NOT NULL,        -- "mcp" / "cli" / "api"
    tool        TEXT NOT NULL,
    status      TEXT NOT NULL CHECK (status IN ('started', 'ok', 'error')),
    input       JSONB,                -- 開始レコードのみ
    output      JSONB,                -- 結果レコードのみ。確定後明細と PolicyNote
    error_code  TEXT,
    entry_id    UUID                  -- 外部キーは張らない（下記）
);

CREATE INDEX ON audit_log (request_id);
CREATE INDEX ON audit_log (occurred_at);
```

- **`BIGSERIAL` を使わない。** `BIGSERIAL` の既定値 `nextval` にはテーブル権限とは別に
  シーケンスの `USAGE` が必要で、付与し忘れると `kaikei_app` の INSERT が
  `permission denied for sequence` で失敗する。しかもその SQLSTATE は `42501` であり、
  `kaikei-store/src/sqlstate.rs` がこれを `RepoError::AppendOnlyViolation` に写像するため、
  エラーメッセージは「帳簿への訂正は逆仕訳のみです」になる。
  **監査ログが書けないという事象に対して完全に誤った対処法を案内する**ことになり、
  D-038 が潰したのと同じ誤診クラスの欠陥を再生産する。
  `GENERATED ALWAYS AS IDENTITY` ならテーブル権限だけで INSERT できる
  （稼働中の `postgres:17-alpine` で両方を実測して確認済み）。
  `BIGSERIAL` を残す場合は `GRANT USAGE ON SEQUENCE audit_log_id_seq TO kaikei_app;` を
  DDL に併記すること。
- **`request_id` はサーバが採番する**（UUID）。JSON-RPC の `id` は number にもなりうるので
  流用しない。2行の突き合わせが読み取りの基本操作なのでインデックスを張る。
- **`occurred_at` に `DEFAULT now()` を付けない。** `Clock`
  （`kaikei-app::clock::SystemClock`）から取得した時刻を明示的に渡す
  （`CLAUDE.md` §7。テストで時刻を固定できなくなる）。UTC 保存。
- **`entry_id` に外部キーを張らない。** 別トランザクションで書くため、
  rollback された操作の `entry_id` は存在しえない。
  そして「存在しないことを記録できること」自体に価値がある。
- **`error_code` には分類コードだけを入れる**（§6 の対応表の値）。
  AI に返した本文は `output` に入れる。
  接続文字列・認証情報を含みうる下位層のエラー本文をそのまま転記しない
  （`RepoError::Backend { reason }` には DB が返した文字列がそのまま入るため、
  分類コードと汎用メッセージに正規化する）。

### append-only の強制

**「append-only。」と書くだけでは守られない。** 帳簿本体と同じ4点セットで守る。

1. `GRANT SELECT, INSERT ON audit_log TO kaikei_app;`
   `REVOKE UPDATE, DELETE, TRUNCATE ON audit_log FROM kaikei_app;`
2. **`reject_mutation()` を流用しない。** audit_log 専用のトリガ関数を新設する。
   流用すると例外メッセージが「append-only table: % は変更できません
   （訂正は逆仕訳で行ってください）」になり、監査ログに対しては的外れな案内になる
   （監査ログは逆仕訳で直すものではない）。これも D-038 と同じ誤診クラス。
   メッセージ例:「監査ログは追記のみです。記録の訂正は新しい行の追加で行ってください」。
3. **専用 ERRCODE を割り当てる**（`P0010` / `P0011` の次、例えば `P0012`）。
   `kaikei-store/src/sqlstate.rs` の写像表に追加し、`AppendOnlyViolation` に寄せない。
4. `crates/kaikei-store/tests/privileges.rs` に audit_log の行を追加する。

`docs/03-database.md` §1 の GRANT 例にも audit_log を追加すること。

### 個人情報

**パスワード・接続情報**: 入力として受け取るツールを Phase 3 の11ツールに置かない。
この不変条件は維持する。

**個人情報は入る前提で考える。** 確定した11ツールには自由記述欄が複数ある:
`post_journal_entry` の `description`（例「A社への請求」）と明細の `memo`、
`tags.counterparty`、`reverse_journal_entry` の `reason`、
`suggest_tax_category` の取引内容、`search_entries` の検索語（摘要・取引先で検索する）。
個人事業主の取引相手は個人であることが普通なので、氏名・屋号は入る。
`validate_invoice_number` の登録番号も、公表事項を通じて事業者名に結び付く識別子である。

→ **`audit_log.input` / `output` は帳簿本体と同等の機微度として扱う。**
閲覧権限・エクスポート時の扱いを帳簿と揃える。「入らないから対策不要」と読める
書き方をしない。

「AI が何をしたか」を後から追えることは、AI に会計を触らせる前提条件。

---

## 10. テストケース

**この一覧は Phase 3 の完了条件。** 実装より先にこの一覧を全部テストとして書く
（失敗する状態でよい。`CLAUDE.md` §9）。

- 命名規約は `docs/02-test-cases.md` に揃える（`#[test] fn 分類_期待動作()`。
  日本語識別子は使わない）。
- 置き場は `crates/kaikei-mcp/tests/`。実 PostgreSQL が要るものは `pg-tests` feature 配下。
  **`kaikei-e2e` のコードを流用・依存しない**（D-068 の制約を壊さない）。

### 書き込み系

| # | ケース | 期待 |
|---|---|---|
| MC-01 | 貸借一致の仕訳を post | 成功。確定後明細が返る |
| MC-02 | 貸借不一致の仕訳を post | エラー。差額が含まれる（`hint` は PR-B で追加。§3） |
| MC-03 | `auto_tax_lines: true` で税行が自動追加（**税抜経理・課税事業者の設定**） | 貸借一致する。税込経理・免税事業者の設定では税行が生成されず貸借不一致になることも併せて確認する |
| MC-04 | 存在しない科目コード | エラー。`hint` には全件ではなく候補を絞って返す（コードの前方一致・名称の部分一致など。件数の上限を決める）。`hint` を組み立てるのは MCP 層（`ChartRepo::load_chart` の結果から作る）で、core のエラーは候補を持たない |
| MC-05 | 締め済み期間への post | エラー。fixture は `period_snapshots` に**直接 INSERT** して締め状態を作る（Phase 3 に `close_period` は無い。`kaikei_app` は `period_snapshots` に INSERT 権限を持つ） |
| MC-12 | `reverse_journal_entry` の理由が省略・空文字・空白のみ | 3ケースともエラー。**MCP 層で検証する**（core も app も DB も空文字を通す。§3） |

### 提案系・検証系

| # | ケース | 期待 |
|---|---|---|
| MC-08 | `suggest_tax_category` | (1) 根拠が空でない、(2) 呼び出しの前後で**帳簿が一切変わらない**（試算表と仕訳件数が不変。§1 ② の機械的検証） |
| MC-28 | `validate_invoice_number` の出力 | 形式検証の結果のみを述べ、「実在する／実在しない」と断定しない（`CLAUDE.md` §10） |

### 読み取り系

| # | ケース | 期待 |
|---|---|---|
| MC-13 | `list_accounts` | 科目種別（`account_type`）と**記帳可否（`postable`）**を含めて返す |
| MC-14 | `get_entry` | 存在する仕訳の明細・タグを返す。存在しない ID では次の手が分かる NotFound を返す |
| MC-15 | `get_trial_balance` | 期間で絞り込める。`group_by` 指定が効く。借方合計＝貸方合計。`from > to` はエラー（空結果にしない） |
| MC-16 | `search_entries` | 日付・金額・科目・摘要で絞り込める。**0件でも成功として空配列を返す**（エラーにしない） |
| MC-17 | `get_ledger` | 科目別に借方・貸方・残高を返す。期間指定が効く |
| MC-18 | `list_tax_categories` | 指定日時点で有効な区分のみを返す。取引日で切り替わる（D-050 / D-055）。該当マスタが無い日付では**エラー**で、メッセージに有効期間が含まれる |
| MC-19 | `get_settings` | 起動時に合成した `JpSettings`（税抜/税込・端数処理・端数処理単位・課税事業者区分・簡易課税）をそのまま返す。日付引数を取らない |

### プロトコル・契約

| # | ケース | 期待 |
|---|---|---|
| MC-09 | 金額を number で渡す | **エラー（整数でも受理しない）。** (1) メッセージが「金額は文字列で渡してください。例: `"amount": "110000"`」という次の手を含む、(2) `isError: true` のツール結果として返る、(3) この呼び出しも audit_log に残る。金額フィールドを素の `String` にするだけでは (1) を満たせない（§5 の実装形を参照） |
| MC-10 | 存在させないツール4件 | `tools/list` の応答に `delete_journal_entry` / `update_journal_entry` / `execute_sql` / `reopen_period` のいずれも現れず、それらの名前で `tools/call` すると未知ツールとして拒否される。**禁止リストをテスト側の定数にして4件すべてをループで検査する**（1件だけの検査では他が復活しても緑のまま通る） |
| MC-26 | ドメインエラー（貸借不一致等） | JSON-RPC のプロトコルエラーではなく `isError: true` のツール結果として返る（D-071） |
| MC-27 | 出力の金額 | 全て JSON 文字列である（入力だけでなく出力側も number にしない。§5） |
| MC-24 | 事業者設定（`is_taxable_business` / `simplified_taxation` 等）を与えずに起動 | 既定値にフォールバックせず**起動が失敗**し、不足項目を名指しするメッセージが出る（§7。D-057） |

### 監査ログ

| # | ケース | 期待 |
|---|---|---|
| MC-11 | 全ツール呼び出しが audit_log に記録される | 1回のツール呼び出しにつき**同一 `request_id` で2行**（`status='started'` と `status='ok'\|'error'`）が残る。`tool` 列が呼び出したツール名と一致する。書き込み系は結果レコードの `entry_id` が返した仕訳IDと一致する。**Phase 3 の全11ツールに対して総当たりで確認する**（1ツールだけのサンプル検査にしない） |
| MC-20 | 開始レコードの書き込みが失敗する状況（audit 用接続を落とす、または `REVOKE INSERT ON audit_log FROM kaikei_app`）で post | ツールは `isError: true` を返し、**帳簿には1件も入っていない**（fail-closed） |
| MC-21 | 結果レコードの書き込みだけが失敗 | 記帳は成功として返り、警告が添えられる。開始レコードだけが残り「結果不明」として識別できる（fail-open） |
| MC-22 | 記帳が失敗（貸借不一致等）してトランザクションが rollback される | **開始レコードは audit_log に残る。** D-070 の存在理由そのもの（同一トランザクションで書く実装に退行したら、このテストだけが落ちる） |
| MC-23 | `kaikei_app` の audit_log への権限 | `SELECT` / `INSERT` はでき、`UPDATE` / `DELETE` / `TRUNCATE` はできない（`crates/kaikei-store/tests/privileges.rs` の `assert_privilege` と同じ形で追加） |
| MC-25 | 経過措置対象の税区分（`PURCHASE_10_NON_QUALIFIED`）で記帳 | `PolicyNote` が post の戻り値に含まれ、かつ audit_log の `output` にも残る（D-059 / D-070） |

### Phase 4 以降に延期したケース（行を消さない）

消すと「なぜ無いのか」が失われ、実装者が善意で復活させる。

| # | ケース | 状態 |
|---|---|---|
| MC-06 | `close_period` を confirm なし → dry_run | **Phase 4 以降に延期**（checksum の計算式が Phase 5 の `kaikei verify` と同一である必要があり、仕様確定前に不可逆操作を実装しない。D-070） |
| MC-07 | `close_period` を confirm あり → 締まる | 同上 |
| MC-08（旧） | `suggest_journal_entry` の reasoning が空でない | **Phase 4 で復活**（`kaikei-import` 未着手。MC-08 は `suggest_tax_category` に差し替えた） |

---

## 付録 A. Phase 4 以降のツール（設計メモ。このフェーズでは実装しない）

**ここに書かれた入出力は確定仕様ではなく、当時の設計意図の保存である。**
実装する Phase で、その時点の実装に照らして書き直すこと。

### suggest_journal_entry（Phase 4）

`kaikei-import` の取込明細（`ImportedTransaction`）と仕訳化ルールに依存する。
Phase 3 時点では `imported_tx_id` を解決する経路も `similar_entries` を引く検索も無い。

```json
{
  "imported_tx_id": "0192...",
  "max_candidates": 3
}
```

```json
{
  "transaction": {
    "occurred_on": "2026-04-20",
    "amount": "1980",
    "direction": "out",
    "description": "ｶ)ｱﾏｿﾞﾝ ｼﾞﾔﾊﾞﾝ"
  },
  "candidates": [
    {
      "confidence": "high",
      "lines": [
        { "account": "609", "side": "debit",  "amount": "1980",
          "tags": { "tax_category": "PURCHASE_10_QUALIFIED" } },
        { "account": "100", "side": "credit", "amount": "1980" }
      ],
      "reasoning": "摘要が仕訳化ルール #3（'ｱﾏｿﾞﾝ' を含む → 消耗品費）にマッチ。過去12ヶ月で同摘要の取引が8件あり、いずれも消耗品費で処理されています。",
      "similar_entries": ["0191...", "0190..."]
    }
  ],
  "warnings": [
    "購入内容により税区分が異なる場合があります。領収書をご確認ください。"
  ]
}
```

**`reasoning` と `similar_entries` を必須にするという設計意図は維持する。**
これが既存の会計ソフトとの差であり、「なぜその科目か」を説明できることに価値がある。
実装時もこの2つを省略可にしないこと。

### close_period（Phase 4 以降）

**着手条件: checksum の計算式が Phase 5 の `kaikei verify` と同一であることを
確認してから。** `period_snapshots.checksum` は「対象仕訳のハッシュ連鎖」と
コメントされているだけで式が未定義であり、式が違えば締めた記録を後から検証できない。

不足しているものは checksum だけではない。`PeriodRepo` は `closed_through`（読み取り）
のみで締める操作が無く、`entry_count` / `last_entry_no` / `unbalanced_check` に
対応するクエリも無い。`pending_transactions` は `kaikei-import`（Phase 4）が要る。

```json
{
  "fiscal_year": 2026,
  "period_end": "2026-12-31",
  "confirm": true
}
```

`confirm` が `false` または省略の場合、実行せずに影響範囲を返す。

```json
{
  "status": "dry_run",
  "message": "この操作は不可逆です。締め後、2026-12-31 以前の日付で仕訳を追加できなくなります。",
  "entry_count": 342,
  "last_entry_no": 342,
  "unbalanced_check": "ok",
  "pending_transactions": 5,
  "warnings": ["未仕訳の取込明細が 5 件あります。締める前に処理を検討してください。"]
}
```

**未処理があれば警告する。** 締めは取り消せないので、事前確認を厚くする
（§1 ④ の唯一の適用対象になる予定のツール）。

### get_statements / explain_balance（Phase 4 以降）

**着手条件: `TrialBalance` / `BalanceRow` を `kaikei-core` の外から構築する手段を
決めてから**（D-031。`GroupKey` に公開コンストラクタが無い）。
read model の DTO から決算書を組み立てる経路を設計する必要がある。
`JpStatementPolicy` はその直前に `chart` を読み直して都度構築する（D-069）。
