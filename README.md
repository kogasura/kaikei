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
| 10 | `docs/07-mcp-server.md` | MCPツール定義と Phase 3 の実装方針 |
| 11 | `docs/08-compliance.md` | 電帳法・インボイス制度の要件整理 |
| 12 | `DECISIONS.md` | 設計判断と却下した選択肢の記録 |
| 13 | `ROADMAP.md` | Phase 0〜5 |

## 進捗

| Phase | 内容 | 状態 |
|---|---|---|
| Phase 0 | `kaikei-core`（貸借不一致の仕訳がプログラム上に存在できない簿記エンジン） | ✅ 完了 |
| Phase 1 | `kaikei-policy`（trait）/ `kaikei-store`（PostgreSQL）/ `kaikei-app`（ユースケース3本） | ✅ 完了 |
| Phase 2 | `kaikei-jp`（消費税の税額計算・勘定科目テンプレート・家事按分・決算振替） | ✅ 完了 |
| Phase 3 | `kaikei-mcp`（rmcp / stdio。読み取り系7 + 書き込み系2 + 提案系・検証系2 + audit_log） | 実装中（`audit_log` と書き込み系2件まで完了。残り9件は未実装） |
| Phase 4〜5 | CSV 取込・証憑 / 帳票・決算 | 未着手 |

各 Phase の実績・設計変更・申し送りは `PROGRESS.md`、設計判断の記録は
`DECISIONS.md` を参照。

**「完了」は `ROADMAP.md` の各 Phase の完了条件を満たしたという意味であり、
その領域の機能が出揃ったという意味ではない。** Phase 2 時点で
**実装していない**主なものは次のとおり（いずれも税務判断が必要なため、
`docs/08-compliance.md` §9 の税理士確認事項として保留している）:

- 非適格請求書の経過措置で**控除できない部分の帳簿処理**
  （控除割合は読み込むが税額計算に反映しない）
- **簡易課税**のみなし仕入率による計算
- **青色申告特別控除**（帳簿科目ではなく申告書上の控除。仕訳を作らない）
- 期首の振替仕訳（`opening_entries`）、減価償却、棚卸、家事按分の年次調整

## ローカル開発環境（PostgreSQL）

Phase 1 以降は PostgreSQL が必要（`docs/03-database.md`、`DECISIONS.md` D-010）。

```sh
cp .env.example .env       # パスワードを埋める
docker compose up -d       # postgres:17-alpine が起動し、ロール作成が自動実行される

set -a; . ./.env; set +a   # cargo は .env を読まないのでシェルに流し込む
cargo run -p kaikei-store --bin kaikei-migrate   # MIGRATOR_DATABASE_URL を読む
```

### PostgreSQL を要するテスト（pg-tests）

`#[sqlx::test]` はテストごとに使い捨てのデータベースを作るため、**`DATABASE_URL` は
`kaikei_migrator`（CREATE DATABASE 権限を持つロール）を指している必要がある**。
`.env.example` はそのように設定してある。`kaikei_app` を指していると
`permission denied for database kaikei`（SQLSTATE 42501）で全滅する。

```sh
set -a; . ./.env; set +a
cargo test -p kaikei-store --features pg-tests
```

```sh
set -a; . ./.env; set +a
cargo test -p kaikei-e2e --features pg-tests   # 合成ルートを模したE2E
cargo test -p kaikei-mcp --features pg-tests   # MCP サーバーの起動（下記の注意）
```

`kaikei-mcp` の `pg-tests` だけは**使い捨てDBを作らない**。この crate は
`sqlx` に依存しない（`docs/07-mcp-server.md` §10 MC-30）ため `#[sqlx::test]`
を使えず、`APP_DATABASE_URL` が指すデータベースをそのまま使う。
書き込むのは**勘定科目マスタだけ**（追加のみ・冪等）で、仕訳は1件も書かない。

`pg-tests` を付けない `cargo test --workspace` は DB を必要としない
（`SQLX_OFFLINE=true` と `.sqlx/` のオフラインキャッシュでコンパイルする）。
`#[ignore]` による無言スキップは使っていない。ローカルで実行されないまま
「通った」と錯覚することを防ぐため、feature で明示的に切り替える。

**開発中に壊れたデータを直す場合は `docker compose down -v`。**
1行だけ `UPDATE` して直そうとしない（`CLAUDE.md` §2 掟3。append-only は
DB権限とトリガで強制されているため、そもそも `UPDATE` は通らない）。
named volume（`kaikei_pgdata`）ごと作り直せば、次回起動時に
`docker/postgres/init/01-roles.sql` からロール作成〜マイグレーションをやり直せる。

逆に、**`docker compose down`（`-v` なし）や再起動ではデータは消えない**。
帳簿は named volume に永続化されており、コンテナを作り直しても残る。

## MCP サーバーを起動する（Phase 3）

`kaikei-mcp` は MCP クライアント（Claude Code 等）が**子プロセスとして起動する**
stdio サーバーです。ソケットは開きません（`docs/07-mcp-server.md` §8）。

### 1. 事業者設定を用意する

**設定が1つでも欠けているとサーバーは起動しません。**
既定値にはフォールバックしません（`DECISIONS.md` D-057 / D-082）。
課税事業者かどうか・税抜経理かどうかといった判断は、このソフトウェアが
代わりに決めるべきものではないためです。

必要な項目と値の例は `.env.example` の「kaikei-mcp（MCP サーバー）の事業者設定」
節にまとまっています。

### 2. 起動する

```sh
docker compose up -d
set -a; . ./.env; set +a
cargo run -p kaikei-store --bin kaikei-migrate   # 初回のみ
cargo run -p kaikei-mcp
```

起動時に次のことが行われます。**どれかが失敗したらサーバーは起動しません**
（ツール応答に到達させない。`docs/07-mcp-server.md` §7）。

1. 事業者設定の検証（不足・不正は**まとめて**stderr に出ます）
2. 同梱 YAML（勘定科目テンプレート・タグ定義・消費税区分マスタ）のロード
3. `APP_DATABASE_URL` への接続と、**接続ロールの権限検査**
   （帳簿への `UPDATE` / `DELETE` を持つロールなら起動を中止します）
4. **勘定科目マスタの投入**（後述）

診断とエラーはすべて **stderr** に出ます。stdout は JSON-RPC 専用チャネルです。

### 3. 勘定科目マスタの投入について

サーバーは起動のたびに、同梱テンプレート（`kaikei-jp-data/chart/sole_proprietor.yaml`）の
科目を `accounts` に投入します。**追加しか行いません**（`DECISIONS.md` D-081）。

| 投入しようとした科目 | 動作 |
|---|---|
| DB に無い | 追加する |
| DB にあり、定義が一致 | 何もしない |
| DB にあり、定義が異なる | **既存を残す**。stderr に差異を出す |

編集した科目名が起動のたびにテンプレートへ戻る、ということは起きません。
テンプレート側を採用したい場合は勘定科目マスタを直接編集してください。

> **`accounts.active` はまだ効きません（Phase 3 時点）。**
> 列は存在しますが、勘定科目表の読み込みは `active` を見ていないため、
> `active = false` にしても**その科目への記帳は成功します**（自動生成される
> 消費税額の行も同様です）。`sort_order` も現時点ではどこからも読まれません。
> 科目の無効化は Phase 4 以降の実装です（`PROGRESS.md` の Phase 3
> 「実装中の申し送り」）。使わない科目がある場合は、当面「使わない」という
> 運用で扱ってください。

### 4. MCP クライアントに登録する

```jsonc
{
  "mcpServers": {
    "kaikei": {
      "command": "cargo",
      "args": ["run", "--quiet", "-p", "kaikei-mcp"],
      "env": {
        "APP_DATABASE_URL": "postgres://kaikei_app:******@localhost:5432/kaikei",
        "KAIKEI_BOOK_CURRENCY": "JPY",
        "KAIKEI_FISCAL_YEAR_RULE": "calendar_year",
        "KAIKEI_TAX_MODE": "exclusive",
        "KAIKEI_ROUNDING": "floor",
        "KAIKEI_ROUNDING_UNIT": "line",
        "KAIKEI_IS_TAXABLE_BUSINESS": "true",
        "KAIKEI_SIMPLIFIED_TAXATION": "false",
        "KAIKEI_CLOSING_ACCOUNT_CAPITAL": "400",
        "KAIKEI_CLOSING_ACCOUNT_OWNER_DRAWINGS": "410",
        "KAIKEI_CLOSING_ACCOUNT_OWNER_CONTRIBUTIONS": "420",
        "KAIKEI_CLOSING_TAX_CATEGORY": "NOT_APPLICABLE"
      }
    }
  }
}
```

この設定ファイルには **DB パスワードが平文で置かれます**。ファイル権限に
注意してください（`docs/07-mcp-server.md` §8）。

**Phase 3 PR-F 時点で登録されているツールは2件です**（`tools/list` は
`post_journal_entry` / `reverse_journal_entry` を返します）。

| ツール | できること |
|---|---|
| `post_journal_entry` | 仕訳を1件起こす。金額は**文字列**で渡します（例: `"110000"`）。貸借が一致しない仕訳は記帳されません。応答には差額（`difference`）が入り、税抜経理の課税事業者で `auto_tax_lines` を使っていない場合は消費税額の行を足す修正案（`hint`）も付きます（税込経理・免税事業者・`auto_tax_lines: true` では `hint` は付かず、理由が `policy_notes` に入ります） |
| `reverse_journal_entry` | 逆仕訳（赤伝）で訂正する。元の仕訳は書き換わりません |

照会系・提案系（`get_trial_balance` / `search_entries` / `list_accounts` など
残り9件）は PR-G / PR-H で追加します。

**削除・更新のツールは存在しません**（`delete_journal_entry` /
`update_journal_entry` / `execute_sql` / `reopen_period` は登録されておらず、
DB のロール権限とトリガでも塞いであります）。

ツールの呼び出しは `audit_log` に**開始レコードと結果レコードの2行**として
記録します。ただし2行が揃わない場合があり、それぞれ意味が違います。

| 状況 | 残る行 | 応答 |
|---|---|---|
| 通常 | 開始 + 結果の2行 | 通常の成功／失敗 |
| 開始レコードが書けない | 0行 | **操作を実行しません**（fail-closed）。帳簿は変更されません |
| 結果レコードだけが書けない | 開始の1行 | 操作は実行され、応答に `warnings` が付きます（fail-open）。その呼び出しは「結果不明」として識別できます |
| 登録されていないツール名 | 0行 | ツール呼び出しに到達しないため、プロトコルエラー（`tool not found`）になります |

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
