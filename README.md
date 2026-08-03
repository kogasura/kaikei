# kaikei — 個人事業主向け 会計インフラ

## これは何か

個人事業主が自分で会計を回せるようにするための、**オープンな会計インフラ**です。
SaaSではなくライブラリ群として提供し、AIエージェント（MCP）から操作できることを主眼に置きます。

- 言語: Rust
- 想定利用形態: 自己ホスト / ライブラリ組み込み / MCP経由でのAI操作
- 対象: 日本の個人事業主（青色申告）**のみ**。法人・他国は当面スコープ外
- ライセンス方針: MIT（予定）

**Phase 3 時点で、Claude Code のような MCP クライアントから実際に記帳できます。**
手順は「[5分で動かす](#5分で動かすclaude-code-から記帳できるまで)」を上から順に実行してください。

## なぜ作るか

AIコーディングエージェントの普及により「会計アプリを自作する」ことは現実的になった。
一方で、毎回ゼロから用意しなければならないものが残っている。

- 複式簿記の正しいデータモデルと不変条件
- 日本の消費税区分・インボイス制度に対応した税区分マスタ
- 電子帳簿保存法の機能要件を意識した保存構造
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

**このソフトウェアは税務判断を行いません。** 税区分の提案は候補と根拠を返すだけで、
どれを使うかを決めるのは利用者です。

## 進捗

| Phase | 内容 | 状態 |
|---|---|---|
| Phase 0 | `kaikei-core`（貸借不一致の仕訳がプログラム上に存在できない簿記エンジン） | ✅ 完了 |
| Phase 1 | `kaikei-policy`（trait）/ `kaikei-store`（PostgreSQL）/ `kaikei-app`（ユースケース3本） | ✅ 完了 |
| Phase 2 | `kaikei-jp`（消費税の税額計算・勘定科目テンプレート・家事按分・決算振替） | ✅ 完了 |
| Phase 3 | `kaikei-mcp`（rmcp / stdio。11ツール + `audit_log`） | ✅ 完了 |
| Phase 4〜5 | CSV 取込・証憑 / 帳票・決算 | 未着手 |

各 Phase の実績・設計変更・申し送りは `PROGRESS.md`、設計判断の記録は
`DECISIONS.md` を参照。

**「完了」は `ROADMAP.md` の各 Phase の完了条件を満たしたという意味であり、
その領域の機能が出揃ったという意味ではない。**
現時点で**できないこと**は「[できないこと](#できないことphase-3-時点)」に一覧してあります。
**先に読んでください。**

---

# 5分で動かす（Claude Code から記帳できるまで）

**この節は上から順に実行すれば動くように書いてあります。**
つまずいたら「[うまくいかないとき](#うまくいかないとき)」を参照してください。

## 0. 用意するもの

| 必要なもの | 確認方法 | 備考 |
|---|---|---|
| Docker（Compose v2） | `docker compose version` | PostgreSQL 17 をこの上で動かします |
| Rust ツールチェーン | `rustc --version` | `rust-toolchain.toml` で 1.97.1 に固定してあるので、`rustup` があれば自動で合わせられます |
| MCP クライアント | Claude Code など | stdio で子プロセスを起動できるもの |

PostgreSQL を別途インストールする必要はありません（`docker compose` が用意します）。

## 1. 取得して `.env` を作る

```sh
git clone https://github.com/kogasura/kaikei.git
cd kaikei
cp .env.example .env
```

`.env` を開き、**`CHANGE_ME` を全て自分のパスワードに置き換えます**（4箇所）。

| 変数 | 誰のパスワードか |
|---|---|
| `POSTGRES_PASSWORD` | コンテナ初期化用のスーパーユーザー |
| `KAIKEI_MIGRATOR_PASSWORD` | マイグレーション実行ロール（テーブル所有者） |
| `KAIKEI_APP_PASSWORD` | アプリ実行ロール |
| 各 `*_DATABASE_URL` の中の `CHANGE_ME` | 上の2つに対応する接続文字列 |

**`:` `@` `/` `?` `#` `%` `[` `]` を含まないパスワードにしてください。**
接続文字列にそのまま埋め込むため、パーセントエンコードが要る値だと
「DB に接続できません」という無関係な失敗として現れます。

`.env` は `.gitignore` 済みでコミットされません。

## 2. PostgreSQL を起動してテーブルを作る

```sh
docker compose up -d       # postgres:17-alpine が起動し、ロール作成が自動実行される

set -a; . ./.env; set +a   # cargo は .env を読まないのでシェルに流し込む
cargo run -p kaikei-store --bin kaikei-migrate
```

最後に `マイグレーションを適用しました` と出れば成功です。

> **`set -a; . ./.env; set +a` は毎回のシェルで必要です。**
> `docker compose` は `.env` を自動で読みますが、`cargo` は読みません。

## 3. サーバーが起動することを確かめる

```sh
cargo run -p kaikei-mcp
```

stderr に起動の診断が出て、**そのまま待ち受け状態になれば成功**です
（stdin から JSON-RPC を待っているので、何も起きないように見えます。`Ctrl+C` で止めます）。

起動時に次のことが行われます。**どれかが失敗したらサーバーは起動しません**
（ツール応答に到達させない）。

1. 事業者設定の検証（不足・不正は**まとめて**stderr に出ます）
2. 同梱 YAML（勘定科目テンプレート・タグ定義・消費税区分マスタ）のロード
3. `APP_DATABASE_URL` への接続と、**接続ロールの権限検査**
   （帳簿への `UPDATE` / `DELETE` を持つロールなら起動を中止します）
4. **勘定科目マスタの投入**（追加のみ。後述）

診断とエラーはすべて **stderr** に出ます。stdout は JSON-RPC 専用チャネルです。

## 4. MCP クライアントに登録する

リポジトリ直下に **`.mcp.json`** を置きます（Claude Code はこのファイルを読みます）。

```jsonc
{
  "mcpServers": {
    "kaikei": {
      "command": "cargo",
      "args": ["run", "--quiet", "-p", "kaikei-mcp"],
      "env": {
        "APP_DATABASE_URL": "postgres://kaikei_app:ここにパスワード@localhost:5432/kaikei",
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

**値をそのまま写さないこと。** `KAIKEI_TAX_MODE` や `KAIKEI_IS_TAXABLE_BUSINESS` は
**あなたの帳簿がどうであるか**を宣言する項目です（意味は
「[事業者設定](#事業者設定12個1つでも欠けたら起動しない)」）。

> **`cargo run` は毎回ビルドを確認します。** 起動が遅いと感じたら、先に
> `cargo build --release -p kaikei-mcp` して
> `"command": "./target/release/kaikei-mcp", "args": []` に置き換えてください。

> **★このファイルには DB パスワードが平文で置かれます★**
> ファイル権限に注意し、リポジトリにコミットしないでください
> （`.gitignore` に `.mcp.json` を足すのが安全です）。

`.mcp.json` を置いたら Claude Code を起動し直します。
接続状態は `/mcp` で確認できます。

## 5. 繋がったことを確かめる

Claude Code に、たとえば次のように頼みます。

```
kaikei の勘定科目を一覧して
```

```
2026-04-15 に A社へ 110,000 円（税込）請求した。売掛金と売上高で記帳して
```

```
いま記帳した仕訳を見せて。試算表も出して
```

`post_journal_entry` の承認を求められたら承認してください
（**記帳するかどうかの最終判断は MCP クライアント側の承認 UI の責務**です。
サーバーは「提案」と「確定」を分けるところまでを担います）。

記帳したものを間違えたときは、**削除ではなく訂正（逆仕訳）**を頼みます。

```
さっきの仕訳、金額の桁を間違えた。訂正して
```

## うまくいかないとき

| 症状 | 原因 | 対処 |
|---|---|---|
| 起動直後に終了し、stderr に項目名が並ぶ | 事業者設定が足りない | 不足項目は**全部まとめて**出ます。`.mcp.json` の `env` に足してください |
| `KAIKEI_...=1` にしたのに起動しない | 真偽値は `true` / `false` のみ | `1` / `yes` / `on` は受け付けません（「設定したつもりで効いていない」を防ぐため） |
| `permission denied for database kaikei` | `DATABASE_URL` が `kaikei_app` を指している | pg-tests は使い捨てDBを作るので `kaikei_migrator` が要ります（`.env.example` のとおりに） |
| 起動時に「帳簿への UPDATE / DELETE を持つ」と言われる | `APP_DATABASE_URL` に `kaikei_migrator` を書いた | `kaikei_app` に直します（理由は「[帳簿は追記のみ](#帳簿は追記のみ訂正は逆仕訳)」） |
| MCP クライアントが「接続できない」と言う | 起動に失敗している | まず `cargo run -p kaikei-mcp` を手で実行し、stderr を読んでください |
| ツール一覧が空 / ツールが見つからない | 古いプロセスに繋がっている | クライアントを再起動してください |

---

# 事業者設定（12個。1つでも欠けたら起動しない）

**既定値にフォールバックしません。** 課税事業者かどうか・税抜経理かどうかは、
このソフトウェアが代わりに決めてよい種類の設定ではないためです。
指定を忘れたときに「もっともらしい既定値」で動くと、誤った前提のまま
消費税額の行が生成される（あるいは生成されない）ことになります。

| 環境変数 | 内容 | 値の例 |
|---|---|---|
| `APP_DATABASE_URL` | **`kaikei_app` ロール**の接続文字列 | `postgres://kaikei_app:...@localhost:5432/kaikei` |
| `KAIKEI_BOOK_CURRENCY` | 帳簿通貨。小数桁数まで解決される | `JPY`（対応は `JPY` / `USD` のみ） |
| `KAIKEI_FISCAL_YEAR_RULE` | 会計年度の区切り規則 | `calendar_year`（現状これのみ） |
| `KAIKEI_TAX_MODE` | 経理方式 | `exclusive`（税抜） / `inclusive`（税込） |
| `KAIKEI_ROUNDING` | 端数処理方式 | `floor` / `ceil` / `half_up` |
| `KAIKEI_ROUNDING_UNIT` | 端数処理の単位 | `line`（明細ごと） / `document`（請求書単位） |
| `KAIKEI_IS_TAXABLE_BUSINESS` | 課税事業者か | `true` / `false` |
| `KAIKEI_SIMPLIFIED_TAXATION` | 簡易課税か | `true` / `false` |
| `KAIKEI_CLOSING_ACCOUNT_CAPITAL` | 元入金の科目コード | `400` |
| `KAIKEI_CLOSING_ACCOUNT_OWNER_DRAWINGS` | 事業主貸の科目コード | `410` |
| `KAIKEI_CLOSING_ACCOUNT_OWNER_CONTRIBUTIONS` | 事業主借の科目コード | `420` |
| `KAIKEI_CLOSING_TAX_CATEGORY` | 決算振替のゼロ化明細に付ける税区分コード | `NOT_APPLICABLE` |

同じ一覧が `.env.example` にも（用途の説明付きで）あります。

- **空文字は「設定した」ことになりません**（`"KEY": ""` は未設定と同じ扱いで起動を止めます）
- **値の語彙も検証します。** 決算3科目は勘定科目表に実在するか、
  `KAIKEI_CLOSING_TAX_CATEGORY` は税区分マスタに実在するかまで見ます
- どの設定で動いているかは `get_settings` ツールで確認できます

## 勘定科目マスタの投入について

サーバーは起動のたびに、同梱テンプレート（`crates/kaikei-jp-data/chart/sole_proprietor.yaml`）の
科目を `accounts` に投入します。**追加しか行いません。**

| 投入しようとした科目 | 動作 |
|---|---|
| DB に無い | 追加する |
| DB にあり、定義が一致 | 何もしない |
| DB にあり、定義が異なる | **既存を残す**。stderr と `get_settings` の `chart_differences` に差異を出す |

編集した科目名が起動のたびにテンプレートへ戻る、ということは起きません。
テンプレート側を採用したい場合は勘定科目マスタ（`accounts` テーブル）を直接編集してください。

> **`accounts.active` はまだ効きません（Phase 3 時点）。**
> 列は存在しますが、勘定科目表の読み込みは `active` を見ていないため、
> `active = false` にしても**その科目への記帳は成功します**（自動生成される
> 消費税額の行も同様です）。`sort_order` も現時点ではどこからも読まれません。
> 使わない科目がある場合は、当面「使わない」という運用で扱ってください。

---

# 使えるツール（11件）

金額は**すべて文字列**で受け渡します（`"110000"`）。JSON の number は
倍精度浮動小数点なので、会計金額には使いません——**整数であっても拒否します**。

## 読み取り系（7件）

| ツール | できること |
|---|---|
| `list_accounts` | 勘定科目一覧。科目コード・名称・5要素分類・記帳可否（`postable`）を返す。`postable_only: true` で記帳できる科目に絞れる |
| `get_entry` | 仕訳1件の詳細。**その仕訳が既に訂正済みなら `reversed_by` が付く**（訂正履歴が応答から読める） |
| `get_trial_balance` | 試算表。期間（取引日・両端含む）は必須。`group_by` に取引先などのタグキーを指定できる |
| `search_entries` | 取引日・金額・科目・摘要・タグで仕訳を検索。既定 20 件・上限 100 件。**切れたことが応答から分かる**（`total_matches` / `has_more` / `next_cursor` / `truncation_note`） |
| `get_ledger` | 総勘定元帳（科目別の明細と残高の推移）。既定 100 行・上限 500 行。**合計はページではなく期間全体**の値 |
| `list_tax_categories` | 指定日時点で有効な消費税区分の一覧。**どのマスタを見た結果か**も返す |
| `get_settings` | 経理方式・端数処理・課税事業者区分・帳簿通貨など、起動時に確定した設定 |

## 書き込み系（2件）

| ツール | できること |
|---|---|
| `post_journal_entry` | 仕訳を1件起こす。貸借が一致しない仕訳は記帳されず、差額と**修正案（`hint`）**が返る。`auto_tax_lines: true` で消費税額の行の生成を試みる |
| `reverse_journal_entry` | 逆仕訳（赤伝）で訂正する。**訂正理由が必須**。元の仕訳は書き換わらない |

`post_journal_entry` の `hint` は、`auto_tax_lines` を指定していない呼び出しに付きます。
内容は帳簿の設定で変わります——税抜経理の課税事業者では消費税額の行を足した
`suggested_lines`、税込経理や免税事業者では「なぜ税額行が作れないか」の `policy_notes` だけ
（それも無ければ `hint` は付きません）。

## 提案系・検証系（2件。帳簿を変更しない）

| ツール | できること |
|---|---|
| `suggest_tax_category` | 取引日時点で有効な税区分の**候補と根拠**を返す。1件に絞らず、順位も信頼度も付けない。摘要の文面からの推論は行わない |
| `validate_invoice_number` | インボイス登録番号の**形式**（先頭の `T`・桁数・文字種・チェックデジット）を検証する |

`validate_invoice_number` は、**その番号が実在するか・適格請求書発行事業者として
有効かは確認しません**（応答の `not_checked` にそう書いてあります）。
国税庁への照会は行いません。

## 存在しないツール（4件）

```
delete_journal_entry   ← 訂正は reverse_journal_entry のみ
update_journal_entry   ←   同上
execute_sql            ←   任意 SQL の実行経路を開かない
reopen_period          ← 締め（close_period）自体が Phase 4 以降なので、今は作りようがない
```

**上の4件は理由が同じではありません。**

- 前の3件は**将来も作りません**（D-014）。「Phase 4 以降に回す」ではない
- `reopen_period` だけは性質が違う。締めの取り消し手段を設けるかどうかは、
  `close_period` を実装する Phase で決めます

**登録しないことは4層の防御のうち最も外側の1層にすぎません。**
残り3層は DB のロール権限・トリガ・アプリ層のポート定義です
（「[帳簿は追記のみ](#帳簿は追記のみ訂正は逆仕訳)」）。

---

# できないこと（Phase 3 時点）

**正直に書きます。** ここに書いていないことが「できる」という意味ではありませんが、
ここに書いてあることは**確実にできません**。

## ツールが無いもの（Phase 4 以降）

| やりたいこと | 対応するツール | なぜ今は無いか |
|---|---|---|
| 期間を締める | `close_period` | 期間内の仕訳を列挙する経路が無く、締めの記録（ハッシュ連鎖）の計算式も Phase 5 の検証コマンドと揃える必要がある |
| 貸借対照表・損益計算書を出す | `get_statements` | 試算表の型を `kaikei-core` の外から組み立てる手段が未設計 |
| ある科目の残高の内訳を説明する | `explain_balance` | 同上 |
| 銀行・カードCSVを取り込む | `list_pending_transactions` / `journalize_transaction` / `ignore_transaction` / `suggest_journal_entry` / `upsert_journalize_rule` | `kaikei-import` が未着手 |
| 証憑（領収書等）を仕訳に紐付ける | `attach_document` / `search_documents` | `kaikei-blob` が未着手。現状 `post_journal_entry` に証憑を渡す引数はありません |
| 取引先マスタを更新する | `upsert_counterparty` | 書き込み用のポートが未定義（DB 権限だけは既にあります） |

決算振替仕訳の**生成**（収益・費用のゼロ化）は `kaikei-jp` に実装済みですが、
**MCP ツールとしては公開していません**。

## 会計・税制として実装していないもの

いずれも税務判断が必要なため、`docs/08-compliance.md` §9 の税理士確認事項として
保留しています。

- 非適格請求書の経過措置で**控除できない部分の帳簿処理**
  （控除割合は読み込みますが税額計算に反映せず、注記で伝えるに留めます）
- **簡易課税**のみなし仕入率による計算（設定を保持するだけです）
- **青色申告特別控除**（帳簿科目ではなく申告書上の控除。仕訳を作りません）
- 期首の振替仕訳、減価償却、棚卸、家事按分の年次調整

## 仕様上の制約（既知）

| 制約 | 内容 |
|---|---|
| **`tags` の重複キーは MCP 経由では検出できません** | 同じキーを2回書くと**後の指定が黙って採用されます**（`{"tax_category": "SALES_10", "tax_category": "SALES_8_REDUCED"}` は税率が無言で入れ替わる）。JSON-RPC メッセージ全体をパースする時点で畳み込まれるため、サーバー側で検出する手段がありません。`audit_log` にも畳み込まれた後の値しか残りません |
| 認証機構がありません | stdio でクライアントが子プロセスとして起動する形なので、ソケットを開きません。信頼境界は「このプロセスを起動できる OS ユーザー」と「接続に使う DB ロール」の2つです |
| 通貨は `JPY` / `USD` のみ | 未知の通貨コードは小数桁数を推測せずエラーにします（推測すると金額が100倍ずれます） |
| 金額の上限 | 最小通貨単位で 64bit 整数の範囲を超える金額は記帳時にエラーになります |
| `accounts.active` / `sort_order` が効きません | 上の「勘定科目マスタの投入について」を参照 |
| `entry_counters.skipped` は書き込み未実装 | 意図的な欠番の理由を記録する欄です（下の「仕訳番号と欠番」） |
| 帳簿を見る画面がありません | 閲覧は MCP ツール経由か、PostgreSQL を直接読むかです。CSV 出力は Phase 5 |
| 読み取りも `audit_log` に記録されます | 「誰がいつ何を読んだか」も監査の対象なので、行数は読み取りのぶんだけ増えます（読み取り系の記録内容は要約です） |

---

# 帳簿は追記のみ（訂正は逆仕訳）

記帳した仕訳は**更新も削除もできません**。訂正は逆仕訳（赤伝）で行い、
元の仕訳と訂正の経緯が両方残ります。電子帳簿保存法の「訂正削除の履歴」に関する
機能要件を意識した設計です。

これは4層で守っています。**MCP にツールが無いことは、そのうちの1層にすぎません。**

| 層 | 実体 |
|---|---|
| 1. MCP にツールを登録しない | `delete_journal_entry` / `update_journal_entry` / `execute_sql` を実装しない |
| 2. DB ロール権限 | `kaikei_app` から `journal_entries` / `journal_lines` の `UPDATE` / `DELETE` / `TRUNCATE` を `REVOKE` |
| 3. トリガ | 行トリガ + `TRUNCATE` 文トリガ。**テーブル所有者にも効く唯一の防御** |
| 4. アプリ層のポート | リポジトリに更新系メソッドを定義しない |

**`APP_DATABASE_URL` に `kaikei_migrator` を書かないでください。** 所有者ロールは
2層目の `REVOKE` をバイパスするので、防御が1層失われます。サーバーは起動時に
接続ロールの権限を検査し、帳簿への `UPDATE` / `DELETE` / `TRUNCATE` を1つでも
持っていれば起動を中止します。

append-only の対象は `journal_entries` / `journal_lines` と `audit_log` です。
マスタ（`accounts` / `counterparties` / `entry_counters`）は更新できます。

# 監査ログ

ツールの呼び出しは `audit_log` に**開始レコードと結果レコードの2行**として
記録します（**読み取り系も含めて全て**）。帳簿本体とは別のコネクションで書くので、
記帳が失敗して巻き戻っても「何をしようとしたか」は残ります。

ただし2行が揃わない場合があり、それぞれ意味が違います。

| 状況 | 残る行 | 応答 |
|---|---|---|
| 通常 | 開始 + 結果の2行 | 通常の成功／失敗 |
| 開始レコードが書けない | 0行 | **操作を実行しません**。帳簿は変更されません |
| 結果レコードだけが書けない | 開始の1行 | 操作は実行され、応答に `warnings` が付きます。その呼び出しは「結果不明」として識別できます |
| 登録されていないツール名 | 0行 | ツール呼び出しに到達しないため、プロトコルエラー（`tool not found`）になります |

`audit_log` の `input` / `output` には摘要・取引先・検索語がそのまま入ります。
**個人情報が入る前提で**、帳簿本体と同等の機微度として扱ってください。

# 仕訳番号と欠番

仕訳番号は**会計年度ごとの連番**です。

採番（`entry_counters` の更新）は仕訳の INSERT と**同一トランザクション**で行うため、
検証失敗時はカウンタの増分も一緒に巻き戻り、**通常は欠番が発生しません**
（欠番が出るのは PostgreSQL の `SEQUENCE` を別トランザクションで払い出す場合）。

`entry_counters.skipped` は、それでも**意図的に**番号を飛ばした場合の理由を
記録するための専用フィールドであり、現時点では書き込みを実装していません。

---

# 開発者向け

## ドキュメントの読む順序

| # | ファイル | 内容 |
|---|---|---|
| 1 | `CLAUDE.md` | **Claude Codeが最初に読む。作業規律と禁止事項** |
| 2 | `DOMAIN.md` | 会計ドメインの前提知識。簿記/会計基準/税制の分離 |
| 3 | `ARCHITECTURE.md` | crate構成、依存方向、フォルダ分割の原則 |
| 4 | `docs/01-core-types.md` | Phase 0 の型定義仕様 |
| 5 | `docs/02-test-cases.md` | Phase 0 のテストケース一覧 |
| 6 | `docs/03-database.md` | DBスキーマ、append-only強制 |
| 7 | `docs/04-jp-tax.md` | 日本税制アダプタ、YAMLスキーマ |
| 8 | `docs/05-csv-import.md` | CSV取込設計 |
| 9 | `docs/06-documents.md` | 証憑ファイル管理（Content-Addressed Storage） |
| 10 | `docs/07-mcp-server.md` | **MCPツール定義と Phase 3 の実装方針**（ツールの入出力はここが一次情報） |
| 11 | `docs/08-compliance.md` | 電帳法・インボイス制度の要件整理 |
| 12 | `DECISIONS.md` | 設計判断と却下した選択肢の記録 |
| 13 | `ROADMAP.md` | Phase 0〜5 |
| 14 | `PROGRESS.md` | 各 Phase の実績・教訓・申し送り |

## テスト

DB を必要としないテストは feature 無しで走ります
（`SQLX_OFFLINE=true` と `.sqlx/` のオフラインキャッシュでコンパイルします）。

```sh
cargo fmt --all -- --check
SQLX_OFFLINE=true cargo clippy --workspace --all-targets -- -D warnings
SQLX_OFFLINE=true cargo test --workspace
```

PostgreSQL を要するテストは `pg-tests` feature 配下です。
`#[ignore]` による無言スキップは使っていません（ローカルで実行されないまま
「通った」と錯覚することを防ぐため）。

```sh
set -a; . ./.env; set +a
cargo build -p kaikei-mcp                                       # ★先に必要（下記）
DATABASE_URL="$MIGRATOR_DATABASE_URL" cargo test -p kaikei-store --features pg-tests
DATABASE_URL="$MIGRATOR_DATABASE_URL" cargo test -p kaikei-e2e   --features pg-tests
DATABASE_URL="$MIGRATOR_DATABASE_URL" cargo test -p kaikei-mcp   --features pg-tests
```

`#[sqlx::test]` はテストごとに使い捨てのデータベースを作るため、**`DATABASE_URL` は
`kaikei_migrator`（CREATE DATABASE 権限を持つロール）を指している必要があります**。
`kaikei_app` を指していると `permission denied for database kaikei`（SQLSTATE 42501）で
全滅します。

`kaikei-e2e` の `tests/mcp_stdio_server.rs` と `tests/mcp_walkthrough.rs` は
**`kaikei-mcp` の実行ファイルを子プロセスとして起動**し、stdio で
`initialize` → `tools/call` を送ります。`CARGO_BIN_EXE_<name>` は同じ package の
テストにしか渡らないので、**先に `cargo build -p kaikei-mcp` しておくこと**
（無い場合・`crates/kaikei-mcp/` のどのファイルより古い場合は、その旨を書いて落ちます）。

`kaikei-mcp` の `pg-tests` だけは**使い捨てDBを作りません**。この crate は
`sqlx` に依存しないため `#[sqlx::test]` を使えず、`APP_DATABASE_URL` が指す
データベースをそのまま使います。書き込むのは**勘定科目マスタだけ**（追加のみ・冪等）で、
仕訳は1件も書きません。

### 通し E2E（Phase 3 の完了条件そのもの）

`crates/kaikei-e2e/tests/mcp_walkthrough.rs` が、**AI が実際にやる一連の流れ**を
実バイナリに1本通します。

```
科目を確認 → 記帳（1回目は貸借不一致 → hint に従って自己修正）→ 読み戻し
→ 検索 → 元帳 → 試算表 → 間違いに気づく → 逆仕訳 → 訂正済みと分かる
→ 正しい金額で記帳し直す → 監査ログに全部残っている
```

最後の突き合わせは**登録済みツールの一覧から導出**しているので、
ツールを1つ足してこの通しで呼ばなければテストが落ちます。

## 開発中にデータを作り直す

**壊れたデータは1行だけ `UPDATE` して直そうとしないこと**（そもそも DB 権限と
トリガで通りません）。`docker compose down -v` で named volume ごと作り直せば、
次回起動時にロール作成からやり直せます。

逆に、**`docker compose down`（`-v` なし）や再起動ではデータは消えません**。

---

## 未決定事項（着手前に人間が決める）

- crate名 / プロジェクト名の最終決定（`kaikei` は仮）
- リポジトリの公開タイミング
- 税理士レビューの依頼先
