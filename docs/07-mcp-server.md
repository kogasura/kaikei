# 07 — MCP サーバー（kaikei-mcp）

**このプロジェクトの差別化の本体。**
AI エージェントが会計操作を安全に行うための標準インタフェース。

> **この文書の版**: Phase 3 **PR-G**（読み取り系・提案系ツール7件）と
> **PR-H**（`search_entries` / `get_ledger` とその read model）の**両方**を
> 反映している（`DECISIONS.md` D-089 まで）。この2つは並行して開発され、
> 本文書は両者を取り込んだ状態である——**片方だけを名乗らないこと。**
>
> PR-H で**内容が変わったのは4箇所**:
>
> | 節 | 変わった点 | 決定 |
> |---|---|---|
> | §2 | `search_entries` / `get_ledger` の説明を実装に合わせ、「read model の新設が要る」を**実装済み**に直した。件数の上限・ページング・取り消された仕訳の見え方を明記した | D-088 / D-089 |
> | §3 | `search_entries` / `get_ledger` の入出力を追加した（この2つは §3 に無かった） | D-088 / D-089 |
> | §4 | 「Phase 3 で新設が必要なもの」の read model の項を**完了**にした。`entry_detail` は**新設しない**（`get_entry` は `JournalRepo::find_entry` で足りる） | D-070 |
> | §10 | MC-16 / MC-17 に実装した検査の置き場を書いた。MC-11 の総当たりの残りを更新した（PR-G と合流した時点で**残り0件**） | — |
>
> **PR-H レビュー**で更に3箇所:
>
> | 節 | 変わった点 | 決定 |
> |---|---|---|
> | §3 | `search_entries` の `account` に**勘定科目マスタに無いコード**を渡すと `not_found`（0件の成功にしない。`get_ledger` と同じ規律）。`get_ledger` の行に `reverse_reason` を足した。`truncation_note` が**呼び出し元の `limit` で切れたのかサーバの上限で切れたのか**を述べるようにした | D-088 決定3 / D-089 決定3 |
> | §9 | **読み取り系の `audit_log.output` は要約**（条件・合計・件数・`next_cursor`）にした。書き込み系は本文をそのまま残す | D-089 決定6 |
> | §4 | `entry_detail` を新設しない旨は D-070 の**訂正注記1**にも入れた（決定記録そのものが古い指示を残していた） | D-070 訂正注記1 |
>
> PR-G で**内容が変わったのは5箇所**:
>
> | 節 | 変わった点 | 決定 |
> |---|---|---|
> | §2 | `get_entry` の read model（`query/entry_detail.rs`）の新設を**取り下げた**（`JournalRepo::find_entry` を使う）。PR-H が新設するのは `search.rs` / `ledger.rs` の2本である | D-086 |
> | §3 | 読み取り系・提案系7件の入出力を**実サーバの応答**で追記した | D-086 / D-087 |
> | §4 | 経路 (b) から `get_entry` を外し、`list_accounts` と並べて「`Tx` 経由で読む」側に置いた | D-086 |
> | §7 | 「PR-G への申し送り: `kept_existing` の出口を stderr だけにしない」を**実装済み**にした（`get_settings` の `chart_differences`） | D-087 |
> | §10 | MC-08 / MC-13 / MC-14 / MC-15 / MC-18 / MC-19 / MC-28 の実装箇所と、MC-11 の残り（PR-H が実装した2件）を書いた | D-086 |
>
> **PR-G のレビュー指摘（B / C-1 / C-2 / C-3）で §3 が4箇所変わった:**
>
> | 節 | 変わった点 | 決定 |
> |---|---|---|
> | §3 `get_entry` | **その仕訳が既に訂正済みか**（`reversed_by` / `reversed_by_entry_no`）を返すようにした。初版は逆仕訳→原仕訳の向きしか返さず、訂正済みの仕訳の応答が未訂正のものと1バイトも違わなかった | D-086 |
> | §3 `get_trial_balance` | `group_by` の**未登録キー**（`unknown_tag_key`）と**登録済みだが集計軸に使えないキー**（`not_aggregatable`）を別のエラーにし、どちらにも `aggregatable_group_by_keys` を添えた | D-086 |
> | §3 `get_settings` | `chart_differences` を `{"as_of": "startup", "items": [...]}` にした（起動時点のスナップショットであることを応答に残す） | D-087 |
> | §3 `suggest_tax_category` | `filtered_by` に帳簿の設定（`tax_mode` / `is_taxable_business` / `simplified_taxation`）を並べ、`disclaimer` に「帳簿の設定によっては税額行が生成されない」を足した | D-087 |
>
> PR-F（PR-G / PR-H の1つ前の版）で変わったのは次の6箇所である:
>
> | 節 | 変わった点 | 決定 |
> |---|---|---|
> | §3 | `tags` の重複キーについて「エラーにする」を撤回し、**MCP 経由では検出できない**（後勝ちになる）を制約として明記した。貸借不一致の失敗応答の例を、実際に生成される文言に合わせた | D-085 |
> | §4 | ツールの実現機構を `#[tool_router]` / `#[tool]` マクロから **dispatch 層**（`McpTool` + `dispatch::ToolRegistry` / `dispatch::call`）に変えた。1ツール1ファイルは維持。`audit.rs` は**作らない**。`Parameters<T>` を使わない旨もここに揃えた | D-084 / D-085 |
> | §5 | 「rmcp は `Parameters<T>` のデシリアライズ失敗を `CallToolResult::error` に変換する」を削除（`Parameters<T>` は使わない） | D-085 |
> | §9 | 「未知のツール名は `isError: true` + `rejected` で返す」を撤回し、**プロトコルエラー**（§6 が認める唯一の例外）に揃えた。`audit_log.tool` にクライアント由来の文字列を載せない担保の内訳を実態どおりに書き直した（型で閉じているのは `ToolName::resolve` の側で、`AuditCall` を1箇所に閉じているのは走査である）。失敗時の `output` も応答本文をそのまま載せる | D-084 |
> | §10 | 書き込み系の実 DB テストの置き場を `crates/kaikei-e2e/tests/mcp_write_tools.rs` と明記した（`kaikei-mcp` は `sqlx` を持てないので使い捨てDBも `audit_log` の SELECT もできない） | D-084 |
>
> §7（起動と設定）は PR-E 時点のまま（D-082）。
> §8 の接続ロール検査は**2テーブル × 3権限**に広げてある。
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
| `get_entry` | **Phase 3** | 仕訳 1 件の詳細（明細・タグ）。**訂正の関係は両方向**——その仕訳が訂正している原仕訳（`reverses`）と、その仕訳を訂正した赤伝（`reversed_by`）。後者は集約に入っていないので `JournalRepo::find_reversal_of` を同じトランザクションで引く（PR-G レビュー B。§3）。証憑リンクは Phase 4 |
| `get_trial_balance` | **Phase 3** | 試算表。集計期間（`from`/`to`、取引日ベース・両端含む）は**必須**。`group_by` には `aggregatable: true` のタグキーのみ指定可（それ以外は `NotAggregatable`。`CLAUDE.md` §4）。`from > to` は空の試算表ではなく**エラー**（入力ミスを「0件の空の試算表」として静かに成功させない）。集計対象の通貨が単一であることを要求する（D-042。帳簿通貨と異なる行があれば `currency_mismatch`）。**0行の期間でも `currency` を返す**（PR-B 2巡目）。入出力は §3 |
| `search_entries` | **Phase 3** | 取引日・金額・科目・タグ（取引先など）・摘要で仕訳検索。**PR-H で実装**（read model は `crates/kaikei-store/src/query/search.rs`）。**0件は成功**（空配列。エラーにしない）。件数に上限があり、切れたことは応答から読み取れる（`total_matches` / `returned` / `has_more` / `next_cursor` / `truncation_note`。D-089）。タグでの絞り込みは `aggregatable: true` のキーだけ（`group_by` と同じ規則。D-088）。**赤伝で取り消された仕訳も返る**が、`reversed_by` が付いてそれと分かる（D-088）。**`account` に勘定科目マスタに無いコードを指定した場合は `not_found` のエラー**（`get_ledger` と同じ規律。0件の成功にしない。D-088 決定3）。入出力は §3 |
| `get_ledger` | **Phase 3** | 総勘定元帳（科目別の明細と残高の推移）。**PR-H で実装**（read model は `crates/kaikei-store/src/query/ledger.rs`）。集計期間（`from`/`to`、取引日ベース・両端含む）は**必須**。`from > to` は空の元帳ではなく**エラー**。**合計はページではなく期間全体**の値で、行ごとの `running_balance` は期首残高からの累計。**明細が0行の期間は成功**だが、**勘定科目マスタに無い科目コードは `not_found` のエラー**（打ち間違いと「取引が無い」を混同させない）。入出力は §3 |
| `list_tax_categories` | **Phase 3** | 有効な税区分一覧（指定日時点）。該当する年度マスタが存在しない日付では**空配列ではなくエラー**を返し、有効期間を示す（例:「2026-01-01〜2026-12-31 のマスタのみ同梱されています。取引日を確認してください」）。`TaxRuleSets::for_date` は該当なしで `None` を返す（D-055）。**前工事は PR-B 3巡目で完了**: `TaxRuleSets::iter` / `len` / `is_empty`（保持するマスタの列挙）、`TaxCategoryTable::range_display`（`pub` 化）、`TaxRuleSets::available_ranges_display`（適用開始日の昇順に並べた有効期間）、`TaxRuleSets::require_for_date`（`None` を `JpError::NoApplicableTaxRuleSet` にし、有効期間を文言に含める）。**このエラー文言を MCP 層で書き起こさない**（D-072 と同じ理由で、同じ文言が複数ツールに散る）。**空マスタと未収録は意味が違う**——空配列を返すと AI が「この日は税区分が1つも無い」と誤解して税区分なしで記帳しようとする |
| `get_settings` | **Phase 3** | 経理方式（`tax_mode`）／端数処理方式（`rounding`）／端数処理単位（`rounding_unit`）／課税事業者か（`is_taxable_business`）／簡易課税か（`simplified_taxation`）と、会計年度の区切り規則・帳簿通貨を返す。**帳簿通貨は `kaikei_app::context::BookSettings::book_currency` が保持する**（PR-B で追加。`Option` ではない必須フィールドで、既定で JPY にフォールバックしない。D-074）。`JpSettings` 側には持たせていないので、この応答は `JpSettings` と `BookSettings` の2つから組み立てる。**日付引数を取らない**（事業者設定は起動時に一度だけ合成され、取引日に応じて変わらない。D-057）。設定が未指定ならサーバは起動に失敗するので、このツールが既定値を返すことはない（§7） |
| `get_statements` | Phase 4 以降 | B/S・P/L。**延期理由: D-031。** `TrialBalance` / `BalanceRow` は `kaikei-core` の外から構築できず（`GroupKey` に公開コンストラクタが無い）、DTO 経由で組み立て直す設計が要る |
| `list_pending_transactions` | Phase 4 以降 | 未仕訳の取込明細。**延期理由: `kaikei-import` 未着手**（crate もテーブルも存在しない） |
| `search_documents` | Phase 4 以降 | 証憑検索（日付・金額・取引先）。**延期理由: `kaikei-blob` 未着手**（`documents` / `entry_documents` は Phase 4 で設計する。`docs/03-database.md` §1 の注記） |

**`search_entries` / `get_ledger` の read model は PR-H で新設した**
（`crates/kaikei-store/src/query/search.rs` / `ledger.rs`。対応するポートは
`kaikei_app::ports::{SearchEntriesQuery, LedgerQuery}`、DTO は
`kaikei_app::view::{EntrySummaryView, LedgerPageView}`）。書き込み側
`Store`/`PgTx` を経由せず SQL から DTO へ直行する（`CLAUDE.md` §6・D-031）。

**`entry_detail.rs` は新設しない。** `get_entry` が扱うのは仕訳1件で、
集約をそのまま返す `JournalRepo::find_entry` で足りる（集計も結合も無い経路に
read model を増やすと、同じ復元処理が2箇所に育つ）。実装方針は §4。

> **`get_entry` の read model（`entry_detail.rs`）は作らない**（PR-G。D-086）。
> 初版はこの3つを並べていたが、`get_entry` は**集計ではなく集約1件の取得**で
> あり、`JournalRepo::find_entry` が既に `JournalEntry`（明細・タグ・**その
> 仕訳が誰を訂正しているか**）を返す。D-031 が read model を要求したのは
> `TrialBalance` / `BalanceRow` が core の外から構築できないためで、
> `JournalEntry` にその制約は無い（`rehydrate` があり `kaikei-store` が実際に
> 使っている）。同じ「仕訳1件を読む」経路を2本持つと、
> `reverse_journal_entry` が読む姿と `get_entry` が返す姿が別々に育つ。
> **PR-H が新設するのは `search.rs` / `ledger.rs` の2本である。**
>
> **★集約が持つのは後ろ向きのポインタだけである★**（PR-G レビュー B。
> 以前ここは「訂正の関係を含む」と書いていたが、実態より1段強かった）。
> `JournalEntry` にあるのは `reverses` / `reverse_reason`——
> **逆仕訳 → 原仕訳**の向きだけで、**原仕訳 → 逆仕訳**の関係は集約に入って
> いない。read model を作らないという判断は変わらないが、その分
> `get_entry` は `JournalRepo::find_reversal_of`（`reverse_entry::execute` が
> 二重訂正の検出に既に使っているポート）を**同じトランザクションの中で**
> もう1回引いて `reversed_by` を返す。新規 SQL も `.sqlx` の更新も要らない。

### 書き込み系

| ツール | Phase | 説明 | 備考 |
|---|---|---|---|
| `post_journal_entry` | **Phase 3** | 仕訳を起こす | 貸借不一致は必ずエラー（`JournalEntry::new` と DB の遅延制約トリガの二重で防ぐ）。成功時は確定後の明細に加えて **`PolicyNote` の一覧を返す**（非適格の経過措置など、税額に反映されず注記にしか出ない情報がある。D-059）。`PolicyNote` は audit_log の `output` にも残す（D-070） |
| `reverse_journal_entry` | **Phase 3** | 赤伝を起こす | 理由（`reason`）が必須。**空文字・空白のみは `kaikei-app` のユースケース層が拒否する**（PR-B。`reverse_entry::execute` が I/O より前に `AppError::EmptyReverseReason` を返す。D-074）。MCP 層はそれを写像するだけでよい。既に赤伝済みの仕訳を再訂正する場合は `allow_double_reversal: true` が必要（既定は拒否） |
| `journalize_transaction` | Phase 4 以降 | 取込明細を仕訳化 | **延期理由: `kaikei-import` 未着手** |
| `ignore_transaction` | Phase 4 以降 | 取込明細を無視 | **延期理由: `kaikei-import` 未着手** |
| `attach_document` | Phase 4 以降 | 証憑を仕訳に紐付け | **延期理由: `kaikei-blob` 未着手。** 現状 `PgTx::insert_entry` は `document_refs` が非空なら `RepoError::Unsupported` を返す（D-041） |
| `upsert_counterparty` | Phase 4 以降 | 取引先マスタ更新 | **延期理由: ポートに書き込みメソッドが無い**（`ChartRepo` は `load_chart` / `load_counterparties` のみ）。DB 権限（`GRANT ... UPDATE ON counterparties`）は既にある |
| `upsert_journalize_rule` | Phase 4 以降 | 仕訳化ルール更新 | **延期理由: `kaikei-import` 未着手** |
| `close_period` | Phase 4 以降 | 期間を締める | **延期理由: 前工事が大きい。** `ROADMAP.md` の Phase 3 成果物に `close_period` は無い。実装するには `period_snapshots` の NOT NULL 列（`balances` JSONB / `entry_count` / `last_entry_no` / `checksum`）を埋める必要があり、**期間内の仕訳を列挙するポートが存在しない**（`JournalRepo` は `find_entry` / `find_reversal_of` / `insert_entry` のみ）。canonical JSON の正規化・ハッシュ連鎖・新ユースケース・append-only の権限テストが芋づるで要る。`dry_run` 応答の `pending_transactions` は Phase 4（`kaikei-import`）依存。なお **checksum の計算式自体は `docs/03-database.md` §2「checksum の計算方法」に定義済み**（`h_i = sha256(h_{i-1} || canonical_json(entry_i))`）。ただし `canonical_json` が対象とする仕訳の JSON 形は未定義であり、Phase 5 の `kaikei verify` と揃える必要がある（D-070）。`confirm: true` 必須・不可逆という要件は実装時に維持する。仕様案は付録 A |

**勘定科目マスタの投入は MCP ツールではない。**
`kaikei-app` に専用のユースケースを新設して行う（D-070）。
`kaikei_jp::compose` が返す `chart` は埋め込みテンプレート由来であり、
`ChartRepo::load_chart` が読む DB の `accounts` とは別物である点に注意。

**PR-E で実装済み**: `kaikei_app::usecase::import_chart::execute` と
ポート `kaikei_app::ports::ChartWriteRepo`（PostgreSQL 実装は
`crates/kaikei-store/src/chart.rs`）。合成ルートが**起動のたびに**呼び、
**追加しか行わない**（既存の科目定義は上書きしない。D-081）。
`ChartWriteRepo` を `TxOps` の束ねに入れていないので、記帳の経路が
マスタ書き込みの能力を持つことはない。

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

上の4件は理由が同じではない。**混ぜないこと。**

- `delete_journal_entry` / `update_journal_entry` / `execute_sql` は
  **D-014 により将来も作らない。** 「Phase 4 以降に回す」ではない
- `reopen_period` は **D-014 の対象ではない**（D-014 の決定文が名指しするのは
  削除・更新・SQL 実行の3件）。締め（`close_period`）自体が Phase 4 以降なので
  **今は作りようがない**、というだけ。取り消し手段を設けるかどうかは
  `close_period` を実装する Phase で決める

いずれも Phase 3 では**存在しない**ことをテストで機械的に検査する（§10 MC-10）。

締めの取り消しの現状: 手段は**CLI を含めて存在しない**（リポジトリのバイナリは
`crates/kaikei-store/src/bin/kaikei-migrate.rs` のみ。`kaikei-cli` は存在しない）。
`period_snapshots` は `kaikei_app` に `SELECT, INSERT` しか付与されていないため
（`0007_period_snapshots.sql`）、仮に CLI を作っても `kaikei_app` 接続では取り消せず、
DB 所有者権限での手動操作以外に手段は無い。

---

## 3. 主要ツールの入出力

**この節に載せるのは Phase 3 で実装するツールだけ。**
`suggest_journal_entry` / `close_period` の入出力仕様案は付録 A に移してある。

> **未確定事項の現況**（PR-B 時点）。
>
> | # | 論点 | 状態 |
> |---|---|---|
> | 1 | **タグ値の線上形式**（後述） | **PR-B（3巡目）で確定。** 平文の文字列マップ（`{"tax_category": "SALES_10"}`）。`TagSet` への変換は `kaikei_jp::tags::TagCatalog::parse_tag_set`、逆向きは `kaikei_jp::tags::tag_value_to_string`。**`kaikei-core` は変更していない** |
> | 2 | **通貨の指定方法**（§5） | **PR-B で確定。** 帳簿通貨は `kaikei_app::context::BookSettings::book_currency`（必須・既定値なし）。明細で `currency` を省略したらこれを使う（D-074） |
> | 3 | **金額の出力文字列形式**（§5） | **PR-B（2巡目）で確定。** 区切り無し（`"110000"` / USD `"1234.56"`）。整形は `kaikei_app::amount::money_to_plain_string`。エラー本文の整形済み文字列用に `strip_thousands_separators` も同モジュールにある。**`kaikei-core` は変更していない** |
> | 4 | **`hint`（修正案）**（§3 末尾） | **前工事は PR-B（2巡目）で完了。** dry-run ユースケースは `kaikei_app::usecase::post_entry::preview`。`hint` を応答に載せるのは `kaikei-mcp` を新設する PR |
>
> 1 は当初「`kaikei-mcp` crate が存在しないと書き場所が無い」としてスコープ外に
> していたが、**書き場所は `kaikei-jp` だった**（`TagValueType` を知っているのは
> タグスキーマを読むこの層であり、`kaikei-app` は `kaikei-jp` に依存できない）。
> PR-B 3巡目で確定させた（D-074 訂正注記4）。

### 応答を組み立てるのに使う `kaikei-app` の入口（PR-B 2巡目で確定）

**MCP 層はこの表の外で線上表現を発明しない。** 同じ対応表を `kaikei-mcp` /
`kaikei-api` / `audit_log` の3箇所に手書きすると必ず綴りがずれる（D-072）。

| 線上に出るもの | 入口 |
|---|---|
| エラーの分類コード（`error`） | `AppError::code()` / `RepoError::code()` / `core_error_code` / `policy_error_code`（§6） |
| エラーの本文（`message`） | **`AppError::public_message()`**（下記） |
| 金額（`amount` / `debit_total` …） | `kaikei_app::amount::money_to_plain_string` |
| エラー本文中の整形済み金額 | `kaikei_app::amount::strip_thousands_separators` |
| `side` | `kaikei_app::wire::side_code` / `side_from_code` |
| `account_type` | `kaikei_app::wire::account_type_code` / `account_type_from_code` |
| `policy_notes[].severity` | `kaikei_app::wire::note_severity_code` |
| `fiscal_year_rule` | `kaikei_app::wire::fiscal_year_rule_code` / `fiscal_year_rule_from_code` |
| `entry_id`（出力） | `kaikei_app::id::entry_id_to_uuid_string` |
| `entry_id` / `original_id`（入力） | **`kaikei_app::id::entry_id_from_uuid_string`**（`uuid::Uuid::parse_str` を直に書かない。`kaikei-app` は `uuid` を再エクスポートしていないため、直に書くと下流が自分で `uuid` に依存することになる。D-047 と同型の問題） |
| `tax_mode` / `rounding` / `rounding_unit` | `kaikei_jp::tax::{TaxMode, RoundingUnit}::as_code` / `from_code`、`round_mode_code` / `round_mode_from_code`（`kaikei-app` は `kaikei-jp` に依存できないため、この4つだけ置き場が違う） |
| `lines[].tags`（入力） | `kaikei_jp::tags::TagCatalog::parse_tag_set`（同上。キーごとの `TagValueType` を知るのはタグスキーマを読む層） |
| `lines[].tags`（出力） | `kaikei_jp::tags::tag_value_to_string` |
| 税区分マスタの有効期間 | `kaikei_jp::tax::TaxCategoryTable::range_display` / `TaxRuleSets::available_ranges_display`（§2 の `list_tax_categories`） |

`currency` は `kaikei_app::currency::currency_from_code`（§5）。

これらが**公開 API だけで使えること**は、外部 crate としてリンクされる統合
テスト2本が検証している:
`crates/kaikei-app/tests/contract_from_downstream.rs`（経路 (a)(b)）と
`crates/kaikei-jp/tests/contract_from_downstream.rs`（経路 (c) と `tags` 変換）。

**この2本が検査できないこと**（PR-B 3巡目の訂正）: 統合テストのターゲットには
その crate の `[dependencies]` と `[dev-dependencies]` が両方リンクされるため、
**「下流が `uuid` や `rust_decimal` を自分の `Cargo.toml` に足さずに済むこと」は
検査できない**（プローブでは書けてしまう）。各プローブは自分のソースを読んで
「余計な crate に手を伸ばしていない」ことだけを機械的に見張っている。
本物の検査は PR-D（`kaikei-mcp` 新設）で行う（§4「Phase 3 で新設が必要なもの」）。

### `message` に載せるのは `public_message()`（`Display` ではない）

§9 は「接続文字列・認証情報を含みうる下位層のエラー本文をそのまま転記しない」と
定めている。一方この節は当初「`message` は `Display` を写像したもの」と書いていた。
**この2つは両立しない。** `kaikei-store` の `sqlstate::map_sqlstate` は
`reason: format!("...: {message}")` として **DB が返した文字列をそのまま
`RepoError` に埋めている**（`crates/kaikei-store/src/sqlstate.rs`）。

→ PR-B（2巡目）で `kaikei-app` に**外向きの本文を返す入口**を足した。

| 入口 | 宛先 | 内容 |
|---|---|---|
| `Display`（`to_string()`） | **サーバのログ**（stderr。§4 の「stdout は JSON-RPC 専用」に注意） | 下位層の生メッセージを含む |
| **`AppError::public_message()`** | 応答の `message`、`audit_log.output` | 生メッセージを含まない |

- ドメインのエラー（`Unbalanced` / `UnknownAccount` / `EmptyReverseReason` /
  `AlreadyReversed` …）は `public_message()` も `Display` と同じ値を返す。
  文言はこのリポジトリが書いており、**言い換えない**（`CLAUDE.md` §10）。
- `RepoError::NotFound` / `Unsupported` も `Display` のまま
  （`NotFound` の `reason` は app 層が組み立て、**仕訳IDの UUID 正準表記**を含む。
  ここを潰すと MC-14 の要件が応答から消える）。
- `RepoError::AppendOnlyViolation` / `Conflict` / `OutOfRange` / `Corrupt` /
  `Backend` は**正規化**する（分類コード + 汎用の日本語メッセージ）。
  正規化後も「次の手」は残す（append-only なら「訂正は逆仕訳で」、
  `Backend` なら「入力の問題ではない」ことを明示して再試行・管理者連絡へ誘導）。

`error_code` には引き続き `code()` の値だけを入れる（§9）。

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
- `tags` の**値の型付けは PR-B 3巡目で確定**した。線上は上の例のとおり
  **平文の文字列マップ**（キーも値も文字列）で、型はスキーマ側が持つ。
  当初挙げた3案のうち **(a)** を採った——`kaikei-jp` のローダが `TagSchema` と
  併せてキーごとの `TagDef` を保持する
  （`kaikei_jp::tags::TagCatalog`。`kaikei-core` は無変更。**(b)** は core の
  変更で人間の承認が要り、**(c)** の `{"t":..,"v":..}` は AI に冗長）。

  | 入口 | 用途 |
  |---|---|
  | `TagCatalog::parse_tag_set(iter)` | 線上の `tags`（文字列 → 文字列）→ `kaikei_core::TagSet` |
  | `TagCatalog::parse_value(key, text)` | 1件分（キーの妥当性検証を含む） |
  | `tags::tag_value_to_string(&TagValue)` | 応答に載せる逆向き |
  | `TagCatalog::schema()` | `kaikei-app` / `kaikei-core` に渡す `&TagSchema` |

  合成ルートは `kaikei_jp::compose::compose` が返す `Composition::tag_catalog`
  を保持する（§4。`TagSchema` は `tag_catalog.schema()` で取る）。

  値の文字列表現は **D-035 の JSONB 表現の `"v"` と同じ規約**にしてある
  （線上とDBで別の書き方を発明しない）:

  | `value_type` | 線上の文字列 | 例 |
  |---|---|---|
  | `Code` | そのまま | `"tax_category": "SALES_10"` |
  | `Text` | そのまま | `"invoice_reg_no": "T1234567890123"` |
  | `Decimal` | 小数の文字列（number にしない。D-013） | `"business_ratio": "0.30"` |
  | `Date` | ISO 8601（`YYYY-MM-DD`） | `"delivered_on": "2026-04-15"` |

  **未登録キー・型不一致・空値はエラー**にする
  （`CLAUDE.md` §4「`TagSet` はゴミ箱ではない」。黙って落とさない）。
  エラーは有効なキー一覧や期待する書式を含む（同 §11）:
  `unregistered_tag_key` 相当（`JpError::UnregisteredTagKey`）/
  `JpError::InvalidTagValue`。
  経路 (c) と同じく `JpError` なので、MCP 層の対応表に分類コードを足すこと
  （D-072 のトレードオフの項）。
  `Decimal` は丸めずに解釈する（表現できない桁数は受け付けない）。

  > **★MCP 経由では `tags` の重複キーを検出できない（後勝ちになる）★**
  >
  > `{"tax_category": "SALES_10", "tax_category": "SALES_8_REDUCED"}` のように
  > 同じキーを2回指定した場合、**後の指定が黙って採用される**
  > （上の例では税率が無言で入れ替わる）。`audit_log.input` にも
  > 畳み込まれた後の値しか残らない。
  >
  > 理由は経路にある。`rmcp` の `CallToolRequestParams::arguments` は
  > `Option<JsonObject>`（＝ `serde_json::Map`）であり、**JSON-RPC メッセージ
  > 全体が `serde_json` でパースされる時点**——つまり `dispatch::call` に
  > 入るより前——で重複キーは畳み込まれている。ツール側の受け型を工夫しても
  > 検出できない（生の JSON テキストが MCP 層に届かない。持ち込むには
  > `rmcp` の stdio トランスポートを自前に置き換えることになり、得るものに
  > 対して代償が大きい）。
  >
  > したがって **`JpError::DuplicateTagKeyInInput` は MCP 経由では到達不能**
  > である。この判定自体は `TagCatalog::parse_tag_set` に残っており、
  > 生の入力を自分でパースする他の呼び出し元（将来の CLI / `kaikei-api`）
  > からは到達する。
  >
  > PR-F の初版はここで「入力内の重複キーはエラーにする」と書き、
  > `kaikei-mcp` に `TagPairs`（出現順のペアを保持する手書き `Deserialize`）を
  > 置いていたが、**本番経路では一度も効いていなかった**（単体テストが
  > 本番と違う入口を通していたため緑だった）。効かない機構を残すと
  > 次の実装者が「重複は防がれている」と誤解するので、型ごと削除した
  > （D-085 の訂正注記）。`inputSchema` の `tags` の説明文にも
  > 「同じキーを2回指定した場合、後に書いた指定だけが使われます」と書いてある。

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

- `entry_id` は **UUID の正準表記**（ハイフン付き36文字・小文字。生成は UUID v7）。
  `reverse_journal_entry` の `original_id` も同じ表記。

  **PR-B で解決済み。** `reverse_journal_entry` の NotFound は
  `input.original_id.as_u128()`（39桁になりうる10進数）を組み立てていたが、
  `kaikei_app::id::entry_id_to_uuid_string` を経由する形に直した
  （`kaikei-core` は変更していない。`kaikei-app` が既に `uuid` に依存しており、
  `entry_id_to_uuid` で `Uuid` に変換できるため）。
  仕訳IDを人間・AI に見せる箇所は**必ずこの関数を通す**こと
  （表記を各所で再発明しない）。回帰テストは
  `reverse_entry_not_found_reports_the_id_in_canonical_uuid_form`。
- `entry_no` / `fiscal_year` は JSON number（金額ではないため文字列にしない。§5）。
- `policy_notes` は `kaikei_policy::PolicyNote`（`severity`: `info` / `warning`、`message`）。
  非適格の経過措置や簡易課税のように**税額計算に反映されず注記にしか現れない情報**の
  唯一の伝達経路であり、落とすと AI も監査ログも「控除割合の制限があった」ことを
  知る手段が無くなる（D-059 / D-070）。文言は `kaikei-policy` が組み立てたものを
  そのまま素通しする（税務判断を断定する言い換えをしない。`CLAUDE.md` §10）。
- **PR-B で実装済み**: `post_entry::execute` の戻り値は
  `PostEntryOutput { entry: JournalEntry, notes: Vec<PolicyNote> }` になった
  （Phase 2 の申し送り「`PolicyNote` が永続化されない」への回答。D-073）。
  MCP 層は `output.notes` をそのまま `policy_notes` に詰め、`audit_log.output`
  にも同じものを載せる。
  **`policy_notes` が空配列でも「注記が無い」とは限らない。**
  `auto_tax_lines: false` のときは `derive_tax_lines` 自体を呼ばないため常に空になる
  （`validate_tag` は注記を返さない）。リクエストの `auto_tax_lines` と併せて読むこと。

**確定後の明細を必ず返す。** AI が「何が記録されたか」を確認できるようにする。

失敗時（ツール結果エラー。`isError: true` の本文としてこの JSON を返す。
JSON-RPC のエラー応答は使わない。D-071）：

```json
{
  "error": "unbalanced",
  "message": "貸借不一致: 借方 110,000 / 貸方 100,000（差額 10,000）",
  "debit_total": "110000",
  "credit_total": "100000",
  "difference": "10000",
  "policy_notes": [
    { "severity": "info",
      "message": "税込経理の設定のため税額行を生成していません（tax_mode=inclusive）。消費税額は本体価額に含めて記帳する設定であり、auto_tax_lines を true にしても明細は増えません" }
  ]
}
```

上の `policy_notes` は `auto_tax_lines: true` かつ税込経理の設定のときの実物である
（生成経路は `crates/kaikei-jp/src/tax/policy.rs` の `derive_tax_lines`。
**PR-F レビューで追加した**——それまでこの文言は設計書にしか存在せず、
「生成経路の無い文言を確定仕様として書かない」という本節の規律自身に反していた）。
`auto_tax_lines: false` の場合は `policy_notes` が空になるので、
代わりに `hint.policy_notes` に同じ注記が入る（下記の `hint` の項）。

- `error` はエラーコード（§6 の対応表）。`message` は
  **`AppError::public_message()`**（上記「`message` に載せるのは
  `public_message()`」を参照。`Display` をそのまま載せない）。
- **現在の実装が返せるのはここまで。** `CoreError::Unbalanced` が持つのは
  `debit` / `credit` / `diff` の3つの表示用文字列だけで、
  「`tax_category` が `SALES_10` の明細に対する税額行がありません」のような
  税区分に踏み込んだ文言を組み立てる経路は存在しない。
  生成経路の無い文言を確定仕様として書かないこと。
- 金額欄は**区切り無し**（`"110000"`）。`CoreError::Unbalanced` が持つのは
  `Money::to_display_string()` 由来の区切り付き文字列（`"110,000"`）なので、
  `kaikei_app::amount::strip_thousands_separators` を通す（§5）。
  区切り付きの表記が残るのは `message` の**文中**だけである。
- **`policy_notes` は失敗時にも出す（PR-B 2巡目）。**
  `post_entry::execute` / `preview` の失敗値は
  `PostEntryFailure { error, notes }` で、`notes` は失敗経路でも運ばれる。
  注記が最も要るのは失敗したときである——上の例の
  「税込経理の設定のため税額行を生成していません」が無いと、AI は
  「貸借不一致」だけを見て**金額を書き換える**という誤った修正に進む
  （§1③ が空文になる）。
  成功時と同じく、`auto_tax_lines: false` なら常に空になるので、
  空配列は「注記が無い」を意味しない。**そのケースでは `hint.policy_notes`
  に同じ注記が入る**（下記）。

`hint`（修正案）は **PR-F で実装した**
（`crates/kaikei-mcp/src/tools/post_journal_entry.rs`。前工事の dry-run
ユースケースは PR-B 2巡目で完了していた）。実物:

```json
{
  "error": "unbalanced",
  "message": "貸借不一致: 借方 110,000 / 貸方 100,000（差額 10,000）",
  "debit_total": "110000",
  "credit_total": "100000",
  "difference": "10000",
  "policy_notes": [],
  "hint": {
    "message": "auto_tax_lines を true にして同じ明細を渡すと、下の suggested_lines の内容で貸借が一致します。…どちらにするかの判断はこのサーバーでは行いません",
    "suggested_lines": [
      { "account": "135", "side": "debit",  "amount": "110000", "currency": "JPY", "tags": {} },
      { "account": "500", "side": "credit", "amount": "100000", "currency": "JPY", "tags": { "tax_category": "SALES_10" } },
      { "account": "330", "side": "credit", "amount": "10000",  "currency": "JPY", "tags": { "tax_category": "SALES_10" } }
    ],
    "debit_total": "110000",
    "credit_total": "110000",
    "policy_notes": []
  }
}
```

AI の自己修正を一段速くする効果は大きいが、
「税区分 → 税額科目」の対応を知るのは `kaikei-jp` の `TaxCategoryTable` だけで、
`TaxPolicy` trait には引く API が無く（`validate_tag` / `derive_tax_lines` /
`round_mode` / `apply_ratio` の4つのみ）、MCP 層でこの推論を書くのは
「MCP はユースケースを呼ぶ薄い層」（§4）と `CLAUDE.md` §1 に反する。
実現形は次のとおり:

- `auto_tax_lines: false` で貸借不一致になった場合に限り、同じ明細を
  `auto_tax_lines: true` にして
  **`kaikei_app::usecase::post_entry::preview`**（dry-run）を呼ぶ。
  `Ok` なら `PreviewEntryOutput::lines` を `hint.suggested_lines` に載せる。
  `Err` でも、**`PostEntryFailure::notes` が空でなければ `hint.policy_notes`
  として返す**（PR-F レビュー C-1）。税込経理・免税事業者の設定では
  `derive_tax_lines` が税額行を生成しないので `preview` も同じ貸借不一致で
  失敗するが、**その理由は注記に入っている**。これを渡さないと
  `policy_notes: []` と差額だけが AI に届き、「貸借不一致」を見て
  **金額を書き換える**という誤った修正に進む（§1 ③ が空文になる）。
  注記も無ければ `hint` を返さない（税額行を足しても解決せず、
  述べられる事実も無いため）。
  **どの設定なら税額行が生成されないかの判定を MCP 層に書かないこと**
  （それを知っているのは `kaikei-jp` であり、MCP 層は注記を運ぶだけである。
  D-072）。
  `preview` は **`insert_entry` も採番も行わない**が、`execute` と
  **同じ関数**（`prepare` / `build_entry`）を通るので検証の順序が乖離しない。
  **MCP 層で `with_tx` を開いて `load_posting_context` を呼び `TaxContext` を
  自前で組み立てるコードを書かないこと**（§4 違反。1巡目の消費側が
  実際にそう書けてしまい、コンパイルもテストも通った）。
- 存在しない科目コードに対する `hint`（候補の科目）は MCP 層が `ChartOfAccounts` から
  組み立てる。`CoreError::UnknownAccount` のメッセージは
  「勘定科目が見つかりません: {code}」だけであり、**core に候補一覧を持たせない**
  （core の変更は人間の承認事項。`CLAUDE.md` §1・§9）。候補は全件ではなく
  絞って返し、件数の上限を決める。
  **PR-F で実装**（`similar_accounts`。科目コードの**前方一致の長さ**が長い順、
  記帳可能（`postable`）な科目のみ、上限5件。1文字も一致しない場合は
  `hint` ごと返さない——無関係な科目を並べても次の手にならない）。
- **明細のエラーは「何行目か」を添える。** 下位層の文言は言い換えず、
  `明細 2 行目: ` の接頭辞と `line: 2` の欄を足すだけにする
  （`CLAUDE.md` §10 / §11）。

### reverse_journal_entry

```json
{
  "original_id": "0192a7b3-xxxx-7xxx-xxxx-xxxxxxxxxxxx",
  "reverse_date": "2026-05-01",
  "reason": "請求金額の誤り（税率の適用誤り）",
  "allow_double_reversal": false
}
```

- `reason` は必須。**空文字・空白のみは `kaikei-app` のユースケース層が拒否する**
  （PR-B。`reverse_entry::execute` が I/O より前に `AppError::EmptyReverseReason` を返す。
  エラーコードは `empty_reverse_reason`）。**MCP 層に検証を重ねて書かない。**
  MCP 以外の呼び出し元（将来の CLI / `kaikei-api`）にも同じ規律が効く位置に置いた
  （D-074）。
  ユースケース層より下は依然として空文字を通す:
  `JournalEntry::reverse` は `reverse_reason = Some(reason)` と代入するだけ、
  DB の `CHECK ((reverses IS NULL) = (reverse_reason IS NULL))` は NULL の一致しか見ない
  （摘要には `CHECK (btrim(description) <> '')` があるのに `reverse_reason` には無い、
  という非対称が実在する）。
  判定は `str::trim` の結果で行うため、全角スペース（U+3000）のみの理由も拒否される。
  **受理した理由は加工せず、入力のまま保存する**（前後の空白をトリムして保存しない。
  帳簿に残る文言を断りなく書き換えない）。
- `allow_double_reversal` の既定は `false`。既に赤伝済みの仕訳を再訂正しようとすると
  `AppError::AlreadyReversed { entry_no, reversal_no, reversal_id }` になる。
  **`reversal_id`（既存赤伝の仕訳ID）を PR-B 2巡目で追加した。**
  呼び出し元が仕訳を指すのは UUID であって通し番号ではなく、番号だけ返しても
  AI はその赤伝を `get_entry` で開けない（`JournalRepo` に番号から引く経路が無い）。
  応答では `entry_id_to_uuid_string` を通した UUID 正準表記で返すこと。
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

- **`policy_notes` は返さない（キーごと出さない）。** `reverse_entry::execute` は
  `TaxPolicy` を引数に取らず（明細は貸借を反転して複製するだけで、税額行を
  再導出すると二重計上になる）、注記の発生経路が存在しない。
  戻り値の型も `ReverseEntryOutput { entry }` で `notes` を持たない（D-073）。
  ここに `"policy_notes": []` を置くと「policy を通したが注記が無かった」と
  区別できず、AI を誤った方向へ導く（`PROGRESS.md` Phase 1 の教訓3）。

失敗時（二重訂正。ツール結果エラー）：

```json
{
  "error": "already_reversed",
  "message": "仕訳 42 は既に取消（逆仕訳 43、仕訳ID 0192b1c4-xxxx-7xxx-xxxx-xxxxxxxxxxxx）済みです。その赤伝の内容は仕訳ID 0192b1c4-xxxx-7xxx-xxxx-xxxxxxxxxxxx で確認できます。それでも二重取消が必要な場合は allow_double_reversal を指定してください",
  "reversal_id": "0192b1c4-xxxx-7xxx-xxxx-xxxxxxxxxxxx",
  "reversal_no": 43
}
```

### get_trial_balance

```json
{
  "from": "2026-01-01",
  "to": "2026-12-31",
  "group_by": ["counterparty"]
}
```

- `from` / `to` は **取引日**（`AccountingDate`）で**両端を含む**。どちらも必須。
  `from > to` は空の試算表ではなく**エラー**（`error` は `rejected`）。
  「0件の空の試算表」として静かに成功させない（§2 / MC-15）。
- `group_by` は省略可（既定は空 ＝ 科目のみで集計）。
  `aggregatable: true` のタグキーだけを受け付ける（同梱スキーマでは
  `counterparty` / `project` / `tax_category` の3つ）。
  重複したキーは `kaikei-app` が出現順を保ったまま除去する。
- **拒否は2種類あり、別のコードで返す**（PR-G レビュー C-2）:

  | 指定したキー | `error` | 出所 |
  |---|---|---|
  | 帳簿に**登録されていない**（`tags.yaml` に無い） | `unknown_tag_key` | MCP 層（`TagCatalog::def` が `None`） |
  | 登録済みだが `aggregatable: false` | `not_aggregatable` | `kaikei-app`（**SQL に到達する前に**弾く。`CLAUDE.md` §4） |

  どちらの応答にも **`aggregatable_group_by_keys`**（選べるキーの一覧）が付き、
  `DESCRIPTION` にも同じキーが列挙してある。

  > **★なぜ MCP 層で分けるのか★**
  >
  > `TagSchema::is_aggregatable` は**未登録のキーにも `false` を返す**ので、
  > 素直に流すと未登録のキーに対して
  > 「集計軸に使えないタグキーです: memo（**aggregatable = false**）」という
  > **成立していない事実**（`aggregatable` の宣言自体が存在しない）が返る。
  > しかも Phase 3 の11ツールには**有効なタグキーを一覧できるツールが無い**
  > ので、AI はこのエラーを踏んだあと正しいキーに辿り着けない（§11 違反）。
  >
  > 発生源（`kaikei-core` の `CoreError::NotAggregatable`）は凍結層なので
  > 触らない。MCP 層が見るのは**登録の有無だけ**であり、集計軸として妥当かは
  > 従来どおり `report::execute` が決める（同じ検証を2箇所に持たない）。
  > core 側のメッセージ改善（未登録キーと非集計キーを区別する）は別 Issue。

  実サーバの応答（**どちらも実物**）:

  ```json
  { "error": "unknown_tag_key",
    "message": "group_by: タグキー \"memo\" はこの帳簿に登録されていません（登録されているキー: business_ratio, counterparty, imported_tx_id, invoice_reg_no, project, tax_category）。集計軸に指定できるのは、そのうち集計軸として宣言されているキーだけです: counterparty, project, tax_category",
    "aggregatable_group_by_keys": ["counterparty", "project", "tax_category"] }
  ```

  ```json
  { "error": "not_aggregatable",
    "message": "group_by: 集計軸に使えないタグキーです: business_ratio（aggregatable = false）",
    "aggregatable_group_by_keys": ["counterparty", "project", "tax_category"] }
  ```

成功時：

```json
{
  "from": "2026-01-01",
  "to": "2026-12-31",
  "currency": "JPY",
  "debit_total": "1100",
  "credit_total": "1100",
  "rows": [
    {
      "account": "100",
      "account_type": "asset",
      "group": { "counterparty": "CP0001" },
      "debit_total": "1100",
      "credit_total": "0",
      "balance": "1100"
    },
    {
      "account": "500",
      "account_type": "revenue",
      "group": { "counterparty": "CP0001" },
      "debit_total": "0",
      "credit_total": "1000",
      "balance": "1000"
    },
    {
      "account": "330",
      "account_type": "liability",
      "group": { "counterparty": "CP0001" },
      "debit_total": "0",
      "credit_total": "100",
      "balance": "100"
    }
  ]
}
```

写像元は `kaikei_app::view::TrialBalanceView`（`usecase::report::execute` の戻り値）。

- `rows[]` は `BalanceRowView`。`account` は `AccountCode`、`account_type` は
  **`kaikei_app::wire::account_type_code`**（`asset` / `liability` / `equity` /
  `revenue` / `expense`）。`label_ja()` の「資産」等は**分岐に使わせない**ので
  ここには出さない（人間向けに併記したければ別フィールドにする）。
- `group` は `group_by` を指定したときだけ中身が入る（指定しなければ `{}`）。
  キーはタグキー、値はタグ値の文字列。
- 金額はすべて**区切り無しの文字列**（`money_to_plain_string`）。
  `balance` は `account_type.is_debit_normal()` に従った符号付き残高で、
  **負にもなりうる**（`"-1100"`）。
- `debit_total` / `credit_total`（トップレベル）は `TrialBalanceView::totals()`。
  `kaikei-app` が検算済みで、一致しない場合は成功応答ではなく
  `inconsistent` のエラーになる（データ破損・実装バグの兆候）。
- **`currency` は 0 行でも必ず入る（PR-B 2巡目）。** `TrialBalanceView` は
  帳簿通貨（`BookSettings::book_currency`）を明示的に保持しており、
  `TrialBalanceView::currency()` から取れる。行から推論していないので、
  仕訳が1件も無い期間でも通貨を名乗れ、`debit_total` / `credit_total` は
  `"0"` になる（`totals()` は `Option` を返さない）。
  行の通貨が帳簿通貨と食い違えば `currency_mismatch` のエラーになる（D-042）。
- **0件は成功。** 該当する仕訳が無い期間は `rows: []` を返し、エラーにしない
  （エラーにするのは `from > to` の入力ミスだけ）。

実サーバの応答（`from > to` と 0 行の期間。**どちらも実物**）:

```json
{ "error": "rejected",
  "message": "集計期間の開始日が終了日より後です: from=2026-12-31 to=2026-01-01。from と to を入れ替えるか、正しい期間を指定してください" }
```

```json
{ "from": "2020-01-01", "to": "2020-12-31", "currency": "JPY",
  "debit_total": "0", "credit_total": "0", "rows": [] }
```

### list_accounts（PR-G）

```json
{ "postable_only": true }
```

- 引数は `postable_only`（既定 `false`）だけ。`true` なら**記帳に使える科目**
  （`postable: true`）に絞る。絞ったことは応答にも残す
  （件数が少ないのが帳簿の状態なのか絞り込みの結果なのかを、応答だけで
  判断できるようにする）。
- 読むのは **DB の `accounts`**（`ChartRepo::load_chart`）であり、
  埋め込みテンプレートではない（§4）。並びは科目コード昇順。
  表示順（`sort`）は返さない（D-061）。

実サーバの応答（同梱テンプレート投入直後。56件のうち先頭2件）:

```json
{
  "count": 56,
  "postable_only": true,
  "accounts": [
    { "account": "100", "name": "現金", "account_type": "asset", "postable": true },
    { "account": "110", "name": "普通預金", "account_type": "asset", "postable": true }
  ]
}
```

`parent` は親科目がある科目にだけ現れる（`null` を置かない）。
**科目が0件でも成功**（`accounts: []`）であり、「見つからない」ではない。

### get_entry（PR-G）

```json
{ "entry_id": "019fc7c8-21e0-7600-8246-0a29b67d371d" }
```

実サーバの応答（`auto_tax_lines: true` で起こした仕訳。税額行を含む3行）:

```json
{
  "entry_id": "019fc7c8-21e0-7600-8246-0a29b67d371d",
  "entry_no": 1,
  "fiscal_year": 2026,
  "entry_date": "2026-04-15",
  "description": "A社への請求",
  "lines": [
    { "account": "135", "side": "debit",  "amount": "110000", "currency": "JPY", "tags": {} },
    { "account": "500", "side": "credit", "amount": "100000", "currency": "JPY",
      "tags": { "tax_category": "SALES_10" } },
    { "account": "330", "side": "credit", "amount": "10000",  "currency": "JPY",
      "tags": { "tax_category": "SALES_10" } }
  ],
  "debit_total": "110000",
  "credit_total": "110000"
}
```

- `reverses` / `reverse_reason` は**逆仕訳のときだけ**現れる。`null` を置くと
  「逆仕訳ではない」と「訂正理由が空」の区別が応答から消える
  （`lines[].memo` と同じ扱い。`PROGRESS.md` Phase 1 の教訓3）。
- **`reversed_by` / `reversed_by_entry_no` は、その仕訳が既に赤伝で訂正されて
  いるときだけ現れる**（PR-G レビュー B）。上の実サーバの応答を訂正した後に
  同じ `entry_id` で引くと、次の2キーが増える:

  ```json
  { "reversed_by": "019fc7c8-3a11-7600-8246-0a29b67d371d",
    "reversed_by_entry_no": 2 }
  ```

  `reverses` とは**向きが逆**である（`reverses` は「この仕訳が誰を訂正して
  いるか」、`reversed_by` は「この仕訳を誰が訂正したか」）。

  > **★これが無いと訂正済みかどうかが応答から消える★**
  >
  > `JournalEntry`（集約）が持つのは `reverses` / `reverse_reason`——
  > **逆仕訳 → 原仕訳**の向きだけである。初版はそれをそのまま返していたので、
  > 訂正前と訂正後の `get_entry` は**全キーが完全に一致した**。
  > `get_trial_balance` では残高が0になっているのに、`get_entry` では
  > 生きているように見える。しかも同じ応答の説明文が
  > 「訂正は `reverse_journal_entry` で」と誘導するため、AI は訂正済みの
  > 仕訳をもう一度訂正しようとして `already_reversed` を踏む（§11 違反）。
  > **訂正履歴は本プロジェクトの存在意義**（`CLAUDE.md` §2）である。
  >
  > 実装は read model の新設ではなく、
  > `JournalRepo::find_reversal_of`（`reverse_entry::execute` が二重訂正の
  > 検出に既に使っているポート）を**同じトランザクションの中で**もう1回
  > 呼ぶだけである。新しい SQL も `.sqlx` の更新も要らず、
  > `reverse_journal_entry` が二重訂正を拒否する判断と同じ述語を見る。
- 存在しない仕訳IDは**空の成功にしない**（MC-14）。実サーバの応答:

  ```json
  { "error": "not_found",
    "message": "見つかりません: 指定された仕訳（仕訳ID: 0192a7b3-1234-7abc-8def-0123456789ab）。仕訳IDが正しいか確認してください" }
  ```

  文言は `reverse_journal_entry` が組み立てるものと同一（**仕訳IDは UUID の
  正準表記**）。「UUID ですらない文字列」は `invalid_entry_id` であり、
  `not_found` と区別する（§6。AI が取るべき次の手が違う）。
  `RepoError::NotFound` の `Display` が既に `"見つかりません: {reason}"` なので、
  **`reason` 側に「見つかりません」を書かない**（PR-G レビュー D-2。以前は
  「見つかりません: 仕訳が見つかりません（…）」と同じことを2回言っていた）。
- **`recorded_at`（記帳時刻）は返していない。** `Timestamp` を線上の表記に
  する入口が `kaikei-app` に無く、MCP 層で組み立てると `chrono` を名指しする
  ことになる（MC-30 の許可リストに無い）。必要になったら
  `kaikei-app` 側に入口を足すこと（`entry_id_to_uuid_string` と同じ形）。

### list_tax_categories（PR-G）

```json
{ "date": "2026-04-15" }
```

実サーバの応答（9件のうち先頭1件）:

```json
{
  "date": "2026-04-15",
  "table": {
    "label": "kaikei-jp-data/tax/jp/2026.yaml",
    "applies_from": "2026-01-01",
    "range": "2026-01-01 〜 無期限"
  },
  "count": 9,
  "categories": [
    { "code": "SALES_10", "label": "課税売上 10%", "direction": "sales",
      "rate": "0.10", "requires_qualified_invoice": false, "tax_account": "330" }
  ]
}
```

- **どのマスタを見た結果かを必ず返す**（`table`）。取引日で切り替わる以上、
  「いつ時点の一覧か」が応答から読めないと、AI は別の年度の区分を使い回す。
- `rate` / `deduction_ratio` は**文字列**（number にしない。§5 と同じ理由）。
  `applies_to` / `rate` / `deductible` / `tax_account` / `note` は値が無ければ
  **キーごと出さない**（`deductible: null` を「控除できない」と読ませない）。
- `direction` は `kaikei_jp::tax::TaxDirection::as_code`（`sales` /
  `purchase` / `none`）。**この綴りを MCP 層で作らない**（PR-G で
  `kaikei-jp` に `as_code` / `from_code` / `CODES` を足した。D-087）。
- 同梱していない日付は**空配列ではなくエラー**（実物）:

  ```json
  { "error": "no_applicable_rule_set",
    "message": "取引日 2000-01-01 に適用される消費税区分マスタがありません（読み込まれているマスタの適用期間: 2026-01-01 〜 無期限）。取引日を確認してください" }
  ```

  この文言は `TaxRuleSets::require_for_date` が組み立てる。**MCP 層で
  書き起こさない**（D-072）。

### get_settings（PR-G）

引数は無い（`{}`。日付も取らない。D-057）。実サーバの応答:

```json
{
  "tax_mode": "exclusive",
  "rounding": "floor",
  "rounding_unit": "line",
  "is_taxable_business": true,
  "simplified_taxation": false,
  "fiscal_year_rule": "calendar_year",
  "book_currency": { "code": "JPY", "minor_unit": 0 },
  "chart_differences": { "as_of": "startup", "items": [] }
}
```

- 機械可読名はすべて `kaikei_jp::tax` / `kaikei_app::wire` の入口を通す。
- `book_currency` は**コードと小数桁数の組**で返す（桁数を1つ間違えると
  金額が100倍ずれる。`CLAUDE.md` §8）。
- **`chart_differences`**（PR-E からの申し送り。§7）: 起動時の科目投入で
  テンプレートと定義が食い違い、**既存を残した**科目
  （`ImportChartOutput::kept_existing`。D-081）。`items` の要素の形は

  ```json
  { "account": "500", "fields": ["name"],
    "in_use":   { "account": "500", "name": "売上（本業）", "account_type": "revenue", "postable": true },
    "template": { "account": "500", "name": "売上高",       "account_type": "revenue", "postable": true },
    "message": "科目 500 の定義が既存と異なります（相違: name）。既存の定義を残し、テンプレートでは上書きしていません。…" }
  ```

  `message` は `ChartDifference::describe()` をそのまま使う（起動時に stderr へ
  出しているものと同一。言い換えると説明が2つに育つ）。
  食い違いが無ければ `items` は空配列（**キーは必ず出す**）。
- **`chart_differences.as_of` は `"startup"` 固定である**（PR-G レビュー C-3）。
  `list_accounts` が**毎回 DB の `accounts` を読む**のに対し、これは
  `startup::assemble` が**起動時に一度だけ**採取した `Vec` である。
  稼働中に `accounts` が編集されると両者は食い違う（`accounts` は帳簿本体と
  違い append-only ではない）。裸の配列で返すと「毎回 DB を見た結果」と
  区別が付かないので、**いつ時点の観測かを応答に残す**。
  0行の試算表でも `currency` を名乗る（D-074）／`suggest_tax_category` が
  `filtered_by` を必ず返す、と同じ規律である。

### suggest_tax_category（PR-G）

```json
{ "date": "2026-04-15", "direction": "sales", "description": "A社へのコンサル料" }
```

実サーバの応答（3件のうち先頭1件）:

```json
{
  "date": "2026-04-15",
  "table": { "label": "kaikei-jp-data/tax/jp/2026.yaml",
             "applies_from": "2026-01-01", "range": "2026-01-01 〜 無期限" },
  "filtered_by": { "direction": "sales", "description_used_for_filtering": false,
                   "tax_mode": "exclusive", "is_taxable_business": true,
                   "simplified_taxation": false,
                   "book_settings_used_for_filtering": false },
  "description": "A社へのコンサル料",
  "count": 3,
  "candidates": [
    { "code": "SALES_10", "label": "課税売上 10%", "direction": "sales",
      "rate": "0.10", "requires_qualified_invoice": false, "tax_account": "330",
      "reason": "2026-04-15 時点で有効なマスタ「kaikei-jp-data/tax/jp/2026.yaml」（2026-01-01 〜 無期限）に、売上側の区分「課税売上 10%」として登録されています。税率は 0.10 です" }
  ],
  "disclaimer": "候補と根拠のみを返しています。どの区分を使うかの判断はこのサーバーでは行いません。候補は、指定された取引日の時点で有効な消費税区分マスタに登録されている区分です。取引内容の文面からの推論は行っていません。この帳簿の設定（filtered_by の tax_mode / is_taxable_business / simplified_taxation）でも候補を絞っていません。帳簿の設定によっては、候補の区分で記帳しても税額の行が生成されないことがあります。"
}
```

**★断定しない形を仕様として固定する★**（`CLAUDE.md` §10 / D-087）:

- **1件に絞らない。** 順位・信頼度・推奨に相当するキー（`recommended` /
  `confidence` / `score` / `rank` / `selected` / `best`）を応答に置かない
  （`crates/kaikei-mcp/src/tools/suggest_tax_category.rs` の
  `the_response_does_not_single_out_one_category_or_rank_them` が
  キー名を機械的に検査する）。
- **`reason` はマスタに書かれている事実だけ**で構成する（適用期間・向き・
  税率・適格請求書の要否・注記）。推論の説明ではないので、断定にも
  言い換えにもならない。注記はマスタの文言をそのまま運ぶ。
- **`description` は絞り込みに使わない。** 受け取ってそのまま echo し、
  `filtered_by.description_used_for_filtering: false` と `disclaimer` で
  使っていないことを明示する。摘要の語から税区分を決める規則は
  このリポジトリのどの層にも無く（仕訳化ルールは Phase 4 の
  `kaikei-import`）、無い規則を MCP 層で発明しないためである。
  回帰検知は `the_description_is_echoed_but_never_used_to_filter`
  （摘要を付けても候補が変わらないことを見る）。
- **`filtered_by` に帳簿の設定を並べる**（PR-G レビュー C-1）。
  初版は免税事業者（`is_taxable_business: false`）や簡易課税の帳簿でも
  「税率は 0.10 です」と述べ `tax_account` を返し、応答が課税事業者の帳簿と
  **完全に同一**だった。その区分で実際に記帳しても**税額行は1行も生成
  されない**のに、`disclaimer` が否定していたのは「文面からの推論」だけで、
  「帳簿の設定は考慮していない」とはどこにも書いていなかった。
  §10 の「断定しない」（1件に絞らない・順位を付けない・摘要から推論しない）は
  満たしていたが、**根拠が不完全なまま税率だけが目立つ**状態だった。
  そこで `tax_mode` / `is_taxable_business` / `simplified_taxation`
  （`get_settings` が返すのと同じ値）を並べ、`disclaimer` に
  「帳簿の設定によっては税額行が生成されないことがある」を足す。
  **候補は絞らない**（どの区分を使うかの判断はこのサーバーが行わない）ので、
  これは業務判断ではなく**事実の提示**である（D-072 に反しない）。
  回帰検知は `the_book_settings_that_decide_whether_tax_lines_appear_are_reported_back`
  （免税事業者の帳簿で `filtered_by` が変わり、`candidates` は変わらないことを
  同時に見る）。
- 帳簿は一切変更しない（MC-08 の (2)。`Tx` を開かず DB にも触らない）。

### validate_invoice_number（PR-G）

```json
{ "registration_number": "T7123456789012" }
```

実サーバの応答:

```json
{
  "format_valid": true,
  "registration_number": "T7123456789012",
  "corporate_number": "7123456789012",
  "checked": [
    "先頭が T であること",
    "T に続く部分が13文字であること",
    "13文字がすべて半角数字であること",
    "先頭1桁の検査用数字が残り12桁から計算した値と一致すること"
  ],
  "not_checked": [
    "その番号が国税庁に実在登録されているかどうか",
    "登録されている事業者が現時点で適格請求書発行事業者として有効かどうか",
    "その事業者名・所在地がこの取引の相手方と一致するかどうか"
  ],
  "message": "形式（先頭の T・桁数・文字種・チェックデジット）は確認しました。この番号が実在するか、適格請求書発行事業者として有効かどうかは確認していません。取引先が適格請求書発行事業者かどうかの判断はこのサーバーでは行いません"
}
```

- **キー名を `valid` にしない**（「登録番号として有効」と読める）。
  確認したのは形式だけなので `format_valid` である。
- **`checked` と `not_checked` を必ず並べる**（MC-28。§10 の「税務判断を
  断定しない」はこの並記で担保する）。
- 形式が不正なら**最初に失敗した観点だけ**を返す（D-053）。前後の空白は
  トリムしない（D-052）ので、貼り付け由来の空白はここで見つかる（実物）:

  ```json
  { "error": "invoice_reg_no_missing_prefix",
    "message": "インボイス登録番号は先頭が \"T\"（大文字）である必要があります: \" T7123456789012\"" }
  ```

  4つの観点が別々の分類コードを持つ理由は §6。

### search_entries

```json
{
  "from": "2026-01-01",
  "to": "2026-12-31",
  "account": "600",
  "description": "A社",
  "min_amount": "1000",
  "max_amount": "5000",
  "tags": { "counterparty": "CP0001" },
  "limit": 20,
  "cursor": "2026-04-15:1:019fc7cd-5b9d-75b1-95e6-c9814c075a53"
}
```

**すべて省略可**（`{}` は全件検索）。複数指定した場合は**すべてを満たす**仕訳だけが返る。

- `from` / `to` は**取引日**（`AccountingDate`）で**両端を含む**。片側だけの指定も、
  両方の省略もできる（`get_ledger` と違い期間は必須ではない。理由は
  `crates/kaikei-app/src/usecase/ledger.rs` の doc）。
  `from > to` は空の結果ではなく**エラー**（`rejected`）。
- `account` はその科目の明細を**含む**仕訳に絞る（明細だけを返すわけではない）。
  **勘定科目マスタに無い科目コードは `not_found` のエラー**（0件の成功にしない。
  `get_ledger` と同じ規律。D-088 決定3）。実在する科目に該当が無いだけなら
  0件の成功である。打ち間違いは `list_accounts` でコードを調べ直す、
  0件は期間や条件を広げる——**次の手が違う**。
- `description` は摘要の**部分一致**（英字の大小を無視する）。検索語に含まれる
  `%` / `_` / `\` は**ワイルドカードとして効かない**（エスケープする。効かせると
  「多すぎる結果」が正しい結果として返る）。**空文字は拒否**する（全件一致に化けるため）。
- `min_amount` / `max_amount` は**明細1行**の金額と比較する（仕訳の合計ではない）。
  **文字列**で指定する（§5）。帳簿通貨建てとして解釈し、通貨が一致する明細だけを見る。
- `tags` はキーも値も文字列。**`aggregatable: true` のタグキーだけ**が指定できる
  （`get_trial_balance` の `group_by` と同じ規則。それ以外は `not_aggregatable`、
  未登録キーは `unknown_tag_key`。D-088）。判定は仕訳単位で、
  「キーごとにいずれかの明細が一致すればよい」（同じ1行が全部のタグを持つ必要はない）。
- `limit` は既定 20・上限 100（`kaikei_app::usecase::search_entries::{DEFAULT_LIMIT, MAX_LIMIT}`）。
  **上限を超える値は丸めずにエラー**（`rejected`）。
- `cursor` は直前の応答の `next_cursor` を**そのまま**渡す。壊れた値は
  「先頭から」にフォールバックせず `rejected`（D-089）。

成功時（**実サーバの応答**。摘要に「請求」を含む3件）：

```json
{
  "entries": [
    {
      "entry_id": "019fc7cd-5b9d-75b1-95e6-c9814c075a53",
      "entry_no": 1,
      "fiscal_year": 2026,
      "entry_date": "2026-04-15",
      "description": "A社への請求",
      "lines": [
        { "account": "135", "side": "debit",  "amount": "110000", "currency": "JPY", "tags": {} },
        { "account": "500", "side": "credit", "amount": "100000", "currency": "JPY",
          "tags": { "tax_category": "SALES_10" } },
        { "account": "330", "side": "credit", "amount": "10000", "currency": "JPY",
          "tags": { "tax_category": "SALES_10" } }
      ]
    },
    {
      "entry_id": "019fc7cd-5bc6-7a32-a8c1-19687f2a0529",
      "entry_no": 3,
      "fiscal_year": 2026,
      "entry_date": "2026-05-08",
      "description": "B社への請求",
      "lines": [ /* 略 */ ],
      "reversed_by": {
        "entry_id": "019fc7cd-5bd8-7bd3-bb84-8cc0acab1c14",
        "entry_no": 4,
        "entry_date": "2026-05-10"
      }
    },
    {
      "entry_id": "019fc7cd-5bd8-7bd3-bb84-8cc0acab1c14",
      "entry_no": 4,
      "fiscal_year": 2026,
      "entry_date": "2026-05-10",
      "description": "【訂正】B社への請求",
      "lines": [ /* 借方・貸方を入れ替えた明細 */ ],
      "reverses": "019fc7cd-5bc6-7a32-a8c1-19687f2a0529",
      "reverse_reason": "請求金額の誤り（税率の適用誤り）"
    }
  ],
  "total_matches": 3,
  "returned": 3,
  "has_more": false
}
```

- 並びは**取引日 → 仕訳番号 → 仕訳ID**の昇順。
- `lines` は確定後の明細（自動生成された税額行を含む）。金額は**区切り無しの文字列**。
- **`reverses` / `reverse_reason` / `reversed_by` は該当するときだけ現れる**
  （`null` を置かない。D-088）。上の例では、**取り消された仕訳（`entry_no: 3`）にも
  赤伝（`entry_no: 4`）にも** それと分かる欄が付いている。
  帳簿は追記のみなので、取り消された仕訳も検索結果に残り続ける。
- **0件は成功。** `entries: []` / `total_matches: 0` を返し、エラーにしない。

上限で切れたとき（`limit: 1` の実応答。**切ったことが応答から分かる**）：

```json
{
  "entries": [ /* 1件 */ ],
  "total_matches": 4,
  "returned": 1,
  "has_more": true,
  "next_cursor": "2026-04-15:1:019fc7cd-5b9d-75b1-95e6-c9814c075a53",
  "truncation_note": "条件に一致した 4 件のうち 1 件を返しました（この呼び出しで指定された limit=1 で切りました。1回に返せる上限は 100 件なので、limit を上げれば1回で受け取れる件数を増やせます）。続きは cursor に next_cursor の値を渡して取得してください。件数を絞りたい場合は期間や科目の条件を追加してください"
}
```

`has_more` が偽のときは `next_cursor` も `truncation_note` も**キーごと出さない**。

`truncation_note` は**何が件数を決めたのか**を述べる。`limit` を指定して
呼んだ場合は「自分の指定で切れた」、上限まで返した場合は「サーバの上限で
切れた」と分かる。**次の手が違う**ので上限だけを述べない（D-089 決定3）。

### get_ledger

```json
{
  "account": "500",
  "from": "2026-01-01",
  "to": "2026-12-31",
  "limit": 100,
  "cursor": "2026-04-15:1:019fc7cd-5b9d-75b1-95e6-c9814c075a53:2"
}
```

- `account` / `from` / `to` は**必須**。`from` / `to` は取引日で両端を含み、
  `from > to` は空の元帳ではなく**エラー**（`rejected`）。
  期間を必須にしている理由は「省略時の既定が『開設以来の全明細』になり、
  実質的に上限で切られた先頭だけが返る」ため（`crates/kaikei-app/src/usecase/ledger.rs`）。
- `limit` は既定 100・上限 500。上限超過は丸めずにエラー。
- `cursor` は `search_entries` と同じ扱い。

成功時（**実サーバの応答**。売上高の元帳。3件目が赤伝で取り消されている）：

```json
{
  "account": "500",
  "account_name": "売上高",
  "account_type": "revenue",
  "currency": "JPY",
  "from": "2026-01-01",
  "to": "2026-12-31",
  "opening_balance": "0",
  "debit_total": "50000",
  "credit_total": "150000",
  "closing_balance": "100000",
  "total_lines": 3,
  "returned": 3,
  "has_more": false,
  "rows": [
    {
      "entry_id": "019fc7cd-5b9d-75b1-95e6-c9814c075a53",
      "entry_no": 1,
      "entry_date": "2026-04-15",
      "line_no": 2,
      "description": "A社への請求",
      "side": "credit",
      "amount": "100000",
      "currency": "JPY",
      "running_balance": "100000",
      "counter_accounts": ["135"],
      "tags": { "tax_category": "SALES_10" }
    },
    {
      "entry_id": "019fc7cd-5bc6-7a32-a8c1-19687f2a0529",
      "entry_no": 3,
      "entry_date": "2026-05-08",
      "line_no": 2,
      "description": "B社への請求",
      "side": "credit",
      "amount": "50000",
      "currency": "JPY",
      "running_balance": "150000",
      "counter_accounts": ["135"],
      "tags": { "tax_category": "SALES_10" },
      "reversed_by": {
        "entry_id": "019fc7cd-5bd8-7bd3-bb84-8cc0acab1c14",
        "entry_no": 4,
        "entry_date": "2026-05-10"
      }
    },
    {
      "entry_id": "019fc7cd-5bd8-7bd3-bb84-8cc0acab1c14",
      "entry_no": 4,
      "entry_date": "2026-05-10",
      "line_no": 2,
      "description": "【訂正】B社への請求",
      "side": "debit",
      "amount": "50000",
      "currency": "JPY",
      "running_balance": "100000",
      "counter_accounts": ["135"],
      "tags": { "tax_category": "SALES_10" },
      "reverses": "019fc7cd-5bc6-7a32-a8c1-19687f2a0529",
      "reverse_reason": "請求金額の誤り（税率の適用誤り）"
    }
  ]
}
```

- 並びは**取引日 → 仕訳番号 → 仕訳ID → 明細行番号**の昇順。
- **合計はページではなく期間全体の値。** `opening_balance` / `debit_total` /
  `credit_total` / `closing_balance` / `total_lines` は `limit` に関係なく
  指定期間の全明細から求める。**ページの行を足しても `debit_total` にはならない**
  （そう読ませないために、行側には合計を置いていない）。
- `opening_balance` は **`from` より前のすべての明細**から求めた残高であり、
  会計年度の期首ではない。収益・費用は決算振替でゼロに戻るため、期首日を
  年度途中に取ると「その年度の期首からの累計」にはならない。
- `running_balance` は**期首残高からの累計**。ページをまたいでも連続する
  （ウィンドウ関数をカーソルで絞る前に計算している）。
- 残高の符号は `account_type.is_debit_normal()` に従う（上の例は収益なので
  貸方が正）。**負にもなりうる**（`"-500"`）。
- `counter_accounts` は同じ仕訳の**反対側**にある科目コード（重複を除いた昇順）。
- `reverses` / `reverse_reason` / `reversed_by` は `search_entries` と同じ規則
  （D-088）。**赤伝の行も元帳に残るので、残高は取り消し後の姿になる**（上の例は
  150,000 → 100,000）。赤伝の行だけを見て「なぜ取り消されたか」が読める
  （`search_entries` に引き直さなくてよい）。
- **同じ仕訳が赤伝で2回以上訂正されていても、行は明細1行につき1行**である
  （`allow_double_reversal`。`reversed_by` に載るのは最も古い赤伝1件）。
- **0行は成功。** その期間に明細が無いだけなので `rows: []` を返し、
  `opening_balance` はその期間より前の累計として残る。

失敗時（勘定科目マスタに無い科目コード。**実サーバの応答**）：

```json
{
  "error": "not_found",
  "message": "見つかりません: 勘定科目 99999 は勘定科目マスタにありません。list_accounts で登録済みの科目コードを確認してください"
}
```

**「0行の元帳」を返さないこと。** 科目コードの打ち間違いと「その期間に取引が無い」は
呼び出し元が取るべき次の手が違う（前者はコードを調べ直す、後者は期間を広げる）。

---

## 4. 実装方針

- **MCP サーバーは薄い層にする。** ビジネスロジックを MCP 層に書かない。
- ただし「薄い」の中身は経路によって3種類ある（下記）。
- ツール定義は `kaikei-mcp/src/tools/*.rs` に **1 ツール 1 ファイル**。
  ファイル名は **MCP のツール名と1対1**にする（`post_journal_entry.rs` /
  `get_trial_balance.rs` …）。`post_entry.rs` のような名前にすると
  `kaikei-app/src/usecase/post_entry.rs` と同名になり、grep でどちらの層の話か
  区別できなくなる。

### dispatch 層 — 監査ログを通らないツールを書けない形にする（型＋ファイル許可リスト。PR-F。D-084）

**11ツールが個別に監査ログの手順を書く形にしない。**
D-076 が `kaikei_app::audit::with_audit` に手順を閉じたのは
「fail-closed の書き忘れは正常系テストでは検出できない」からであり、
**同じ理由が MCP 層にも当てはまる**。

各ツールは `crates/kaikei-mcp/src/dispatch.rs` の `McpTool` を実装するだけで、
監査ログの手順も `isError` の扱いも書かない。

```rust
pub trait McpTool: Send + Sync + 'static {
    type Input: DeserializeOwned + JsonSchema + Send + 'static;
    const NAME: &'static str;
    const DESCRIPTION: &'static str;   // ツールの説明文（AI が最初に読む）
    fn run(ctx: &ToolContext<'_>, input: Self::Input)
        -> impl Future<Output = Result<ToolSuccess, ToolFailure>> + Send;
}

// rmcp の ToolRouter を private フィールドとして包む。ツールを載せる口はこれだけ。
pub struct ToolRegistry { /* rmcp の ToolRouter を包んで隠す */ }
impl ToolRegistry {
    pub fn new() -> Self;
    pub fn with<T: McpTool>(self) -> Self;             // 唯一の登録経路
}

pub async fn call<T: McpTool>(runtime: &Runtime, arguments: Option<Map<String, Value>>)
    -> CallToolResult;                                  // 唯一の実行経路
```

| 塞ぐもの | 実体 | 手段 |
|---|---|---|
| ツールが応答を組み立てる | `run` の戻り値は `Result<ToolSuccess, ToolFailure>`。`CallToolResult`（`isError` を含む）を作れるのは `dispatch::call` と `ToolError` だけ | 型 |
| ツールが監査ログを書く／書き忘れる | `run` が受け取る `ToolContext` は `AuditSink` を**露出しない**（`Runtime` 自体が渡らない） | 型 |
| `ToolContext` を自作する | フィールドも `new` も private | 型 |
| `ToolRegistry` に `McpTool` 以外を載せる | ツールを載せる口は `with::<T: McpTool>` だけで、内側の `ToolRouter` は private フィールド | 型 |
| **別のルータ・別の `ServerHandler` を書き足す** | **`rmcp` を名指しできるファイルを `dispatch.rs` / `error.rs` の2つに限る許可リスト** | 検査 |
| **走査の外にファイルを置く**（`#[path = "../foo.rs"]` / `include!("foo.inc")`） | `tests/source_scan/mod.rs` の `assert_no_out_of_tree_inclusion`（走査ヘルパは `audit_is_structural.rs` と `stdout_is_json_rpc_only.rs` で共有） | 検査（4巡目 B） |
| **上の全部**（書き方に依らず、監査ログが2行残ること） | 実バイナリに `tools/call` を送り `audit_log` を数える（`crates/kaikei-e2e/tests/mcp_stdio_server.rs`） | **振る舞い検査**（4巡目 A） |
| fail-open の警告を捨てる | `call` は `AuditedCall::into_result_noting_outcome(&mut notes)`（既定経路）しか使わず、警告を必ず応答の `warnings` に載せる。`into_parts_unchecked` はこの crate に1箇所も無い | 型＋走査 |
| クライアント由来の名前が `audit_log.tool` に載る | `AuditCall` を組み立てるのは `dispatch.rs` の1箇所だけ（走査で担保）で、そこは `ToolName::resolve`（レジストリを引く。**登録済み以外を弾くことは型で閉じている**）を通す（§9） | 走査＋型 |

> **★識別子の禁止リストは2巡続けて破られた。許可リストに反転した★**
> （PR-F レビュー B-1 / 3巡目 B）
>
> | 巡 | 破り方 | 当時の禁止リストに無かった識別子 |
> |---|---|---|
> | 1 | `ToolRouter::with_async_tool::<T>()` / `with_sync_tool` / `(Tool, handler)` タプル | `with_async_tool` / `AsyncTool` / `ToolBase` / `IntoToolRoute` |
> | 2 | `#[tool_handler]` の impl に `call_tool` を**手書き**する | `call_tool` / `CallToolRequestParams` / `ToolCallContext` / `into_call_tool_result` |
>
> 1 は `ToolBase` + `AsyncTool` を実装した型が**ハンドラ本体ごと**ルータに
> 載る形で、`ToolRoute` も `CallToolResult` も `with_audit` も `#[tool(` も
> 書かない。`rmcp` 3.1 の module doc が「1ツール1ファイルならこの形」と
> 勧めてすらいた（レビュー2名が独立に再現し、`get_settings` を名乗る probe が
> `tools/list` に載り、`audit_log` に1行も残さずに実 DB へ記帳できた）。
>
> 2 はさらに静かだった。`rmcp-macros` 3.1.0 の `#[tool_handler]` は
> `if !has_method("call_tool", &item_impl)` で条件付き生成するので、**同じ
> impl ブロックに `call_tool` を手書きするとマクロが生成する dispatch 経路が
> 黙って置き換わる**。`tools/list` は正規の2件のまま、`tools/call` を1回
> 送っただけで `journal_entries` に1件・`audit_log` に0行。
> `cargo build` / `clippy -D warnings` / `cargo test -p kaikei-mcp` は全緑。
> しかもその impl は既に `get_info` を手書きしており、「メソッドを自分で
> 書けばマクロが引き下がる」形が**見本として目の前に置かれていた**。
>
> レビューはこの時点でまだ塞がっていない口として `ToolRouter::add_route` /
> `merge`、`IntoToolRoute` の `WithToolAttr` /
> `ToolAttrGenerateFunctionAdapter`、`CallToolHandlerExt`（`mentions` の
> 識別子境界判定により `CallToolHandler` 規則には**一致しない**）も挙げていた。
> **識別子を足し続ける限りこれは終わらない**（禁止リストは原理的に不完全で
> あり、`rmcp` が API を1つ増やすたびに穴が開く）。
>
> そこで向きを反転し、**`kaikei-mcp/src/` のうち `rmcp` という識別子を
> 書いてよいファイルを許可リストで限定した**。
>
> | ファイル | なぜ必要か |
> |---|---|
> | `dispatch.rs` | ルータ、`ServerHandler` の実装（`call_tool` / `list_tools` / `get_tool` / `get_info`）、stdio トランスポートの起動 |
> | `error.rs` | `ToolError::into_call_tool_result`（D-071） |
>
> どの API を使う迂回であっても `rmcp` の名前は必要なので、迂回は必ず
> 許可された2ファイルのどちらかに現れる。`#[tool_handler]` は使わず
> **`ServerHandler` の4メソッドを `dispatch.rs` で手書き**してあるので、
> 「`call_tool` を手書きする」形自体が許可リストの内側にある。
> MC-30（依存の許可リスト）や `tests/forbidden_tools.rs` の
> `every_registered_tool_is_one_of_the_eleven_phase_3_tools`（禁止4件だけで
> なく許可11件の側からも閉じる）と同じ形である。

> **★「型で閉じた」と書かないこと★**（PR-F レビュー3巡目 C-1）
>
> `rmcp` は `kaikei-mcp` の直接依存であり `ToolRouter` は `pub` である。
> **同一 crate の他モジュールから import を妨げる仕組みは Rust に無い**
> （レビュアーが実際にコンパイルを通している）。以前ここと
> `crates/kaikei-mcp/src/dispatch.rs` / `src/server.rs` / `DECISIONS.md` D-084 に
> あった「`ToolRouter` は見えない」「型として存在しない」「監査ログを通らない
> ツールは書けない」は**いずれも成立していなかった**。止めているのは検査で
> あって型ではない。提供していない保証を書かない。

型で閉じられない残り（同一 crate 内から `with_audit` を呼ぶ、`dispatch.rs` の
中で `ToolRouter` に別のツールを足す、`dispatch.rs` が `rmcp` の型を再輸出する）は
`crates/kaikei-mcp/tests/audit_is_structural.rs` が見張る。
走査の中での一次の担保はファイル許可リスト
（`rmcp_is_named_only_in_the_files_allowed_to_name_it`）であり、識別子の
閉じ込め（`ToolRouter` / `with_async_tool` / `AsyncTool` / `IntoToolRoute` /
`CallToolHandler` …）は**再輸出に対する second line** として残してある。
**識別子の一覧が網羅であるとは主張しない。**

> **★「唯一の穴」と書かないこと★**（PR-F レビュー4巡目 C-1）
>
> ここには以前「再輸出が許可リストの**唯一の**穴である」と書いてあったが、
> **少なくとも3つある**。
>
> | 穴 | 何ができるか | 見張り |
> |---|---|---|
> | `dispatch.rs` が `rmcp` の型を再輸出する | 他のファイルが `rmcp` を名指しせずに登録経路へ届く | 識別子の閉じ込め（second line） |
> | 走査の外にファイルを置く（`#[path = "../foo.rs"]` / `include!("foo.inc")`） | crate の一部なのに一度も読まれないファイルに何でも書ける | `assert_no_out_of_tree_inclusion`（4巡目 B で追加） |
> | 許可された `dispatch.rs` の中に**別のプロトコル入口**を足す | `ServerHandler` は `call_tool` 以外にも既定実装を持つ（`read_resource` / `get_prompt` / `complete` / `get_task` / `update_task` / `cancel_task` …）。そこに書けば `tools/call` を通らずに操作できる | **振る舞い検査**（`mcp_stdio_server.rs` の `no_protocol_entry_point_other_than_tools_call_touches_the_ledger`。`tools/call` を1回も送らずに `resources/read` / `prompts/get` / `completion/complete` / `resources/subscribe` を送り、帳簿と `audit_log` が1行も動かないことを見る） |
>
> 3つ目は許可リストの内側なので、走査では原理的に見張れない。
> **網羅を主張するのはやめる。網羅を担うのは走査ではなく振る舞い検査で
> ある**（下記）。

> **★網羅は振る舞いで担保する★**（PR-F レビュー4巡目 A）
>
> 走査は3巡続けて外側から破られた（1: 禁止識別子の一覧に無い API、
> 2: `#[tool_handler]` の条件付き生成、3: `#[path]` / `include!` で走査の外に
> ファイルを置く）。3巡目の再現では、監査ログを通らない別の `ServerHandler`
> を `main.rs` から**実際に待ち受けさせた**状態で `cargo build` /
> `clippy -D warnings` / `fmt --check` / `cargo test -p kaikei-mcp` が全緑
> だった。**走査は「ソースがどう書かれているか」しか見られない。**
>
> そこで `crates/kaikei-e2e/tests/mcp_stdio_server.rs` に、
> **実バイナリを子プロセスとして起動し、stdio で `initialize` →
> `tools/call` を送る**検査を置いた（`pg-tests`。使い捨てDBを作り、
> `journal_entries` と `audit_log` を SQL で数える）。
>
> | 見るもの | 期待 |
> |---|---|
> | `post_journal_entry`（成功） | 帳簿1件・`audit_log` に `started` / `ok` の2行・`entry_id` が応答と一致 |
> | `post_journal_entry`（貸借不一致） | `isError: true`・帳簿0件・`audit_log` に `started` / `error` の2行 |
> | `reverse_journal_entry`（成功・失敗） | 同じ経路を通る（ツールごとに迂回できない） |
>
> 識別子が何であれ、ファイルがどこに在ろうと、別の入口から来ようと、
> **監査ログが2行無ければ落ちる**。走査は「書いた瞬間に、DB 無しで、手元で
> 落ちる」二線目として残す。



**`rmcp` のツールマクロ（`#[tool]` / `#[tool_router]`）は使わない。**
マクロで書くとハンドラ本体をその場に書くことになり、`dispatch::call` を
経由しない登録経路が生まれる。`rmcp` 3.1 が「ツールが増えたらこちら」と
勧めている `AsyncTool` / `ToolBase` trait も使えない——`invoke` の失敗値が
`Into<ErrorData>`（＝**JSON-RPC のプロトコルエラー**）に固定されており、
D-071 の「ドメインのエラーは全てツール結果エラー」と両立しないため。
**却下しただけでは使えないままにならない**（読み取り系・成功前提のツールなら
普通に書けてしまう）ので、上記のとおり型と走査の両方で塞いである。

**入力 DTO のデシリアライズは `dispatch::call` が `with_audit` の操作の中で
行う（`Parameters<T>` は使わない）。** `Parameters<T>` はツール本体に入る
**前**に走るので、失敗した呼び出しが `audit_log` に1行も残らない
（§10 MC-09 の (3) / D-085）。`input_schema` は `dispatch` が
`schema_for_input::<T::Input>()` で生成する。

### MCP SDK とトランスポート

| 項目 | 選定 |
|---|---|
| SDK | **`rmcp` 3.x**（`default-features = false`, `features = ["server", "macros", "transport-io", "schemars"]`） |
| トランスポート | **stdio**（MCP クライアントが子プロセスとして起動する） |
| 却下 | 手書き JSON-RPC / 第三者 SDK（D-071） |

- 入力 DTO（`McpTool::Input`）に `#[derive(Deserialize, JsonSchema)]` を付けると
  input_schema が生成できる（`schemars` feature がこれを有効にする）。
  ~~`Parameters<T>` で受ける~~ → **PR-F で却下**（D-085。ツール本体に入る前に
  デシリアライズが走るため、失敗した呼び出しが `audit_log` に残らない）。
  生成は `dispatch::route` が `schema_for_input::<T::Input>()` で行い、
  デシリアライズは `dispatch::call` が `with_audit` の操作の中で行う。
- ツールの説明文は `McpTool::DESCRIPTION`（上記 dispatch 層）に書く。
  `CLAUDE.md` §11（次の手が分かる文言）と §10（税務判断を断定しない）の規律は
  この文面にも及ぶ。§5 の「金額は文字列」も説明文に書く
  （`crates/kaikei-mcp/src/server.rs` のテストが全登録ツールの説明文を検査する）。
- 1ツール1ファイルは `src/tools/<ツール名>.rs` に `impl McpTool` を置き、
  `server.rs` の `tool_registry()` で `ToolRegistry::with::<T>()` を並べて実現する。
  ~~`#[tool_router(router = ..., vis = "pub")]` を各ファイルに置いて `+` で合成する~~
  → **PR-F で撤回**（D-084。マクロで書くと dispatch 層を迂回できる）。
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
| (a) 書き込み | `post_journal_entry` / `reverse_journal_entry` | `usecase::post_entry::execute` / `reverse_entry::execute` を `tx::with_tx` で包んで呼ぶ。**`post_entry::execute` / `preview` は失敗値が `PostEntryFailure`（`error` + `notes`）なので `tx::with_tx_err` を使う**（`with_tx` は `AppError` 固定。両者は同じ実装を通る） |
| (b) 読み取り（read model 直行） | `get_trial_balance` / `search_entries` / `get_ledger` | `Tx` を通さず read model クエリを呼ぶ（`usecase::report::execute` は `ports::TrialBalanceQuery` と `context::BookSettings` を受け取る。`CLAUDE.md` §6「Repository を通さず SQL から DTO へ直行」） |
| (c) 税制の問い合わせ | `list_tax_categories` / `get_settings` / `suggest_tax_category` / `validate_invoice_number` | `kaikei-app` を経由せず、合成ルートが保持する `kaikei-jp` の値（`TaxRuleSets::require_for_date`（該当なしをエラーにする入口。`for_date` は `Option` のまま） / `JpSettings` / `InvoiceRegistrationNo::parse`）から直接組み立てる |

`list_accounts` と **`get_entry`** は `Tx` 経由で読む（それぞれ
`ChartRepo::load_chart` と `JournalRepo::find_entry`。集計ではなく
「マスタ1枚」「集約1件」の取得であり、read model を新設しない。D-086）。
記帳が科目・仕訳を解決するのと同じ経路なので、**ツールの応答と記帳が見ている
帳簿が食い違わない**。

### Phase 3 で新設が必要なもの

- ~~`crates/kaikei-store/src/query/{search,ledger,entry_detail}.rs`（read model）。~~
  → **PR-H で完了**（`query/search.rs` / `query/ledger.rs`。`entry_detail.rs` は
  §2 のとおり**作らない**）。クエリ trait は `kaikei-app/src/ports.rs` に
  `TrialBalanceQuery` と同型で追加した（`SearchEntriesQuery` / `LedgerQuery` と
  その条件構造体 `SearchEntriesParams` / `LedgerParams`）。DTO は
  `kaikei-app/src/view.rs`（`EntrySummaryView` / `EntrySearchPageView` /
  `LedgerRowView` / `LedgerPageView` / `ReversalRef` / `EntryCursor` /
  `LedgerCursor`）。ユースケースは `kaikei-app/src/usecase/search_entries.rs` /
  `ledger.rs`（`from > to` の拒否・`limit` の範囲・`aggregatable` の検証・
  帳簿通貨との突き合わせを SQL 到達前に行う）。
  `.sqlx` オフラインキャッシュも再生成した
  （`.github/workflows/database.yml` の `cargo sqlx prepare --workspace --check`）。
- ~~`kaikei-app` の**勘定科目マスタ投入ユースケース**（D-070）。~~
  → **PR-E で完了**（`kaikei_app::usecase::import_chart` /
  `kaikei_app::ports::ChartWriteRepo` / `kaikei_store` の実装。D-081）。
  `crates/kaikei-e2e/tests/e2e_jp.rs` の `seed_chart` も**この経路を呼ぶ形に
  書き換えた**ので、テスト用シードと本番の投入経路は同じ1本である
  （二重管理にしない。D-047）。
- ~~**audit_log 書き込みポート**（§9）~~
  → **PR-C で完了**（`kaikei_app::ports::AuditSink` /
  `kaikei_app::audit::with_audit` / `kaikei_store::audit::PgAuditSink` /
  `crates/kaikei-store/migrations/0009_audit_log.sql`）。分類コード
  （`audit_log_unavailable`）は PR-B 2巡目で語彙に入れてあったものを使う。
- ~~`post_entry::execute` の戻り値拡張（`PolicyNote` を返す。§3）~~
  → **PR-B で完了**（`PostEntryOutput { entry, notes }` /
  `ReverseEntryOutput { entry }`）。失敗経路の `PolicyNote`
  （`PostEntryFailure { error, notes }`）は **PR-B 2巡目で完了**。
- ~~`hint` 用の dry-run ユースケース（§3）~~
  → **PR-B 2巡目で完了**（`usecase::post_entry::preview`）。
- ~~線上表現（金額の文字列・列挙型の機械可読名・仕訳IDの入力側）~~
  → **PR-B 2巡目で完了**（`kaikei_app::{amount, wire, id}` と
  `kaikei_jp::tax` の `as_code` / `from_code`。§3 の表）。
- ~~線上の `tags` → `TagSet` の変換、税区分マスタの列挙~~
  → **PR-B 3巡目で完了**（`kaikei_jp::tags::TagCatalog` と
  `kaikei_jp::tax::TaxRuleSets::{iter, available_ranges_display, require_for_date}`）。
- ~~**【PR-D への申し送り】`kaikei-mcp` の依存が最小限であることを CI で検査する。**~~
  → **PR-D で完了。** `.github/workflows/architecture.yml` の
  `dependency-direction` ジョブに「kaikei-mcp の依存は最小限（MC-30）」
  ステップを追加した（**新しいジョブは作っていない**ので必須チェックの
  登録＝ブランチ保護の変更は不要。`CLAUDE.md` §13）。**禁止リストではなく
  許可リスト**にしてある（`kaikei-core` / `kaikei-app` / `kaikei-jp` /
  `kaikei-store` / `rmcp` / `tokio` / `serde` / `serde_json` / `schemars`）。
  依存の取得は **`cargo metadata --no-deps`**（`packages[].dependencies[].name`）で
  行う。`cargo tree` は既定でホストターゲットに解決される依存しか見ないため、
  `[target.'cfg(windows)'.dependencies]` の下に `uuid` を置くと ubuntu-latest の
  CI を素通りする（レビュー2巡目で実測）。`cargo metadata` の宣言一覧は
  **kind（normal / dev / build）・target 指定・optional を問わず全て**現れるので、
  **dev-dependency 経由の抜け道も target-cfg 経由の抜け道も塞がる**（D-078）。
  許可リストとの照合は `case " $ALLOWED " in *" $d "*)` による**完全一致**。
  `grep -qw` は `-` を単語構成文字として扱わないため、`kaikei` / `core` /
  `app` のような**成分名の crate** を足すと緑のまま通ってしまう（同じく実測）。
  以下は申し送り当時の記述:
  §3 の表と2本のプローブ（`crates/{kaikei-app,kaikei-jp}/tests/contract_from_downstream.rs`）は
  「線上表現を MCP 層で再発明しない」ことを狙っているが、**プローブでは
  下流の依存が増えないことを検査できない**（統合テストにはその crate の
  dev-dependencies までリンクされるため、`uuid` / `rust_decimal` を直接
  使っても通ってしまう。実測済み）。`kaikei-mcp` が実在してはじめて、
  その `Cargo.toml`（`kaikei-app` / `kaikei-jp` / `kaikei-core` / MCP SDK /
  非同期ランタイム / シリアライズ以外を足していないこと）を
  `.github/workflows/architecture.yml` の依存方向チェックと同型の
  ステップで機械的に検査できる。**新しいジョブを足したら必須チェックへの
  登録も同じ PR で行うこと**（`CLAUDE.md` §13）。
- **【PR-D で完了】`JpError` → 分類コードの対応表**（§6）。
  `crates/kaikei-mcp/src/error.rs` の `jp_error_code`（網羅 `match`）。
- **【PR-D で完了】ツールレジストリと「存在させないツール」の検査**（§10 MC-10）。
  `crates/kaikei-mcp/src/server.rs` と `crates/kaikei-mcp/tests/forbidden_tools.rs`。
- **【PR-D で完了】金額を number で渡したときの日本語エラー**（§5・MC-09 の (1)）。
  `crates/kaikei-mcp/src/wire.rs` の `AmountStr`。
  (2)（`isError: true` で返ること）は rmcp が担保し、(3)（audit_log）は PR-F。

**`kaikei-mcp` が新しく書かなくてよいもの**（§3 の表を再掲）: エラーコードの
対応表、金額の文字列化、`side` / `account_type` / `severity` /
`fiscal_year_rule` / `tax_mode` / `rounding` / `rounding_unit` の文字列、
仕訳IDのパースと表示、貸借の検算、dry-run の手順、
線上の `tags` と `TagSet` の相互変換、税区分マスタの有効期間の文言。
これらを MCP 層に書いたら、それは「薄い層」ではない。

### ディレクトリ構成

```
kaikei-mcp/src/
├── main.rs               起動（設定ロード → 合成 → stdio サーバ）        ← PR-E で新設
├── startup.rs            合成ルート。kaikei_jp::compose::compose + PgStore の結線 ← PR-E で新設
├── config.rs             事業者設定の読み込みと必須検証（欠けていたら起動失敗。§7） ← PR-E で新設
├── dispatch.rs           ★監査ログを通す唯一の経路★ McpTool / ToolContext / call / ToolRegistry / ServerHandler の実装 / serve_stdio ← PR-F で新設。**rmcp を名指しできる2ファイルのうちの1つ**
├── wire.rs               線上の DTO（AmountStr 等）。★整形は書かない★ ← PR-D で新設
├── server.rs             レジストリに何を載せるか（tool_registry）とサーバー情報の文面 ← PR-D で新設
├── error.rs              JpError → 分類コード、ToolError → CallToolResult::structured_error（§6） ← PR-D で新設。**rmcp を名指しできる2ファイルのうちの1つ**
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

**`amount.rs` は作らない**（初版の構成案にはあったが、PR-B 2巡目で整形手段が
`kaikei_app::amount` に置かれたため、同じ整形が2箇所に育つだけになる。§5）。
線上の金額の型（`AmountStr`。number を日本語のエラーで拒否する）は `wire.rs` に
置き、文字列化は `money_to_plain_string` に委ねる（D-079）。

**`audit.rs` も作らない**（初版の構成案にはあったが、手順は PR-C が
`kaikei_app::audit::with_audit` に閉じた。MCP 層に残る仕事は「入力・出力の
JSON を組み立てて `with_audit` を1箇所から呼ぶ」ことだけで、それは
`dispatch.rs` の役目である。§9・D-076 / D-084）。

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

→ ログ・診断出力は必ず **stderr** に出す。`println!` を書かない。

**PR-E の実装**: `kaikei-mcp` は `tracing_subscriber` を持たない
（MC-30 の許可リストにも無い）。購読者を登録しない限り `tracing` の
イベントはどこにも出力されないため、下位層の `tracing::warn!` が
stdout に漏れることは**構造上起きない**。診断は `eprintln!` で stderr に出す。
**将来購読者を入れる場合は writer を stderr に固定すること**
（既定は stdout であり、入れた瞬間にプロトコルが壊れる）。
検査は §10 MC-33。

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

- 金額フィールドは専用の newtype（`AmountStr`。`kaikei-mcp/src/wire.rs`）にし、
  `Deserialize` を**手書き**して number / bool / null を受けたら
  `de::Error::custom` で日本語のメッセージを返す。
  素の `String` にしておくと AI には `invalid type: integer 110000, expected a string`
  という英語の型エラーしか届かず、`CLAUDE.md` §11 を満たさない
  （D-019 が `{:?}` の英語バリアント名を禁じたのと同型の問題）。
  **実装済み**（PR-D）。実際の出力:

  ```
  金額は文字列で渡してください（例: "110000"、USD なら "1234.56"）。
  JSON の number は倍精度浮動小数点のため、会計金額には使えません
  ```

  **手書きの形は「独自 Visitor」ではなく「一度 `serde_json::Value` として
  受けてから判定」にした**（D-079）。`deserializer.deserialize_str` では
  serde 側が `invalid_type` を組み立ててしまい custom メッセージが無視される、
  という当初の指摘は正しい。しかし独自 Visitor で `visit_f64` を実装すると
  ソースに浮動小数点の型名が現れ、`.github/workflows/architecture.yml` の
  「f64 が金額に使われていない」ステップ（コメント行以外の該当語を全て落とす）
  が赤になる。`Value` 経由なら型名を書かずに number / bool / null / 配列 /
  オブジェクトのすべてを同じ日本語メッセージで拒否できる。
- `JsonSchema` は `#[schemars(with = "String")]` 等で `"type": "string"` を出す。
  スキーマ上 number も許されているように見えると、AI が number を送る動機を作る。
- **`as_f64()` を書かない。** `.github/workflows/architecture.yml` の
  「f64 が金額に使われていない」ステップ（コメント行以外の `f64` を全て落とす）が
  必ず赤になる。
- **デシリアライズの失敗は `dispatch::call` が `rejected` の構造化エラー
  （`isError: true`）にして返す**ので、この経路は §6・D-071 と整合する。
  ~~rmcp は `Parameters<T>` のデシリアライズ失敗を `CallToolResult::error` に
  変換する~~ → **PR-F で `Parameters<T>` を却下**（D-085。§4 を参照。
  あの経路は `dispatch::call` の外側なので `audit_log` に1行も残らない）。

### 通貨の決め方（PR-B で確定）

`Money::parse(s, currency)` は `Currency` を**必須の引数**として要求し、
`Currency::new(code, minor_unit)` は小数桁数まで必須である。
つまり `{"amount": "110000"}` だけでは `Money` を構築できない。確定した形:

1. **帳簿通貨（コード＋小数桁）は `kaikei_app::context::BookSettings::book_currency`
   が保持する。** `Option` ではない必須フィールドで、`Default` も実装しない
   （既定で JPY にフォールバックしない。§7 と同じ扱い。D-074）。
   `JpSettings` 側には持たせない（帳簿通貨は日本の税制固有の設定ではなく、
   `kaikei-jp` を差し替えても必要になる帳簿全体の設定であるため）。
2. 明細で `currency` を省略した場合は帳簿通貨を使う。
3. 明示された場合は `kaikei_app::currency::currency_from_code` で解決する。
   **未知のコードは桁数を推測せずエラー**（推測すると金額が100倍ズレて記帳される。
   `CLAUDE.md` §8）。現在このホワイトリストにあるのは `JPY` / `USD` の2つだけで、
   それ以外の通貨で帳簿を付けるには先にこの関数へ追加する必要がある。
   **`Currency::new(code, 0)` を書いて迂回しないこと。**
4. 1仕訳内で通貨が混在すると `JournalEntry::new` が `CurrencyMismatch` を返す。
   この制約をツールの説明文に書く。
5. 小数桁は通貨ごとに検証される（JPY に `"1000.5"` を渡すとエラー、USD は2桁まで）。

`book_currency` は**既定の通貨**であって「唯一許される通貨」ではない
（外貨建ての明細を将来受け付ける余地を型で塞がない）。

### 出力の文字列形式（PR-B 2巡目で確定）

`kaikei-core` が持つ唯一の文字列化は `Money::to_display_string()` で、
これは**3桁区切りカンマ付き**（`"110,000"`、USD なら `"1,234.56"`）を返す。
区切り無しの文字列化 API は core に無い（`minor()` は最小通貨単位の整数であり、
`minor_unit > 0` の通貨では金額そのものではない）。

**確定した分離:**

- 機械可読フィールド（`amount` / `debit_total` / `balance` 等）は**区切り無し**
  （`"110000"` / USD は `"1234.56"`）。
- 区切り付きの表記は `message` の**文中**でのみ使う（`Money::to_display_string()`）。

整形手段は **`kaikei_app::amount`** に置いた。**`kaikei-core` は変更していない**
（`Money::minor()` と `Currency::minor_unit()` はどちらも公開されており、
この2つから組み立てられる）。

| 関数 | 用途 |
|---|---|
| `money_to_plain_string(&Money) -> String` | `Money` → 区切り無し（`"110000"` / `"1234.56"` / `"-0.05"`） |
| `strip_thousands_separators(&str) -> String` | 整形済み文字列から区切りだけを除く（`"110,000"` → `"110000"`） |

後者が要るのは、`CoreError::Unbalanced` / `AppError::Inconsistent` が持つのが
`Money` ではなく**整形済みの `String`** だからである。区切りは `,`、小数点は `.` と
決まっているので、通貨の小数桁数を知る必要はない。

`kaikei-mcp` 側に `amount.rs` を作らないこと（同じ整形が2箇所に育つ。
`kaikei-api` も同じ形式を返す必要があり、presentation 層ごとに持つと必ずずれる）。
`Money::parse` とのラウンドトリップは `crates/kaikei-app/src/amount.rs` の
テストが JPY / USD の正負・ゼロ・`i64` 境界で検証済み
（`Money::parse` は正しい3桁区切り付きも受理するので、入力側の互換は保たれる）。

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

**対応表は PR-B で確定し、`crates/kaikei-app/src/error.rs` に1箇所だけ持つ**
（`codes` モジュールの定数と、それを引く4つの入口。D-072）。
**MCP 層はこの表を再実装しない。** `err.code()` を呼ぶだけにする。

| 写像元 | 入口 |
|---|---|
| `AppError` | `AppError::code()` |
| `RepoError` | `RepoError::code()` |
| `CoreError`（15 バリアント） | `kaikei_app::error::core_error_code()` |
| `PolicyError`（8 バリアント） | `kaikei_app::error::policy_error_code()` |

`CoreError` / `PolicyError` が自由関数なのは、定義元の crate が凍結層で
`impl` を生やせないため（`CLAUDE.md` §1）。

#### 規約

- コードは **snake_case の安定した識別子**であり、**メッセージ（日本語の
  `Display`）とは別物**。文言を改善してもコードは変わらない。逆にコードの
  変更は、応答・`audit_log.error_code`・それを読む AI の分岐を同時に壊す
  破壊的変更である。意味が変わったらコードを付け替えず**新しいコードを足す**。
- 層ごとの接頭辞は付けない（`RepoError::AppendOnlyViolation` は
  `append_only_violation`）。例外は**異なる層に同名のバリアントが実在する場合**
  だけで、現状は `Unsupported` の1件のみ。
- `AppError::Repo` / `Policy` / `Core` は**中身へ委譲**する
  （`AppError::Core(CoreError::Unbalanced)` は `unbalanced`）。
  AI が知りたいのは「何が起きたか」であって「どの層を経由したか」ではない。

#### `#[non_exhaustive]` の実測結果と受け皿

| 型 | `#[non_exhaustive]` | `kaikei-app` 内の `match` | 下流の `match` |
|---|---|---|---|
| `AppError` | **付いている** | 網羅（受け皿を置かない） | `_` の腕が**必須**。既定は `"internal"` |
| `RepoError` | 付いていない | 網羅 | 網羅できる |
| `CoreError` | 付いていない | 網羅 | 網羅できる |
| `PolicyError` | 付いていない（意図的。`kaikei-policy/src/error.rs` の doc） | 網羅 | 網羅できる |

**`AppError::code()` にワイルドカードの腕を置かない（PR-B 2巡目で変更）。**
1巡目は `_ => "internal"` + `#[allow(unreachable_patterns)]` を置いていたが、
その腕があるとバリアント追加時に `code()` のコンパイルが壊れず、
新しいバリアントが黙って `"internal"` になる。それを別のテスト関数
（重複した網羅 `match`）で見張るのは、**同じ一覧を2つ手で維持する**ことであり
`PROGRESS.md` Phase 1 の教訓6「手で維持する一覧は必ず腐る。構造で閉じる」に反する。

受け皿を消すと `#[allow(unreachable_patterns)]` も不要になる
（この lint は受け皿の腕自身が作り出していた問題だった。
`cargo clippy -p kaikei-app --all-targets -- -D warnings` が警告0で通ることを実測済み）。

`#[non_exhaustive]` は crate の**外**にしか効かないので、この変更は下流の
網羅性要件を何も変えない。下流の `match` には引き続き `_` の腕が必須で、
そこで使う既定値が `codes::INTERNAL` である
（`crates/kaikei-app/tests/contract_from_downstream.rs` が
外部 crate として実際に踏んでいる）。

#### 対応表（全バリアント）

**`CoreError`（15）**

| バリアント | コード |
|---|---|
| `Unbalanced` | `unbalanced` |
| `TooFewLines` | `too_few_lines` |
| `UnknownAccount` | `unknown_account` |
| `NotPostable` | `not_postable` |
| `CurrencyMismatch` | `currency_mismatch` |
| `InvalidAmount` | `invalid_amount` |
| `UnknownTagKey` | `unknown_tag_key` |
| `TagTypeMismatch` | `tag_type_mismatch` |
| `MissingRequiredTag` | `missing_required_tag` |
| `DateOutOfFiscalYear` | `date_out_of_fiscal_year` |
| `PeriodClosed` | `period_closed` |
| `EmptyDescription` | `empty_description` |
| `InvalidChart` | `invalid_chart` |
| `NotAggregatable` | `not_aggregatable` |
| `InvalidValue` | `invalid_value` |

**`PolicyError`（8）**

| バリアント | コード |
|---|---|
| `Core(CoreError)` | 中身の `CoreError` へ委譲 |
| `NoApplicableRuleSet` | `no_applicable_rule_set` |
| `UnknownTaxCategory` | `unknown_tax_category` |
| `TaxCategoryNotApplicable` | `tax_category_not_applicable` |
| `UnknownCounterparty` | `unknown_counterparty` |
| `QualifiedInvoiceUnverified` | `qualified_invoice_unverified` |
| `InvalidPolicyData` | `invalid_policy_data` |
| `Unsupported` | `policy_unsupported` |

**`RepoError`（7）**

| バリアント | コード |
|---|---|
| `NotFound` | `not_found` |
| `AppendOnlyViolation` | `append_only_violation` |
| `Conflict` | `conflict` |
| `Corrupt` | `corrupt` |
| `OutOfRange` | `out_of_range` |
| `Unsupported` | `repo_unsupported` |
| `Backend` | `backend` |

**`AppError`（8）**

| バリアント | コード |
|---|---|
| `Repo(RepoError)` | 中身の `RepoError` へ委譲 |
| `Policy(PolicyError)` | 中身の `PolicyError` へ委譲 |
| `Core(CoreError)` | 中身の `CoreError` へ委譲 |
| `AlreadyReversed` | `already_reversed` |
| `EmptyReverseReason` | `empty_reverse_reason` |
| `InvalidEntryId` | `invalid_entry_id` |
| `Inconsistent` | `inconsistent` |
| `Rejected` | `rejected` |

`EmptyReverseReason` は PR-B で追加したバリアント（§3 の
`reverse_journal_entry` の `reason` 検証。D-074）。

`InvalidEntryId` は PR-B 2巡目で追加（`kaikei_app::id::entry_id_from_uuid_string`
が返す）。**`not_found` と混同しないこと**——「その UUID の仕訳が無い」
（IDを調べ直す）と「送られた文字列が UUID ですらない」（表記を直す）は
AI が取るべき次の手が違う。

**`AppError` のバリアントを持たないコード**

| コード | 使う場所 |
|---|---|
| `internal` | **下流**の `match` の `_` の腕の既定値（`kaikei-app` からは返らない） |
| `audit_log_unavailable` | §9 の **fail-closed**（開始レコードが書けずツールを実行しなかった） |

`audit_log_unavailable` は PR-B 2巡目で語彙に追加した（`AuditSink` ポート自体は
後続 PR）。**`rejected` を借りてはならない**——`rejected` は
「集計期間の開始日が終了日より後です」のような**入力を直せば通る**拒否に
使っており、同じコードにすると AI が「入力を直せばよいのか」
「サーバ都合で今は実行できないのか」を区別できず、無意味な作り直しを繰り返す。

#### `JpError` の写像元（**PR-D で実装済み**）

**上の表は `kaikei-app` から見えるエラーだけを覆う。**
§4 の呼び出し経路 (c)（`list_tax_categories` / `get_settings` /
`suggest_tax_category` / `validate_invoice_number`）は `kaikei-app` を経由せず
`kaikei-jp` を直接呼ぶため、`kaikei_jp::JpError` が返る。
**書き込み系（経路 (a)）も、線上の `tags` を `TagSet` にする段
（§3。`TagCatalog::parse_tag_set`）だけは `JpError` が返る**——
`UnregisteredTagKey` / `InvalidTagValue` / `DuplicateTagKeyInInput` /
`NoApplicableTaxRuleSet` は PR-B 3巡目で追加したもので、いずれも
「入力を直せば通る」拒否である（`internal` に潰さないこと）。
`kaikei-app` は `kaikei-jp` に依存できない（`CLAUDE.md` §1・CI が検査）ので、
**`JpError` → コードの対応表は `kaikei-app` には置けない。**

→ `kaikei-mcp` 側（`error.rs`）に `JpError` 用の対応表を置く。
`kaikei-app` の `codes` モジュールの定数を再利用し、**同じ意味には同じコードを
使う**こと（例: `JpError::InvoiceRegNo*` 系には `invoice_*` の新しいコードを
起こしてよいが、`unknown_tax_category` のように既に語彙がある概念に別名を
作らない）。

**PR-D で実装した**（`crates/kaikei-mcp/src/error.rs` の `jp_error_code`。
`JpError` は `#[non_exhaustive]` ではないので**網羅 `match`** にしてあり、
バリアント追加はこの関数のコンパイルを壊す）。

| `JpError` | コード | 出所 |
|---|---|---|
| `Core(_)` | 中身の `CoreError` へ委譲 | `kaikei_app::error::core_error_code` |
| `UnregisteredTagKey` | `unknown_tag_key` | 既存（`CoreError::UnknownTagKey` と同義） |
| `InvalidTagValue` | `tag_type_mismatch` | 既存（`CoreError::TagTypeMismatch` と同義） |
| `DuplicateTagKeyInInput` | `duplicate_tag_key` | **新規** |
| `NoApplicableTaxRuleSet` | `no_applicable_rule_set` | 既存（`PolicyError` と同義） |
| `UnknownTaxCategoryCode` | `unknown_tax_category` | 既存（`PolicyError` と同義） |
| `InvoiceRegNoMissingPrefix` | `invoice_reg_no_missing_prefix` | **新規** |
| `InvoiceRegNoWrongLength` | `invoice_reg_no_wrong_length` | **新規** |
| `InvoiceRegNoNonDigit` | `invoice_reg_no_non_digit` | **新規** |
| `InvoiceRegNoCheckDigit` | `invoice_reg_no_check_digit` | **新規** |
| `InvalidSettingCode` | `invalid_setting_code` | **新規** |
| `InvalidBusinessRatio` | `invalid_value` | 既存 |
| `InvalidHouseholdSplitTotal` | `invalid_amount` | 既存 |
| `InvalidChart` | `invalid_chart` | 既存（`CoreError::InvalidChart` と同義） |
| `YamlParse` / `Io` / `InvalidTaxCategoryTable` / `OverlappingTaxPeriods` / `InvalidTagSchema` / `MissingClosingAccount` / `NotPostableClosingAccount` / `DuplicateClosingAccount` / `ClosingTagSchemaMismatch` | `invalid_policy_data` | 既存 |

登録番号の4件を別コードにしているのは、検証順が「先頭文字 → 桁数 → 文字種 →
チェックデジット」に固定されており（D-053）、**最初に失敗した観点だけが返る**
ため。1つに潰すと AI は何を直せばよいかを本文の日本語から推測するしかない。

`InvalidChart` だけをロード失敗の集約から外しているのは、`invalid_chart`
（「勘定科目表そのものが不正」）が既に語彙にあり、`CoreError::InvalidChart` が
そちらに写像されているため。`JpError::InvalidChart` は
`ChartOfAccounts::new` が返した `CoreError::InvalidChart` を包み直したものを
**含む**（`crates/kaikei-jp/src/chart.rs` の `from_raw`）ので、集約に入れると
**同一の条件が経路によって2つのコードになる**——このセクションが禁じている
「既に語彙がある概念に別名を作る」そのものになる（レビュー2巡目で是正）。

この一致は散文ではなく検査で固定してある。
`crates/kaikei-mcp/src/error.rs` の
`jp_and_core_invalid_chart_resolve_to_the_same_code` が
`jp_error_code(&JpError::InvalidChart { .. })` と
`kaikei_app::error::core_error_code(&CoreError::InvalidChart { .. })` の
**戻り値**を突き合わせるので、`kaikei-app` 側が `CoreError::InvalidChart` の
写像を変えたら落ちる（レビュー3巡目で追加）。

最後の9件を1つにまとめているのは、いずれも**サーバ側の同梱マスタ・起動設定が
不正**で、呼び出し元の入力を直しても解消しない——つまり
`PolicyError::InvalidPolicyData`（「policy が構築時に受け取ったデータが不正」）
と意味が一致するため（別名を作らない）。なお通常これらはツール応答に現れない
（設定・マスタの不備は起動時に検出して**起動を中止する**。§7）。

---

## 7. 起動と設定

**事業者設定は明示必須。1つでも欠けたら起動を中止する。既定値で走らない。**

`JpSettingsOverrides` の `is_taxable_business` / `simplified_taxation` は
`Option` ではない素の `bool` で、`Default` も**意図的に実装されていない**
（「指定し忘れたら免税事業者扱いになる」事故を避けるため。D-057）。
設定ファイルで省略して `Default` に落ちる実装にすると、
**無言で免税事業者として税額計算される**。

### 設定項目（**PR-E で全て必須に確定**。環境変数で渡す）

| 環境変数 | 内容 |
|---|---|
| `APP_DATABASE_URL` | `kaikei_app` ロールの接続文字列（§8） |
| `KAIKEI_BOOK_CURRENCY` | 帳簿通貨のコード。`kaikei_app::currency::currency_from_code` が桁数まで解決する（§5。既定で JPY にフォールバックしない。D-074） |
| `KAIKEI_FISCAL_YEAR_RULE` | 会計年度の区切り規則（`BookSettings::fiscal_year_rule`。現状 `calendar_year` のみ） |
| `KAIKEI_TAX_MODE` | 経理方式（`exclusive` / `inclusive`） |
| `KAIKEI_ROUNDING` | 端数処理方式（`floor` / `ceil` / `half_up`） |
| `KAIKEI_ROUNDING_UNIT` | 端数処理の単位（`line` / `document`） |
| `KAIKEI_IS_TAXABLE_BUSINESS` | 課税事業者か（`true` / `false`） |
| `KAIKEI_SIMPLIFIED_TAXATION` | 簡易課税か（`true` / `false`） |
| `KAIKEI_CLOSING_ACCOUNT_CAPITAL` / `_OWNER_DRAWINGS` / `_OWNER_CONTRIBUTIONS` | 決算3科目（元入金・事業主貸・事業主借。`ClosingAccounts`。**科目の実在は `JpSoleProprietorClosingPolicy::new` が構築時に検証する**。D-066） |
| `KAIKEI_CLOSING_TAX_CATEGORY` | 決算振替のゼロ化明細に付ける税区分コード。**起動時点で有効な税区分マスタに実在するかを合成ルートが照合する**（空でないことだけを見ると、存在しないコードで起動できてしまう） |

> **PR-E での変更（D-082）**: 本節の初版は `tax_mode` / `rounding` /
> `rounding_unit` を「省略時はマスタの `settings_defaults`」としていた。
> **PR-E でこの3つも必須にした。** 税抜/税込は課税事業者区分と同じく
> **税務判断そのもの**であり、「たまたまその年度のマスタが推奨している値」で
> 黙って動くのは D-057 が `is_taxable_business` について避けた事故と同型である。
> `JpSettingsOverrides` の型は変えていない（3つとも `Option` のまま）。
> 変わったのは**合成ルートが常に `Some` を詰める**という点だけで、
> `kaikei-jp` 側の契約には手を触れていない。

### 実装（PR-E）

- 検証は `config.rs`（`ServerConfig::from_env`）に閉じ、`main.rs` は
  「読めなければ起動しない」だけ。値の語彙（`exclusive` / `floor` /
  `line` / `calendar_year`）は `kaikei_jp::tax` と `kaikei_app::wire` の
  `from_code` を通す（同じ綴りの表を MCP 層で作らない。D-072）。
- **不足・不正は最初の1件で打ち切らず、全部まとめて返す**
  （1件ずつ潰させると12回起動し直すことになる。`CLAUDE.md` §11）。
- **空文字は「設定した」ことにしない**（`"KEY": ""` / `KEY=` は未設定と
  同じ扱いで起動を止める。ただし文言では区別する）。
- 真偽値は `true` / `false` のみ。`1` / `yes` / `on` は受けない
  （受け入れ方言が増えるほど「設定したつもりで効いていない」事故が増える）。
- `ServerConfig` の `Debug` は**接続文字列を伏せる**（§8。パスワードが平文で入る）。
- `kaikei_jp::compose` が返す `ComposeError` の日本語メッセージは、
  そのまま起動失敗の理由として stderr に出す（言い換えない）。
  **言い換えない代わりに、その値をどの環境変数から渡したかを後ろに足す。**
  `ComposeError` は `kaikei-jp` の語彙で書かれており、決算科目が見つからない
  場合の次の手として「正しい科目コードを `JpSoleProprietorClosingPolicy::new`
  に指定してください」という**利用者が触れない Rust の構築関数名**を提示する。
  `KAIKEI_CLOSING_ACCOUNT_CAPITAL` / `_OWNER_DRAWINGS` / `_OWNER_CONTRIBUTIONS`
  / `KAIKEI_CLOSING_TAX_CATEGORY` の**現在の値**を添えることで、どれを直せば
  よいかが対応付く（DB 接続の失敗が `APP_DATABASE_URL` を添えているのと
  同じ形。`CLAUDE.md` §11）。決算設定に由来しない失敗（同梱 YAML の破損等）
  には足さない——直す先が環境変数ではないため。
- **`KAIKEI_CLOSING_TAX_CATEGORY` は語彙も検証する。**
  この項目だけ「空でないこと」しか見ていないと、税区分マスタに存在しない
  コードでもサーバが正常に起動する（`KAIKEI_TAX_MODE=zeinuki` が
  「有効な値: exclusive, inclusive」で起動を中止するのと対照的だった）。
  合成ルートは `compose` の前に `TaxRuleSets::for_date(as_of)` →
  `TaxCategoryTable::categories()` と照合する（追加の I/O は無い）。
  エラーには**有効な値の一覧**を載せる（他の項目と揃える）。
  Phase 3 には `close_period` が無いので実害は出ないが、決算振替を実装した
  Phase で「起動は通るのに決算だけが落ちる」形になるため、本節の
  「起動時に検出して起動を中止する」に従う。
- `get_settings` は起動時に合成した `JpSettings` をそのまま返すツールであり、
  **未設定時に既定値を返すことはない**（そもそも起動していない）。

### 起動時に行うこと（すべて失敗したら起動を中止する）

| # | 内容 | 失敗したときに出る場所 |
|---|---|---|
| 1 | 事業者設定の検証（`config.rs`） | stderr。不足項目を全て名指し |
| 2 | `KAIKEI_CLOSING_TAX_CATEGORY` を税区分マスタと照合 | stderr。有効な値の一覧を添える |
| 3 | 同梱 YAML のロードと policy の構築（`kaikei_jp::compose::compose`） | stderr。`ComposeError` の文言をそのまま + 該当する環境変数名と現在の値 |
| 4 | `APP_DATABASE_URL` への接続 | stderr。**接続文字列そのものは出さない**（変数名だけ） |
| 5 | **接続ロールの権限検査**（§8） | stderr。保持している権限を名指しし、`kaikei_app` を指すよう案内する |
| 6 | **勘定科目マスタの投入**（追加のみ・冪等。D-081） | stderr。投入件数と、既存を優先した科目の差異 |

### `kept_existing` の出口を stderr だけにしない（**PR-G で実装済み**）

勘定科目マスタの投入は、DB の科目定義がテンプレートと食い違っていても
**既存を残して起動を続ける**（D-081）。記帳は DB の chart を正とするので
帳簿は自己整合しており、起動を中止する理由は無い。

しかし現状、その食い違い（`ImportChartOutput::kept_existing`）の**唯一の
出口が stderr** である。D-082 は「未設定を警告付きで既定値にする」案を
**「警告は stderr にしか出ず、AI にも利用者にも届かない（MCP クライアントが
サーバの stderr を表示する保証は無い）」**という理由で却下しており、
同じ理由がここにも当てはまる。

**PR-G の `get_settings` に「テンプレートと食い違っている科目」を載せること。**
起動時に組み立てた `ImportChartOutput::kept_existing` を `Runtime` に持たせ、
科目コードと相違フィールドを返す（`ChartDifference::describe` の文言をそのまま
使えばよい）。AI が `get_settings` を読めば、`list_accounts` が返す名称が
テンプレートと違う理由を自分で説明できるようになる。

→ **PR-G で実装した**（D-087）。合成ルート（`startup::assemble`）が
`imported.kept_existing` を `Runtime::chart_differences` に持たせ、
`get_settings` が `chart_differences` として返す（形は §3）。
**stderr への出力はやめていない**——起動時の診断としては引き続き出し、
出口を1つ増やしただけである。

`defaults_as_of`（`ComposeOptions`）には**起動時点の UTC 日付**を渡す。
「今日が何日か」の決定は presentation 層の責務であり、`kaikei-app` の
`SystemClock` は `Timestamp` までしか返さない（`CLAUDE.md` §7）。
この日付が影響するのは「その時点で有効な税区分マスタが同梱されているか」だけで、
事業者設定を全て明示必須にした結果、マスタの `settings_defaults` は1項目も
採用されない。**取引日はこれとは別物**で、常にツールの引数として渡ってくる。

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

**PR-E で実装した。** ただしロール名の文字列比較ではなく、
**守りたい性質そのもの**を見る:

```sql
SELECT current_user::text, t.table_name, p.privilege,
       has_table_privilege(t.table_name, p.privilege)
FROM UNNEST($1::text[]) AS t(table_name)      -- journal_entries, journal_lines
CROSS JOIN UNNEST($2::text[]) AS p(privilege) -- UPDATE, DELETE, TRUNCATE
```

**対象は `0003_journal.sql` の `REVOKE` と同じ集合**（2テーブル × 3権限）で
あること。`journal_entries` だけを見ると、`journal_lines` に権限を与えた
環境（明細だけを書き換えれば貸借も金額も変えられる）を見逃す。`TRUNCATE`
を見ないと、テーブルごと空にできるロールを見逃す。組み合わせは
`kaikei_store::pool::{JOURNAL_TABLES, FORBIDDEN_JOURNAL_PRIVILEGES}` から
導出しており、SQL 側に手で書き写さない。

1つでも保持していれば起動を中止する（`inspect_journal_privileges`
が生データを返し、**起動を止めるかどうかの判断は合成ルート**が行う。
「起動時に何を致命的とするか」は presentation 層の方針であって永続化層が
決めることではない）。ロール名を変えた環境や、`kaikei_app` に誤って
`GRANT` してしまった環境でも検出できる。

**この検査は「持っていないこと」を主張するため、見ている対象が狭くても
健全な環境では緑のまま通る。** そこで、実際に権限を与えて落ちることを
見る側を別に置く。

| 見るもの | テスト |
|---|---|
| `kaikei_app`（正常系）が通り、所有者ロールでは落ちる | `crates/kaikei-store/tests/chart_import.rs` の `import_chart_is_idempotent_against_a_real_database`（両ロールの `is_append_only` を直接比べる） |
| 所有者ロールの接続文字列を渡した**起動**が拒否される | `crates/kaikei-mcp/tests/startup_pg.rs` の `assembling_with_the_owner_role_is_refused`（落ちる側だけを見る。通る側は同ファイルの `assembling_twice_succeeds_and_leaves_the_chart_unchanged` が `kaikei_app` で最後まで通ることで示す） |
| `journal_lines` / `TRUNCATE` への `GRANT` が検出される | `crates/kaikei-store/tests/privileges.rs` の `inspect_journal_privileges_detects_a_grant_on_journal_lines` / `_detects_a_truncate_grant`（実際に `GRANT` を発行する） |

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

> **この節は PR-C で実装済み**（`DECISIONS.md` D-075〜D-077）。
> 残っているのは `kaikei-mcp` 側の結線（各ツールが
> `kaikei_app::audit::with_audit` を呼び、`input` / `output` の JSON を
> 組み立てること）だけである。
>
> | 実装 | 置き場 |
> |---|---|
> | テーブル・トリガ・権限 | `crates/kaikei-store/migrations/0009_audit_log.sql` |
> | ポート（trait） | `kaikei_app::ports::AuditSink` |
> | 記録する値と手順 | `kaikei_app::audit`（`RequestId` / `AuditCall` / `AuditStart` / `AuditResult` / `AuditOutcome` / `with_audit`） |
> | PostgreSQL 実装 | `kaikei_store::audit::PgAuditSink` |
> | 実 PostgreSQL のテスト | `crates/kaikei-store/tests/audit_log.rs` |
>
> **fail-closed / fail-open をツールごとに手で書かないこと。**
> 手順は `with_audit` に閉じてある（D-076）。MCP 層が書くのは
> 「入力・出力の JSON」と「`actor` / `tool`」だけである。

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
  `error` には **`kaikei_app::error::codes::AUDIT_LOG_UNAVAILABLE`
  （`"audit_log_unavailable"`）** を使う（PR-B 2巡目で語彙に追加）。
  `rejected` を借りない理由は §6 を参照。
- **結果レコードが書けなかった場合は操作を成功として返す（fail-open）。**
  結果に警告を添える（「記帳は完了しましたが監査ログの結果記録に失敗しました。
  request_id=... の行を確認してください」）。**再実行を促す文言にしない**
  （二重計上を招く）。
  警告を添える規律はツール側の手作業に残さない。`with_audit` の戻り値
  `AuditedCall` はフィールドが private で、結果を取り出す口は
  `into_result(&mut notes)`（警告を注記に積んでから結果を返す。**既定**）と
  `into_parts_unchecked()`（結果と警告を同時に返す。**逃げ道**）の2つだけである
  （D-076）。後者は `let (result, _) = ...` で警告を無言で捨てられる
  （`#[must_use]` はタプル分解に効かない）ので、
  **警告を自分で応答へ載せる責任を負う場合にだけ**使う。
  名前に `_unchecked` が入っているのはそのためで、逃げ道を使っている箇所は
  grep で全て出る。
  **`kaikei-mcp` は `into_result_noting_outcome`（既定経路）だけを使う**
  （PR-F。`into_parts_unchecked` はこの crate に1箇所も無く、
  `tests/audit_is_structural.rs` がそれを見張る）。積まれた警告は
  `dispatch::call` が応答の **`warnings`**（文字列の配列）に必ず載せる。
  成功応答にも失敗応答にも付き、警告が無ければキーごと出さない。
- **失敗した呼び出しには失敗用の文言を添える**（PR-F レビュー C-2）。
  拒否応答（`isError: true`。帳簿には1行も入っていない）に
  「操作は完了しましたが……操作は既に完了しているため、やり直さないで
  ください」が付くと、同じ応答が示している次の手（`hint` に従って入力を
  直して再送する）と**真っ向から矛盾**し、AI は訂正して再送すべきかを
  判断できない。`into_result_noting_outcome` は操作の成否で文言を切り替え、
  失敗時は「**この呼び出しは失敗しており、帳簿は変更されていません。……
  入力を直し、同じ操作を送り直してかまいません**」を積む
  （`AuditResultNotRecorded::public_message_when_operation_failed`）。
  成功時の文言は従来どおり（再実行を促さない）。
- **`warnings` は予約キーである。** ツールの応答本文で同じキーを使わない
  こと（`dispatch::call` が `debug_assert!` で毎回検査し、release では
  値を捨てずに併合する。実害が出るのは fail-open のときだけなので、
  正常系のテストで踏めるようにしてある）。
- 開始レコードだけが残り結果レコードが無い行は「**結果不明**」として読む。

### `output` に何を残すか（読み取り系は要約。D-089 決定6）

| 種別 | `audit_log.output` |
|---|---|
| 書き込み系（`post_journal_entry` / `reverse_journal_entry`） | **応答本文そのもの**（`lines` / `policy_notes` を含む） |
| 読み取り系（`search_entries` / `get_ledger`） | **要約**（本文から明細 `entries` / `rows` と `truncation_note` を落とした残り） |
| 失敗（`isError: true`） | **応答本文そのもの**（`hint` / `candidate_accounts` / `difference` を含む） |

読み取り系だけを縮めるのは、**監査ログにおける読み取りの目的が
「誰がいつ何を読んだか」**だからである。返した内容そのものは
(`input` の問い合わせ条件 + その時点の帳簿) から再現できる——
帳簿は追記のみなので過去の状態を再構成できる。書き込み系は逆で、
**結果そのものが変更の記録**なので縮めない。**この非対称は意図的である。**

要約に残るもの: 問い合わせ条件（`get_ledger` の `account` / `from` / `to`）、
期間全体の合計、件数（`total_matches` / `total_lines` / `returned` /
`has_more`）、読み終わった位置（`next_cursor`）。
**「何件のうち何件を、どこまで読んだか」は監査ログだけで追える。**

実測（`crates/kaikei-e2e/tests/mcp_search_ledger.rs` の
`a_read_records_a_summary_in_the_audit_log_while_a_write_records_the_whole_body`）:
`search_entries`（8件）は本文 3,307 バイトに対し `output` 49 バイト、
`get_ledger`（3行）は本文 1,327 バイトに対し `output` 262 バイト。
本文は件数に比例して伸びる（上限まで返すと1回で数十〜百数十 KB）が、
要約は件数に依らず一定である。

実装は `kaikei_mcp::dispatch::ToolSuccess::with_audit_summary`。
**要約は応答本文から落として作る**こと（別に組み立てると `total_matches` が
2箇所で計算され、応答と記録が食い違いうる）。

### 入力を理由に fail-closed へ落とさない

fail-closed は「監査ログが使えないなら操作しない」という規律であって、
「**記録しにくい入力なら操作しない**」ではない。`input` に
`{"description":"A\u0000B"}`（JSON としても JSON-RPC としても正当）が来ると、
PostgreSQL の `jsonb` は U+0000 を格納できず SQLSTATE `22P05` で拒否する。
これをそのまま fail-closed にすると、

1. 正常に動いている監査ログを「使えない」と**誤診**し、
2. 同じ入力で再試行する限り**永久に成功せず**、
3. AI には原因（自分が送った1文字）が**分からない**

という詰みになる（`CLAUDE.md` §11。D-038 が潰した誤診クラス）。

そこで `PgAuditSink` は**格納できない内容を無害化してでも記録を残す**（D-075）。

- U+0000 は U+FFFD に置換し、置換したときは `_audit.verbatim = false` と
  置換数を添えた封筒に包む（**原文どおりと読める形にはしない**）
- それでも **DB がその文を拒否した**場合は、`input` / `output` を
  「記録できなかった旨のプレースホルダ」に差し替えて1度だけやり直す。
  **誰がいつ何のツールを呼んだかは必ず残す**
- JSON として解釈できない文字列（presentation 層のバグ）も同じ扱い

**やり直すのは「DB が SQLSTATE 付きで拒否した」場合だけ**である。
接続断・プールのタイムアウトのような転送レベルの失敗ではやり直さない。
理由は2つある。

1. 転送レベルの失敗は**コミット済みか分からない**。1回目が実はコミットされて
   応答だけ失われていた場合、やり直すと同じ `request_id` の `started` 行が
   2行になる
2. 差し替え後に残る行は `reason: "not_storable_as_jsonb"` を名乗る。
   転送の失敗で差し替えると、**実入力を失ったうえに事実と違う理由**が
   記録に残る。監査ログは内容の忠実さが本体である

SQLSTATE を名乗れない失敗でも差し替えない（`sqlstate: null` の
「格納できなかった」は、後から読む側に何も伝えない）。

fail-closed に落ちるのは「**sink が本当に使えない**」場合
（権限剥奪・接続断、`actor`/`tool` が空という呼び出し側のバグ）だけである。
後者は再試行しても直らないので、`AuditLogUnavailable::public_message()` は
「時間をおいて再試行」ではなく「同じ内容で再試行しても成功しません」を返す。

### 帳簿側は U+0000 を「位置を添えて」拒否する（PR-F レビュー C-3）

監査ログ側の規律（記録できない入力でも**行は残す**）と、帳簿側の規律
（**保存できないものは受理しない**）は別である。`description` に U+0000 を
入れた記帳をそのまま下ろすと、`journal_entries` への INSERT が
SQLSTATE `22P05` → `RepoError::Corrupt` になり、その `public_message()` は

> この操作は完了していません。**入力を変えても解消しません**。
> サーバのログを添えて管理者に連絡してください

になる。**実際には入力の1文字を取り除けば通るので、断定が事実と逆**であり、
上と同じ誤診クラス（D-038）を帳簿側の経路で再演する。

そこで `dispatch::call` が `with_audit` の**操作の中**で入力を走査し、
U+0000 があれば **どこに入っているか**（`description` /
`lines[1].memo` / `lines[0].tags.counterparty` / キー名。何文字目か）を
添えて `rejected` で返す。実際の応答:

```json
{
  "error": "rejected",
  "field": "description",
  "message": "入力の description に制御文字 U+0000（NUL）が含まれています（2 文字目）。この文字は帳簿に保存できないため、post_journal_entry は実行していません。帳簿は変更されていません。該当箇所からその1文字を取り除いて送り直してください（他の箇所は直す必要がありません）"
}
```

操作の中で判定するので、**この呼び出しも `audit_log` に2行残る**
（`input` は `PgAuditSink` が U+FFFD に置換して記録する。上記 MC-31 と同じ）。

### `tool` / `actor`（TEXT 列）は無害化を通らない ★PR-F への申し送り

上の無害化が掛かるのは **JSONB 列（`input` / `output`）だけ**である。
`tool` / `actor` は TEXT 列にそのまま入り、差し替えのやり直しも
`input` / `output` しか触らないので**救済されない**。

PR-C の時点では両方ともサーバ側の定数なので実害は無い。しかし
**`tools/call` の `name` はクライアント（AI）由来**であり、受け取った名前を
そのまま `AuditCall.tool` に載せる実装にすると、`tool` に U+0000 を1文字
入れられた時点で開始レコードが書けず fail-closed になる——
D-075 が JSONB 側で塞いだ「**入力1文字で操作が実行できない**」が TEXT 側で
そっくり再発する。

> **`AuditCall.tool` にはレジストリ（`tools/list` に出す11ツール）に
> 登録済みのツール名以外を載せない。未知の名前は audit に載せる前に弾く。**
> `actor` は `kaikei_app::audit::actor` の定数（`mcp` / `cli` / `api`）だけを使う。

**PR-F で実装した（D-084）。担保は「1箇所に閉じ込める走査」＋「その1箇所での
型」の重ね合わせである。**

`AuditCall` を組み立てるのは `crates/kaikei-mcp/src/dispatch.rs` の
`dispatch::call` 1箇所だけで、そこは `dispatch::ToolName` を通す。
`ToolName` はフィールドが private で、唯一の構築子 `ToolName::resolve(name)` は
`crate::server::is_registered_tool`（＝`tools/list` が返すのと同じ
レジストリ）を通る。**登録済みの名前以外から `ToolName` を作る方法が存在
しない**ので、`tool` 列に入るのは常にサーバが知っている有限個の文字列になる。
`tool` に U+0000 を1文字入れた名前は `resolve` が `None` を返す
（`dispatch.rs` の `a_nul_character_in_the_tool_name_never_reaches_the_audit_log`）。

> **提供していない保証を書かない**（PR-F レビュー C-6）。初版はここで
> 「`AuditCall.tool` に渡せる型を `ToolName` に限定した」と書いていたが、
> `AuditCall` は凍結済みの `kaikei-app` にあり、そのフィールドは
> `pub tool: &'a str` である（PR-F で `kaikei-app` のこの型は変えていない）。
> `dispatch.rs` の中で `tool: T::NAME` と直接書くことは型としては
> 妨げられていない。実際の担保は
> **(1) `AuditCall` という識別子を `dispatch.rs` に閉じ込める走査**
> （`tests/audit_is_structural.rs`）と
> **(2) その1ファイルの中で `ToolName::resolve` を通す慣行**であり、
> 型で閉じているのは「`resolve` が登録済み以外を弾く」ことである。

**未知のツール名はプロトコルエラーで返る（§6 が認めている唯一の例外）。**
初版はここで「`isError: true` + `rejected` で返す」と書いていたが、
`rmcp` の `ToolRouter` は未登録名をハンドラに**到達させず**
`invalid_params: tool not found` を返す。§6・§2（MC-10）および PR-D の
`server.rs` の doc は最初からそう書いており、**この節の記述だけが食い違って
いた**ので §6 に合わせる。この節が本当に守りたいのは
「`audit_log.tool` にクライアント由来の文字列を載せない」であり、
それは上記のとおり型で保証されている。

### 42501 を帳簿と同じ分類のまま返さない

`REVOKE INSERT ON audit_log FROM kaikei_app` は SQLSTATE `42501` を返し、
`crates/kaikei-store/src/sqlstate.rs` は**関与テーブルを見ない**ため
これを `RepoError::AppendOnlyViolation` に写す。その `public_message()` は
「訂正は逆仕訳（`reverse_journal_entry`）で行ってください」であり、
監査ログに対しては的外れである。`AuditLogUnavailable::cause` は pub なので、
MCP 層が診断のつもりで `cause.public_message()` を出せば誤案内が復活する。

`kaikei_store::audit` がこの経路の `42501` を `RepoError::Backend` に
**包み直す**（共通写像は帳簿側に波及するので触らない）。
`crates/kaikei-store/tests/audit_log.rs` が実際に `REVOKE` して、
`cause` 側にも「逆仕訳」が現れないことを確認している。

### 書き込み経路

素直に書くと同一トランザクションになるので、経路を明示する。

- リポジトリはすべて `&mut Tx` 経由（`TxOps`。D-029）で、`with_tx` が
  commit/rollback を握っている。**`TxOps` に audit 用メソッドを生やすと必ず
  同一トランザクションになり、上の目的が消える。**
- `kaikei-app/src/ports.rs` に `TxOps` とは独立した監査ログ用ポート
  **`AuditSink`**（`&self` で `PgPool` から別コネクションを acquire する）を新設し、
  実装は `kaikei-store`（`kaikei_store::audit::PgAuditSink`）に置いた
  （§4「ビジネスロジックを MCP 層に書かない」と整合）。
- 呼ぶのは `with_tx` の**外側**（開始レコード → `with_tx(...)` → 結果レコード）。
  この順序と fail-closed / fail-open は **`kaikei_app::audit::with_audit`** に
  閉じてある。MCP 層で組み立て直さないこと（D-076）。
- **1リクエストが帳簿と監査ログの接続を同時に保持することは無い。**
  `with_audit` は `record_start` → `with_tx` → `record_result` の順で、
  どの時点でも保持している接続は1本である（`connect_app` の
  `max_connections` は 10）。**同時に2本保持している状態を観測したら、
  それは「`with_tx` の内側から監査ログを書いている」証拠**であり、
  上で禁じている形への退行を疑うこと（プール枯渇より先にこちらを見る）。

### スキーマ

実装は `crates/kaikei-store/migrations/0009_audit_log.sql`。

```sql
CREATE TABLE audit_log (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    request_id  UUID NOT NULL,        -- ツール呼び出しごとにサーバが採番
    occurred_at TIMESTAMPTZ NOT NULL, -- Clock から取得した値を明示的に渡す
    actor       TEXT NOT NULL CHECK (btrim(actor) <> ''),  -- "mcp" / "cli" / "api"
    tool        TEXT NOT NULL CHECK (btrim(tool) <> ''),
    status      TEXT NOT NULL CHECK (status IN ('started', 'ok', 'error')),
    input       JSONB,                -- 開始レコードのみ
    output      JSONB,                -- 結果レコードのみ。確定後明細と PolicyNote
    error_code  TEXT,
    entry_id    UUID,                 -- 外部キーは張らない（下記）
    -- status と error_code の対応（kaikei_app::audit::AuditOutcome が型でも保証する）
    CHECK ((status = 'error') = (error_code IS NOT NULL)),
    CHECK (status <> 'started' OR output IS NULL)
);

CREATE INDEX idx_audit_log_request  ON audit_log (request_id);
CREATE INDEX idx_audit_log_occurred ON audit_log (occurred_at);
```

- **`status` と `error_code` の対応は型と CHECK の二重で守る**（PR-C で追加）。
  `kaikei_app::audit::AuditOutcome` は `Succeeded { output_json }` /
  `Failed { error_code, public_message }` の2バリアントで、
  「成功なのに `error_code` がある」「`'started'` を結果レコードに書く」を
  **構築できない**。DB 側の CHECK は、ポートを経由しない直接 INSERT
  （将来の移行スクリプト等）に対する最後の砦である。

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
- **`error_code` には分類コードだけを入れる**（§6 の対応表の値。`AppError::code()`）。
  AI に返した本文は `output` に入れる。
- **失敗の結果レコードの `output` にも、AI に返した応答本文をそのまま載せる**
  （PR-F レビュー C-4）。成功時は応答 body 全体が `output` に入るのに、
  失敗時が `{"message": ...}` だけだと、`hint.suggested_lines` /
  `candidate_accounts` / `difference` / `policy_notes` / `line` が記録に
  残らない。**とくに `hint` は AI の次の記帳内容を直接決める提案**であり、
  「サーバが何を返して次の一手を誘導したか」を後から追う（D-070）目的に
  対して失敗側だけ情報が薄くなる。
  組み立てるのは presentation 層
  （`AuditableError::audit_output_json`。`kaikei-mcp` の `ToolFailure` が
  `ToolError::to_json()` を返す）。`kaikei-store` は
  `output_json` があればそれを、無ければ `public_message` だけを載せる。
  **どちらも `Display` は経由しない**（次項）。
- **`output` に入れる本文は `AppError::public_message()`**（PR-B 2巡目）。
  `Display`（`to_string()`）を転記しないこと。`RepoError::Backend { reason }` /
  `Corrupt` 等には **DB が返した文字列がそのまま入っており**
  （`crates/kaikei-store/src/sqlstate.rs` の `format!("...: {message}")`）、
  接続文字列・ロール名・制約定義が混じりうる。
  `public_message()` が該当バリアントを分類コードと汎用メッセージに
  正規化する（対応表は §3「`message` に載せるのは `public_message()`」）。
  **失われた詳細は `Display` 側に残っている**ので、サーバのログ（stderr）には
  そちらを出す。この使い分けは §3 の `message` と同じ規律である。

### append-only の強制

**「append-only。」と書くだけでは守られない。** 帳簿本体と同じ4点セットで守る。

1. `GRANT SELECT, INSERT ON audit_log TO kaikei_app;`
   `REVOKE UPDATE, DELETE, TRUNCATE ON audit_log FROM kaikei_app;`
2. **`reject_mutation()` を流用しない。** audit_log 専用のトリガ関数
   `reject_audit_log_mutation()` を新設した（行トリガ + TRUNCATE の
   STATEMENT トリガ）。流用すると例外メッセージが「append-only table: % は
   変更できません（訂正は逆仕訳で行ってください）」になり、監査ログに
   対しては的外れな案内になる（監査ログは逆仕訳で直すものではない）。
   これも D-038 と同じ誤診クラス。
   実際の文言:「監査ログ（%）は追記のみです。記録の訂正は新しい行の追加で
   行ってください」。
3. **専用 ERRCODE `P0012`**（`P0010` / `P0011` の次）。
   `kaikei-store/src/sqlstate.rs` の写像表に追加済みで、
   **`AppendOnlyViolation` には寄せていない**（写像先は `RepoError::Backend`。
   `RepoError` は `#[non_exhaustive]` ではなくバリアント追加が破壊的変更に
   なるため、専用バリアントは作らなかった。理由は D-075 と
   `sqlstate.rs` の doc）。
4. `crates/kaikei-store/tests/privileges.rs` に audit_log の権限マトリクスを、
   `tests/append_only.rs` に実際に UPDATE / TRUNCATE を発行して
   `42501` / `P0012` を確認するテストを追加済み（MC-23）。

`docs/03-database.md` §1 の GRANT 例にも audit_log を追加済み。

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
- **`kaikei-mcp` の `pg-tests` は使い捨てDBを作れない。** この crate は
  `sqlx` に依存しない（MC-30 の許可リスト）ため `#[sqlx::test]` が使えず、
  `APP_DATABASE_URL` が指す DB をそのまま使う。したがって**書き込むのは
  勘定科目マスタだけ**（追加のみ・冪等）に留め、仕訳は書かない。
  実 DB に仕訳を書くテスト（記帳が通ること・試算表・決算）は
  `kaikei-e2e` 側に置く（あちらは `#[sqlx::test]` でテストごとに
  使い捨てDBを作れる）。
- **書き込み系ツールの実 DB テストは
  `crates/kaikei-e2e/tests/mcp_write_tools.rs`**（PR-F で新設）。
  `audit_log` を SELECT するにも SQL が要るので、`kaikei-mcp` 側には置けない。
  依存の向きは `kaikei-e2e` → `kaikei-mcp` であり、
  「`kaikei-e2e` は誰からも依存されない」（D-068）は保たれる。
  そこで組み立てる `Runtime` は本番の `startup::assemble` と**同じ部品**
  （`kaikei_jp::compose` / `PgStore` / `PgAuditSink`）から作る
  （`assemble` 自身は `APP_DATABASE_URL` を読むので使い捨てDBを指せない）。
  ツールは `dispatch::call`（ルータのハンドラが呼ぶのと**同じ関数**）を
  直接呼ぶ——`rmcp::service::Peer::new` が `pub(crate)` で `RequestContext` を
  外部 crate から組み立てられないため（MC-10 の注記と同じ理由）。

### 書き込み系

**PR-F で実装済み。** 置き場は `crates/kaikei-e2e/tests/mcp_write_tools.rs`
（実 PostgreSQL・使い捨てDB）と、入力 DTO の単体検査が
`crates/kaikei-mcp/src/tools/*.rs`。

| # | ケース | 期待 |
|---|---|---|
| MC-01 | 貸借一致の仕訳を post | 成功。確定後明細が返る |
| MC-02 | 貸借不一致の仕訳を post | エラー。`error` は `unbalanced`、`message` に差額が含まれる。金額フィールドは**区切り無し**（§5）。`auto_tax_lines: true` で `derive_tax_lines` が注記を返していた場合、**失敗応答にも `policy_notes` が入る**（PR-B 2巡目。app 層の回帰テストは `post_entry_carries_policy_notes_on_the_failure_path`）。`hint` の組み立て（`preview` を呼ぶ）はこの PR で追加する（§3） |
| MC-03 | `auto_tax_lines: true` で税行が自動追加（**税抜経理・課税事業者の設定**） | 貸借一致する。税込経理・免税事業者の設定では税行が生成されず貸借不一致になることも併せて確認する |
| MC-04 | 存在しない科目コード | エラー。`hint` には全件ではなく候補を絞って返す（コードの前方一致・名称の部分一致など。件数の上限を決める）。`hint` を組み立てるのは MCP 層（`ChartRepo::load_chart` の結果から作る）で、core のエラーは候補を持たない |
| MC-05 | 締め済み期間への post | エラー。fixture は `period_snapshots` に**直接 INSERT** して締め状態を作る（Phase 3 に `close_period` は無い。`kaikei_app` は `period_snapshots` に INSERT 権限を持つ） |
| MC-02b | 税込経理・免税事業者の設定で貸借不一致（PR-F レビュー C-1） | 差額だけを返さない。`auto_tax_lines: true` なら `policy_notes` に、`false` なら `hint.policy_notes` に「税込経理の設定のため税額行を生成していません」（`kaikei-jp` が組み立てた文言）が入る。`an_unbalanced_entry_under_tax_inclusive_settings_explains_why_no_tax_lines_appear` |
| MC-02c | `description` に U+0000 を含めて post（PR-F レビュー C-3） | `rejected` で拒否し、**どこに入っているか**（欄と何文字目か）を添える。「入力を変えても解消しません」と誤診しない。帳簿は0件で `audit_log` には2行。同じ入力から1文字取り除けば記帳できる。`a_nul_character_in_the_description_points_at_the_offending_character` |
| MC-12 | `reverse_journal_entry` の理由が省略・空文字・空白のみ | 3ケースともエラー（`error` は `empty_reverse_reason`）。**検証は `kaikei-app` のユースケース層にある**（PR-B で実装。`AppError::EmptyReverseReason`）。MCP 層は写像するだけで、同じ検証を重ねて書かない。「省略」だけは DTO のデシリアライズで弾かれる（`reason` は `Option` にしない）。app 層側の回帰テストは `crates/kaikei-app/src/usecase/reverse_entry.rs` の `reverse_entry_rejects_blank_reason`（全角スペース U+3000 を含む7ケース） |

### 提案系・検証系

| # | ケース | 期待 |
|---|---|---|
| MC-08 | `suggest_tax_category` | (1) 根拠が空でない、(2) 呼び出しの前後で**帳簿が一切変わらない**（試算表と仕訳件数が不変。§1 ② の機械的検証）。**PR-G で実装**: 候補ごとの `reason` は `crates/kaikei-mcp/src/tools/suggest_tax_category.rs` の `every_candidate_carries_a_non_empty_reason`、帳簿が動かないことは `crates/kaikei-e2e/tests/mcp_stdio_server.rs` の `the_read_tools_answer_through_the_real_binary_and_are_audited`（読み取り・提案を8回呼んで `journal_entries` が1件のまま）。**断定しないこと**は `the_response_does_not_single_out_one_category_or_rank_them`（順位・信頼度のキーが無い）と `the_description_is_echoed_but_never_used_to_filter`（摘要で候補が変わらない）が見る（D-087）。**根拠が帳簿の設定を落としていないこと**は `the_book_settings_that_decide_whether_tax_lines_appear_are_reported_back`（免税事業者の帳簿で `filtered_by` が変わり、`candidates` は変わらない。PR-G レビュー C-1） |
| MC-28 | `validate_invoice_number` の出力 | 形式検証の結果のみを述べ、「実在する／実在しない」と断定しない（`CLAUDE.md` §10）。**PR-G で実装**: `crates/kaikei-mcp/src/tools/validate_invoice_number.rs` の `a_well_formed_number_reports_the_format_only_without_claiming_it_exists`（`not_checked` が空でないこと・断定に読める語が無いこと・キー名が `valid` でないこと）と `each_failed_check_has_its_own_error_code`（D-053 の4観点） |

### 読み取り系

| # | ケース | 期待 |
|---|---|---|
| MC-13 | `list_accounts` | 科目種別（`account_type`）と**記帳可否（`postable`）**を含めて返す。**PR-G で実装**: `crates/kaikei-mcp/src/tools/list_accounts.rs` の `every_account_carries_its_type_and_whether_it_can_be_posted_to`（並びが科目コード昇順であることも見る）と `postable_only_hides_the_headings_and_says_so_in_the_response`。実バイナリ経由は `crates/kaikei-e2e/tests/mcp_stdio_server.rs`。**同梱テンプレートの科目は現時点で全て記帳可能**なので、見出し科目を含む絞り込みの検査は単体側にある |
| MC-14 | `get_entry` | 存在する仕訳の明細・タグを返す。存在しない ID では次の手が分かる NotFound を返し、**仕訳IDは UUID の正準表記**で示す（10進表記にしない。§3。`reverse_journal_entry` 側は PR-B で解決済み）。**PR-G で実装**: `crates/kaikei-mcp/src/tools/get_entry.rs` の `a_missing_entry_is_reported_as_not_found_with_the_canonical_uuid`（10進表記が漏れていないことと、`Display` の「見つかりません」が二重にならないことを見る）／`a_reversal_entry_points_at_the_original_and_carries_the_reason`。read model は新設していない（`JournalRepo::find_entry`。D-086）。**訂正済みの仕訳が未訂正のものと区別できること**は `an_entry_that_has_already_been_reversed_says_so_with_the_reversal_id`（単体）と `crates/kaikei-e2e/tests/mcp_stdio_server.rs` の `reversing_through_the_real_binary_goes_through_the_same_audited_path`（実バイナリで訂正前後の応答を突き合わせる。PR-G レビュー B） |
| MC-15 | `get_trial_balance` | 期間で絞り込める。`group_by` 指定が効く。借方合計＝貸方合計。`from > to` はエラー（空結果にしない）。**PR-G で実装**: 実 DB は `crates/kaikei-e2e/tests/mcp_stdio_server.rs` の `the_read_tools_answer_through_the_real_binary_and_are_audited`（`group_by: ["tax_category"]` が効くこと）と `the_read_tools_tell_an_empty_result_apart_from_a_bad_request`（0行の期間は成功・`from > to` は `rejected`・未登録のタグキーは `unknown_tag_key`・登録済みだが非集計のキーは `not_aggregatable`）。応答の詰め替えと2種類の拒否の区別は `crates/kaikei-mcp/src/tools/get_trial_balance.rs` の単体検査（`an_unregistered_key_is_not_reported_as_a_non_aggregatable_one` / `the_description_lists_exactly_the_aggregatable_keys`。PR-G レビュー C-2） |
| MC-16 | `search_entries` | 日付・金額・科目・摘要・タグで絞り込める。**0件でも成功として空配列を返す**（エラーにしない）。**PR-H で実装**: `crates/kaikei-e2e/tests/mcp_search_ledger.rs`（実 DB・`dispatch::call` 経由）と `crates/kaikei-store/tests/search_ledger_differential.rs`（read model の差分）。上限で切ったことが応答から分かること（**呼び出し元の `limit` で切れたのかサーバの上限で切れたのかを含む**）・カーソルで続きを最後まで辿れること・取り消された仕訳が判別できること（D-088 / D-089）もここで見る。**二重訂正された仕訳が1件だけ返ること**（土台に `allow_double_reversal` で2回訂正した仕訳を1件置いてある。素朴な `LEFT JOIN` に戻すと差分が7本落ちる）、**未登録の科目コードが `not_found` になること**、**`audit_log.output` が要約であること**（D-089 決定6）も見る |
| MC-17 | `get_ledger` | 科目別に借方・貸方・残高を返す。期間指定が効く。**PR-H で実装**（同上）。合計が**ページではなく期間全体**の値であること、`running_balance` がページをまたいで連続すること、**0行の期間は成功で未登録の科目コードは `not_found`** であることを併せて見る。**`from` / `to` に明細のある日を取ったケースで4つの合計を対照実装と突き合わせる**（`the_totals_include_the_lines_dated_exactly_on_from_and_to`。`>= from` を `> from` に書き換えると落ちることを実測）。**二重訂正された仕訳の行が1行だけ**であること、赤伝の行に `reverse_reason` が付くことも見る |
| MC-18 | `list_tax_categories` | 指定日時点で有効な区分のみを返す。取引日で切り替わる（D-050 / D-055）。該当マスタが無い日付では**エラー**で、メッセージに有効期間が含まれる。**PR-G で実装**: `crates/kaikei-mcp/src/tools/list_tax_categories.rs` の `the_categories_valid_on_the_given_date_are_returned_with_their_source_table` / `a_date_outside_the_embedded_masters_is_an_error_that_shows_the_available_range`。**収録外になるのは開始日より前**である（同梱マスタの `applies_to` は未指定＝未来側は開いている） |
| MC-19 | `get_settings` | 起動時に合成した `JpSettings`（税抜/税込・端数処理・端数処理単位・課税事業者区分・簡易課税）をそのまま返す。日付引数を取らない。**PR-G で実装**: `crates/kaikei-mcp/src/tools/get_settings.rs` の `the_composed_settings_are_returned_with_their_machine_readable_codes` / `the_input_takes_no_arguments` / `accounts_kept_from_the_database_are_reported_in_the_response`（§7 の申し送り。D-087）／`the_chart_differences_say_when_they_were_observed`（`as_of: startup`。PR-G レビュー C-3） |

### プロトコル・契約

| # | ケース | 期待 |
|---|---|---|
| MC-09 | 金額を number で渡す | **エラー（整数でも受理しない）。** (1) メッセージが「金額は文字列で渡してください。例: `"amount": "110000"`」という次の手を含む、(2) `isError: true` のツール結果として返る、(3) この呼び出しも audit_log に残る。金額フィールドを素の `String` にするだけでは (1) を満たせない（§5 の実装形を参照）。**PR-D で (1) を実装**（`kaikei_mcp::wire::AmountStr`。`crates/kaikei-mcp/src/wire.rs` のテスト）。**(2)(3) は PR-F で実装**——`rmcp` の `Parameters<T>` に任せると (2) は満たせても (3) が満たせない（デシリアライズがツール本体に入る前、＝ `dispatch::call` の外側で走るため audit_log に1行も残らない）。そこで `dispatch::call` が `with_audit` の**操作の中**でデシリアライズし、失敗を `rejected` の構造化エラーとして返す（D-085）。実 DB での確認は `crates/kaikei-e2e/tests/mcp_write_tools.rs` の `an_amount_given_as_a_json_number_is_rejected_in_japanese_and_still_audited` |
| MC-10 | 存在させないツール4件 | `tools/list` の応答に `delete_journal_entry` / `update_journal_entry` / `execute_sql` / `reopen_period` のいずれも現れず、それらの名前で `tools/call` すると未知ツールとして拒否される。**禁止リストをテスト側の定数にして4件すべてをループで検査する**（1件だけの検査では他が復活しても緑のまま通る）。**PR-D で実装済み**（`crates/kaikei-mcp/tests/forbidden_tools.rs`）。検査はレジストリ（PR-F 以降は `dispatch::ToolRegistry`）から導出しており、`tools/list` が返す集合と同一。`tools/call` 側は `get` と `has_route` で見る（`rmcp::service::Peer::new` が `pub(crate)` で `RequestContext` を外部 crate から組み立てられないため、`call` を直接叩けない）。**PR-E でサーバーを組み立てずに検査する形に変えた**——`KaikeiServer` は実行時依存（`Runtime`）を必須で持つようになったので、レジストリを見るために DB 接続を要求しないよう `kaikei_mcp::server::{registered_tool_names, is_registered_tool, tool_definition}`（サーバー本体と同じ `tool_registry()` から導出する自由関数）を通す。それが**本物のサーバー**（`ServerHandler::get_tool`）と同じ集合を指すことは `tests/startup_pg.rs` が突き合わせる。**許可リスト側（Phase 3 の11件）からも閉じてある**——禁止4件だけを見張ると新しい名前の破壊的ツールが素通りする |
| MC-26 | ドメインエラー（貸借不一致等） | JSON-RPC のプロトコルエラーではなく `isError: true` のツール結果として返る（D-071） |
| MC-27 | 出力の金額 | 全て JSON 文字列である（入力だけでなく出力側も number にしない。§5） |
| MC-24 | 事業者設定（`is_taxable_business` / `simplified_taxation` 等）を与えずに起動 | 既定値にフォールバックせず**起動が失敗**し、不足項目を名指しするメッセージが出る（§7。D-057）。**PR-E で実装済み**: `crates/kaikei-mcp/tests/startup_config.rs`。**必須項目を1つずつ外して総当たりで**（1項目だけの検査では他が既定値に落ちても緑のまま通る）、実際に `cargo` がビルドしたバイナリを子プロセスとして起動して確かめる——`config.rs` の単体テストだけでは「その検証が起動経路に繋がっているか」を見られない。単体側（`ServerConfig::from_lookup`）にも同じ総当たりがある。**必須項目の一覧（`REQUIRED_ENV_VARS`）は `.env.example` と `README.md` にも現れることを `include_str!` で検査する**——`ConfigError` の本文がその2つを一次情報として名指ししているので、実装とテストが緑のまま `.env.example` だけが欠けると誘導先が嘘になる（D-047 と同型） |
| MC-29 | 未登録のタグキー（例: `tax_cat`）で post | **エラー**（黙って落とさない）。メッセージに**有効なタグキー一覧**が含まれる（§3。`CLAUDE.md` §4・§11）。型に合わない値（`business_ratio: "3割"`）も同様に、期待する書式を示すエラー |
| MC-30 | `kaikei-mcp` の依存 | `Cargo.toml` が `kaikei-app` / `kaikei-jp` / `kaikei-core` / MCP SDK / 非同期ランタイム / シリアライズ以外に依存していない（`uuid` / `rust_decimal` を自前で足していない）。**CI で機械的に検査する**（§4 の申し送り。プローブでは検査できない）。**PR-D で実装済み**: 既存ジョブ `dependency-direction` への**ステップ追加**（新しいジョブではないので必須チェックの登録は不要。`CLAUDE.md` §13）。依存の取得は `cargo metadata --no-deps` の `packages[].dependencies[].name`（`cargo tree` はホストターゲットの依存しか見ず、`[target.'cfg(...)'.dependencies]` を素通りする）、許可リストとの照合は `case` による完全一致（`grep -qw` は `kaikei` のような成分名を通す）。`uuid` を normal / dev / target-cfg のいずれで足しても、また `kaikei` という名前の crate を足しても落ちることを実測した（照合部分は ubuntu:24.04 コンテナでも同じ結果を確認）。`jq` の出力は `tr -d '\r'` を通す——Git Bash の `jq.exe` は stdout をテキストモードで開くため、これが無いと Windows の開発機では**健全な状態でも全依存が「禁止された依存」になる**（手元で回せない検査は回されなくなる）。許可リストは「足してよい上限」であって「足さねばならない一覧」ではないので、実際の `Cargo.toml` と一致していなくてよい（PR-D 時点では `tokio` を宣言していない。使うのは合成ルートを置く PR-E） |

### 起動と合成ルート（**PR-E で追加**）

| # | ケース | 期待 |
|---|---|---|
| MC-33 | 起動しても **stdout に JSON-RPC 以外が出ない** | 3段で見る。(1) `crates/kaikei-mcp/src/` に `println!` / `print!` / `io::stdout` が現れない（`tests/stdout_is_json_rpc_only.rs`。**`eprintln!` を `println!` と取り違えない**ことも検査する——取り違えると「stderr に出せ」という指示に従ったコードが落ちる）、(2) 起動に失敗したとき stdout が**1バイトも**出ない（`tests/startup_config.rs`。設定不足・値の不正・決算設定の誤りをバイナリの起動で確かめる。**「DB に繋げない」ケースだけは合成ルートを直接呼ぶ**——到達しない接続先に対して `sqlx` のプールは接続確保の待ち時間が満了するまでリトライするので、既定の 30 秒のままだとこの1件だけで `cargo test --workspace` が 30 秒伸びる。待ち時間は `kaikei_store::pool::connect_app_with` に渡す。**本番の待ち時間は縮めない**）、(3) 実際に起動して `initialize` を送ったとき、stdout に出た行が JSON-RPC のメッセージである（`tests/startup_pg.rs`。`pg-tests`） |
| MC-34 | 勘定科目マスタの投入が**冪等** | 2回流しても2回目は1行も追加されない。既存と定義が異なる科目は**上書きせず既存を残し**、差異を診断として返す（D-081）。単体は `crates/kaikei-app/src/usecase/import_chart.rs`、実 DB は `crates/kaikei-store/tests/chart_import.rs` と `crates/kaikei-e2e/tests/e2e_jp.rs` |
| MC-35 | 投入した後に**記帳が通る** | 「構築は通るが記帳できない」（`PROGRESS.md` Phase 2 の教訓2）への回帰検知。`crates/kaikei-e2e/tests/e2e_jp.rs` の `chart_import_is_idempotent_and_posting_still_works`。同ファイルの既存テストも `seed_chart` が本番の投入経路を呼ぶ形になったので、全て同じ経路を通っている |
| MC-36 | `APP_DATABASE_URL` に `kaikei_migrator` を渡して起動 | 起動を**中止**する（§8）。`assembling_with_the_owner_role_is_refused`（`crates/kaikei-mcp/tests/startup_pg.rs`）は**落ちる側だけ**を見る。通る側（`kaikei_app` なら最後まで組み立つ）は同ファイルの `assembling_twice_succeeds_and_leaves_the_chart_unchanged` が、両ロールの権限の差そのものは `crates/kaikei-store/tests/chart_import.rs` の `import_chart_is_idempotent_against_a_real_database` が持つ |
| MC-37 | `KAIKEI_CLOSING_TAX_CATEGORY` に税区分マスタに無いコードを渡して起動 | 起動を**中止**し、有効な値の一覧を出す（§7）。`crates/kaikei-mcp/tests/startup_config.rs` の `an_unknown_closing_tax_category_aborts_the_startup_listing_the_valid_codes`。DB には到達しない（この検証は接続より前） |
| MC-38 | 決算科目が勘定科目表に無い状態で起動 | `ComposeError` の文言をそのまま出したうえで、**どの環境変数の現在値が原因か**が分かる（§7）。`crates/kaikei-mcp/tests/startup_config.rs` の `a_closing_account_that_is_missing_from_the_chart_names_the_environment_variable` |
| MC-39 | `journal_lines` / `TRUNCATE` に `GRANT` した環境で起動 | 権限検査が**検出する**（§8）。`crates/kaikei-store/tests/privileges.rs` が実際に `GRANT` を発行して確かめる（`journal_entries` の UPDATE / DELETE しか見ていなかった初版はこれを素通りした） |

### 監査ログ

| # | ケース | 期待 |
|---|---|---|
| MC-11 | 全ツール呼び出しが audit_log に記録される | 1回のツール呼び出しにつき**同一 `request_id` で2行**（`status='started'` と `status='ok'\|'error'`）が残る。`tool` 列が呼び出したツール名と一致する。書き込み系は結果レコードの `entry_id` が返した仕訳IDと一致する。**Phase 3 の全11ツールに対して総当たりで確認する**（1ツールだけのサンプル検査にしない）。**PR-F で書き込み系2件を実装**（`crates/kaikei-e2e/tests/mcp_write_tools.rs`）。**PR-G で読み取り系・提案系7件**（`crates/kaikei-e2e/tests/mcp_stdio_server.rs`。呼び出した順に `(tool, status)` の対を突き合わせる）。**PR-H で残る読み取り系2件**（`search_entries` / `get_ledger`。`crates/kaikei-e2e/tests/mcp_search_ledger.rs` と、実バイナリ経由の `tests/mcp_stdio_server.rs`）。**これで 2 + 7 + 2 = 11 件が揃った。** なお**「総当たり」の性質そのものは PR-F で大部分が構造に移った**——ツールをレジストリに載せる経路が `ToolRegistry::with::<T: McpTool>` の1本しか無く、その中身が必ず `dispatch::call`（監査ログで挟む）である。ただし「監査ログを通らないツールを書けない」を成立させているのは**型だけではない**（`rmcp` を直接名指しすれば別のルータを作れる）。そこを閉じているのは `rmcp` を名指しできるファイルの許可リスト（`crates/kaikei-mcp/tests/audit_is_structural.rs` の `rmcp_is_named_only_in_the_files_allowed_to_name_it`。D-084 の訂正注記3）である。総当たりのテストは、その構造が守られていることを実 DB で追認するものになった |
| MC-20 | 開始レコードの書き込みが失敗する状況（audit 用接続を落とす、または `REVOKE INSERT ON audit_log FROM kaikei_app`）で post | ツールは `isError: true` を返し、**帳簿には1件も入っていない**（fail-closed） |
| MC-21 | 結果レコードの書き込みだけが失敗 | 記帳は成功として返り、警告が添えられる。開始レコードだけが残り「結果不明」として識別できる（fail-open） |
| MC-22 | 記帳が失敗（貸借不一致等）してトランザクションが rollback される | **開始レコードは audit_log に残る。** D-070 の存在理由そのもの（同一トランザクションで書く実装に退行したら、このテストだけが落ちる） |
| MC-23 | `kaikei_app` の audit_log への権限 | `SELECT` / `INSERT` はでき、`UPDATE` / `DELETE` / `TRUNCATE` はできない（`crates/kaikei-store/tests/privileges.rs` の `assert_privilege` と同じ形で追加） |
| MC-25 | 経過措置対象の税区分（`PURCHASE_10_NON_QUALIFIED`）で記帳 | `PolicyNote` が post の戻り値に含まれ、かつ audit_log の `output` にも残る（D-059 / D-070）。app 層まで運ばれることは PR-B で担保済み（`crates/kaikei-e2e/tests/e2e_jp.rs` の `condition_3_*` が `PostEntryOutput.notes` を検証する）。MCP 層に残るのは「応答と audit_log に載せる」部分 |
| MC-31 | `input` に U+0000 を含む JSON（`{"description":"A\u0000B"}`）で post | **操作は実行され**、audit_log に2行残る。`input` は U+FFFD に置換されたうえで `_audit.verbatim = false` を伴って記録される（fail-closed に落ちない。§9「入力を理由に fail-closed へ落とさない」） |
| MC-32 | `REVOKE INSERT ON audit_log FROM kaikei_app` で post | fail-closed（MC-20）に加えて、`AuditLogUnavailable::cause` の分類が `backend` であり、`cause.public_message()` にも「逆仕訳」が現れない（§9「42501 を帳簿と同じ分類のまま返さない」） |
| MC-21b | 記帳が**失敗**した呼び出しで結果レコードだけが書けない（PR-F レビュー C-2） | fail-open の警告に「操作は完了しました」「やり直さないでください」が現れず、「帳簿は変更されていません／送り直してかまいません」になる。`crates/kaikei-app/src/audit.rs` の `a_failed_operation_gets_a_warning_that_permits_resending`（成功時に従来の文言が残ることの対照実験付き） |
| MC-11b | 失敗応答の `audit_log.output`（PR-F レビュー C-4） | AI に返した本文が**そのまま**残る（`error` / `difference` / `hint` / `policy_notes` を含む）。`crates/kaikei-e2e/tests/mcp_write_tools.rs` の `an_unbalanced_entry_is_rejected_with_a_hint_while_the_audit_rows_survive` が応答の `hint` と `output.hint` の一致を見る |

**MC-20 / MC-21 / MC-23 / MC-31 / MC-32 は PR-C で実装済み**
（`crates/kaikei-store/tests/audit_log.rs` / `tests/privileges.rs` /
`tests/append_only.rs`。`pg-tests` feature 配下で `database` ジョブが実行する）。
`kaikei-mcp` 側で重ねて書く必要は無い。

**MC-22（記帳が失敗しても監査ログが残る）だけは PR-F でも重ねて書いた。**
PR-C の同名テストは `AuditSink` と `with_tx` を直接組み合わせて確かめており、
**ツール経由でもそうなっている**ことは示していない。D-070 の存在理由その
ものなので、`post_journal_entry` を貸借不一致で呼んで「帳簿0件・監査ログ
2行」を見る側を `crates/kaikei-e2e/tests/mcp_write_tools.rs` に置いた
（`an_unbalanced_entry_is_rejected_with_a_hint_while_the_audit_rows_survive`）。

MC-11 のうち「**Phase 3 の全11ツールに対して総当たり**」は、書き込み系2件が
PR-F、読み取り系・提案系7件が **PR-G**（`crates/kaikei-e2e/tests/mcp_stdio_server.rs`
の `the_read_tools_answer_through_the_real_binary_and_are_audited` と
`the_read_tools_tell_an_empty_result_apart_from_a_bad_request` が、
**呼び出した順に `(tool, status)` の対**を突き合わせる）で済んだ。
残る `search_entries` / `get_ledger` の2件は **PR-H**
（`crates/kaikei-e2e/tests/mcp_search_ledger.rs` と、実バイナリ経由の
`tests/mcp_stdio_server.rs`）で済み、**11件が揃った**。

**読み取り系も同じ経路（`dispatch::call`）を通り、成功・失敗を問わず2行残る**
（§9 の1行目。D-086）。読み取りで `audit_log` が伸びることは受け入れる——
「誰がいつ何を読んだか」も監査の対象であり、間引く仕組みを入れると
**間引きの条件そのものが監査ログを通らない経路**になる。
ただし**`audit_log.output` に載せるのは要約**である（読み取り系だけ。D-089
決定6。§9「`output` に何を残すか」）。経路は分岐させず、残す量だけを縮める。

### Phase 4 以降に延期したケース（行を消さない）

消すと「なぜ無いのか」が失われ、実装者が善意で復活させる。

| # | ケース | 状態 |
|---|---|---|
| MC-06 | `close_period` を confirm なし → dry_run | **Phase 4 以降に延期**（期間内の仕訳を列挙するポートが無く前工事が大きい。D-070） |
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
