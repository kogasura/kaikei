# kaikei — 個人事業主向け 会計インフラ（設計ドキュメント一式）

## これは何か

個人事業主が自分で会計を回せるようにするための、**オープンな会計インフラ**の設計ドキュメントです。
SaaSではなくライブラリ群として提供し、AIエージェント（MCP）から操作できることを主眼に置きます。

- 言語: Rust
- 想定利用形態: 自己ホスト / ライブラリ組み込み / MCP経由でのAI操作
- 対象: 日本の個人事業主（青色申告）**のみ**。法人・他国は当面スコープ外
- ライセンス方針: MIT（予定）

## なぜ作るか

AIコーディングエージェントの普及により「会計アプリを自作する」ことは現実的になった。
一方で、毎回ゼロから用意しなければならないものが残っている。

- 複式簿記の正しいデータモデルと不変条件
- 日本の消費税区分・インボイス制度に対応した税区分マスタ
- 電子帳簿保存法を意識した保存構造
- 銀行・カードCSVの取込
- AIエージェントが安全に叩けるインタフェース

これらは**共有インフラであるべき**で、各自が再実装すべきものではない。

## 訴求点（個人事業主にとっての金額的価値）

電子帳簿保存法における「優良な電子帳簿」の要件を満たすと、過少申告加算税の軽減と
**65万円の青色申告特別控除**（通常は55万円）が得られる。
つまりこのプロジェクトの価値は「便利」ではなく **10万円の控除差** として表現できる。

ただし後述の免責を厳守すること。

## 絶対に守る免責（重要）

**「法令準拠を保証する」とは名乗らない。**

- JIIMA認証は「市販ソフトウェア/サービス」を対象とした制度であり、ライブラリには付与されない
- 電子帳簿保存法の要件には、機能要件以外に運用要件（事務処理規程の備付け等）がある
- 税制・法令の解釈は税理士等の専門家の領域

READMEおよびドキュメントでの表現は
「これらの機能要件を意識して設計しているが、認証は取得しておらず、運用要件は利用者の責任」
に統一する。この一線は譲らない。

## ドキュメントの読む順序

| # | ファイル | 内容 |
|---|---|---|
| 1 | `CLAUDE.md` | **Claude Codeが最初に読む。作業規律と禁止事項** |
| 2 | `DOMAIN.md` | 会計ドメインの前提知識。簿記/会計基準/税制の分離 |
| 3 | `ARCHITECTURE.md` | crate構成、依存方向、フォルダ分割の原則 |
| 4 | `docs/01-core-types.md` | Phase 0 の型定義仕様（最初の実装対象） |
| 5 | `docs/02-test-cases.md` | Phase 0 のテストケース一覧 |
| 6 | `docs/03-database.md` | DBスキーマ、append-only強制 |
| 7 | `docs/04-jp-tax.md` | 日本税制アダプタ、YAMLスキーマ |
| 8 | `docs/05-csv-import.md` | CSV取込設計 |
| 9 | `docs/06-documents.md` | 証憑ファイル管理（Content-Addressed Storage） |
| 10 | `docs/07-mcp-server.md` | MCPツール定義 |
| 11 | `docs/08-compliance.md` | 電帳法・インボイス制度の要件整理 |
| 12 | `DECISIONS.md` | 設計判断と却下した選択肢の記録 |
| 13 | `ROADMAP.md` | Phase 0〜5 |

`skeleton/` にはワークスペースの `Cargo.toml`、CI設定、データファイルの雛形がある。

## 進捗

| Phase | 内容 | 状態 |
|---|---|---|
| Phase 0 | `kaikei-core`（貸借不一致の仕訳がプログラム上に存在できない簿記エンジン） | ✅ 完了 |
| Phase 1 | `kaikei-policy`（trait）/ `kaikei-store`（PostgreSQL）/ `kaikei-app`（ユースケース3本） | ✅ 完了 |
| Phase 2 | `kaikei-jp`（消費税・勘定科目・青色申告） | 未着手 |
| Phase 3〜5 | MCP サーバー / CSV 取込 / 帳票 | 未着手 |

各 Phase の実績・設計変更・申し送りは `PROGRESS.md`、設計判断の記録は
`DECISIONS.md` を参照。

## ローカル開発環境（PostgreSQL）

Phase 1 以降は PostgreSQL が必要（`docs/03-database.md`、`DECISIONS.md` D-010）。

```sh
cp .env.example .env   # パスワードを埋める
docker compose up -d   # postgres:17-alpine が起動し、ロール作成が自動実行される
sqlx migrate run --source crates/kaikei-store/migrations \
  --database-url "$MIGRATOR_DATABASE_URL"
```

**開発中に壊れたデータを直す場合は `docker compose down -v`。**
1行だけ `UPDATE` して直そうとしない（`CLAUDE.md` §2 掟3。append-only は
DB権限とトリガで強制されているため、そもそも `UPDATE` は通らない）。
named volume（`kaikei_pgdata`）ごと作り直せば、次回起動時に
`docker/postgres/init/01-roles.sql` からロール作成〜マイグレーションをやり直せる。

逆に、**`docker compose down`（`-v` なし）や再起動ではデータは消えない**。
帳簿は named volume に永続化されており、コンテナを作り直しても残る。

## 仕訳番号と欠番

仕訳番号は**会計年度ごとの連番**とする。

採番（`entry_counters` の更新）は仕訳の INSERT と**同一トランザクション**で行うため、
検証失敗時はカウンタの増分も一緒に巻き戻り、**通常は欠番が発生しない**
（欠番が出るのは PostgreSQL の `SEQUENCE` を別トランザクションで払い出す場合。
`docs/03-database.md` §4、`DECISIONS.md` D-023）。

`entry_counters.skipped` は、それでも**意図的に**番号を飛ばした場合の理由を
記録するための専用フィールドであり、Phase 1 では書き込みを実装していない。

## 未決定事項（着手前に人間が決める）

- crate名 / プロジェクト名の最終決定（`kaikei` は仮）
- リポジトリの公開タイミング
- 税理士レビューの依頼先
