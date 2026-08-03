# ROADMAP.md

**Phase 0 が完成し全テストが通るまで、他の crate に着手しない。**

---

## Phase 0 — kaikei-core（1〜2 週間）

複式簿記エンジンのみ。税制なし、DB なし、非同期なし。

### 成果物

- `crates/kaikei-core/` 一式
- `docs/02-test-cases.md` の全ケースが通る
- CLI の動作確認用バイナリ（`examples/hello_kaikei.rs`）

### 完了条件

- 全テスト green
- `cargo clippy -- -D warnings` が通る
- `kaikei-core` の依存が `rust_decimal`, `thiserror` のみ（CI で検査）
- 公開 API に doc コメントがある

### この時点で言えること

**「貸借不一致の仕訳がプログラム上に存在できない Rust 製簿記エンジン」**

これ単体で公開・議論に足る。ここまで作ってから反応を見るのが投資対効果が最も良い。

### 動作確認の例

```
$ cargo run --example hello_kaikei
仕訳を登録: 現金 100,000 / 売上高 100,000
仕訳を登録: 消耗品費 1,980 / 現金 1,980

試算表:
  100 現金         98,020
  500 売上高      100,000
  609 消耗品費      1,980
  借方合計: 101,980 / 貸方合計: 101,980  ✓
```

---

## Phase 1 — kaikei-store + kaikei-app（1〜2 週間）

永続化とユースケース。

### 成果物

- `kaikei-policy`（trait 定義のみ。実装は Phase 2）
- `kaikei-store`（Repository、migrations、query/）
- `kaikei-app`（`post_entry`, `reverse_entry`, `report`）
- Docker Compose で PostgreSQL が立つ

### 完了条件

- 再起動してもデータが残る
- `kaikei_app` ロールで UPDATE を試みると失敗することをテストで確認
- 逆仕訳が正しく記録される
- 試算表が SQL 集計で出る
- `UnitOfWork` を実際に使ってみて、借用チェッカとの相性を評価する
  （苦痛なら設計を見直してよい。理論より実感を優先）

---

## Phase 2 — kaikei-jp（2〜3 週間）

日本税制アダプタ。**ここが最も泥臭い。**

### 成果物

- 税区分マスタ YAML（2026 年度）
- `TaxPolicy` 実装（税抜経理、端数処理）
- 勘定科目テンプレート（個人事業主）
- `TagSchema` 提供
- インボイス登録番号の検証
- 家事按分ヘルパー

### 完了条件

- 税抜経理で消費税行が自動生成される
- 8% 軽減税率、非課税、不課税が扱える
- 非適格の経過措置が YAML で表現できている
- 家事按分の 3 行仕訳が生成される
- 年度別 YAML の切り替えが取引日で行われる

### 注意

**この Phase で最も多くの疑問が出る。**
`docs/08-compliance.md` §9 の質問リストを埋めながら進め、
判断に迷う箇所は実装せず人間に返すこと。

---

## Phase 3 — kaikei-mcp（1 週間）

MCP サーバー。**差別化の本体。**

### 成果物

**MCP に登録するのは11ツール**（一覧と延期したツールの理由は
`docs/07-mcp-server.md` §2、`DECISIONS.md` D-070）。

- 読み取り系(7): `list_accounts` / `get_entry` / `get_trial_balance` /
  `search_entries` / `get_ledger` / `list_tax_categories` / `get_settings`
  - `get_statements` / `explain_balance` は Phase 4 以降。
    core の `TrialBalance` / `BalanceRow` を外から構築できないため（D-031）
  - `list_pending_transactions` / `search_documents` も Phase 4 以降
    （`kaikei-import` / `kaikei-blob` が未着手）
- 書き込み系(2): `post_journal_entry` / `reverse_journal_entry`
- 提案系・検証系(2): `suggest_tax_category` / `validate_invoice_number`
- `audit_log`（別コネクションで開始・結果の2行を書く。D-070）
- MCP SDK は `rmcp` 3.x / stdio（D-071）

### 完了条件

- **Claude Code から記帳できる**
- 貸借不一致のとき、AI が自己修正できるエラーが返る
- 全操作が audit_log に記録される
- 削除系ツールが存在しない

### ★ ここがドッグフーディングの起点

**この時点で自分の帳簿を付け始める。**
以降の優先順位は自分の痛みが教えてくれる。ロードマップより実感を優先してよい。

---

## Phase 4 — kaikei-import + kaikei-blob（2〜3 週間）

CSV 取込と証憑管理。

### 成果物

- `ImportedTransaction` と Repository
- CSV プロファイル（主要な銀行・カード 5〜10 種）
- 仕訳化ルールエンジン
- `suggest_journal_entry`（根拠付き）
- `LocalBlobStore`
- `documents` テーブルと検索
- `kaikei-api`（axum。MCP と同じユースケースを HTTP で公開）

### 完了条件

- Shift-JIS の CSV が文字化けせず取り込める
- 同じ CSV を 2 回取り込んでも重複しない
- ルールで自動仕訳化される
- AI の提案に reasoning が付く
- 証憑のハッシュ検証が通る

---

## Phase 5 — kaikei-report（2〜3 週間）

出力。**ここまで来れば実用。**

### 成果物

- 仕訳日記帳 / 総勘定元帳 / 試算表 CSV
- **弥生インポート形式 CSV**（優先度最高。税理士連携）
- 全件 JSON エクスポート
- 決算処理（`JpSoleProprietorClosingPolicy`）
- 青色申告決算書のデータ出力
- `export_for_audit`
- `kaikei verify` コマンド

### 完了条件

- **自分の確定申告に使える**
- 税理士に渡せる形式で出力できる
- 整合性検査が通る

---

## Phase 5 以降の検討事項（着手しない）

優先度順ではなく、思いついたものの置き場。

| 項目 | 備考 |
|---|---|
| 減価償却 | 個人事業主にも必要。定額法・定率法、少額減価償却資産の特例 |
| 棚卸 | 業種によって必要 |
| 簡易課税の税額計算 | 事業区分ごとのみなし仕入率 |
| 2 割特例 / 少額特例 | 要件を要確認 |
| e-Tax 連携 | 仕様調査が必要。優先度は低い |
| JP PINT の読み込み（受領側） | CSV 取込の延長として自然。比較的低コスト |
| スキャナ保存 | 解像度・階調の要件がある |
| 法人対応 | `kaikei-jp/src/corporation/` を追加 |
| SQLite 対応 | 権限による append-only 強制ができない点をどう補うか |
| Web UI | 最初は作らない。MCP + CLI で十分 |
| トークン認証 | 外部公開する場合 |
| S3BlobStore | |
| 外貨換算（FxPolicy） | Stripe USD 入金が実際に来たら |

---

## 並行して進めること（人間の作業）

| # | 作業 | タイミング |
|---|---|---|
| 1 | プロジェクト名・crate 名の決定 | Phase 0 中 |
| 2 | 税理士の確保とレビュー依頼 | Phase 2 開始前 |
| 3 | `docs/08-compliance.md` §9 の質問リストを税理士に投げる | Phase 2 中 |
| 4 | README の表現の税理士レビュー | 公開前（必須） |
| 5 | リポジトリ公開 | Phase 0 完了後が推奨 |
| 6 | 実装過程の記事化 | Phase 3 完了後 |

### 5 と 6 について

Phase 0 完了時点で公開すると、
「貸借不一致が型で防がれる簿記エンジン」という技術的に語れる成果物がある。
会計の話に興味がない Rust 開発者にも届く。

Phase 3 完了後の記事は
「Claude Code で自分の帳簿を付ける」という切り口になり、対象読者が変わる。
2 段階で出すのが効く。

---

## 進捗の記録

各 Phase 完了時に以下を `PROGRESS.md`（新規作成）に追記する。

- 完了日
- 実際にかかった時間と見積との差
- 設計変更が必要になった箇所（`DECISIONS.md` に追記）
- 次 Phase への申し送り
- 税理士に確認すべき事項として新たに出てきたもの
