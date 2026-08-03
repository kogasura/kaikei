# DECISIONS.md — 設計判断の記録

**却下した選択肢とその理由**を残す。後から「なぜこうなっているのか」を辿れるようにする。
新しい判断を追加するときは同じ形式で追記する。

---

## D-001 層の分離を crate 境界で行う

**決定**: DDD の層をフォルダではなく Cargo の crate で分ける。

**却下した選択肢**: 単一 crate 内で `domain/`, `application/`, `infrastructure/` に分割。

**理由**: フォルダは規約なので破れる。`use crate::infrastructure::db` と書ける状態では、
数年後に必ず誰かが書いている。crate 境界なら `Cargo.toml` に依存が無い限り
コンパイルできない。**規律をコンパイラに委譲できる。**

**トレードオフ**: crate 数が多くなり、ビルド設定が煩雑。小さな型を共有したいときに面倒。
→ ワークスペースで管理し、CI で依存方向を検査することで受け入れる。

---

## D-002 ORM を使わず Data Mapper を手書きする

**決定**: sqlx + 永続化専用 Row 型 + `TryFrom` による変換。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| Toasty | ActiveRecord パターン。モデル型自身が `get_by_id` を持ち、`Deferred<T>` が構造体に混入する。DDD のカプセル化と両立しない |
| SeaORM | ActiveModel で多少 Data Mapper 寄りだが、エンティティは CLI の生成物。DTO 扱いにして変換層を挟むなら sqlx で十分 |
| Diesel | 型安全は最強だがスキーマ由来の型がコードベース深くに入る。async は後付け |

**理由**: DDD ではエンティティのフィールドが private で、生成・変更はドメインのメソッド経由。
ORM は DB の行から復元するためにカプセル化を破る必要がある。
C#/Java はリフレクションで解くが、**Rust にはランタイムリフレクションが無い。**

derive macro なら private フィールドに触れるが、ドメイン構造体にインフラ由来の属性が付き、
避けたかった結合が戻る。

**トレードオフ**: 集約あたり 20〜30 行のマッピングコードを書く。
→ AI が数秒で書ける規模。かつてより安いコスト。

---

## D-003 Money は型パラメータで通貨を分けない

**決定**: `Money { minor: i128, currency: Currency }`。異通貨演算は `Result` で弾く。

**却下した選択肢**: `Money<JPY>` のように通貨を型パラメータにする。

**理由**: 通貨は実行時データ（DB から来る、ユーザーが設定する）。
型パラメータにすると `JournalLine<C>`, `JournalEntry<C>`, `Repository<C>` と
全体に伝染し、通貨を動的に扱えなくなる。

**トレードオフ**: 異通貨の混在をコンパイル時に防げない。
→ `JournalEntry::new` が通貨の同一性を検証するので、集約の境界で必ず捕まる。

---

## D-004 TagSet を不透明な袋にする（+ スキーマ検証）

**決定**: `TagSet(BTreeMap<TagKey, TagValue>)` を `JournalLine` に持たせ、
core は意味を解釈しない。ただし `TagSchema` による形式検証は core が行う。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `JournalLine<Ext>` で拡張型をジェネリック化 | `Ledger<Ext>`, `Repository<Ext>` と全体に伝染する |
| 消費税を core に入れる | core が国別・年度別になり、不変層の意味が消える |
| タグを完全に自由にする（スキーマなし） | 3 年でキーが 50 種類になり typo 混入。R1 のリスク |

**理由**: 消費税は仕訳の中に入ってくるが、core は税制を知ってはいけない。
「意味は知らないが形は守らせる」が唯一の両立点。**逃げ道に鍵をつける。**

**制約**: 金額に影響する情報をタグに入れない（貸借一致の検証を迂回できてしまう）。

---

## D-005 policy trait は純関数に固定する

**決定**: `TaxPolicy` 等の trait は `async` にせず、必要なデータは `TaxContext` を引数で渡す。

**却下した選択肢**: trait を async にして内部で Repository を引く。

**理由**: 一度 I/O が入るとテストが重くなり、core の純粋性が段階的に失われる。
「取引先マスタを引いて適格事業者か確認したい」という要求は必ず来るが、
そのとき trait を変えるのではなく、呼び出し側がデータを集めて渡す。

**規律**: **I/O は application 層だけが行う。** 一度崩れると取り戻せない。

---

## D-006 append-only を DB 権限で強制する

**決定**: `kaikei_app` ロールから帳簿本体テーブルの UPDATE/DELETE を REVOKE。
加えてトリガでも拒否する。

**却下した選択肢**: アプリのコードだけで守る。

**理由**: 「1 件だけ UPDATE すれば直る」という誘惑が何度も来る。
コードの規約では防げない。しかもこの不変性は
電帳法の「訂正削除の履歴」要件を構造的に満たす根拠なので、破ると存在意義が消える。

**トレードオフ**: 開発中のデータ修正が面倒（DB リセットが必要）。
→ それが正しいコスト。本番では逆仕訳で直す訓練になる。

---

## D-007 残高テーブルを作らず都度 SUM する

**決定**: `TrialBalance` は SQL の `GROUP BY` + `SUM` で毎回計算する。

**却下した選択肢**: マテリアライズドビュー、トリガによる残高テーブル更新、
イベントソーシングの projection。

**理由**: 個人事業主の規模は年間数百〜数千件。インデックス付き SUM で十分。
残高テーブルは「帳簿と残高がズレる」という最悪のバグを生む。

**閾値**: 10 万件を超えたら月次スナップショット + 差分に切り替える。
それまではやらない。

---

## D-008 取込明細を帳簿とは別集約にする

**決定**: `ImportedTransaction` を `kaikei-import` に置き、
`kaikei-core` に依存させない。UPDATE 可なテーブルとする。

**却下した選択肢**: CSV の行を直接仕訳に変換する。仕訳に「取込元」フラグを持たせる。

**理由**: 「入金/出金」と「借方/貸方」は別の語彙。
`ImportedTransaction` は status が変わる（可変）が、帳簿は不変。
**この分離があるから帳簿の不変性を守れる。**
境界づけられたコンテキストが 2 つあることを構造で表現する。

---

## D-009 証憑は Content-Addressed Storage で保存する

**決定**: SHA-256 をファイル名にして保存。メタデータは DB。`delete` を trait に定義しない。

**却下した選択肢**: 日付や取引先でフォルダ階層を作り、元のファイル名で保存。

**理由**:

1. 改変が自動検出される（ハッシュ不一致）→ 真実性の担保が構造から出る
2. 重複排除が無料で付く
3. 日本語ファイル名、macOS の NFD 正規化、長さ制限を全部回避

**補完**: 人間が見るためのフォルダ構造は `export_for_audit` で生成する。
保存構造と閲覧構造を分離する。

---

## D-010 PostgreSQL 固定（SQLite を初期対応しない）

**決定**: Phase 1 以降は PostgreSQL のみ。

**却下した選択肢**: SQLite を先に対応して手軽さを優先する。

**理由**: JSONB（タグ）、GIN インデックス、**テーブル単位の権限制御**が必要。
特に権限による append-only 強制は SQLite では不可能。

**トレードオフ**: 個人利用のハードルが上がる。
→ Docker Compose を提供する。SQLite 対応は Phase 5 以降の検討事項。

---

## D-011 会計基準（③層）は実装しない

**決定**: `ClosingPolicy` / `StatementPolicy` の穴だけ空け、
実装は個人事業主向けの 1 種のみ。

**却下した選択肢**: IFRS / 日本基準の選択に対応する。

**理由**: 新リース会計基準等の対象は主に上場企業・大会社。
個人事業主は「中小企業の会計に関する基本要領」等で足りる。
工数が桁違いで、かつ上場企業が OSS を採用する確率は低い。

---

## D-012 タイムスタンプ（認定業者）を実装しない

**決定**: 真実性の確保は append-only（訂正削除履歴）で対応する。

**理由**: タイムスタンプ付与には認定業者との契約が必要で有料。
「誰もが無料で使える」というプロジェクトの前提と衝突する。

**方針の明示**: README に「タイムスタンプ方式ではなく訂正削除履歴方式で
設計している」と書く。ただし要件充足の主張はしない。

---

## D-013 JSON では金額を文字列で扱う

**決定**: MCP / HTTP API の入出力で金額は文字列（`"110000"`）。

**却下した選択肢**: JSON の number を使う。

**理由**: JSON の number は IEEE 754 倍精度。大きな整数や小数で誤差が出る可能性がある。
会計データでこれは許容できない。

**トレードオフ**: AI が number を渡してくることがある。
→ エラーメッセージで明示的に案内する。整数なら警告付きで受理する余地を残す。

> 【後日の訂正】この「整数なら警告付きで受理する余地」は Phase 3（`kaikei-mcp`
> の設計確定）で**却下した**。`{"amount": 110000}` がサーバに届いた時点では、
> クライアント側で既に IEEE 754 倍精度に丸められたかどうかを**サーバから
> 検出できない**。受理すれば「警告付きで壊れた金額を記帳する」ことになり、
> `CLAUDE.md` 冒頭「会計データは間違うと実害が出る」に反する。
> **number は整数であってもエラーにする。** JSON スキーマ上も金額フィールドを
> `"type": "string"` にし、number を受ける余地を型で塞ぐ
> （実装形は `docs/07-mcp-server.md` §5）。本決定（金額を文字列で扱うこと
> そのもの）は有効。

---

## D-014 削除系の MCP ツールを作らない

**決定**: `delete_journal_entry` / `update_journal_entry` / `execute_sql` を実装しない。

**理由**: API に存在しなければ AI が暴走しても帳簿は壊れない。
D-006（DB 権限）と多層防御になる。

**代替**: `reverse_journal_entry` のみ。理由の記述を必須にする。

---

## D-015 認証を Phase 4 まで実装しない

**決定**: ローカル・単一ユーザー・自己ホスト前提で進める。

**理由**: 早すぎる認証は開発を遅くする。
デフォルトで `127.0.0.1` にのみバインドし、外部公開時の注意を README に書く。

**留保**: Phase 5 でトークン認証を追加する余地を残す（構造上、後から入れられる）。

> 【後日の訂正】本決定は「バインドするソケットがある」ことを前提に書かれているが、
> Phase 3 の `kaikei-mcp` は **stdio トランスポート**（D-071）であり、
> **ソケットを一切開かない**。MCP クライアントが子プロセスとして spawn する
> ため、バインドアドレス・ポートという概念が存在せず、listen アドレスの
> 設定項目を作ってはならない。`kaikei-mcp` の信頼境界は「このプロセスを
> 起動できる OS ユーザー」と「接続に使う DB ロール」の2つだけであり、
> 認証機構を持たないのは**延期ではなく、認証する対象がそもそも無いため**である。
>
> 「デフォルトで `127.0.0.1` にのみバインドする」が実際に効いているのは
> (a) PostgreSQL のポート公開（`docker-compose.yml` の
> `127.0.0.1:5432:5432`。実装済み）と (b) Phase 4 の `kaikei-api`（axum / HTTP）
> であり、外部公開時の注意書きの宛先もそちらである。トークン認証の留保が
> 論点になるのも `kaikei-api` 以降（`docs/07-mcp-server.md` §8）。

---

## D-016 外貨は型だけ用意して換算は後回し

**決定**: `Currency` は最初から持つ。`FxPolicy` は Phase 後半。

**理由**: 個人事業主でも Stripe（USD 入金）、Upwork、海外 SaaS は普通にある。
後から `Money` に通貨を足すのは全書き換えになるが、
換算ポリシー（期中平均か直物か）は後から足せる。

**現状の動作**: JPY 以外の仕訳は作れるが換算は未実装。
異通貨の混在は集約の境界で弾く。

---

## D-017 個人事業主のみをターゲットにする

**決定**: 法人・他国は当面スコープ外。

**理由**:

- 要件が小さく、Phase 5 まで到達できる現実性がある
- 開発者自身が確定申告でドッグフーディングできる
- 「65 万円控除」という金額的な訴求点が明確

**拡張の余地**: `kaikei-jp/src/sole_proprietor/` と切ってあるので、
将来 `corporation/` が並ぶだけ。core は無変更で済む
（`ClosingPolicy` を trait にした D-001 の設計が効く）。

---

## D-018 Money::mul_ratio は Result を返す

**決定**: `pub fn mul_ratio(&self, ratio: Ratio, mode: RoundMode) -> Result<Money, CoreError>`。
`minor` を `rust_decimal::Decimal` に変換できない場合（表現上限 約7.9×10^28 を超える場合）は
`CoreError::InvalidAmount` を返す。

**却下した選択肢**: シグネチャを `-> Money` のまま維持し、変換失敗時は内部で panic させる。

**理由**: Phase 0 コードレビューで、初期実装が `Decimal::from(self.minor)`
（内部で `unwrap()` する変換）を使っており、`i128` の値が `Decimal` の表現上限を
超えると `mul_ratio` がパニックすることが発覚した。`Money` は `i128` 全域の値を
保持できるが `Decimal` は保持できないため、両者の境界には失敗しうる変換が必ず存在する。
`JournalEntry::new` の貸借検証と同様、失敗しうる操作は `Result` で呼び出し側に返すべきで、
core 内部での panic は避ける（`CLAUDE.md` の「次の手が分かる文言にする」という
エラーメッセージ方針とも整合する）。

**トレードオフ**: 呼び出し側は `mul_ratio` の結果を都度 `?` や `match` で処理する必要があり、
他の演算メソッド（`add`/`sub`）と同じく `Result` ベースの API になる。
`Money` は元々 `i128` の全域を扱える設計だが、実務上の按分計算でこの上限を
超える金額はまず発生しない。安全側に倒すコストとして許容する。

---

## D-019 エラーメッセージ内の列挙型は `{:?}` ではなくラベル関数で表示する

**決定**: `CoreError` のメッセージに列挙型（`AccountType`, `TagValueType` 等）を埋め込む際は
`{:?}`（derive された `Debug`）を使わず、`AccountType::label_ja()` /
`TagValueType::label_ja()` のような日本語ラベルを返す専用メソッドを使う。

**却下した選択肢**: `#[error("... {account_type:?} ...")]` のように `Debug` 表示をそのまま使う
（`docs/01-core-types.md` の初期の擬似コードはこの形だった）。

**理由**: `{:?}` はバリアント名（`Expense`, `Liability` 等）が英語のままメッセージに出る。
`CLAUDE.md` §11 が求める「MCP 経由で AI が自己修正できる文言」は日本語の文中に
英語のバリアント名が混ざると可読性が落ち、この方針と整合しない。
`AccountType::label_ja()`（`account.rs`）・`TagValueType::label_ja()`（`tag.rs`）として
既に実装済み。

**`CLAUDE.md` §1 との関係**: 同節は「`kaikei-core` に勘定科目の日本語名を書くこと」を禁止しているが、
これは現金・売掛金のような**個別の勘定科目名**を指す（`AccountDef.name` として外部から渡す
設計になっている）。`AccountType` と `TagValueType` は特定の勘定科目名ではなく、
資産・負債・純資産・収益・費用（5要素）やコード・テキスト・小数・日付のような
**世界共通の分類**であり、`docs/01-core-types.md` が明示的に core に置くと定めている型なので、
この禁止事項の対象外である。

**トレードオフ**: 新しい列挙型をエラーメッセージに含めるたびに `label_ja()` の実装が必要になる。
`{:?}` を使うより一手間増えるが、メッセージ品質の一貫性を保つコストとして許容する。

---

## D-020 Currency の minor_unit に上限 18 を設ける

**決定**: `Currency::new` の `minor_unit: u8` に上限（`Currency::MAX_MINOR_UNIT = 18`）を設け、
超える値は `CoreError::InvalidValue` で拒否する。

**却下した選択肢**: `minor_unit: u8` の型が表現できる 0〜255 をそのまま無検証で受理する。

**理由**: 横断監査で、`Currency::new("XXX", 40)` のような極端な `minor_unit` が無検証で
通り、`to_display_string` 内の `10u128.pow(minor_unit)` が桁あふれすることが発覚した。
debug ビルドでは `panic!`、release ビルド（`overflow-checks` 既定で wrapping）では
**無言に桁の違う金額を表示する**（実測: 正しい金額と全く異なる文字列が返り、しかも
`Money::parse` でラウンドトリップできない）。これは既に対応済みの `Money::neg()` /
`Money::abs()` の「release で無言に誤った値」と同じ欠陥クラスであり、`Currency::new` の
入口で塞ぐべき。

上限を 18 とした根拠:

- ISO 4217 の実在通貨で最大の小数桁数は 4（チリ・ウニダー・デ・フォメント等）
- `DOMAIN.md` §7 が「暗号資産は8桁以上」の小数桁数に言及しており、4 では狭すぎる
- Ethereum の最小単位 Wei は 18 桁。暗号資産のうち広く使われる基準として妥当な上限
- `i128::MAX`（約1.7×10^38）に対し `10^18` を掛けても整数部として扱える桁数の余裕が
  十分に残り、`Money::from_minor` が保持しうる実務上の金額と乗じても破綻しにくい

**トレードオフ**: 18 桁を超える極めて特殊な最小通貨単位（一部のメームコイン等）は
表現できない。→ 個人事業主向けという `DECISIONS.md` D-017 のスコープでは
現実的に発生しない。将来必要になれば `MAX_MINOR_UNIT` の見直しとして検討する。

---

## D-021 `journal_lines` / `period_snapshots` の `currency` / `currency_minor_unit` に DEFAULT を付けない

**決定**: `journal_lines.currency_minor_unit`（`SMALLINT NOT NULL CHECK (BETWEEN 0 AND 18)`）
を新設し、`currency` と合わせて両カラムとも `DEFAULT` を付けない。同じ理由で
`period_snapshots` にも `currency` / `currency_minor_unit` の組を新設し、同じく
`DEFAULT` を付けない（`balances` の金額をどの通貨・何桁の最小単位で解釈するかを
一意に決めるため。`journal_lines` と対称な構造にする）。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `currency_minor_unit SMALLINT NOT NULL DEFAULT 0` | `currency='USD'` のように通貨コードだけ指定して `currency_minor_unit` を書き忘れると既定で 0 になり、**本来 2 桁の通貨の金額が 100 倍ズレて保存されても `CHECK` 制約に引っかからない**。無言のデータ破損を許す設計になる |
| 通貨マスタテーブルを作って `minor_unit` を JOIN で引く | マスタは可変なので、後からマスタの `minor_unit` を直すと過去に記帳した金額の意味が変わってしまう。append-only の思想（D-006）に反する |

**理由**: `kaikei-core` の `Currency::new(code, minor_unit)` は `minor_unit` を必須で
要求し、`Money::to_display_string()` は `10^minor_unit` で小数点位置を決める。列が
無いと `Money` を復元できず、store 層に JPY=0/USD=2 のようなハードコード表を作る
しかなくなる（`CLAUDE.md` §8 違反）。また `CLAUDE.md` §2 マイグレーションの掟2
「カラム追加は NULL 許容のみ」により、**後から `NOT NULL` の列を足す道は塞がっている**
ため、テーブルがまだ存在しない Phase 1 が唯一の機会だった。上限 18 は
`Currency::MAX_MINOR_UNIT`（D-020）と一致させる。

**トレードオフ**: `INSERT` 時に毎回 `currency_minor_unit` を明示する必要があり、
省略すると（デフォルトが無いため）`NOT NULL` 違反で拒否される。→ これは意図した
挙動であり、トレードオフではなく設計の目的そのもの。

---

## D-022 ロール作成・権限付与をマイグレーションから分離する

**決定**: `kaikei_migrator` / `kaikei_app` ロールの作成とパスワード設定を
`docker/postgres/init/01-roles.sql` に集約し、マイグレーション（`crates/kaikei-store/migrations/`）
には一切書かない。`docker-compose`（`docker-entrypoint-initdb.d` 経由）と CI
（`database.yml` が明示的に `psql -f` で実行）の両方から、この同一ファイルを流す。

**却下した選択肢**: ロール作成を `0001` マイグレーションの中で行う
（`docs/03-database.md` の初期案 `0001_roles_and_grants.sql`）。

**理由**: ロールは PostgreSQL の**クラスタ単位**のオブジェクトであり、特定の
データベースに属さない。一方 `#[sqlx::test]` はテストのたびに新しいデータベースを
作成してマイグレーションを最初から再実行するため、ロール作成をマイグレーションに
書くと **2件目以降のテストが「ロールは既に存在する」エラーで全滅する**。
また、マイグレーションと初期化スクリプトを分けることで「ロール作成・権限付与の
記述箇所はここだけ」という真実の点が1つに定まり、docker-compose と CI で
権限設定がズレる事故を防げる。

**トレードオフ**: マイグレーション一式（`sqlx migrate run`）だけを実行しても
ロールが無ければ何も始まらず、`docker/postgres/init/01-roles.sql` を先に実行する
という前提知識が別途必要になる。→ `0001_baseline_privileges.sql` の `DO` ブロックが
前提条件（`kaikei_app` の存在と権限属性）を検証し、満たされていなければ
明示的なエラーメッセージで教える形で緩和する。

---

## D-023 採番は仕訳 INSERT と同一トランザクションで行う（欠番は原理的に発生しない）

**決定**: `entry_counters` の採番（`SELECT ... FOR UPDATE` → `UPDATE next_no`）を
仕訳（`journal_entries` / `journal_lines`）の `INSERT` と**同一トランザクション**で
行う。`entry_counters.skipped` は、それでも**意図的に**番号を飛ばした場合の理由を
記録するための専用フィールドであり、Phase 1 では書き込みを実装しない。

**却下した選択肢**: 採番を別トランザクション（例: Postgres の `SEQUENCE`）で行い、
検証失敗時は欠番として `skipped` に自動記録する。

**理由**: Postgres の `SEQUENCE` はトランザクションのロールバックとは独立に値を
払い出すため、検証失敗でロールバックしても採番した値は消費済みのままになり、
欠番が生じる。これに対し、`entry_counters` 行のロック＋更新を仕訳 INSERT と同じ
トランザクションに置けば、検証失敗時はカウンタの増分も仕訳行も一緒に巻き戻るため、
**通常は欠番が発生しない**。`docs/03-database.md` 初期案の「トランザクションが
失敗した場合、その番号は使用されず欠番となる」という記述は、採番を別トランザクションで
行う実装を前提としたものであり、同一トランザクション採番とは整合しないため
本決定に合わせて改訂した。

**トレードオフ**: 欠番の自動記録は行わない。運用上「意図的に」番号を飛ばしたい
（例: 誤って仕訳を作りかけて中断した痕跡を残したい）場合は、`skipped` への
書き込みを別途実装する必要がある。→ Phase 1 のスコープ外とし、必要になった時点で
追加する（欠番はロールバックで消えるため、記録には別トランザクションが要る。
監査ログ基盤を入れる Phase 3 に合わせるのが自然）。

---

## D-024 （欠番）

Phase 1 の PR-2 と PR-3 を並列で進めた際、両方が同じ番号を採ってしまい、
片方を採番し直した際に生じた欠番。内容のある決定は存在しない。
以降は並列作業の開始前に D 番号のレンジを事前割当する運用にしている。

---

## D-025 TaxContext は国非依存の4項目に限定する

**決定**: `kaikei-policy::TaxContext<'a>` は `{ as_of, chart, tag_schema,
counterparties }` の4項目（`AccountingDate` と core の型2つ、
`kaikei-policy` 自身の `CounterpartyIndex`）のみを持つ。年度別税区分マスタ
（`TaxCategoryTable`）や事業者設定（`JpSettings`）等の `kaikei-jp` 固有の型は
一切含めない。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `docs/04-jp-tax.md` 初期案（`categories: &TaxCategoryTable` / `settings: &JpSettings` を含む） | `TaxCategoryTable` / `JpSettings` は `kaikei-jp` の型（`ARCHITECTURE.md` §7）。そのまま `kaikei-policy` に書くと policy → jp の循環依存になり、`kaikei-app` も jp の型を知る必要が生じて `CLAUDE.md` §1 の依存方向が崩れる |
| 国非依存の `TaxCategoryTable` を `kaikei-policy` に置く | `direction: sales\|purchase` や `deductible` は VAT型（日本・EU）固有の構造で、`DOMAIN.md` §5 が言う米国 Sales Tax 型と両立しない。抽象層に置くのは筋が悪い |
| `TaxContext` を lookup trait のトレイトオブジェクトにする | 抽象が1段増えるだけで、実効は上記の決定と変わらない |

**理由**: 年度別税区分マスタと事業者設定は、`JpTaxPolicy`（Phase 2 の
`kaikei-jp` 実装）が**構築時**に保持すればよい。年度の選択は
`TaxContext::as_of`（取引日）で行える。YAML の読み込みは合成ルートの
起動時 I/O であり、trait メソッド自体は引き続き純関数のままになる
（D-005 を満たす）。

**トレードオフ**: 設定変更（税率改定・事業者区分の変更等）を反映するには
`Arc<dyn TaxPolicy>` を作り直す必要がある。単一ユーザー・自己ホスト前提
（D-015）なので、プロセス再起動で足りる。

---

## D-026 derive_tax_lines は確定後の明細一覧を返し、round は apply_ratio に置き換える

**決定**: `TaxPolicy::derive_tax_lines` の戻り値は
`TaxDerivation { lines: Vec<JournalLine>, notes: Vec<PolicyNote> }` とし、
`lines` は入力の明細＋生成された税額行を含む**確定後の明細一覧**（追加行だけ
ではない）とする。加えて `docs/04-jp-tax.md` 初期案にあった
`round(&self, raw: Money) -> Money` は定義せず、
`round_mode(&self, ctx: &TaxContext<'_>) -> RoundMode` と、それを使う既定実装
付きの `apply_ratio(&self, ctx: &TaxContext<'_>, base: Money, ratio: Ratio)
-> Result<Money, PolicyError>` に置き換える。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `derive_tax_lines` が追加行のみを返す（`docs/04-jp-tax.md` 初期案どおり） | 呼び出し側が「置き換えるべきか追加すべきか」を毎回判断する必要が生じる。誤って `extend` すると税額行が重複計上される事故が起きやすい。「確定後の一覧を返し置き換える」規約の方が呼び出し側の実装を単純化できる |
| `TaxPolicy::round(&self, raw: Money) -> Money`（初期案どおり） | `Money` は最小通貨単位の整数（`i128`）で端数（小数）を保持できない。呼び出す時点で既に整数に丸められた値しか渡せないため、この形は事実上の恒等関数にしかならず、実際に必要な「金額×比率を丸める」という操作を表現できない |
| `apply_ratio` が `-> Money`（`Result` を返さない） | `Money::mul_ratio` は D-018 により `Result` を返す（`rust_decimal` の表現上限超過時に `InvalidAmount`）。`apply_ratio` がそれをラップする以上、同じく `Result` にする必要がある |

**理由**: `Money` の内部表現（`i128` の整数）と `TaxPolicy` の責務（端数処理を
含む金額計算）を踏まえると、丸めは「金額×比率」を計算する瞬間にしか意味を
持たない。また `derive_tax_lines` の戻り値を「確定後の一覧」にすることで、
呼び出し側（`kaikei-app`）は常に `entry.lines = derivation.lines` という
単純な置き換えで済み、`extend` によるやり直し不能な重複計上を構造的に避け
られる。`notes: Vec<PolicyNote>` を持たせたのは、`CLAUDE.md` §10（提案系の
機能は候補と根拠を返し確定は人間に残す）が求める「非適格の経過措置がある
場合の扱い」等、断定を避けた注記を戻り値に含められるようにするため。

**トレードオフ**: `TaxDerivation` という中間型が1つ増える（`Vec<JournalLine>`
を直接返すより呼び出し側の型が1段増える）。`notes` の使い道が Phase 2 時点で
まだ具体化していないため、当面は空の `Vec` を返すだけの実装が大半になる
可能性がある。

---

## D-027 ClosingPolicy は ProposedEntry を返す（採番情報を持たない）

**決定**: `ClosingPolicy::closing_entries` / `opening_entries` は
`kaikei_core::NewEntry` ではなく `kaikei-policy::ProposedEntry`
（`entry_date` / `description` / `lines` のみ）を返す。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `docs/04-jp-tax.md` 初期案どおり `Vec<NewEntry>` を返す | `NewEntry` は `id: EntryId` と `entry_no: EntryNumber` を要求するが、この2つの採番は store の I/O（`kaikei-app` が `tx.next_entry_no` 等で払い出す）。policy は I/O を行わないため、`NewEntry` を構築する時点で本来存在しないデータを要求することになり、trait を純関数に保てなくなる |
| `ClosingPolicy` に `id`/`entry_no` をダミー値で埋めさせる | ダミー値を後から正しい採番で上書きし忘れる事故を誘発する。「採番前の状態」を型で表現する方が安全 |

**理由**: 採番前の仕訳は「まだ確定していない提案」であり、`EntryId` /
`EntryNumber` を持たない `ProposedEntry` という別の型で表現するのが自然。
呼び出し側（`kaikei-app`）が採番したうえで `NewEntry` に詰め替え、
`JournalEntry::new` で最終的な不変条件検証を行う。

**トレードオフ**: `ProposedEntry` という中間型が増え、`kaikei-app` 側で
`ProposedEntry` → `NewEntry` への変換コードが必要になる（採番と
`JournalEntry::new` 呼び出しをまとめた小さなヘルパーで吸収できる想定）。

---

## D-028 Counterparty はインボイス関連フィールドを直接持つ（D-025 との整合の例外）

**決定**: `kaikei-policy::Counterparty` は、取引先の識別情報として
インボイス登録番号（`invoice_registration_no`）や適格請求書発行事業者かどうか
（`is_qualified_invoice_issuer`）という**日本のインボイス制度固有の属性**を
フィールドとして直接持つ。これは D-025 で `TaxCategoryTable` / `JpSettings`
という国固有の型を `kaikei-policy` から排除したのとは扱いを変える例外である。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `Counterparty` を汎用の属性バッグ（例: `BTreeMap<String, String>`）にし、インボイス関連情報は `kaikei-jp` 側の別テーブルとして分離する | 汎用の属性バッグ機構を導入するコストが Phase 1 のスコープに対して過大。`TagSchema` に相当する検証機構を属性バッグにも別途用意しないと `CLAUDE.md` §4「ゴミ箱にしない」の規律が崩れる |
| インボイス関連フィールドを削除し、`TaxPolicy` が必要な情報を都度取得できる別 trait を追加する | 抽象が1段増えるだけで、`docs/03-database.md` の `counterparties` テーブル定義（`invoice_reg_no` / `is_qualified` 列）と実質的に同じデータを扱うことになり、複雑化の見返りが薄い |

**理由**: (a) `docs/03-database.md` の `counterparties` テーブル定義には、
このPR以前から既に `invoice_reg_no` / `is_qualified` 列が存在しており、この
PRが新たに持ち込んだ矛盾ではない。(b) 取引先マスタを完全に国非依存にするには
汎用の属性バッグ機構が必要になり、Phase 1 で導入するには過剰である。

**トレードオフ**: 将来、日本以外の国に対応する場合、これらのフィールドは
その国の `TaxPolicy` 実装にとって意味を持たない（常に `None` になる）。
他国対応が具体化した時点で、属性バッグ化（例: 汎用のキー・値フィールドへの
置き換え）を検討する余地を残す。

---

## D-029 トランザクション境界は `&mut Tx` を引数で引き回す（`kaikei-app` のポート層）

**決定**: `kaikei-app::ports::Store` は `begin(&self) -> Result<Self::Tx, RepoError>`
のみを持ち、ユースケース本体は `begin` も `commit`/`rollback` も呼ばない。
リポジトリ trait（`JournalRepo` / `ChartRepo` / `PeriodRepo` / `NumberingRepo`）は
すべて `&mut self` を取り、ユースケースは `&mut Tx`（`Tx: TxOps`）を引数として
受け取ってこれらを直接呼ぶ。`Store::Tx` はライフタイム引数を持たない関連型
（GAT にしない）。`begin`/`commit`/`rollback` は `tx::with_tx` ヘルパに一本化し、
`Store::begin` は `#[doc(hidden)]` にして直接呼ばないよう案内する。

**却下した選択肢**（3案を独立に設計・採点し、3人の審査員が 9/9/9 の満場一致で
本決定を選んだ）:

| 候補 | 却下理由 |
|---|---|
| 案1: UnitOfWork（`&mut dyn UnitOfWork` のような trait object にリポジトリ群を束ね、`execute` と `execute_in_tx` の2関数構成でユースケースの合成を扱う） | 「トランザクション内から別ユースケースを呼べない」問題を2関数構成で毎回回避する必要があり、`CLAUDE.md` §6「1ユースケース=1関数」を素直に満たせない。`&mut dyn Tx` はどのリポジトリに依存するかがシグネチャから消える |
| 案3: クロージャに実行させる（`Box<dyn for<'t> FnOnce(&'t mut (dyn Tx + 't)) -> BoxFuture<'t, Result<Box<dyn Any + Send>, _>> + Send + 's>` のような型でユースケースを表現） | GAT・HRTB・`Box<dyn Any>`・`self: Box<Self>` レシーバが要る最も複雑な形。`implementation of FnOnce is not general enough` を誘発しやすく、案3自身が回避策を3つ要すると認めている |

**理由**:

1. `ARCHITECTURE.md` §5 の規定シグネチャ（`execute<R, T>(repo: &R, tax: &T, ...) where R: JournalRepository`）とほぼ同型で、`CLAUDE.md` §6「依存が引数に全部現れる」を `where Tx: JournalRepo + ChartRepo + ...`（束ね trait `TxOps` を使えば `where Tx: TxOps`）という境界の形で素直に満たす
2. GAT・HRTB・`Box<dyn Any>` のような exotic な型機構が一切不要。`&mut Tx` の連続呼び出しは各式で借用が閉じるため、Rust が最も得意とする形になる
3. `begin`/`commit` を持たないため、ユースケースの合成（あるユースケースの中から別のユースケースを呼ぶ）が構造的に無料で手に入る
4. テストが境界を完全に握れる（`with_tx` に閉じたことで、fake に対する commit/rollback の検証が1箇所で完結する。`crates/kaikei-app/src/tx.rs` の `with_tx` テストを参照）

採用にあたり、他案から次の要素を取り込んだ:
- `AppClock: Clock + Send + Sync` のブランケット trait（`&dyn Clock` が `!Send` になる問題を1箇所に閉じる。`ports.rs`）
- 束ね trait `TxOps`（`where` 句を毎回4本書かずに済む）
- `RepoError` をドメイン語彙の enum にする（`error.rs`。D-032 参照）
- `with_tx` を唯一の推奨入口にし、`Store::begin` を `#[doc(hidden)]` にする

**トレードオフ**: `Store::Tx` に `'static` 相当の関連型を要求するため、実装側
（`kaikei-store`）はトランザクションを所有型として持つ必要がある
（`sqlx::PgPool::begin()` が `Transaction<'static, Postgres>` を返すため、この
制約は無理なく満たせる）。また `Store::begin` を直接呼んで `commit` を書き忘れる
と、エラーも警告も出ずに何も保存されない構造的リスクが残る（`with_tx` への
一本化と `#[doc(hidden)]` で緩和するが、完全には防げない）。

**合成ルート（axum の `State`）への実装指針**: 合成ルートでは `Arc<dyn
Store<Tx = ..>>` のような trait object ではなく、**具象型 `Arc<PgStore>`**
を `State` に積む。`Store` は関連型 `Tx` を持つため、trait object 化するには
`dyn Store<Tx = 具象型>` のように `Tx` を dyn 化の時点で具象型に固定する必要が
あり、その時点で「実装を差し替えられる」という抽象化の利点がほとんど残らない
（本番で使う `Store` 実装は通常1つ、`kaikei-store::PgStore` だけである）。
一方 `with_tx<S: Store>` はジェネリックのまま使えるため、具象型を渡しても
呼び出し側のコードが実装の詳細を意識する必要はない。

**これに伴い `ARCHITECTURE.md` §6 の記述（`Arc<dyn JournalRepository>` を
`State` に入れる、という旧・案1相当の設計）は本決定と矛盾するため、
古い記述のまま残っている。改訂は PR-8（結線 + E2E + ドキュメント改訂）で
まとめて行う。**

---

## D-030 `SystemClock` は記帳時刻をマイクロ秒粒度に丸めて返す

**決定**: `kaikei-app::clock::SystemClock`（`kaikei_core::Clock` の実装）は、
`SystemTime::now()` から得たナノ秒単位の値を、生成した時点でマイクロ秒に
丸めてから `Timestamp` に格納する（`nanos / 1_000 * 1_000`）。

**却下した選択肢**: ナノ秒のまま `Timestamp::from_unix_nanos` に渡す（丸めない）。

**理由**: `journal_entries.recorded_at` は `TIMESTAMPTZ`（PostgreSQL、マイクロ秒
精度）で保存される一方、`kaikei_core::Timestamp` はナノ秒精度を持つ。丸めずに
生成すると、保存して読み戻した値がナノ秒未満の端数だけ元の値と食い違い、
save → find の往復同値性を検証する proptest が必ず失敗する。生成時点（`Clock`
の実装）で丸めておけば、store 側は何も丸めずに素直に保存・復元でき、
「テストの方を緩める」という誤った圧力を避けられる。

**トレードオフ**: `SystemClock::now()` はナノ秒精度の情報を意図的に捨てる。
`kaikei_core::Timestamp` 自体はナノ秒を保持できる型なので、`FixedClock` 等の
テスト用 `Clock` にナノ秒未満の値を直接渡すことは引き続き可能（この場合は
丸めの対象外であり、`Timestamp` の往復同値性テスト自体は core 側に既に
存在する）。丸めるのは実時刻を返す `SystemClock` の実装だけであり、
`Timestamp` という型自体の精度を落とす決定ではない。

---

## D-031 read model 用の DTO（`view.rs`）を `kaikei-app` に持つ

**決定**: `kaikei_core::GroupKey` / `BalanceRow` / `TrialBalance` を read model の
戻り値としてそのまま使わず、`kaikei-app::view::BalanceRowView` /
`TrialBalanceView` という DTO を新設し、`ports::TrialBalanceQuery::trial_balance`
はこの DTO を返す。

**却下した選択肢**: `kaikei_core::BalanceRow` / `TrialBalance` を `kaikei-store`
の SQL 集計結果から直接構築して返す。

**理由**: `kaikei_core::GroupKey`（`trial_balance.rs`）には `impl` ブロックが
1つも無く、公開コンストラクタもアクセサも存在しない（実測確認済み）。
`BalanceRow` のフィールドは `pub` だが `group: GroupKey` を構築する手段が
core の外に無いため、`BalanceRow` / `TrialBalance` は core の外から**構築不能**
である。SQL 集計（`kaikei-store::query`）から直接組み立てられる DTO が
存在しないと read model 自体を実装できない。`GroupKeyView`（`BTreeMap<String,
String>`）はキーの型を `TagKey` ではなく `String` にし、SQL の集計結果
（例: `jsonb_object_agg`）から検証済みキーの再構築を経ずに直接組み立てられる
ようにしている。

**トレードオフ**: `kaikei_core::BalanceRow` と `kaikei-app::view::BalanceRowView`
はフィールド構成が似た別の型として並存する（呼び出し側が2つの型を意識する
必要がある）。`GroupKey::iter()` のようなアクセサを core に追加すれば
DTO を無くせる可能性があるが、core の変更は人間の承認事項（`CLAUDE.md` §9）
であり、Phase 1 ではこの DTO で対応する。

---

## D-032 `RepoError` はドメイン語彙の enum にする（`Box<dyn Error>` 一本にしない）

**決定**: `kaikei-app::error::RepoError` を `NotFound` / `AppendOnlyViolation` /
`Conflict` / `Corrupt` / `OutOfRange` / `Unsupported` / `Backend` の7バリアントを
持つ enum として定義する。SQLSTATE（`42501` = 権限拒否 / `P0001` = トリガ /
`23505` = 一意制約 等）の判別・写像は実装側（`kaikei-store`）が行う。

> 【後日の訂正】ここでの「`P0001` = トリガ」は D-038 で覆した。append-only 違反は
> `P0010`、貸借不一致は `P0011` という専用 ERRCODE に分離してあり、汎用の
> `P0001` は「どちらのトリガか断定できない」を意味する `RepoError::Backend`
> に写像される。本決定（enum を分ける方針そのもの）は有効。

**却下した選択肢**: `RepoError::Backend(Box<dyn std::error::Error + Send +
Sync>)` のような単一バリアントに永続化層のエラーをすべて包む。

**理由**: 単一バリアントに包むと、append-only 違反（DB権限の REVOKE、または
トリガによる拒否）が「ただの DB エラー」に潰れてしまい、`CLAUDE.md` §11
「次の手が分かる文言にする」を満たせなくなる。`RepoError::AppendOnlyViolation`
を受け取ったユースケースは「訂正は逆仕訳（`reverse`）で行ってください」と
案内できるが、単一バリアントではこの分岐ができない。同様に `Corrupt`
（`rehydrate` 前の再検証で検出した保存データの不整合）と `OutOfRange`
（`i128→i64`・`u32→i32` の変換失敗）を分けたのは、いずれも呼び出し側が
取るべき対応が異なるため。

**トレードオフ**: `kaikei-store` 側で SQLSTATE からこの enum への写像コードを
書く手間が生じる（`Box<dyn Error>` に包むだけの実装より初期コストが高い）。
会計データにおいて「次の手が分かるエラー」の価値がこのコストに見合うと判断した。

---

## D-033 試算表の `SUM(amount_minor)` は SQL 側で `BIGINT` にキャストする（PR-6 への申し送り）

**決定**: `kaikei-store::query::trial_balance`（PR-6）が発行する SQL は、
`SUM(amount_minor)` をそのまま受けず `SUM(amount_minor)::BIGINT` のように
明示的に `BIGINT` へキャストしたうえで `i64` として受け取る。桁あふれで
発生する SQLSTATE `22003`（numeric_value_out_of_range）は
`RepoError::OutOfRange` に写像する。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `SUM(amount_minor)` を `NUMERIC`/`Decimal` として受け取る | PostgreSQL の `SUM(bigint)` は規格上 `NUMERIC` を返すが、ワークスペースの sqlx feature（`Cargo.toml` の `[workspace.dependencies].sqlx`）に `NUMERIC` の decode 先（`rust_decimal` 等の feature）が無い。追加すると依存が増え、`kaikei-store` の許可された依存範囲を広げることになる |
| `rust_decimal` feature を sqlx に追加する | 依存を増やさない方針（`CLAUDE.md` §1「依存を追加したくなったら設計を疑うべきサイン」）に反する。`amount_minor` は元々 `BIGINT` 列であり、`SUM` の結果が実務上 `i64` の範囲を超えることは通常無い |

**理由**: `journal_lines.amount_minor` は `BIGINT`（`i64` 相当）の列であり、
個人事業主規模の年間仕訳数を前提とする限り `SUM` の結果が `i64` の範囲を
超えることは通常無い。SQL 側で明示的に `::BIGINT` へキャストして sqlx に
`i64` として decode させれば、追加の依存無しに実装できる。桁あふれが
万一発生した場合は `22003` を検出して `RepoError::OutOfRange` に写像し、
`as i64` のような無検証キャストで無言に切り詰めない（R7 と同じ規律）。

**トレードオフ**: 理論上 `i64::MAX` を超える合計金額（暗号資産等、極端な
`minor_unit` を持つ通貨での大量取引）は表現できない。`DECISIONS.md` D-017
（個人事業主のみをターゲットにする）のスコープでは現実的に発生しない。

---

## D-034 PR-5 は小さな先行コミットで骨組みを固めてから PR-6 と並列化する（申し送り）

**決定**: `kaikei-store`（PR-5）は、まず `lib.rs` の骨組み・`sqlstate.rs`（SQLSTATE
→ `RepoError` の写像）・`tags.rs`（`TagSet` ⇄ JSONB 表現）だけの小さな
先行コミットを入れる。この3点が固まった後で PR-6（read model）を並列に
開始する。

**却下した選択肢**: PR-5 と PR-6 を最初から完全並列で始める。

**理由**: `sqlstate.rs`（SQLSTATE の判別規則）と `tags.rs`（JSONB との
相互変換規則）は、書き込み側（PR-5 本体）と read model 側（PR-6）の両方が
参照する共通基盤である。この2点の規約が固まる前に両方が並走すると、
どちらが正かの手戻りが発生しやすい。先に小さくマージすることで、
後続の並列作業が共通基盤の上に乗る形になる。

**トレードオフ**: PR-5 の着手から PR-6 の並列開始までにわずかな直列区間が
生まれる。共通基盤のブレを防ぐコストとして許容する。

---

## D-035 `TagSet` の JSONB 表現は `{"t": <型>, "v": <値>}`。`"v"` は常に文字列にする

**決定**: `journal_lines.tags`（JSONB）は、`TagSet` を JSON オブジェクトとして
表現する。キーはタグキー文字列（`TagKey::as_str()`）、値は
`{"t": <型識別子>, "v": <値>}` という2フィールドのオブジェクトにする。
`"t"` は `"code"` / `"text"` / `"decimal"` / `"date"` のいずれか
（`TagValueType` の4バリアントに対応）。**`"v"` は型によらず常に JSON
文字列にする**（`TagValue::Decimal` は `rust_decimal::Decimal` の
`Display`/`FromStr` を経由した文字列、`TagValue::Date` は
`AccountingDate::to_iso_string()`/`parse()` を経由した ISO 文字列）。

```json
{
  "tax_category": {"t": "code", "v": "10"},
  "business_ratio": {"t": "decimal", "v": "0.8"},
  "memo_date": {"t": "date", "v": "2026-04-15"}
}
```

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `"v"` を型に応じて JSON の number/string を使い分ける（`Decimal` は number） | `DECISIONS.md` D-013「JSON では金額を文字列で扱う」と同じ理由で、IEEE754 の丸め誤差を経由する可能性がある。`business_ratio` のような按分比率は税額計算の入力になりうるため、number 化による精度劣化を避ける |
| タグ全体を `[{"k": ..., "t": ..., "v": ...}, ...]` の配列にする | PR-6 の SQL（`l.tags -> k ->> 'v'`）がキーで直接オブジェクトを引けなくなり、`jsonb_array_elements` 経由の展開が必要になって集計 SQL が複雑化する |

**理由**: `"v"` を常に文字列にすることで、PR-6 の read model が
`l.tags -> k ->> 'v'`（`->` でキー `k` に対応する `{"t","v"}` オブジェクトを
取り出し、`->>'v'` でその `"v"` フィールドをテキストとして取り出す）という
型を意識しない素直な SQL でグルーピング用の値を取り出せる。`kaikei-store`
の `tags.rs`（`tag_set_to_json`/`tag_set_from_json`）がこの表現の唯一の
読み書き経路であり、想定外の形（オブジェクトでない、`"t"`/`"v"` が無い、
未知の型タグ、パースできない小数・日付）は panic せず `RepoError::Corrupt`
を返す。

**トレードオフ**: `"t"` フィールドの分だけ JSONB のサイズがわずかに増える
（1行あたり数バイト）。個人事業主規模のデータ量では無視できるコストと判断した。

---

## D-036 store 側は記帳時刻の丸めを行わない（`Timestamp` ⇄ `chrono::DateTime<Utc>`）

**決定**: `kaikei-store::convert::timestamp_to_datetime` /
`datetime_to_timestamp` は、`Timestamp`（ナノ秒精度）と
`chrono::DateTime<Utc>` の間でナノ秒精度をそのまま保持して変換する
（マイクロ秒への丸めをここでは行わない）。

**却下した選択肢**: `timestamp_to_datetime` の内部でマイクロ秒未満を
切り捨ててから `chrono::DateTime<Utc>` に変換する（DB 列の精度に合わせて
store 側でも丸める）。

**理由**: `kaikei-app::clock::SystemClock` が記帳時刻を生成する時点で
既にマイクロ秒粒度に丸めているため（D-030）、通常の記帳経路でこの変換に
渡される `Timestamp` は常にマイクロ秒境界に揃っている。したがって store
側で重ねて丸める必要が無い。実際にマイクロ秒未満の端数が失われうるのは
`sqlx` が `chrono::DateTime<Utc>` を `TIMESTAMPTZ` の実際のワイヤ形式へ
エンコードする段階（`TIMESTAMPTZ` という DB 列の型そのものが持つ精度）で
あり、そこは store の変換コードの責務の外にある。`convert.rs` 自身が
丸めを行わないことで、往復同値性を検証する proptest（PR-5 本体
`tests/round_trip.rs`）はマイクロ秒に揃った値を入力すればそのまま
「ちょうど一致」を期待でき、丸めロジックの二重実装（`SystemClock` と
`convert.rs` の両方）による不一致のリスクを避けられる。

**トレードオフ**: `FixedClock` 等でマイクロ秒に揃っていない `Timestamp`
を明示的に作り、これを実際に PostgreSQL へ保存して読み戻すと、
マイクロ秒未満の端数は失われる（`convert.rs` の変換自体は保持するが、
DB のワイヤ形式が保持できないため）。PR-5 本体の `tests/round_trip.rs` は
この性質を踏まえ、マイクロ秒に揃った値のみを生成対象にすること。

---

## D-037 SQLSTATE `23502`/`23514`（not_null_violation/check_violation）は `RepoError::Corrupt` に写像する

**決定**: `kaikei-store::sqlstate::map_sqlstate` は SQLSTATE `23502`
（not_null_violation）と `23514`（check_violation）の両方を
`RepoError::Corrupt` に写像する。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `RepoError::Conflict` に写像する | `Conflict` は一意制約違反（重複データ）を指す（`DECISIONS.md` D-032）。not_null/check 違反は意味が異なり、呼び出し側に誤った対応（重複解消）を促してしまう |
| `RepoError::Backend`（その他扱い）に写像する | 「保存しようとしたデータの構造そのものが不正」という診断情報が、接続断等の無分類failureに埋もれて消えてしまう |

**理由**: `insert_entry` 等に渡すデータは `kaikei_core::JournalEntry::new`
/ `reverse` が既に検証済みのため、この2つの SQLSTATE が実際に発生するのは
「store 層のマッピングコードがドメインの不変条件と食い違う行を組み立てた」
場合にほぼ限られる。`RepoError::Corrupt` の doc コメントは主に「復元処理の
直前に行う再検証で検出した不整合」を指すが、「永続化しようとしたデータが
構造的な不変条件を満たさない」という点で性質が同じであり、既存の7
バリアントの中ではこれが最も近い。

**トレードオフ・既知の限界**: SQLSTATE `P0001`（raise_exception）は
`migrations/0004_append_only_triggers.sql` の `reject_mutation`
（append-only 違反）と `assert_entry_is_balanced`（貸借不一致検出）の
両方が使う共通コードであり、`map_sqlstate` は現状どちらも
`RepoError::AppendOnlyViolation` として扱う。後者が実際に発火するのは
`JournalEntry::new` の検証を経ずに `journal_lines` へ書き込まれた場合
（store 層のバグ）に限られ、通常運用では発生しない前提のため、メッセージ
文字列による判別ロジックは本コミットのスコープでは追加しない。

> **追記（この決定は覆した）**: 上記の「既知の限界として受容する」という
> 判断は、`CLAUDE.md` §11（次の手が分かる文言にする）違反であり、
> Phase 0 の循環参照バグ（無関係の科目を犯人として名指しし、破壊的で
> 無駄な修正に誘導する）と同じ欠陥クラスであると判明したため、
> **D-038 で覆した**。`P0001` は現在 `RepoError::Backend` に写像される
> （`AppendOnlyViolation` ではない）。詳細は D-038 を参照。

---

## D-038 append-only 違反と貸借不一致の SQLSTATE を分離する（D-037 を覆す決定）

**決定**: 新規マイグレーション `migrations/0008_distinct_error_codes.sql`
（`CREATE OR REPLACE FUNCTION`。適用済みの `0004_append_only_triggers.sql`
は編集しない）で、`reject_mutation()`（append-only 違反）と
`assert_entry_is_balanced()`（貸借不一致検出）の2つのトリガ関数それぞれに
`RAISE EXCEPTION ... USING ERRCODE = '...'` で異なる SQLSTATE を明示する。

- `P0010`: `reject_mutation()`（append-only 違反）
- `P0011`: `assert_entry_is_balanced()`（貸借不一致。store層のバグ検出）

これに伴い `kaikei-store::sqlstate::map_sqlstate` を更新する。
`P0010` → `RepoError::AppendOnlyViolation`、`P0011` → `RepoError::Corrupt`
（`AppendOnlyViolation` ではない。「逆仕訳で訂正してください」とは案内
しない）。`P0001` 自体は ERRCODE を指定しない汎用の `raise_exception` として
残り、`RepoError::Backend` に写像する（どちらのトリガかを断定できない
汎用コードから特定の対処法を案内するとかえって誤りうるため）。エラー
メッセージ本文は変更しない。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `0004_append_only_triggers.sql` を直接編集して ERRCODE を追加する | `CLAUDE.md` §2 マイグレーションの掟（適用済みファイルを書き換えない）に反する。`sqlx` は `_sqlx_migrations` テーブルで各マイグレーションの checksum を検証しており、適用済みファイルを1バイトでも書き換えると次回の `migrate run` が checksum 不一致で失敗する（R11） |
| `P0001` のままメッセージ文字列の内容で判別する（D-037 の「既知の限界」を維持する） | メッセージ文字列のパターンマッチはトリガの文言変更に対して壊れやすく、`kaikei-store::sqlstate` という「SQLSTATE を見て判断する」という設計方針（`DECISIONS.md` D-032）にも反する。ERRCODE そのものを分ける方が構造的に確実 |
| `P0001` を維持しつつ `assert_entry_is_balanced()` だけ新しいコードに変える（`reject_mutation()` は `P0001` のまま） | `reject_mutation()` は append-only 違反という最も高頻度に発生しうる経路であり、これを汎用コード `P0001` に残したままにすると、将来 ERRCODE 未指定の別の `RAISE EXCEPTION` が追加されたときに再び判別不能になる。両方を専用コードへ移し、`P0001` 自体は「どちらでもない」ことを表す汎用コードとして空けておく方が将来の拡張に対して頑健 |

**理由**: 当初（D-037）は「`P0001` の共有は起こりにくい状況（store層の
バグでしか貸借不一致トリガは発火しない）なので、意図的に許容する」と
判断していた。しかしこの判断は、`RepoError::AppendOnlyViolation` の
Display（`kaikei-app::error::RepoError`）が常に「訂正は逆仕訳で行って
ください」という文言を含むため、貸借不一致（store層のバグ、逆仕訳では
直せない）が発生した場合に**完全に誤った対処法を案内する**ことを意味する。
これは `CLAUDE.md` §11「次の手が分かる文言にする」に反し、Phase 0 で
発見された循環参照バグ（`ChartOfAccounts::new` が循環に無関係な科目を
犯人として名指しし、その科目を修正するという無駄で的外れな対応に
誘導した）と同じ欠陥クラス（**誤った診断が誤った、時に破壊的な対応に
誘導する**）である。SQLSTATE を実際に分離することで、`map_sqlstate`
という1箇所の写像ロジックだけで正しい案内を機械的に保証できる。

ERRCODE の選定は PostgreSQL のエラーコード一覧でクラス `P0`
（PL/pgSQL Error Codes）を使う。このクラスの `P0000`〜`P0004`
（`plpgsql_error` / `raise_exception` / `no_data_found` / `too_many_rows` /
`assign_incompatible_datatypes`）は PL/pgSQL 本体の組み込み擬似エラーとして
既に割り当て済みだが、`P0005` 以降はどの組み込みコードにも割り当てられて
いない。将来 PostgreSQL 本体がこのクラスに新しい組み込みコードを追加する
可能性に備えて `P0005`〜`P0009` を空けたうえで `P0010`/`P0011` を割り当てた
（クラス自体は PL/pgSQL の `RAISE EXCEPTION` から生じるアプリケーション
固有のエラーという意味で一致しており、既存の標準 SQLSTATE とは衝突しない）。

**トレードオフ**: 新規マイグレーション1件が増える（`0004` を直接編集
できないため）。また `P0001` を `Backend` に落とすことで、将来
ERRCODE 未指定の `RAISE EXCEPTION` が別の意図で追加された場合、その
エラーも「未分類」として扱われる（適切な専用 ERRCODE を都度割り当てる
運用を続ける必要がある）。

---

## D-039 `PgTx<'c>` は `Option<Transaction<'c, Postgres>>` を保持し、`Drop` は `Option` の状態だけで commit 忘れを判定する

**決定**: `kaikei-store::store::PgTx<'c>` は `sqlx::Transaction<'c, Postgres>` を
直接ではなく `Option<Transaction<'c, Postgres>>` として保持する。
`TxScope::commit`/`rollback`（いずれも `self` を値で取る）は
`Option::take()` で中身を取り出してから `sqlx::Transaction::commit`/
`rollback` に渡す。`Drop::drop` は「`tx` フィールドがまだ `Some` のまま
破棄された」ことだけを見て `tracing::warn!` する（`committed: bool` の
ような別フラグは持たない）。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `tx: Transaction<'c, Postgres>`（`Option` で包まない）＋ `committed: bool` フラグ | `PgTx` が `Drop` を実装した時点で、`commit`/`rollback` 内から `self.tx`（フィールド）を直接ムーブして `Transaction::commit(self.tx)` を呼ぶことができない（E0509: 型が `Drop` を実装している場合、そのフィールドを部分的にムーブできない）。実測でこのエラーを確認した |
| `committed: bool` フラグを `Option` と併用する | `Option::take()` 後は `tx` が必ず `None` になるため、`committed` フラグは常に `tx.is_none()` と同じ値になり、状態を二重に持つだけで一方が他方の劣化コピーになる。片方を更新し忘れるバグの余地を残すだけで得るものが無い |

**理由**: `Option::take()` は `&mut self` を取るメソッドであり、`Drop` を
実装した型に対しても問題なく呼べる（部分ムーブと違い、`Option` 自体を
その場で `None` に差し替えるだけで所有権の移動を型システムの外から
安全に行える、`Option` 型に用意された標準的な回避パターン）。これにより
`committed` の追加フィールド無しで、「`tx` が `Some` のまま `Drop` された
＝ `with_tx`（`crates/kaikei-app/src/tx.rs`）を経由せず commit/rollback を
呼び忘れた」ことを1フィールドだけで判定できる（phase1計画 G5 / R10）。

**トレードオフ**: `conn()`（各 repo 実装がクエリ発行に使う接続を返す
メソッド）は `self.tx.as_mut().expect(...)` という一段の間接参照を経る。
`commit`/`rollback` 後にこのメソッドを呼び出すコードは存在しない
（`TxScope::commit`/`rollback` が `self` を消費するため型で防がれる）ので
`expect` が実際に失敗することは無いが、理論上のパニック経路が1つ増える
（呼び出し側のバグでのみ到達する）。

---

## D-040 採番は `RETURNING next_no - 1` の1文 upsert、明細の一括 INSERT は `UNNEST` にする

**決定**: `kaikei-store::numbering::PgTx::next_entry_no` は以下の1文で
採番する。

```sql
INSERT INTO entry_counters (fiscal_year, next_no) VALUES ($1, 2)
ON CONFLICT (fiscal_year) DO UPDATE SET next_no = entry_counters.next_no + 1
RETURNING next_no - 1
```

`entry_counters.next_no` は「次に払い出す仕訳番号」を表す。初回
（該当年度の行が無い）は `next_no = 2` で `INSERT` し、`RETURNING
next_no - 1` で `1`（今回払い出す番号）を返す。2回目以降は既存行を
`+1` した上で、更新後の `next_no - 1`（＝更新前の `next_no`。今回
払い出す番号）を返す。

また `kaikei-store::journal::PgTx::insert_entry` の明細一括 INSERT は
`UNNEST` で1文にまとめる。

```sql
INSERT INTO journal_lines (entry_id, line_no, account_code, side, amount_minor,
                            currency, currency_minor_unit, tags, memo)
SELECT $1, u.line_no, u.account_code, u.side, u.amount_minor, u.currency,
       u.currency_minor_unit, u.tags, u.memo
FROM UNNEST($2::smallint[], $3::text[], $4::smallint[], $5::bigint[],
            $6::text[], $7::smallint[], $8::jsonb[], $9::text[])
     AS u(line_no, account_code, side, amount_minor, currency,
          currency_minor_unit, tags, memo)
```

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| 採番: `SELECT next_no FROM entry_counters WHERE fiscal_year = $1 FOR UPDATE` の後に別途 `UPDATE`（2文） | 往復が増えるだけでなく、「行が無い場合（その年度で最初の仕訳）」を別途 `INSERT ... ON CONFLICT DO NOTHING` で分岐する必要があり、競合時に再試行ロジックが要る。1文の `ON CONFLICT DO UPDATE` なら初回・2回目以降が同じ1文で閉じ、行ロックの取得からカウンタ更新までが単一のアトミックな操作になる |
| 明細 INSERT: `entry.lines()` をループして1行ずつ `INSERT`（`n` 回の往復） | 明細数が増えるほど往復回数が線形に増える。`UNNEST` なら明細数によらず常に2回の往復（仕訳ヘッダ1回＋明細一括1回）に収まる（phase1計画 G7） |

**理由**: 採番と仕訳 INSERT を同一トランザクションで行うため、検証
失敗時はカウンタの増分も一緒に巻き戻り、欠番は原理的に発生しない
（`migrations/0006_entry_counters.sql` のコメント、D-023 相当の決定と
整合）。`UNNEST` への配列バインドは `Vec<T>`（`T` が `PgHasArrayType` を
実装する型。`i16`/`i64`/`String`/`serde_json::Value`/`Option<String>` は
いずれも sqlx-postgres が実装済み）をそのまま `.bind()` に渡すだけで済み、
`&Vec<T>` のような参照を経由する必要は無い（実装時に `Vec<T>` を直接
渡す形で確認済み）。

**トレードオフ**: `RETURNING next_no - 1` という引き算は、列名
`next_no`（「次に払い出す番号」）と実際に返す値（「今回払い出した番号」）
の間に1つ間接がある。可読性はコメントに委ねる。

---

## D-041 `mapper.rs` の検証テストは `tests/` の統合テストではなく `#[cfg(test)] mod tests` として同一ファイルに置く。書き込み時の追加防御（`document_refs` 非対応・NULバイト摘要の拒否）を `insert_entry` に実装する

**決定その1（テスト配置）**: `journal/row.rs` の `JournalEntryRow` /
`JournalLineRow` / `EntryRows` は `pub(crate)` のまま維持し（store crate
内部の実装詳細を外部に公開しない設計）、`mapper.rs` の
`TryFrom<EntryRows> for JournalEntry` の9項目の検証を確認するテストは、
`crates/kaikei-store/tests/mapper_guard.rs` のような別クレートとしての
統合テストではなく、`mapper.rs` 自身の `#[cfg(test)] mod tests` として
実装する。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `tests/mapper_guard.rs` を作り、`JournalEntryRow` 等を `pub`（`pub(crate)` ではなく）にする | Rust の可視性規則上、`tests/*.rs` は当該クレートを外部依存として参照する別クレートであり、`pub(crate)` 項目には到達できない（実測で `E0603` 相当の非公開エラーになることを確認）。到達可能にするには `pub` にする必要があるが、それは「DB行の生表現は crate 内部の実装詳細」という設計意図（`row.rs` のモジュール doc）を、テストの都合だけのために崩すことになる |

**理由**: `convert.rs` / `tags.rs` / `sqlstate.rs` は元々すべて
`#[cfg(test)] mod tests` を同一ファイルの末尾に置く配置をこの crate で
既に確立しており、`mapper.rs` もこの規約に揃える方が一貫性が高い。
9項目の検証は「壊れた `Row` を渡したときに panic せず
`RepoError::Corrupt` になること」を確認できれば目的を達成し、
`#[should_panic]` を使わず素直に `Result` を検査するだけで良い
（呼び出し側から見た「panic しないことの検証」は、通常の `#[test] fn`
がテスト自体をパニックさせずに完走することで自然に満たされる）。

**決定その2（書き込み側の追加防御）**: `journal::PgTx::insert_entry` は
`JournalEntry::rehydrate` 側の検証とは別に、書き込み前の2点を明示的に
検証する。

1. `entry.document_refs()` が非空なら `RepoError::Unsupported` を返す
   （F-1・人間承認済み。逆仕訳・証憑紐付けは Phase 4 の
   `attach_document` ユースケースに送る。core の `JournalEntry::reverse`
   は `document_refs` を複製しないため、この制約は `reverse_entry` の
   実装（PR-7）には影響しない）
2. `entry.description()` が U+0000（NUL）を含むなら `RepoError::Corrupt`
   を返す（phase1計画 R12。PostgreSQL の `text` は NUL を格納できないが、
   `JournalEntry::new` の摘要検証は `trim().is_empty()` のみで NUL を
   拒否しないため、ドメイン検証を通過したデータが保存段階で分かりにくい
   DB エラーとして落ちる経路を塞ぐ）

**却下した選択肢**: 検証を行わずそのまま SQL に渡し、Postgres 側の
エラー（`document_refs` は列が無いため静かに欠落、NUL は
`invalid_byte_sequence` 系のエラー）に委ねる。

**理由**: 「保存できないものを静かに落とさない」という会計データの
正しい振る舞いのため。`document_refs` は保存先の列自体が存在しないため
検証しなければ**エラーにもならず単に消える**（最悪の失敗モード）。
NUL バイトは Postgres 側のエラーメッセージが「なぜ拒否されたか」を
`CLAUDE.md` §11 が求める水準で説明しないため、店側で意味のある
`RepoError` に変換する。

**トレードオフ**: core（`kaikei-core::JournalEntry`）の摘要検証に
制御文字拒否を足すべきかは未解決のまま残る（core は不変層であり変更は
人間の承認事項。`CLAUDE.md` §9）。この疑問は `docs/` へ書き出す判断を
人間に委ねる（Phase 1 の他の申し送りと同様の扱い）。
## D-042 試算表read modelは集計対象全体で通貨が単一であることを要求する

**決定**: `kaikei-store::query::trial_balance`（PR-6）は、集計結果に
2種類以上の `(currency, currency_minor_unit)` の組が現れた場合、
`RepoError::Unsupported` を返す。`journal_lines` は行ごとに
`currency`/`currency_minor_unit` を持つため理論上は同一期間・同一科目に
複数通貨が混在しうるが、判定の粒度は科目単位ではなく**集計対象全体**とする。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| 科目単位で通貨が一致していればよいとする（`GROUP BY` に通貨を含め、科目ごとに異なる通貨の行を許容する） | `kaikei_core::TrialBalance::from_entries` は対象の仕訳集合**全体**で通貨が単一であることを要求し（`CoreError::CurrencyMismatch`）、科目単位の判定ではない。read model側だけ緩い基準にすると、同じデータに対してcoreとSQL集計で異なる成功/失敗の結果になり、差分テスト（`tests/trial_balance_differential.rs`）で対照できなくなる |
| 複数通貨をそのまま複数行として返す（呼び出し側に通貨ごとの合算を委ねる） | Phase 1は個人事業主のJPY単一通貨を前提としており（`README.md`）、呼び出し側（app層、PR-7）に複数通貨対応の合算ロジックを要求するのはスコープ外の先取り実装になる（YAGNI）。外貨対応は`DECISIONS.md` D-016で明示的に将来課題とされている |

**理由**: `kaikei_core::TrialBalance::from_entries` と同じ粒度で失敗させることで、
「SQL集計とcoreの`from_entries`が一致する」という差分テストの前提
（`tests/trial_balance_differential.rs::trial_balance_rejects_mixed_currencies_like_core_does`）
が成立する。エラー種別は`RepoError::CurrencyMismatch`のような専用バリアントを
`kaikei-app::error::RepoError`に追加するのではなく、既存の`Unsupported`
（「現在の実装ではサポートしていない操作」）を使う。複数通貨のデータ自体は
不正ではなく（`journal_lines`のスキーマ上正当なデータ）、単に Phase 1 の
read modelがそれを表示する手段を持たないだけなので、`Corrupt`（データが
不正）ではなく`Unsupported`（機能未対応）が意味的に正しい。

**トレードオフ**: `RepoError`に新しいバリアントを追加する余地は今回は
使わない。`Unsupported`は元々「逆仕訳への証憑紐付け」等の別の意味でも
使われており、`reason`文字列を読まないと具体的に何が未対応なのか
分からない。次の手が分かる文言（`CLAUDE.md` §11）は`reason`側で担保する
（「期間や科目を絞り込んで再実行してください」という具体的な対処法を含める）。

---

## D-043 差分テストは`group_by`空のケースを主戦場にし、`group_by`ありは科目単位のロールアップで間接検証する

**決定**: `tests/trial_balance_differential.rs`は、`group_by = &[]`
（グループ化なし）のケースを主戦場にして、SQL集計とcoreの
`TrialBalance::from_entries`の結果を行単位で完全に比較する
（`trial_balance_matches_core_for_empty_group_by`。5科目種別すべての
残高の向きもここで検証する）。`group_by`ありのケースは、SQL側の結果を
科目ごとにロールアップして`TrialBalance::balance_of`と突き合わせ、
かつ各グループの内容（`GroupKeyView`）はテストが構築した既知のタグ
割り当てに対する期待値と直接比較する
（`trial_balance_group_by_rolls_up_to_the_same_balance_as_core`）。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `kaikei_core::GroupKey`にアクセサ（`iter()`等）を追加し、SQL側の`GroupKeyView`と直接比較する | phase1計画 §0-7/R9で確認済みのとおり、`GroupKey`は現在`impl`ブロックを1つも持たない不変層（`kaikei-core`）の型であり、変更は人間の承認事項（`CLAUDE.md` §9）。Phase 1のスコープでは変更しない。必要と判断されたらPhase 2以降に人間の承認を得て提案する |
| `group_by`ありのケースを丸ごとテストしない（`group_by = &[]`のみで済ませる） | `group_by`はこのPRの完了条件の一部（`kaikei_app::ports::TrialBalanceQuery`のシグネチャに含まれる主要な引数）であり、SQL側の実装（`unnest`+`jsonb_object_agg`によるグルーピング）を全く検証しないのは欠陥を見逃すリスクが高い |

**理由**: `GroupKey`を直接比較できないという制約の下で、実務上十分な
検証強度を得るために2段構えにした。「主戦場」（`group_by`なし）は
最も基本的なケースであり、残高の向き・SUMの正しさ・LEFT JOINの正しさ
など、grouping以外の全てのロジックを完全な行単位一致で検証する。
「間接検証」（`group_by`あり）は、coreが提供する唯一の公開API
（`balance_of`。科目単位で全グループを合算する）を使ってロールアップの
整合性を検証しつつ、グルーピングの分割そのものが正しいことは、
テストが構築した既知の入力データに対する直接的な期待値比較で担保する。

**トレードオフ**: 「SQL側が生成した`GroupKeyView`の集合とcoreの
`GroupKey`の集合が完全に同型である」ことを機械的に証明してはいない
（テストデータに対する期待値ベースの検証にとどまる）。`GroupKey`に
アクセサが追加されれば、より強い自動的な差分比較に置き換えられる。

---

## D-044 `journal_lines.account_code`に対応する`accounts`行が無い場合は`RepoError::Corrupt`にする（`accounts`へのJOINは`LEFT JOIN`）

**決定**: `kaikei-store::query::trial_balance`は`accounts`へ`LEFT JOIN`する
（`INNER JOIN`にしない）。対応する科目が見つからない
（`account_type`が`NULL`）行が集計結果に含まれる場合、その行を黙って
除外せず`RepoError::Corrupt`を返す。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `accounts`へ`INNER JOIN`する | `journal_lines.account_code`に`accounts.code`への外部キー制約は無い（`docs/03-database.md` §2、`crates/kaikei-store/tests/common/mod.rs`のコメント）。`INNER JOIN`にすると、対応する科目が存在しない行が**黙って**試算表の集計から除外される。これは「借方合計と貸方合計が一致しない試算表を返す」という、CLAUDE.md §2・§11が最も嫌う「気づかれない静かなデータ破損」を生む |
| 科目が見つからない場合は`account_type`不明のまま行を返す（呼び出し側に判断を委ねる） | `kaikei_app::view::BalanceRowView::account_type`は`Option`ではなく必須フィールドであり、契約（`kaikei-app/src/view.rs`、★凍結済み）を変更する必要がある。PR-6の権限内では契約を変更しない |

**理由**: phase1計画 R4「無検証APIの危険面積を減らす」と同じ規律
（`journal/mapper.rs`の9項目再検証と同種）を、read model側にも適用した。
`accounts`はマスタ（可変）であり、`journal_lines`は帳簿（append-only）
なので、両者の整合性はDBの制約だけでは保証されない
（`docs/03-database.md` §2「過去の仕訳が参照しているため物理削除しない」
という運用上の前提はあるが、DBのCHECK/FKとして強制されてはいない）。
「保存できないものを静かに落とさない」という規律を、書き込み側
（`journal/mapper.rs`、PR-5）だけでなく読み取り側（read model、PR-6）にも
一貫して適用する。

**トレードオフ**: 通常運用（マスタと帳簿が整合している）では
`LEFT JOIN`と`INNER JOIN`の実行結果に差は無く、`LEFT JOIN`によるJOIN
コストのわずかな増加のみがトレードオフになる（個人事業主規模の
仕訳件数では無視できる）。
## D-045 ユースケース関数は依存を素の引数として受け取る（`PostEntryDeps` のような集約構造体を導入しない）

**決定**: `kaikei-app::usecase::{post_entry, reverse_entry, report}::execute`
は、依存（`&dyn TaxPolicy` / `&TagSchema` / `&dyn IdGenerator` /
`&dyn AppClock` / `&BookSettings` 等）を1つずつの素の引数として受け取る。
これらをまとめる `PostEntryDeps<'a>` のような集約構造体は導入しない。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `PostEntryDeps<'a> { tax: &'a dyn TaxPolicy, tag_schema: &'a TagSchema, .. }` に依存をまとめ、`execute(tx, deps, input)` の3引数にする | `CLAUDE.md` §6「1ユースケース = 1関数。依存が引数に全部現れる」の趣旨（呼び出し側が何を注入する必要があるかシグネチャを見ただけで分かる）が、`Deps` 構造体1個の中に隠れてしまう。呼び出し側は結局 `Deps` の各フィールドを埋める必要があり、引数の見た目が減るだけで実質的な複雑さは変わらない |

**理由**: `post_entry::execute` の引数は現時点で7本
（`tx, tax, tag_schema, id_gen, clock, settings, input`）で、
`clippy::too_many_arguments` の既定閾値（7本まで許容、8本以上で警告）に
ちょうど収まっている。素の引数のままで `CLAUDE.md` §6 の利点を最大化しつつ、
clippy 違反も発生しない。

**トレードオフ**: 8本目の依存が必要になった瞬間、
`#[allow(clippy::too_many_arguments)]` を付けるか、`PostEntryDeps` の導入と
すべての呼び出し側（PR-8 の合成ルート、Phase 3 の MCP サーバー等）の
書き換えのどちらかを迫られる。**その時点で初めて `PostEntryDeps` の導入を
検討する**方針とし、今は導入しない（YAGNI。今必要ないものは作らない）。

---

## D-046 試算表read modelは`journal_lines`全件走査の現在の形のままPhase 1を完了とする（人間承認済み）

**決定**: `kaikei-store::query::trial_balance`（試算表の SQL 集計）は、
`journal_lines` に対して集計対象期間だけを絞り込むインデックスを持たない
現在のスキーマのまま Phase 1 を完了とする。`journal_entries` 側は
`idx_entries_date` により取引日で絞り込めるが、`journal_lines` 自体には
日付列が無く `journal_entries` との JOIN 経由でしか絞り込めないため、
累積データが増えるほど「直近1年だけの試算表」であっても `journal_lines`
の全行を毎回スキャンする（実行計画は `journal_lines` に対する Seq Scan）。

**実測値**（2回目レビュアーによる実DB計測）:

| データ量 | クエリ内容 | 所要時間 | 備考 |
|---|---|---|---|
| 2026年のみ: 2万仕訳/4万明細 | 「2026年通期」の集計 | 41ms | 対象期間＝全データ |
| 2018〜2026の9年分: 18万仕訳/36万明細 | 同じ「2026年通期のみ」（全体の約11%） | 90ms | `journal_lines` を36万行全件 Seq Scan |

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `journal_lines` に `entry_date` を非正規化して持たせる（`journal_entries` からコピー） | `journal_entries.entry_date` と `journal_lines.entry_date` の2箇所が食い違わない保証という新たな負債を生む。append-only なテーブル同士とはいえ、コピー元とコピー先の整合性は `insert_entry` の実装が正しく2箇所へ同じ値を書くことに依存し続けることになり、スキーマの二重管理が発生する |
| `fiscal_year` によるテーブルパーティショニング | Phase 1 の想定規模（個人事業主、単年数千〜数万仕訳）に対しては明らかにオーバーエンジニアリング。パーティション管理・マイグレーション・バックアップ運用の複雑さが実際の性能問題を解決する前から先取りで発生する |

**理由**: 単年数千〜数万仕訳という Phase 1 の想定規模では 41ms は十分実用的であり、
9年分累積時点でも 90ms は許容範囲内である。YAGNI（今必要ないものは作らない）
を優先し、実際にボトルネックとして顕在化した時点で対処する方が、
非正規化やパーティショニングが生む複雑さ・新たな整合性負債を先取りで
背負うより合理的である。ユーザーに実測値を提示のうえ、この方針で
Phase 1 を完了することの承認を得た。

**トレードオフ**: **コストは「集計対象期間の長さ」ではなく「`journal_lines`
の累積総行数」に比例する**。複数年にわたって運用が続くと、たとえ
毎回「直近1年だけ」を集計する場合でも所要時間は線形に悪化する
（実測: 9年で約90ms。単純な比例計算では50年相当の累積データでは
数百ミリ秒〜秒オーダーになりうる）。実際に体感できる遅さとして
顕在化した段階で、`journal_lines` への `entry_date` 非正規化、または
`fiscal_year` によるパーティショニングを検討する。

---

## D-047 `kaikei-app` の公開シグネチャに現れる `kaikei-policy` の型はすべて再エクスポートする

**決定**: `kaikei-app` の公開 API に現れる `kaikei-policy` の型
（`PolicyError` / `TaxPolicy` / `TaxContext` / `TaxDerivation` /
`PolicyNote` / `NoteSeverity`）を `kaikei-app` の crate ルートから
再エクスポートする。`Counterparty` / `CounterpartyIndex`（PR-4 で既に
再エクスポート済み）と同じ扱いに揃える。

**あわせて `pub mod policy { pub use kaikei_policy::*; }` を置く**
（`kaikei-policy` の公開型すべてへの経路）。ルートの明示リストは
**手で維持している以上いずれ漏れる**ことが確定しているため、その受け皿。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| 呼び出し側が `kaikei-policy` に直接依存する | `post_entry::execute` を呼ぶだけの crate（E2E テスト、Phase 3 の MCP サーバー）が `kaikei-policy` を `Cargo.toml` に書く必要が出る。とくに `kaikei-store` は「`kaikei-policy` に依存しない」ことを CI で機械的に検査しているため、E2E テストのために dev-dependency を足すと、その CI ステップが守っている境界の意味が読み手に伝わらなくなる（`--edges normal` は dev-dependency を見ないので機械的には通ってしまう分、かえって悪い） |
| `TaxPolicy` を `kaikei-app` 側で再定義し、`kaikei-policy` の trait とブランケット実装で繋ぐ | trait が2つに増え、どちらを実装すべきかが実装者から見て曖昧になる。`CLAUDE.md` §1 の「policy は trait 定義のみ」という役割分担も崩れる |
| `pub use kaikei_policy::*;` をクレートルートに撒く（`policy` モジュールを作らない） | `kaikei-app` 自身の型と名前が衝突したとき、glob 側が**黙って負ける**（コンパイルエラーにならない）。気づかないうちに別の型を指す事故になりうる。モジュールに閉じれば衝突自体が起きない |

**理由**: PR-4（契約凍結点）のレビューで「`kaikei-store` が
`CounterpartyIndex` を名指しできない」という E0433 を実際に踏んだのと
**まったく同じ穴**が、`TaxPolicy` と `PolicyError` にも残っていた。
`post_entry::execute` の引数 `tax: &dyn TaxPolicy` と
`AppError::Policy(PolicyError)` は公開 API の一部なのに、呼び出し側が
その型を名指しする手段が `kaikei-app` 経由では存在しなかった。

当初はこれを「`lib.rs` の doc に『型 → 現れる場所』の対応表を置き、
公開シグネチャに policy 型を出すときは表と `pub use` の両方に足す」という
**運用ルール**で防ごうとした。しかし**その対応表を書いたコミット自身が
`PolicyNote` / `NoteSeverity`（`TaxDerivation::notes` の要素型と
そのフィールド型）を落としており**、同じ PR のレビューで実際のコンパイル
エラーとして検出された。同じ穴を3回踏んだことになる。

**運用ルールでは防げないと判断し、`pub mod policy` という構造で塞いだ。**
表に載っていない policy の型が必要になっても `kaikei_app::policy::` から
取れるため、表の更新漏れが「下位層が `kaikei-policy` に依存せざるを得ない」
状態に化けることはない。ルートの明示リストを残すのは、**どの型が
`kaikei-app` の契約の一部か**を読み手に示すため（`policy` からすべて
取れることと、`ClosingPolicy` 等が `kaikei-app` の契約に含まれることは別）。

**トレードオフ**: 参照経路が2つになる（`kaikei_app::TaxPolicy` と
`kaikei_app::policy::TaxPolicy` はどちらも同じ型を指す）。どちらを使うべきか
迷う余地が生まれるが、「漏れたら下位層が層の境界を越えるしかなくなる」という
失敗の重さに対しては安い代償と判断した。

---

## D-048 `DATABASE_URL` は sqlx ツール専用（`kaikei_migrator`）とし、アプリの接続は `APP_DATABASE_URL` に分ける

**決定**: 環境変数の役割を1つずつに分ける。

| 変数 | ロール | 誰が読むか |
|---|---|---|
| `DATABASE_URL` | `kaikei_migrator` | sqlx のツール群（`#[sqlx::test]` / `cargo sqlx prepare` / `sqlx migrate run` / `sqlx::query!` のコンパイル時検証） |
| `MIGRATOR_DATABASE_URL` | `kaikei_migrator` | `kaikei-store` の `kaikei-migrate` バイナリ |
| `APP_DATABASE_URL` | `kaikei_app` | アプリの合成ルート（Phase 3 以降） |

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `DATABASE_URL` をアプリ用（`kaikei_app`）のままにし、pg-tests 実行時だけ呼び出し側が上書きする | まさにこの状態が壊れていた。`#[sqlx::test]` は変数名 `DATABASE_URL` を sqlx 側で固定しており変更できないため、`.env.example` どおりに設定した開発者は `cargo test --features pg-tests` を叩いた瞬間に10本全滅する。しかも失敗は sqlx-core の奥（`testing/mod.rs`）で起きる生の panic で、`permission denied for database kaikei` からロールの向き先の問題だと辿るのは難しい |
| `DATABASE_URL` を廃止して `SQLX_DATABASE_URL` 等に改名する | sqlx が名前を固定しているので不可能 |

**理由**: `.env.example` は `DATABASE_URL` を `kaikei_app` に向けたうえで、
コメントに「`DATABASE_URL`（kaikei_app）のままテストを実行すると権限不足で
全滅する」と**問題を記述だけして解決していなかった**。一方 CI
（`database.yml`）は `DATABASE_URL` に migrator を入れており、
`tests/common/mod.rs` の doc も「migrator の URL を想定」と書いている。
**同じ変数について3箇所が違うことを言っている状態**で、CI だけが通り
ローカルは通らないという最悪の組み合わせになっていた（PR-8 の E2E テストを
実際に `.env.example` どおりの設定で走らせて発覚）。

sqlx が名前を固定している以上、`DATABASE_URL` は「sqlx ツールのもの」と割り切り、
アプリの接続には別名を与えるのが唯一の整合する形。

**トレードオフ**: `DATABASE_URL` と `MIGRATOR_DATABASE_URL` の値が同一になり、
一見冗長に見える。それでも分けるのは、`kaikei-migrate` バイナリが
「所有者ロールで行う操作」であることを呼び出し側に明示させるため
（`DATABASE_URL` を別の向き先にした環境でもマイグレーションが誤ったロールで
走らない）。

---

## D-049 YAML パーサは `serde_yaml` ではなく `serde_norway` を使う

**決定**: ワークスペースの YAML パーサを `serde_yaml`（dtolnay）から
`serde_norway`（`serde_yaml` 0.9 の直系フォーク。API 互換）に置き換える。
`Cargo.toml`（`[workspace.dependencies]`）の `serde_yaml = "0.9"` を削除し
`serde_norway = "0.9"` を追加する。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `serde_yaml` を維持する | 2024年にアーカイブされ、作者自身が別ライブラリへの移行を勧めている。7年保存の会計データ（電子帳簿保存法の機能要件を意識したスコープ、`CLAUDE.md` §10）を扱う基盤の税区分マスタ・科目表・タグスキーマのパーサを、保守されていない状態のまま使い続けるのはリスクが大きい |
| `serde_yml`（別フォーク） | コード品質への懸念が指摘されており、7年保存を前提とする基盤の入力パーサとして採用するには材料が不足している |
| `yaml-rust2` / `saphyr` | どちらも serde 統合を持たず、`#[derive(Deserialize)]` で書いたスキーマ型（`deny_unknown_fields` 含む）をそのまま使えない。手書きで `Visitor` 実装が必要になり、`docs/04-jp-tax.md` のスキーマ定義がそのまま使えなくなる |
| YAML をやめて JSON にする | 既存の YAML（`tax/jp/2026.yaml` 等）は「★要確認」等のコメントで、税務上不確実な箇所を編集者に警告する設計になっている（`CLAUDE.md` §10「税務判断を断定しない」の実装）。JSON はコメントを書けないため、この警告の置き場所が失われる |

**理由**: `serde_norway` は `serde_yaml` 0.9 の API をそのまま踏襲したフォークで、
実測で `tax/jp/2026.yaml` のパース、`rate: "0.10"` のような文字列から
`rust_decimal::Decimal` への変換、`deny_unknown_fields` 違反時の行・列つき
エラーメッセージ（例: `"...at line 4 column 5"`）が期待どおり動くことを
確認した（スクラッチ検証。`serde_norway::Error` の `Display` に解析位置が
含まれ、`Location::line()`/`column()` でも取得できる）。

**トレードオフ**: `serde_norway` もまた個人（コミュニティ）が保守する
フォークであり、`serde_yaml` と同じ運命（メンテナ不在化）をたどる可能性は
ゼロではない。その場合は改めて別のフォーク・別形式への移行を検討する。
現時点で API 互換のフォークに切り替えるコストは、コード側の変更が
`use serde_yaml` を `use serde_norway` に置き換える程度で済むため小さい。

---

## D-050 `kaikei-jp-data` は依存ゼロの埋め込みデータ crate とし、税区分マスタは「暦年キー」ではなく**適用期間で選ぶための全件リスト**で公開する

**決定**: `kaikei-jp-data` は `[dependencies]` を空にし（CI が検査する）、
`include_str!` で YAML を `&'static str` として埋め込む。公開する形は
`EmbeddedYaml { label, source }`（エラーメッセージ用のラベルと中身の対）とし、
消費税区分マスタは **`TAX_CATEGORY_SOURCES: &[EmbeddedYaml]`（埋め込み済み
全件のスライス）** で公開する。適用期間による選択は `kaikei-jp` 側
（`JpTaxPolicy` の構築時）が各 YAML の `applies_from` / `applies_to` を
読んで行う。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `tax_category_yaml_for_year(year: i32) -> Option<&'static str>`（暦年をキーにして1件引く） | **日本の消費税率改正は年度途中に起きるのが通例**（2019年10月の軽減税率導入など）。個人事業主は暦年会計（`CLAUDE.md` §7）なので、**1つの暦年に2つのマスタが適用される期間が実際に生じる**。暦年をキーにすると `2019.yaml` と `2019b.yaml` のような便宜的な命名を強いられ、`BTreeMap<i32, _>` のキー型では表現できなくなる。データが1件しか無い今のうちに直さないと、複数年度が並んだ後でキー型を変えるのは全ファイルのリネームを伴う |
| 暦年キーを維持しつつ、別途「利用可能な年度の一覧」定数を持つ | 「年度が増えたら `const` と `match` アームと一覧定数の3箇所を更新する」形になり、手で維持する一覧が2つに増える。D-047 で「手で維持する一覧は必ず腐る」と結論づけたばかりの形をそのまま再演することになる |
| 実行時に `tax/jp/` ディレクトリを走査して列挙する | 実行時のファイルパス依存が生まれ、バイナリを配布しただけでは動かなくなる（`ARCHITECTURE.md` §7 の設計意図に反する） |
| `build.rs` でディレクトリを走査し `include_str!` を自動生成する | ビルド時の複雑さに対して得るものが小さい（YAGNI）。生成コードはレビュー時に直接読めない |
| `phf` 等の静的マップ crate を使う | 依存ゼロの方針に反する。件数は多くても数十件で、線形走査で足りる |

**理由**: `kaikei-jp-data` の責務は「どのマスタが同梱されているか」を示すことに
限る。解釈（デシリアライズ・バリデーション）も選択（どの取引日にどのマスタを
使うか）も `kaikei-jp` の責務（`docs/04-jp-tax.md` §1、`DECISIONS.md` D-025）。

`TAX_CATEGORY_SOURCES` を**唯一の情報源**にすることで、マスタを追加したときに
触る箇所が「YAML ファイルを置く」「このスライスに1行足す」の2つに閉じる。
`kaikei-jp` 側が年度の一覧を別に持つ必要が無くなる。

`label` を定数と対にして持つのは、呼び出し側がラベル文字列を手で書くと
定数との対応がずれても気づけるのがエラーメッセージの文言だけになるため。
対応の正しさ（`label` が指すファイルの中身と `include_str!` した内容が
一致すること）は `kaikei-jp-data` 側のテストが機械的に検証する。

**トレードオフ**: 「2026年のマスタが欲しい」という単純な引き方が直接はできず、
呼び出し側は全件を読んでから適用期間で絞る必要がある。マスタが数十件に
なっても起動時の1回だけの処理なので、性能上の問題にはならない。

---

## D-051 `test_chart()` との乖離検出は、`test_chart()` の**ソースを読んで**科目定義を抽出する

**決定**: `crates/kaikei-core/tests/common/mod.rs` の `test_chart()` と
`kaikei-jp-data/chart/sole_proprietor.yaml` の乖離を検出するテストを
`crates/kaikei-jp/tests/chart_drift.rs` に置く。core 側の科目一覧は
**`test_chart()` のソースファイルを読み、`account("100", "現金",
AccountType::Asset, true),` の形の行から (コード, 名称, 種別, 記帳可否) を
抽出する**。`include_str!` ではなく実行時の `std::fs::read_to_string` を使い、
`CARGO_MANIFEST_DIR` からの相対パスで辿る（crate 依存は発生しない）。

`"999"`（`test_chart()` 専用の見出し科目ダミー）は YAML 側に存在しないことが
正しいため比較対象から除外し、除外理由をコメントに書く。除外が妥当であり
続けることは対抗テスト（`code_999_is_intentionally_absent_from_yaml`）が
別途検証する。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `test_chart()` の科目一覧を `chart_drift.rs` に定数として複製する | **これでは申し送りの目的を満たさない。** 検出できるのは「複製と YAML の乖離」だけで、`test_chart()` 本体が変わっても複製は追随せず、乖離は検出されないまま緑になる。手で維持する一覧が腐ることは D-047 で3回連続して実証済み |
| `kaikei-core` に乖離検出テストを置く | `kaikei-core` は `kaikei-jp-data` を読めない（`CLAUDE.md` §1、依存追加は人間承認事項） |
| `test_chart()` を `kaikei-core` の公開 API として再エクスポートし `kaikei-jp` から呼ぶ | `tests/common/mod.rs` は統合テスト専用モジュールで他 crate からリンクできない。`src/` にテスト専用の科目データを置くのは本末転倒 |
| `test_chart()` 自体を YAML から生成する | `kaikei-core` は `serde_norway` にも依存できない不変層。`kaikei-core` 側の変更（人間承認事項）になる |

**理由**: 「乖離を機械的に検出する仕組みが無い」という Phase 0 の申し送り (b) に
対して、**手で複製した一覧を突き合わせる実装は答えになっていない**。
複製と YAML がズレたときしか鳴らず、`test_chart()` 本体が変わった場合には
鳴らない。しかも**緑のテストが「乖離は検出されている」という誤った確信を
生む**ため、テストが無い状態より悪い（このプロジェクトは「誤診を招く診断」を
誤値と同格の欠陥として扱う。`PROGRESS.md` Phase 1 の教訓3）。

ソースをテキストとして読む方式は一見乱暴だが、対象は同一リポジトリ内の
既知の書式であり、`test_chart()` を編集すれば抽出結果が自動的に追随する。
正規表現クレートは導入していない（`strip_prefix`/`split` で足りる書式のため、
依存を1つ増やすほどの価値が無い）。

**トレードオフと、レビューで実際に踏んだ2つの脱落**: `test_chart()` の
書き方が変わると抽出できなくなる。当初は行単位で
「`account(` で始まり `),` で終わる行」を拾う実装にしていたが、
レビュー（1回目・2回目が独立に別パターンで再現）で**1行だけが黙って
抽出から漏れる**ことが分かった:

1. **行末コメント**: `account("615", "地代家賃", ..., true), // TODO: 名称要確認`
   （`sole_proprietor.yaml` は実際にデータ行へ行末コメントを付けている）
2. **rustfmt の折返し**: 行が長くなると `account(` と引数と `),` が別行に割れる

いずれも件数は 12→11 に減るだけで下限（当時 10）は満たすため、
**その科目の乖離だけが永久に検出されないまま緑になる**。「0件で全件一致」
という壊滅的な機能停止は防げていたが、より発見しにくい部分版を見落としていた。

対策として、(a) 行コメントを除去してから全体を1本の文字列にまとめ、
`account(` から対応する `)` までを**括弧の対応**で切り出す方式に変え
（どちらの書き方でも同じ結果になる）、(b) `account(` の**呼び出し箇所の数**と
実際に解析できた件数の**一致**を要求し、1件でも解析できなければ
「解析できなかった呼び出しがあります」と落ちるようにした。
(c) 呼び出しが1つも見つからない場合は (b) が `0 == 0` で通ってしまうため、
下限の番人（`MIN_EXTRACTED_ACCOUNTS`）も残している。

これらの番人が実際に働くことは `#[should_panic]` のテストで確認し、
脱落しないことは実際の書式（行末コメント／rustfmt 折返し）を模したテストで
確認している。さらに `test_chart()` の「地代家賃」を「支払家賃」に書き換えて
**両方の書式で**実行し、いずれも
`615: 名称が不一致です（test_chart()="支払家賃" / sole_proprietor.yaml="地代家賃"）`
と具体的に指摘して落ちることを確認済み。

抽出ロジックを検証するテストは、**本体の関数をそのまま呼ぶ**。検証側で
抽出を再実装すると「複製は本体の変更に追随しない」という、この決定自身が
否定したのと同じ形をテストの中で再演してしまうため（2回目レビュー指摘）。

---

## D-052 `InvoiceRegistrationNo::parse` は前後の空白をトリムしない

**決定**: `kaikei-jp::invoice::InvoiceRegistrationNo::parse` は入力文字列を
一切トリムしない。先頭・末尾に空白を含む入力は、トリムして受理するのではなく
エラーとして拒否する（先頭空白は `InvoiceRegNoMissingPrefix`、末尾空白は
`T` の後の文字数が13からずれるため `InvoiceRegNoWrongLength` になる）。

**却下した選択肢**: `s.trim()` してから検証する。

**理由**: `AccountCode::parse`（`kaikei-core::account`）をはじめ、この
リポジトリの値オブジェクトの `parse` はいずれも入力をトリムせず、
英数字以外を含む文字列をそのまま拒否する規約になっている
(`docs/04-jp-tax.md` はトリムの要否まで明記していないため、既存の `parse`
群との一貫性を優先した)。トリムして受理すると、CSV取込やフォーム入力に
混入した空白が「見た目は正しい登録番号だが実際には前後に空白を含む文字列」
として保存されうる。`InvoiceRegistrationNo` は `Counterparty` の
`invoice_registration_no: Option<String>`（`kaikei-policy::counterparty`、
`DECISIONS.md` D-028）としてそのまま保存される想定であり、ここで空白混じりの
値を通すと、後段の完全一致検索や表示で気づきにくい不整合を生む。
ユーザー入力の境界（フォーム・CSV取込側）でトリムするかどうかは、この型の
責務ではなく呼び出し側が判断する。

**トレードオフ**: コピー&ペーストで末尾に改行や空白が混入した場合、
ユーザーは「桁数が違います」というエラーメッセージを見ることになり、
本当の原因（空白混入）に気づくには入力を目視確認する必要がある。
実害が確認された場合はメッセージ側に「前後の空白を確認してください」
という文言を足すか、境界層（アプリ層）で明示的にトリムしてから渡す運用で
対応する（この型自体の挙動は変えない）。

---

## D-053 `InvoiceRegistrationNo::parse` の検証順序は「先頭文字 → 桁数 → 文字種 → チェックデジット」の順に固定する

**決定**: `parse` は (1) 先頭が `'T'` か、(2) `T` の後が13文字か、
(3) その13文字がすべて半角数字か、(4) チェックデジットが一致するか、の順に
検証し、最初に失敗した段階のエラーバリアントを返す（後続の検証は行わない）。
桁数チェックは「文字数」であり、時点では数字であることを要求しない
（全角文字1文字・空白1文字もそれぞれ1文字として数える）。

**却下した選択肢**: 全項目を検証してから複数のエラーをまとめて返す、
または文字種チェックを桁数チェックより先に行う。

**理由**: `CLAUDE.md` §11 が求めるのは「次の手が分かる」単一の具体的な
指摘であり、複数エラーの同時報告は複式簿記のドメインエラー
（例: 貸借不一致）とは異なりこの型では必要性が薄いと判断した。
桁数チェックを文字種チェックより先に行うのは、`T` の後の文字数がそもそも
13でない入力（例: 末尾に空白が混入して14文字になったもの）に対して
「何文字あったか」を先に伝える方が、後から「数字ではない文字が混ざっている」
と言われるより原因を特定しやすいと判断したため。この結果、末尾空白混入は
`InvoiceRegNoWrongLength`（実際 14 文字）に分類され、`InvoiceRegNoNonDigit`
（空白等が数字でない）には分類されない。

**トレードオフ**: 「桁数は合っているが数字でない文字が混ざっている」場合と
「桁数自体が違う」場合を、入力によっては呼び出し側が意図せず混同しうる
（上記の末尾空白の例）。エラーメッセージ自体にどちらの原因かを明記して
いるため、実害は小さいと判断した。

---

## D-054 税区分マスタの適用期間の重なりはロード時にエラーにする（後勝ち・先勝ち・警告のみは却下）

**決定**: `kaikei-jp::tax::TaxRuleSets::new`（`crates/kaikei-jp/src/tax/rule_sets.rs`）は、
渡された `TaxCategoryTable` 群の適用期間（`applies_from`〜`applies_to`。両端含む
閉区間、`applies_to == None` は無期限）が1組でも重なっていたら
`JpError::OverlappingTaxPeriods` を返して構築を失敗させる。エラーメッセージには
重なっている**両方**のマスタの識別子（`label`）と適用期間を含める。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| 後から追加された（`Vec` の後方の）マスタを優先する「後勝ち」 | ある取引日の解釈が「マスタを追加した順序」という記帳内容と無関係な事情で決まってしまう。後からマスタを1件足しただけで、既存の取引日の解釈が無言で変わりうる |
| 先に登録されたマスタを優先する「先勝ち」 | 同上。優先順位の理由が「たまたま先に書いた」でしかなく、YAML を読む人には分からない |
| 重なりを許し、`for_date` 側で「複数該当したら先頭を返す」等の解決規則を設ける | 判定ロジックが `TaxRuleSets::new`（構築時）と `for_date`（参照時）に分散し、後から `for_date` を読む人が「複数該当した場合にどちらが勝つか」を毎回確認する必要が生じる。かつ、この解決規則自体が実質「先勝ち/後勝ち」の焼き直しで、上2つと同じ問題を抱える |
| ロード時に警告ログのみ出し、処理は継続する | 会計データを扱う基盤で、税率・控除割合の適用マスタが一意に決まらない状態を「警告だけ出して動かす」のは危険側に倒しすぎている（`CLAUDE.md` 冒頭「迷ったら実装を止めて確認を求める」）。警告はログを見ていなければ気づけない |

**理由**: 重なりを許すと、ある取引日にどちらのマスタを適用するかが一意に
決まらない。これは「消費税率が改正日をまたいで2通り読める」状態であり、
静かに解決してよい種類の曖昧さではない。ロード時（構築時）に人間の手で
`applies_from`/`applies_to` を直すことを強制する方が、記帳時に初めて
気づく（＝既に間違った税額で仕訳が切られた後に気づく）よりはるかに安全。

**トレードオフ**: 経過措置のように「同じ日に新旧2つの制度が並走しうる」
実務上のケースがあっても、この crate のデータモデルでは1マスタ＝1適用期間
という単純な区分しか表現できない。そのようなケースは `categories[]` 側に
両方の区分を並べ、`requires_qualified_invoice` 等のフラグで使い分ける形で
表現する（`2026.yaml` の `PURCHASE_10_QUALIFIED` / `PURCHASE_10_NON_QUALIFIED`
がその実例）。

---

## D-055 `TaxRuleSets::for_date` は該当マスタが無ければ `None` を返す（エラーにしない）

**決定**: `TaxRuleSets::for_date(&self, date: AccountingDate) -> Option<&TaxCategoryTable>`
は、どのマスタの適用期間にも入らない取引日に対して `Err` ではなく `None` を返す。
呼び出し側（`kaikei-jp::JpTaxPolicy`。別 PR）が `None` を
`kaikei_policy::PolicyError::NoApplicableRuleSet { as_of }` に写像することを想定する
（`PolicyError::NoApplicableRuleSet` は Phase 1（`crates/kaikei-policy/src/error.rs`）
で既に定義済みであることを確認した上でこの設計にした）。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `for_date` 自身が `Result<&TaxCategoryTable, JpError>` を返す | `kaikei-jp` は `kaikei-policy::PolicyError` を知らない層ではないが（`kaikei-jp` は policy の実装層）、「マスタが無い」というエラーの意味付け（`as_of` を含むメッセージの文言）は `PolicyError::NoApplicableRuleSet` が既に持っている。`JpError` 側に同じ意味のバリアントを重複して持たせると、`JpTaxPolicy` が `JpError` → `PolicyError` の変換で二重にラップすることになり、呼び出し側にとって「結局どちらのエラー型を見ればよいか」が曖昧になる |
| `TaxCategoryTable::empty()` のような「空マスタ」を返す | 「税区分が1つも無いマスタ」と「マスタが本当に存在しない」は意味が異なる。前者は「この期間は消費税区分マスタが空である」という正当なデータでありうるが、後者は「このマスタ群の対象外の日付」という別の事象であり、混同するとバグを埋め込む |

**理由**: `for_date` はマスタの集合に対する検索であり、「見つからない」は
`HashMap::get` が `None` を返すのと同種の正常系である。`TaxRuleSets` 自体は
`kaikei-policy` を知らない（`kaikei-jp` は policy を実装する層だが、
`tax` モジュールは `kaikei-policy` に依存しない設計にしてある。依存すると
`TaxRuleSets` という汎用的なデータ構造が `PolicyError` という特定の trait
実装の都合に引きずられる）。「取引日に応じたエラーメッセージを組み立てる」
という trait 実装側の責務と、「マスタ群から該当するものを引く」という
このモジュールの責務を分けるため、`Option` を返すところまでに留める。

---

## D-056 税区分マスタの `version`・`country` はサポート外の値を構築時に拒否する

**決定**: `TaxCategoryTable::new`（YAML 経由の構築時のみ。直接構築する
`TaxCategoryTable::new` はそもそも `version`/`country` を引数に取らない）は、
読み込んだ YAML の `version` が `1` 以外、または `country` が `"JP"` 以外の場合、
`JpError::InvalidTaxCategoryTable` で拒否する。`version`/`country` はドメイン型
`TaxCategoryTable` のフィールドとしては保持しない（検証にのみ使い、変換後は破棄する）。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `version` が未知でも警告のみで読み進める（フィールドを追加してもエラーにしない） | `#[serde(deny_unknown_fields)]` の方針（未知フィールドは黙って無視せず落とす）と矛盾する。スキーマ自体のバージョンが変わったのに、その版で追加された意味論を無視して古い解釈のまま読み進めるのは、税率・控除割合を扱うデータとして危険 |
| `country` を検証しない（`kaikei-jp` は日本専用 crate だから自明という前提を置く） | YAML は人間が手で書く・差し替えるものであり（`docs/04-jp-tax.md` の「ユーザーが自分の YAML に差し替える経路」）、コピペ起因で他国向けデータや書き間違いを読み込んでしまう事故を、パース成功後の実行時まで持ち越さずに済む。検証コスト（文字列比較1回）に対して事故の重大さ（誤った税区分で記帳される）が見合わない |
| `TaxCategoryTable` に `version`/`country` フィールドを持たせ続ける | 検証が終わった後、このドメイン型のどの操作もこれらの値を使わない（`kaikei-jp` は日本専用、スキーマ版は構築時にしか意味を持たない）。使われないフィールドを持たせると「後で `country` を見て分岐する」ような誤用を誘発する |

**理由**: `version`/`country` はどちらも「この YAML をこの crate のこのバージョンで
読んでよいか」を判定するための構築時メタデータであり、税額計算のドメインロジックが
参照する値ではない。未対応バージョン・国違いのデータを読み進めてしまうと、
新しいスキーマで意味が変わったフィールドを旧解釈のまま読む・他国向けデータを
日本の税制として扱う、といった**パースは成功するが意味的には誤り**という
発見しにくい壊れ方をする。構築時に確実に落とすことで、この種の事故を
「YAML を読み込んだ瞬間に気づける」形にする。

**トレードオフ**: 将来スキーマ版が増えたとき（`version: 2` 等）、
`TaxCategoryTable::SUPPORTED_VERSION` を単純に書き換えるだけでは済まず、
新旧両方のバージョンを読める変換ロジックが必要になる可能性がある。
これは実際にバージョン2が必要になった時点で設計する（YAGNI。現時点で
1種類しか存在しないスキーマに対して複数バージョン対応の抽象を先取りしない）。

---

## D-057 `JpSettings` は `settings_defaults` を構築時に一度だけ合成し、`ctx.as_of` に応じて再合成しない

**決定**: `JpTaxPolicy` が保持する事業者設定（`JpSettings`）は、
`JpSettings::compose(defaults: TaxSettingsDefaults, overrides: JpSettingsOverrides)`
により**構築時に一度だけ**合成する。`tax_mode` / `rounding` / `rounding_unit` は
`overrides` が `Some` を指定していればそれを、`None` なら `defaults`（呼び出し側が
選んだ1件のマスタの `settings_defaults`）を使う。`is_taxable_business` /
`simplified_taxation` はマスタ側に対応する既定値が存在しない事業者固有の設定
のため `JpSettingsOverrides` で `Option` にせず必須項目にする（`Default` も
実装しない）。合成された `JpSettings` は `JpTaxPolicy` の全メソッド呼び出しで
使い回され、`TaxPolicy::round_mode(&self, ctx: &TaxContext<'_>)` は `ctx` を
受け取りながら中身（`ctx.as_of`）を見て別マスタの `settings_defaults` を
引き直すことはしない。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `round_mode` 等の各メソッド呼び出しのたびに `rule_sets.for_date(ctx.as_of)` の `settings_defaults` を引いて合成し直す | 同じ取引（同じ `lines`）を複数回 `derive_tax_lines` に通したとき、`ctx.as_of` を変えていないのに何らかの理由でマスタの構成が変わると結果が変わりうる、という不安定さを抱え込む。加えて `apply_ratio` の既定実装が `self.round_mode(ctx)` を使う以上、事業者が明示的に選んだ丸め方式が「たまたまその取引日に有効なマスタの既定値」で上書きされる経路が生まれ、事業者設定より年度マスタの既定値が優先されるという直感に反する挙動になる |
| `JpSettings` を廃止し、`TaxContext` に事業者設定を含める | `TaxContext` は国非依存の4項目に限定する（`DECISIONS.md` D-025）。`JpSettings` は日本固有の型であり、`TaxContext` に含めると policy → jp の循環依存を生む |
| `overrides` も含めて全フィールドを `Option` にし、全て `None` なら丸ごとエラーにする | `is_taxable_business` は「指定を忘れると免税事業者として扱われ税額行が生成されない」という、会計上の実害が大きい間違いを起こしやすい設定。`Option` にして「指定忘れ＝合理的な既定値にフォールバック」を許すと、事故が起きても構築時にはエラーにならず気づけない。呼び出し側に必ず明示させる方が安全 |

**理由**: 事業者設定（税抜/税込・丸め方式・課税事業者区分等）は、本来
「その事業者が今どう記帳したいか」という運用上の選択であり、年度別マスタの
`settings_defaults` はあくまで「はじめて設定するときの初期値の提案」以上の
意味を持たせるべきではない。合成を構築時の1回に閉じることで、`TaxPolicy`
の各メソッドは純粋に `(ctx, tags/lines)` だけから結果が決まる関数のままになる
（`CLAUDE.md` §3、D-025 と同じ設計原則）。

**トレードオフ**: 消費税率改正で新マスタの `settings_defaults` が旧マスタと
異なっていても（例: 旧マスタは `rounding_unit: line` 推奨、新マスタは
`document` 推奨）、事業者が明示的に `JpTaxPolicy` を作り直して設定を
切り替えない限り自動追従しない。これは D-025 のトレードオフ（設定変更には
`Arc<dyn TaxPolicy>` の作り直し＝プロセス再起動が必要。単一ユーザー・
自己ホスト前提の D-015 と整合）の直接の帰結として許容する。「`defaults` に
どのマスタを渡すか」は呼び出し側（合成ルート）の責務とし、`JpSettings::compose`
自体はそれを判断しない。

---

## D-058 `rounding_unit: Document` は「同じ（税区分, 側, 税額科目）の本体を合算してから1回だけ丸める」実装にする

**決定**: `JpTaxPolicy::derive_tax_lines` の `RoundingUnit::Document` は、
税額計算の対象になった明細を `(tax_category のコード, 借方/貸方, tax_account)`
の組でグルーピングし、**各グループの本体金額を先に合算してから、合算結果に
1回だけ `apply_ratio` を適用**して税額行を1行生成する。グルーピングに
`tax_category` のコードを含めるため、同じ側・同じ税額科目（例: 仮受消費税）
であっても税区分が異なれば必ず別の行になる。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| 明細ごとに `apply_ratio` した税額を求め、それを何らかの方法で合計してから請求書単位の1行にまとめ直す | Phase 1 で踏んだ「明細ごとに丸めると合計が1円ずれる」バグ（`PROGRESS.md` Phase 0/1 の教訓）と同型の罠。個々の端数処理の結果を後から合算しても、本体を合算してから1回丸めた値とは一致しない（例: 15円+15円、rate 10%、floor の場合、明細ごとの丸めは 1円+1円=2円だが、合算してからの丸めは (15+15)×10%=3円）。「明細ごとに計算してから合計する」という形は Document の定義そのものと矛盾する |
| `(tax_account, 側)` のみでグルーピングし、`tax_category` を無視して合算する | 同じ税率・同じ税額科目でも税区分が異なれば集計上区別する必要がある（`tags.yaml` の `tax_category: aggregatable: true`）。異なる税区分の税額行を1本にまとめると、生成された税額行に単一の `tax_category` タグを付けられなくなり（`docs/04-jp-tax.md` §7「生成した税行にも元の `tax_category` を付ける」）、税区分別の集計が壊れる |
| グループごとの税額を明細に按分し直して複数行として出力する | 端数の配り方（先頭の明細に寄せる／均等に配る等）自体が新たな仕様判断を要求し、かつ Document 集計を選んだ意図（請求書単位で1行にまとめたい）に反する。1グループ1行のままにする |

**理由**: 「合算してから1回丸める」がそのまま `RoundingUnit::Document`
（請求書単位で端数処理する）の定義であり、実装をこれと違う手順にすると
定義と実装が乖離する。`tax_category` をグルーピングキーに含めるのは、
生成される税額行が集計軸として意味を持つために必須（`docs/04-jp-tax.md` §7）。

**トレードオフ**: 同じ側・同じ税額科目・同じ税率でも税区分コードが違えば
別行になるため、税区分の切り方次第では Document 集計でも複数行の税額行が
生成されうる（例: `SALES_10` と `PURCHASE_10_QUALIFIED` が同じ `tax_account`
を共有していたとしても、direction が異なる時点で別コードのため別行になる）。
これは「請求書1枚あたり税額行1本」を保証するものではなく、「同じ税区分の
本体合計に対して1回だけ丸める」ことを保証するものである、という理解で
運用すること。

**生成される税額行の並び順**（レビューで論点になったため明記する）:
グルーピングに `BTreeMap` を使うため、税額行は
**（税区分コードの辞書順 → 借方/貸方 → 税額科目）**の順に、入力明細の後ろへ
まとめて追加される。入力での出現順には追従しない。

「本体行の直後にその税額行を置く」形も検討したが、**採らない**。理由:

- `kaikei-store` は `entry.lines().iter().enumerate()` で `line_no` を採番する。
  ここで求められるのは**決定的であること**であり、`BTreeMap` の順序は
  実行ごと・プロセスごとに変わらない（`HashMap` だとハッシュシード依存で
  変わりうる。実測で20回連続実行して一致することをテストで固定した）
- 「読みやすい並び」は仕訳帳の**表示層**の関心事であり、append-only で
  保存される `line_no` に持たせる意味は薄い。表示側は必要なら並べ替えられるが、
  保存された順序は後から変えられない
- 本体行の直後に差し込む実装は、入力明細への挿入位置の計算が必要になり、
  `Document` 集計（複数明細を1行にまとめる）と噛み合わない

---

## D-059 非適格の経過措置（`deduction_ratio < 1`）は税額計算に反映せず `PolicyNote` に留める

**決定**: `JpTaxPolicy::derive_tax_lines` は、税区分の `deduction_ratio` が
1未満であっても、生成する税額行の金額は `rate` のみで計算し、
`deduction_ratio` を掛け合わせたり本体側へ配分したりする処理を行わない。
代わりに、そのような区分が使われた仕訳には
`PolicyNote { severity: NoteSeverity::Warning, .. }` を1件（同じ区分が
複数明細で使われても重複させず1件）添え、控除割合の値・「控除できない部分の
帳簿上の処理はこの実装では行っていない」こと・判断は税理士に確認すべきこと
（`docs/08-compliance.md` §9-1 を参照させる）を断定しない文言で伝える。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `docs/04-jp-tax.md` §7 の記述どおり「仮払消費税を減らして本体に足す」処理を実装する | どの科目にいくら振り替えるか、決算のどのタイミングで処理するかは税務上の判断を要し、`kaikei-jp` の README・`docs/08-compliance.md` §9-1 が明示的に「税理士に確認」と保留している事項。実装すると、確認前の未確定の処理方針を既成事実化してしまう（`CLAUDE.md` §10「確定は人間に残す」に反する） |
| `deduction_ratio` を無視し、注記も出さない | 経過措置対象の区分を使っていること自体は機械的に判定できる有用な情報であり、それを黙って捨てると、税額が「仕入税額控除の全額」で計上されているように見えてしまい、決算時に気づきにくい。何もしないより「気づける形にする」方が安全側 |
| `PolicyError` にしてブロックする（記帳自体を拒否する） | 経過措置対象の区分は正当な税区分であり、記帳自体は成立する。断定的な処理方針が決まっていないことをエラーとして記帳をブロックするのはやり過ぎで、`CLAUDE.md` §10 が求める「提案は候補と根拠を返し確定は人間に残す」の趣旨（拒否ではなく情報提供）にも合わない |

**理由**: 控除できない部分の会計処理は税務判断そのものであり、この PR
（Phase 2 の本丸ではあるが税理士確認前）で実装すべきではない。一方で
「経過措置対象の区分が使われている」という事実自体は `TaxCategoryTable`
から機械的に判定できるため、それだけを `PolicyNote` として返し、確定を
人間（最終的には税理士）に委ねる設計にした。

**トレードオフ**: 税額は常に `rate` ベースの全額控除相当で計上されるため、
経過措置により実際には控除できない部分がある場合、この実装の出力する
仮払消費税額は最終的な申告額と一致しない。`PolicyNote` を見落とすと
この乖離に気づけないまま決算を迎えるリスクがある。将来、税理士確認の
結果として具体的な処理方針が決まった時点で `derive_tax_lines` を拡張する。

---

## D-060 `validate_tag` は `tax_category` の必須チェックと `direction`/科目種別の整合チェックを実装しない

**決定**: `JpTaxPolicy::validate_tag` は以下の2つを検証**しない**。

1. `tax_category` タグがそもそも付いているかどうか（必須チェック）
2. 付いている `tax_category` の `direction` と、明細の科目種別（`AccountType`）
   の組み合わせが妥当かどうか（例: 売上区分が費用科目に付いている等）

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| 1 を `validate_tag` でも検証する（「タグが無ければエラー」を policy 側でも返す） | `kaikei_core::TagSchema::validate` が `tags.yaml` の `required_for: [Revenue, Expense]` を根拠に既に同じ検証を行っている（`crates/kaikei-core/src/tag.rs` の `MissingRequiredTag`）。同じ不備に対して `CoreError::MissingRequiredTag` と `PolicyError` の2種類のエラーが存在すると、呼び出し順序（`TagSchema::validate` を先に呼ぶか `TaxPolicy::validate_tag` を先に呼ぶか）によってどちらが実際に返るかが変わってしまい、`CLAUDE.md` §11 が求める「次の手が分かる文言」の一貫性が崩れる |
| 2 を実装する（`direction: sales` は Revenue 科目のみ、`direction: purchase` は Expense 科目のみ許可する、等） | 課税仕入（`direction: purchase`）は費用科目だけでなく固定資産の取得（Asset 科目）にも付きうるなど、正当な組み合わせが科目種別だけからは機械的に決め切れない。誤って正当な記帳を拒否する（偽陽性）リスクの方が、組み合わせチェックを省略するリスクより大きいと判断した |

**理由**: `validate_tag` が実装するのは「機械的に判断できるものだけ」
（税区分コードの存在確認、`requires_qualified_invoice` と取引先登録状況の
明示的な不整合）に限定し、他の層が既に担保している検証を重複させない
（1）、および税務ドメインの判断を要する検証は実装しない（2、`CLAUDE.md`
「迷ったら実装を止めて確認を求める」）という2つの方針をそのまま適用した。

**トレードオフ**: 2 を実装しないことで、「売上区分が資産科目に付いている」
といった、本来なら機械的に検出できたかもしれない誤りの一部を通してしまう
可能性がある。将来、税務ドメインの知見をもとに「明らかに誤りと言える
組み合わせ」の一覧が定まった時点で、別 PR として追加を検討する。

---

## D-061 勘定科目テンプレートの `postable`/`parent`/`sort` の扱い（`kaikei-jp::chart`）

**決定**: `kaikei-jp-data/chart/*.yaml` → `kaikei_core::ChartOfAccounts` の
ロード（`kaikei-jp::chart`）で、YAML の生の形（`AccountRaw`）は以下のように扱う。

1. `postable`（`kaikei_core::AccountDef::postable`）: **省略可能**（`#[serde(default)]`）。
   省略時は `true`（記帳可能）。同梱の `sole_proprietor.yaml` には1件も
   書かれていないが、YAML 自身の先頭コメントが「`postable: false` なら
   見出し科目」と明記しているため、フィールド自体は受理し、明示すれば
   効くようにする
2. `parent`（`kaikei_core::AccountDef::parent`）: **省略可能**（`#[serde(default)]`）。
   省略時は `None`。同梱データは階層を持たないフラットな科目表だが、
   ユーザーが差し替える科目表（`kaikei-jp::chart::load_from_path`）で
   見出し科目の配下に明細科目をぶら下げたい場合に使える
3. `sort`（YAML にはあるが `kaikei_core::AccountDef` に対応するフィールドが
   無い）: **ドメイン変換では破棄する**。`deny_unknown_fields` による
   スキーマ完全性検証のためだけに受け取り、値そのものは使わない。
   **省略可能**（`#[serde(default)]`、`Option<i64>`）にする。値を使わない以上、
   必須にすると「書いても何も起きないのに、書き忘れると YAML 全体が
   パースエラーになる」という一方通行の罰則になる。`tags.yaml` の
   `description` を省略可能にしたのと同じ扱いに揃える（レビュー指摘）

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `postable`/`parent` を必須フィールドにする（省略を許さない） | 同梱データが実際に両方とも省略しており、必須にすると同梱データ自体が読めなくなる。また「ほとんどの科目は記帳可能・親を持たない」が実態であり、都度書かせるのは YAML の可読性を落とすだけで得るものが無い |
| `sort` を `kaikei_core::AccountDef` に新しいフィールドとして追加し、表示順を保持する | `kaikei-core` の型変更は本 PR のスコープ外（`ChartOfAccounts`/`AccountDef` を使う既存コード全体に影響する）。かつ、実際にその順序を使う画面・帳票がまだ存在しない時点で追加するのは YAGNI。同梱テンプレートは科目コードが固定桁数の数字文字列であるため、`ChartOfAccounts::iter()`（`BTreeMap` 由来でコード昇順）の並びが偶然 `sort` の意図と一致しており、当面はそれで足りる |
| `sort` を YAML から削除する（同梱データを編集する） | `kaikei-jp-data` の YAML を編集しないことがこの PR の制約（担当外の変更を持ち込まない） |

**理由**: `kaikei_core::AccountDef` の形に対して YAML が省略している情報は、
「省略時の既定値が自明で、かつ既定値のままで同梱データが要求を満たす」
（`postable`/`parent`）か、「ドメインモデルに対応する置き場が無く、
今使う見込みも無い」（`sort`）かのどちらかに分類できたため、前者は
`#[serde(default)]` で受理し、後者は明示的に破棄することにした。
どちらも「捨てる/使わない」ことを doc コメントと本エントリで明示し、
無言の仕様にしない。

**トレードオフ**: `sort` を破棄したことで、科目コードの桁数が不揃いな
科目表（例: `"9"` と `"10"` が混在する体系）を書いた場合、
`ChartOfAccounts::iter()` の並びが人間の意図する表示順と一致しなくなる
可能性がある。表示順を厳密に扱う画面・帳票が出てきた時点で、
`kaikei-core` 側の変更として改めて検討する。

**関連**: `crates/kaikei-jp/tests/chart_drift.rs`（`test_chart()` と
`sole_proprietor.yaml` の乖離検出）は、本 PR で `kaikei-jp::chart::load_embedded`
を直接使うように書き換えた。以前はこのテストファイルだけが独自の
`AccountYaml`/`ChartYaml` スキーマ型を持っていたが、正式なローダができた
ことで併存させる理由が無くなったため（2つのパース経路が乖離しうる状態を
残さない。既存7テストは変更後も全て通ることを確認済み）。

---

## D-062 タグスキーマの `description` の破棄と、重複タグキー検出のための独自デシリアライザ（`kaikei-jp::tags`）

**決定**: `kaikei-jp-data/tags.yaml` → `kaikei_core::TagSchema` のロード
（`kaikei-jp::tags`）で以下を決定した。

1. `description`（`kaikei_core::TagDef` に対応するフィールドが無い）:
   **ドメイン変換では破棄する**。`deny_unknown_fields` によるスキーマ
   完全性検証のためだけに受け取る
2. `tags:` マッピング（キー = タグキー）は、`BTreeMap<String, TagDefRaw>`
   のような `serde` 標準のマップ型ではなく、`MapAccess` を直接読む
   独自の `deserialize_with`（`ordered_pairs`）で `Vec<(String, TagDefRaw)>`
   （出現順・**重複を保持**したペア列）として受け取り、`from_raw` 側で
   明示的に重複キーを検出して `JpError::InvalidTagSchema` を返す

**検証した事実**（`serde_norway` 0.9.42、`crates/kaikei-jp/src/tags.rs` 実装前に
一時テストで確認。実装には残していない一過性の確認）:

- `serde_norway::from_str::<BTreeMap<String, i32>>("a: 1\nb: 2\na: 3\n")` は
  **エラーにならず** `{"a": 3, "b": 2}` を返す（`a` の最初の定義は
  エラーも警告も無く消える。後勝ちで上書きされる）
- `MapAccess::next_entry` をループで読む独自 `Visitor` を使うと、
  重複キーを含む全エントリが `Vec<(String, V)>` として**出現順のまま**
  得られる（`"zeta: 1\nalpha: 2\nmid: 3\nalpha: 9\n"` →
  `[("zeta", 1), ("alpha", 2), ("mid", 3), ("alpha", 9)]`）。
  つまり `MapAccess` 自体は YAML のドキュメント順を保っており、
  順序を失う・重複を失うのは「どの Rust の型に集約するか」の選択に依る

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `tags: BTreeMap<String, TagDefRaw>` にする（素直な実装） | 上記の検証どおり、`tags.yaml` に重複キーがあってもエラーにならず1件が黙って消える。`tags.yaml` は「任意パスからの差し替え」（`kaikei-jp::tags::load_from_path`）でユーザー自身が編集するファイルでもあり、コピペミスによる重複キーが記帳可能なタグ定義を1件まるごと黙って失わせるのは `CLAUDE.md` §4「TagSet はゴミ箱ではない」の精神に反する |
| `tags: HashMap<String, TagDefRaw>` にする | `BTreeMap` と同じく重複キーを検出できない。加えて `TagSchema::new` に渡す `Vec` の順序が `HashMap` の反復順（プロセスごとに変わりうる）に依存してしまう。最終的な `TagSchema` は内部で `BTreeMap` に詰め直されるため挙動には影響しないが、順序に依存する将来のコード（例えばエラーメッセージで「最初に見つかった問題」を報告する処理）が追加されたときに再現性の無いバグを生みかねない |
| `description` を `kaikei_core::TagDef` に追加して保持する | `kaikei-core` の型変更は本 PR のスコープ外。現時点で `TagSchema`/`TagDef` の利用側（`kaikei_core::JournalEntry::new` のタグ検証等）がこれを必要としていない（YAGNI）。将来 MCP 経由でタグの意味を AI に説明する用途が生じたら、`kaikei_core::TagDef` への追加を別途検討する |

**理由**: 重複タグキーの検出は「YAML の生の形をどう Rust の型に落とすか」
という一見小さな選択で結果が変わってしまう（マップ型を選んだ時点で
重複情報が失われ、検出しようがなくなる）ことが実測で分かったため、
検出したいなら重複を保持できる形（`Vec`）で読むしかない、という
機構上の制約に従った。`description` の破棄は D-061 の `sort` と同じ理由
（対応する置き場が無く、今使う見込みも無い）。

**トレードオフ**: `MapAccess` を直接読む `Visitor` は標準の `#[derive(Deserialize)]`
より複雑で、YAML デシリアライズの詳細（`serde` の `Deserializer`/`Visitor` API）に
触れるコードをこの crate に持ち込むことになる。`tags.yaml` の `tags:` 以外の
マッピング型フィールドで同様の要求が出た場合、この `ordered_pairs` は
`TagDefRaw` に固定されておらず `V: Deserialize<'de>` でジェネリックにしてあるため、
そのまま再利用できる。

---

## D-063 家事按分の家事分は「総額 − 事業分」の引き算で求め、比率を反転して別途丸めない

**決定**: `household_split`（`docs/04-jp-tax.md` §8）は、事業分を
`Money::mul_ratio(business_ratio, settings.rounding)` で計算した後、
家事分を **`total.sub(&business_amount)`（引き算）** で求める。
`total.mul_ratio(1 - business_ratio, settings.rounding)` のように
家事分を独立してもう一度丸め計算する形にはしない。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| 家事分も `apply_ratio(total, 1 - business_ratio)` で独立に計算する（`docs/04-jp-tax.md` §8 の擬似コードだけを見ると自然に見える形） | 端数処理を事業分・家事分の双方で独立に行うと、丸めの向き（`RoundMode::Ceil`/`Floor`）次第で「事業分 + 家事分」が総額から1円ずれうる。例えば総額100,001円・事業割合30%・`Ceil` の場合、事業分は `ceil(100,001×0.30)=30,001`、家事分を独立に `ceil(100,001×0.70)=70,001` と計算すると合計100,002円になり総額を1円超える。これは貸借不一致に直結する事故で、`PROGRESS.md` の Phase 1 で実際に踏んだ「明細ごとに丸めると合計が1円ずれる」バグ（`DECISIONS.md` D-058 が参照する教訓）と同型 |
| 差額を按分後に事後調整する（大きい方の明細に差額を足し引きする） | 「引き算で求める」だけで済む問題に対して、わざわざ差額検出＋事後修正という複雑な手順を導入することになる。KISS に反し、修正対象をどちらの明細にするかという新たな仕様判断（本質的に不要な判断）を生む |

**理由**: `Money` は最小通貨単位の整数（`i128`）であり、按分は必然的に
端数処理を伴う。2値の合計を固定値（総額）に一致させたいとき、片方を
丸めた後もう片方を「全体 − 丸めた片方」の引き算で求めれば、丸め方式や
比率によらず合計は構造的に総額と一致する。これは「2つの独立した丸め計算の
結果を後から検証する」のではなく、「そもそも合計がずれない計算の組み方を
選ぶ」という設計であり、`CLAUDE.md` の「会計データは間違うと実害が出る」
という前提に対して安全側の実装である。

**トレードオフ**: 事業分・家事分のどちらに端数が寄るかは丸め方式
（`RoundMode`）と事業分を先に計算するという実装順序に依存する。
`RoundMode::Ceil` を選んでも「家事分が必ず切り上げられる」わけではない
（家事分は引き算の結果であり、丸め自体は事業分の計算にしか適用されない）。
この非対称性は「事業分を先に計算する」という実装上の選択の帰結であり、
按分率の妥当性と同様に税務上の意味を持つ差ではないため許容する。

---

## D-064 家事按分は `JpTaxPolicy`/`TaxContext` を経由せず、独立関数として `JpSettings` を直接受け取る

**決定**: `household_split(input: HouseholdSplitInput, settings: &JpSettings)
-> Result<Vec<JournalLine>, JpError>` を `kaikei-policy::TaxPolicy` の
メソッドにはせず、`kaikei-jp::household_split` の独立した関数として実装する。
`kaikei-policy::TaxContext` は要求しない。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `TaxPolicy` に `household_split` 相当のメソッドを追加し、`ctx: &TaxContext<'_>` を受け取る | `TaxContext` の4項目（`as_of` / `chart` / `tag_schema` / `counterparties`）はどれも家事按分の計算に使わない。`tax_category` の実在確認（`TaxCategoryTable` の参照。`as_of` によるマスタ選択が必要）はこの関数では行わず、記帳時に別途 `TaxPolicy::validate_tag` が担う設計（モジュール doc 参照）にしたため、`ctx` を受け取ってもフィールドを使わない引数が残るだけになる。`TaxPolicy` trait は「税額計算と税区分の妥当性」という一貫した責務を持つ trait であり、按分そのもの（`apply_ratio`）とは責務が異なる `household_split` を無理に生やすと trait の凝集度が下がる |
| `JpTaxPolicy::apply_ratio`（`TaxPolicy` の trait メソッド）経由で事業分を計算する | `apply_ratio` は `&self: JpTaxPolicy` のメソッドであり、呼び出すには `JpTaxPolicy`（`TaxRuleSets` を保持する重い構築物）のインスタンスと `TaxContext`（4項目の参照一式）が要る。`household_split` が実際に必要とするのは丸め方式（`JpSettings.rounding`）1個だけであり、`Money::mul_ratio(ratio, round_mode)` を直接呼ぶ方が依存が引数にそのまま現れて単純（`CLAUDE.md` §6「依存が引数に全部現れる」） |

**理由**: `docs/04-jp-tax.md` §8 の擬似コードも `settings: &JpSettings` を
引数に取る独立関数として定義されており、`TaxPolicy` のメソッドにはしていない。
実際に必要なデータ（丸め方式）を精査した結果、`TaxContext` の4項目は
どれも不要だと判明したため、素直にその通りに実装した。`JpSettings` は
`kaikei-jp` 自身の型であり、`kaikei-jp` の中の関数がこれを直接受け取ることは
`CLAUDE.md` §1 の依存方向にも抵触しない。

**トレードオフと、呼び出し経路の制約**: `household_split` は `TaxPolicy` の
一部ではないため `Arc<dyn TaxPolicy>` 経由では呼べない。では誰が呼ぶのか。

**`kaikei-app` から `kaikei-jp::household_split` を直接 import してはいけない。**
`CLAUDE.md` §1 の依存方向（`kaikei-app` は `policy` の trait にのみ依存し、
`jp` は注入される）に反し、`.github/workflows/architecture.yml` の
「kaikei-app は infra を知らない」ステップが**機械的に落とす**。

正しい経路は**合成ルート**（Phase 3 の MCP サーバー等、`kaikei-app` と
`kaikei-jp` の両方を知ってよい最上位の層）が `household_split` を呼んで
`Vec<JournalLine>` を組み立て、それを `kaikei-app` の
`post_entry::execute` に**入力として渡す**形。`household_split` の戻り値は
`kaikei_core::JournalLine` の列であり、`kaikei-jp` の型は一切含まないため、
この受け渡しで層の境界は崩れない。

> 【訂正】この決定の初版は「`kaikei-app` が直接 import することは許容範囲」と
> 書いていたが、**誤り**。CI が禁止しており、そのとおりに実装すればビルドが
> 通らない。レビューで指摘されて上記のとおり改めた。

---

## D-065 `JpSoleProprietorClosingPolicy::opening_entries` は既定実装のまま実装しない

**決定**: `kaikei-policy::ClosingPolicy::opening_entries` の既定実装
（何も生成しない）を `JpSoleProprietorClosingPolicy` でオーバーライドしない。
個人事業主の元入金振替のうち「事業主借 − 事業主貸」を反映する部分（`docs/04-jp-tax.md`
§9 手順3の完全形: `翌年期首の元入金 = 前年元入金 + 前年所得 + 事業主借 − 事業主貸`）と、
事業主貸・事業主借の期首リセットは、この PR では一切実装しない。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| 当年度末（`closing_entries`）に「事業主借 − 事業主貸」を元入金へ振り替え、事業主貸・事業主借もその場でゼロにする | `docs/04-jp-tax.md` §9 の doc、`crates/kaikei-policy/src/closing.rs` の `opening_entries` doc が明示的に「当年度末と翌年期首のどちらに計上するかは未確定」としており、独断で当年度末側を選ぶと `CLAUDE.md` §9「仕様が曖昧な箇所は実装せず、docs に疑問として書き出して人間に返す」に反する |
| `opening_entries` を実装し、翌年期首側で「事業主借 − 事業主貸」の振替と期首リセットを行う | 同上の理由に加え、`docs/08-compliance.md` §9-4「事業主貸・事業主借の期首リセットの仕訳（振替仕訳を起こすか、期首残高として設定するか）」も未解決のままであり、「振替仕訳を起こす」という実装方式自体を選ぶこと自体が税理士確認前の先取りになる |

**理由**: 判断が必要な箇所を実装で先取りすると、`CLAUDE.md` §10「税務判断を
断定するメッセージを出さない」の精神に反する形で帳簿の仕訳を確定させてしまう。
`closing_entries` が実装する「所得を元入金へ振り替える」部分（§9 手順1・2・3の
うち当年度末に計上する分）は、収益・費用のゼロ化に対する貸借上の対応として
構造的に必要であり判断の余地が無いが、「事業主借 − 事業主貸」の反映と
事業主貸・事業主借のリセットはタイミング・方式の両方が未解決であるため、
trait の既定実装（何もしない）に委ねる。

**トレードオフ**: `JpSoleProprietorClosingPolicy` を実際に使う場合、決算後も
事業主貸・事業主借の残高は翌年度に持ち越されたままになる（期首リセットが
一切行われない）。税理士確認が済み次第、`opening_entries` を実装するか、
`closing_entries` 側に統合するかを別 PR で判断する。

---

## D-066 決算科目コードは構築時に受け取り、存在確認も構築時に行う（実行時に決算処理が失敗するより起動時に落とす）

**決定**: `JpSoleProprietorClosingPolicy::new` は元入金・事業主貸・事業主借の
3科目コードを引数で受け取り、`ChartOfAccounts` にそれぞれ存在することを
**構築時に**検証する。存在しなければ `JpError::MissingClosingAccount`
（見つからなかった科目の役割と科目コードを含む）を返し、構築そのものを
失敗させる。3科目とも `pub fn new` の必須引数とし、既定値（`"400"`/`"410"`/
`"420"` 等）にフォールバックする経路は用意しない。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| 科目コードをハードコードする（`sole_proprietor.yaml` の 400/410/420 を定数として埋め込む） | `docs/04-jp-tax.md` §1・§5「科目コード体系はユーザーが変更できる」に反する。`kaikei-jp::chart` はユーザーが独自の科目表に差し替えられる設計であり、決算科目コードだけ固定にすると差し替え時に静かに壊れる |
| `closing_entries` の呼び出し時（`TrialBalance` を渡すたび）に科目の存在を検証する | `JpTaxPolicy` が年度別マスタの整合性を構築時に検証する（構築失敗で早期に気づける）のと非対称になる。決算処理は年に一度しか実行されないため、記帳作業を進めた後の決算処理実行時に初めて「元入金の科目コードが違う」と分かるのは手戻りが大きい。起動時（合成ルートでの `JpSoleProprietorClosingPolicy::new` 呼び出し時）に落とす方が早期発見できる |
| 検証をせず、存在しない科目コードのまま `JournalLine::new` に渡してエラーを委ねる | `JournalLine::new` 自体は科目の実在を検証しない（`ChartOfAccounts` に対する検証は `TrialBalance::from_entries` や `JournalEntry::new` の責務）。検証を委譲する先が無く、決算科目が誤っていても `ProposedEntry` がそのまま生成されてしまう |

**理由**: `docs/04-jp-tax.md` §9「実装上の注意」が「元入金・事業主貸・事業主借の
科目コードは `JpSoleProprietorClosingPolicy` が構築時に保持する」「年度別税区分
マスタ・事業者設定を `JpTaxPolicy` が構築時に保持するのと同じパターン」と
明記しており、`JpTaxPolicy::new`（マスタの整合性を構築時に検証する）と対称的な
設計にした。`CLAUDE.md` §11「次の手が分かる文言にする」に従い、エラーメッセージ
には見つからなかった科目の役割（例: "元入金"）と科目コードの両方を含める。

**トレードオフ**: 事業主貸・事業主借は現時点の `closing_entries`（D-065 により
`opening_entries` は未実装）では使わないが、将来の実装のために構築時に
まとめて要求・検証する。今は使われないフィールドを保持することになるが、
決算科目3つを一括りの構築時データとして扱う方が「決算に必要な科目一式」という
まとまりが分かりやすく、後から1つずつ追加するより認知負荷が低いと判断した。

### 追記: `tax_category` タグの扱いと構築時検証（実際に踏んだ不具合への対応）

**決定**: `JpSoleProprietorClosingPolicy::new` に `schema: &TagSchema` と
`tax_category: Option<String>` を追加する。`tax_category` が `Some` なら、
`closing_entries` が生成する収益・費用のゼロ化明細（元入金の明細は含まない）に
`tax_category` タグとして付与する。**どの区分コードを使うかはここに
ハードコードしない**（呼び出し側が決める）。構築時に、`closing_entries` が
実際に生成するのと全く同じ `TagSet`（`tax_category` が `Some` ならその1タグ、
`None` なら空）を `schema.validate(..., AccountType::Revenue)` /
`schema.validate(..., AccountType::Expense)` の両方に通し、失敗すれば
`JpError::ClosingTagSchemaMismatch` で構築を失敗させる。

同梱の `kaikei-jp-data/tags.yaml` は `tax_category` を
`required_for: [Revenue, Expense]` としているため、`tax_category` を
指定せずに構築した `JpSoleProprietorClosingPolicy` が生成する
`ProposedEntry` を、そのタグスキーマの下で `kaikei_core::JournalEntry::new`
に通すと `CoreError::MissingRequiredTag` で拒否される。**これは PR-7 の
初版で実際に踏んだ不具合**（レビューでの再現テストにより発覚）であり、
この追記はその修正である。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| どの税区分コードを使うか（例: `"NOT_APPLICABLE"`）を `JpSoleProprietorClosingPolicy` にハードコードする | `docs/04-jp-tax.md` §1「税率も控除割合もコードに書かない」・`CLAUDE.md` §10「税務判断を断定するメッセージを出さない」に反する。決算振替仕訳にどの消費税区分を充てるべきかは会計上の判断であり、`kaikei-jp` の実装が決めてよいことではない。同梱の税区分マスタ（`kaikei-jp-data/tax/jp/2026.yaml`）の `NOT_APPLICABLE`（「対象外」。注記に「資産・負債の振替など、消費税と無関係な取引に使う」とある）は doc 上の**候補**として言及するに留め、既定値としては埋め込まない |
| `kaikei_core::TagSchema` に `required_for` を読み出す getter（例: `is_required_for(key, account_type) -> bool`）を追加する | `kaikei-core` は本 PR で変更しない凍結層。加えて、`required_for` を読み出せたとしても「読み出した情報をもとに `tax_category` の型・存在を自前で再検証するロジック」を別途書く必要があり、`TagSchema::validate` が既にそれを正しく行っている（未登録キー・型不一致も含めて）ことの車輪の再発明になる |
| `closing_entries` の呼び出し時にタグスキーマ適合を検証する | D-066 本文の「`closing_entries` の呼び出し時に科目の存在を検証する」を却下した理由と同型。決算処理は年に一度しか実行されないため、記帳作業を進めた後で初めて分かるのは手戻りが大きい |

**理由**: `kaikei_core::TagSchema` は `required_for` を読み出す専用の
公開 API を持たない（`defs` フィールドは非公開、`is_aggregatable` は
無関係）。`kaikei-core` を変更せずにこの不具合を修正する方法を検討した結果、
「`required_for` を間接的に問い合わせる」のではなく、**`closing_entries` が
実際に生成するのと同一の `TagSet` を `TagSchema::validate`（既存の公開 API）に
通す**という形にたどり着いた。これは `required_for` の中身を読むより強い
チェックになる（未登録キー・型不一致も同時に検出でき、「`closing_entries` が
生成する明細が実際にこのスキーマを満たすか」を直接保証する。「必須かどうか」を
間接的に判定してから改めて型や存在を確認する二度手間が要らない）。

**トレードオフ**: `schema.validate` は与えられた `TagSet` に対する最初の
違反しか報告しない設計（`kaikei_core::tag.rs` の doc）だが、この用途では
`closing_entries` が生成する `TagSet` は常に「空」または「`tax_category` の
1タグのみ」であるため、複数タグが絡む曖昧さは生じない。元入金（`Equity`）の明細は常に空の
`TagSet` であり、収益・費用とは付けるタグが違うため、**`AccountType::Equity`
に対しても空の `TagSet` を同じように検証する**。同梱のタグスキーマは `Equity`
に必須タグを持たないので現状では素通りするが、ユーザーが自分のスキーマに
差し替える経路（`kaikei-jp::tags::load_from_path`）がある以上、ここを
検証しないと「決算処理を実行した瞬間に落ちる」経路が残る。
構築時に落とすという本決定の目的そのものが穴だらけになるため、
3つの科目種別すべてを検証対象にした。

**構築時に検証する項目は、最終的に次の4つになった**（レビューで2件の
取りこぼしが再現され、追加した）:

1. 決算科目3つが勘定科目表に**存在する**こと
2. 決算科目3つが**記帳可能**（`postable: true`）であること。
   見出し科目を指定されると `closing_entries` は明細を作れてしまう一方、
   記帳時に `CoreError::NotPostable` で落ちる
3. 収益・費用のゼロ化明細のタグがスキーマに適合すること
4. 元入金の明細（常に空の `TagSet`）が、**勘定科目表に実際に登録されている
   科目種別**に対してスキーマに適合すること。`AccountType::Equity` を
   決め打ちしてはいけない。記帳時に `JournalEntry::new` がタグを検証するのは
   登録種別の方であり、決め打ちすると「構築時は通ったのに記帳時に落ちる」
   食い違いが生まれる（レビューで実際に再現された）

**採らなかった選択肢**: 決算科目が本当に `Equity` として登録されているかを
**意味的に**検証する。`JpTaxPolicy` を含む既存の実装がこの種の意味検証を
持たないこと、およびユーザーが差し替えた科目表で分類が違いうることから、
「記帳できないこと」を防ぐ機械的な検証に留め、「元入金なのに資産として
登録されている」という設定ミス自体は弾かない。上記4項目で
「構築は通ったが決算処理が実行できない」経路は塞げている。

---

## D-067 `JpStatementPolicy` は `Result` を返せない制約に対し、科目名解決はフォールバック、金額計算は既存の `.expect()` 前提を踏襲する

**決定**: `kaikei-policy::StatementPolicy::balance_sheet` / `income_statement`
は `Statement`（`Result` ではない）を返す凍結済みのシグネチャである。
`JpStatementPolicy` はこの制約の下で失敗しうる2箇所を次のように扱う。

1. 科目コードが構築時に保持した `ChartOfAccounts` に見つからない場合、
   `StatementLine.label` は科目コードの文字列（`AccountCode::as_str()`）を
   そのまま使う（行自体は落とさない）
2. 純利益（収益合計 − 費用合計、`Money::sub` が返す `Result`）は `.expect(...)`
   で展開する。失敗しうるのは `i128` の表現上限を超えるオーバーフローのみで、
   通貨不一致は `TrialBalance` の構築時保証により構造的に起こらない

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `kaikei-policy::StatementPolicy` を `Result<Statement, PolicyError>` を返すよう変更する | `kaikei-policy` は Phase 1 で凍結された層であり、この PR（`kaikei-jp` のみ変更するスコープ）で変更しない。変更するとしても trait の破壊的変更であり、影響範囲の精査は本 PR のスコープを超える |
| 科目コードが見つからない場合は該当行を `Statement` から**除外**する | 試算表に実際に存在する残高を無言で欠落させることになり、`CLAUDE.md`「会計データは間違うと実害が出る」に反する。表示名が分からないだけで金額そのものは正しいため、行は残してコードをそのまま表示する方が安全側 |
| 純利益の計算失敗時に `Statement.total` を `Money::zero(...)` にフォールバックする | オーバーフローという明らかな異常時に「純利益0円」という**誤った値**を無言で返すことになり、`CLAUDE.md` §11「誤診は誤値と同じ実害を持つ」（`PROGRESS.md` Phase 1 の教訓）に反する。`.expect()` による panic の方が、誤った金額を静かに返すより安全 |

**理由**: `TrialBalance::total_by_type` / `totals()` 自身が「同一 `TrialBalance`
内の残高は同一通貨であることが構築時に保証されているため、オーバーフローしない
限り失敗しない」という前提で `.expect(...)`（オーバーフロー時は panic）を
既に採用している（`kaikei-core/src/trial_balance.rs`）。`JpStatementPolicy` の
純利益計算はこの2つの戻り値を1回 `sub` するだけであり、同じ前提・同じ対処法を
踏襲するのが一貫性がある。科目名の解決はそもそも「無くても業務を止めるべき
異常」ではなく（科目コードそのものは常に表示できる）、フォールバックで十分と
判断した。

**トレードオフ**: 呼び出し側が保持する `ChartOfAccounts` と `TrialBalance` の
構築に使った `ChartOfAccounts` が食い違っている場合、`Statement` の一部の行が
科目名ではなく科目コードのまま表示される（誤りではないが読みにくい）。
オーバーフローが実際に発生した場合は `Statement` を返せず panic するため、
呼び出し側（将来の `kaikei-app`/`kaikei-mcp` 統合）はこの前提が現実的に
許容できるかを再検討する余地を残す。

---

## D-068 E2E テストの置き場として `kaikei-e2e` を新設する（合成ルートを模した層）

**決定**: `crates/kaikei-e2e` を新設し、Phase 2 の実装が実 PostgreSQL を
通しで動くことを検証する E2E テストをここに置く。`publish = false` の
テスト専用 crate とし、`kaikei-core` / `kaikei-policy` / `kaikei-jp` /
`kaikei-jp-data` / `kaikei-app` / `kaikei-store` / `sqlx` / `tokio` に依存する。
`src/lib.rs` は合成ルートが起動時に一度だけ行う組み立て（YAMLロード →
policy 構築。[`compose`]）のヘルパのみを持ち、業務ロジックは置かない。
`.github/workflows/architecture.yml` に「kaikei-e2e は誰からも依存されない」
検査を追加し、他のどの crate の `Cargo.toml`（`dev-dependencies` を含む）にも
`kaikei-e2e` が現れないことを機械的に検査する。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `kaikei-store/tests/` に置く（Phase 1 の `e2e_usecase.rs` と同じ場所） | `kaikei-store` は `kaikei-jp`/`kaikei-policy` に依存しないことを CI が検査している（`CLAUDE.md` §1）。`JpTaxPolicy` を実際に注入するテストを書くには `kaikei-jp` への依存が必要になり、`--edges normal` を見ない dev-dependency 経由で追加すれば機械的には通ってしまうが、それは D-047 が指摘した「CI の意味が消える」抜け道そのものになる |
| `kaikei-jp/tests/` に置く | `kaikei-jp` は `sqlx`/`tokio`/`kaikei-store` を知らない（`CLAUDE.md` §1）。実 DB に接続するテストを書くには `kaikei-store` への依存が必要で、`kaikei-store` 側と対称の問題が起きる |
| Phase 3 の `kaikei-mcp` を先取りしてこの PR で作り、その中に E2E テストを置く | 本来の目的（MCP ツール・監査ログ等）に対して過大な実装を Phase 2 の PR に持ち込むことになり YAGNI に反する。「両方を知ってよい最上位の層が必要」という要求だけを満たす最小の形が、テスト専用 crate の新設だった |
| `#[cfg(test)]` を使わず、`kaikei-app` の統合テスト（`kaikei-app/tests/`）に `kaikei-jp`/`kaikei-store` の両方を dev-dependency として追加する | `kaikei-app` はユースケース層であり、実装（`kaikei-jp`/`kaikei-store`）を知らないことがこの crate の存在意義そのもの（`CLAUDE.md` §1「jp は注入される」）。テストのためだけにこの原則を緩めると、本体コードと同じ crate に「知ってはいけないものを知っているコード」が混在する |

**理由**: `kaikei-store` は `kaikei-jp`（および `kaikei-policy`）を知らず、
`kaikei-jp` も `kaikei-store`（DB・sqlx・tokio）を知らない
（`.github/workflows/architecture.yml` の「kaikei-store は kaikei-jp /
kaikei-policy に依存しない」「kaikei-jp は infra を知らない」の各ステップが
それぞれ機械的に検査する）。「税抜経理の消費税行が実際に PostgreSQL へ記帳
できる」ことを検証するテストは、原理的に両方を知ってよい層でしか書けない。
本番の合成ルート（Phase 3 の `kaikei-mcp`、または Phase 4 の `kaikei-api`）を
先取りするのは時期尚早（YAGNI）なので、テスト専用の最小の合成ルートとして
`kaikei-e2e` を新設した。

**トレードオフ**: ワークスペースの member が1つ増え、CI（`architecture.yml`/
`database.yml`）に専用の検査・実行ステップが必要になる。また「他のどの
crate からも依存されない」という制約自体を CI で守り続ける必要があり、
守らなければこの crate の存在意義（テスト専用であること）が崩れる。

---


> 【訂正】この決定の初版は、合成ルートの組み立てヘルパ `compose()` も
> `kaikei-e2e` に置いていた。**誤り。** `compose()` が使うのは
> `kaikei-core` / `kaikei-jp` / `kaikei-jp-data` だけで、`kaikei-app` も
> `kaikei-store` も使わない。つまり「両方を知ってよい層」でなければ
> 書けないコードではなく、`kaikei-jp` 単体で閉じている。
>
> テスト専用 crate に置いたままだと、Phase 3（`kaikei-mcp`）と
> Phase 4（`kaikei-api`）の合成ルートが再利用できず、**同じ組み立てが
> 3箇所に複製される**（D-047「手で維持する複製は腐る」と同型）。
> レビュー指摘を受けて `kaikei-jp::compose` へ移した。
>
> `kaikei-e2e` に残るのは**実 DB に繋ぐ E2E テストだけ**。そちらは
> `kaikei-store` を知る必要があるため `kaikei-jp` には置けず、
> この crate を新設した理由はそこにある（本決定の趣旨自体は変わらない）。

## D-069 `JpStatementPolicy` の `chart` は決算書生成の直前に都度読み直したもので構築する（起動時に長期保持しない）

**決定**: `kaikei_jp::compose::compose` が返す `Composition` に
`JpStatementPolicy` を含めない。`JpStatementPolicy::new(chart)` は、決算書
（BS/PL）を組み立てる直前に、その時点で読み込んだ `ChartOfAccounts` から
**都度**呼び出すこと、と `crates/kaikei-jp/src/compose.rs`（および `kaikei-jp`
のクレート doc）に方針として明記する。

> 【後日の訂正】本決定の初版は組み立ての置き場を `kaikei-e2e::compose` と
> 書いていた。**誤り。** D-068 の訂正注記のとおり `compose()` は PR-23 で
> `kaikei-jp` へ移っており、`kaikei-e2e` 側は
> `pub use kaikei_jp::compose::{...}` の再エクスポートに過ぎない。
> Phase 3 の実装者は `kaikei-e2e` に依存できない（`architecture.yml` の
> 「kaikei-e2e は誰からも依存されない」ステップ）ため、初版の記述のままでは
> **方針の置き場所を辿れない**。決定の内容（`JpStatementPolicy` を長期保持
> しない）は変わらない。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `JpTaxPolicy`/`JpSoleProprietorClosingPolicy` と同様に、起動時に一度構築して `Composition` に含め長期保持する | `JpStatementPolicy` が保持する `chart` は、`JpTaxPolicy` の年度別マスタ（YAML由来、`DECISIONS.md` D-025/D-057）や `JpSoleProprietorClosingPolicy` の決算科目コード（構築時に実在検証、D-066）とは性質が異なる。マスタ・決算科目コードは「変更にはプロセス再起動を要する」という前提が既に D-025/D-057 で明示的に許容されているが、`ChartOfAccounts` は `kaikei-app/src/context.rs` の `load_posting_context` が**記帳のたびに** `tx.load_chart()` で読み直している、ユーザーが日常的に編集しうる可変データである。長期保持すると、科目名を変更した直後に決算書を出しても古い名前のまま表示される、という「表示される科目名が古い」バグになる |
| `Composition` に `JpStatementPolicy` を含め、決算書生成のたびに `chart` フィールドだけ差し替える（`with_chart` のような更新メソッドを生やす） | `JpStatementPolicy` は `chart: ChartOfAccounts` を1フィールドだけ持つ薄いラッパであり、可変にするメリットがない。新しい `chart` を受け取るたびに `JpStatementPolicy::new(chart)` で作り直す方が単純（`JpStatementPolicy::new` は YAML 解釈や構築時検証を一切行わないため、作り直しのコストは無視できる） |
| 折衷案として、起動時に構築した `JpStatementPolicy` を保持しつつ、一定時間ごとに再構築する（TTLキャッシュ） | 個人事業主・単一ユーザー・自己ホスト前提（`DECISIONS.md` D-015）の規模で、キャッシュの複雑さに見合う性能上の必要性が無い。`JpStatementPolicy::new` のコストが無視できる以上、「都度作る」方がキャッシュ無効化のバグを構造的に排除できて安全 |

**理由**: `JpTaxPolicy` や `JpSoleProprietorClosingPolicy` が構築時にデータを
保持して問題にならないのは、それらが保持するデータ（年度別マスタ、決算科目
コード）が「変わるとしても稀で、変わったら再起動すればよい」という前提を
明示的に選んでいるため（D-025/D-057/D-066）。`JpStatementPolicy` の `chart`
はこの前提が成り立たない（ユーザーが科目表を編集する経路が通常の運用として
存在する）ため、同じパターンを機械的に適用すると「表示される科目名が古い」
という別のバグ類を生む。`kaikei-e2e/tests/e2e_jp.rs` の
`phase2_end_to_end_scenario_posts_and_closes_the_books` は、決算書生成の
直前に読み直した `chart` から `JpStatementPolicy::new(chart)` する形で
このテストを実装している。

**トレードオフ**: `JpStatementPolicy` を `Composition` から除外したことで、
呼び出し側（合成ルート）は「決算書を生成するタイミングで `chart` を読み直し、
その場で `JpStatementPolicy::new` する」という一手間を明示的に書く必要が
ある。起動時に一度だけ組み立てて済ませたい場合と比べるとコードは長くなるが、
「表示される科目名が古い」という誤診に近いバグ（`CLAUDE.md` §11・
`PROGRESS.md` Phase 1 の教訓「誤診は誤値と同じ実害を持つ」）を構造的に
避けられる利点の方が大きいと判断した。

---

## D-070 Phase 3（`kaikei-mcp`）のスコープを11ツールに確定し、残りは名前を予約したまま延期する

**決定**: Phase 3 で MCP に**登録する**ツールを次の11件に確定する。

- 読み取り系(7): `list_accounts` / `get_entry` / `get_trial_balance` /
  `search_entries` / `get_ledger` / `list_tax_categories` / `get_settings`
- 書き込み系(2): `post_journal_entry` / `reverse_journal_entry`
- 提案系・検証系(2): `suggest_tax_category` / `validate_invoice_number`

次の11件は `docs/07-mcp-server.md` の表に**残したまま「Phase 4 以降」と明記**し、
Phase 3 では MCP に登録しない（登録しないツールは AI からは存在しないのと同じ）。

| 延期するツール | 延期理由 |
|---|---|
| `get_statements` / `explain_balance` | `TrialBalance` / `BalanceRow` を `kaikei-core` の外から構築できない（D-031。`GroupKey` に公開コンストラクタが無い） |
| `close_period` | `ROADMAP.md` の Phase 3 成果物に無い。`period_snapshots` の NOT NULL 列を埋めるには**期間内の仕訳を列挙するポートが必要だが存在しない**（`JournalRepo` は find/insert のみ）。canonical JSON 正規化・ハッシュ連鎖・新ユースケース・権限テストが芋づるで要り、`dry_run` の `pending_transactions` は Phase 4 依存。**checksum の計算式自体は `docs/03-database.md` §2 に定義済み**だが、`canonical_json` が対象とする仕訳の JSON 形は未定義で、Phase 5 の `kaikei verify` と揃える必要がある |
| `list_pending_transactions` / `journalize_transaction` / `ignore_transaction` / `upsert_journalize_rule` / `suggest_journal_entry` | `kaikei-import` が未着手（crate もテーブルも存在しない） |
| `search_documents` / `attach_document` | `kaikei-blob` が未着手（`documents` / `entry_documents` は Phase 4 で設計する） |
| `upsert_counterparty` | `ChartRepo` に書き込みメソッドが無い（DB 権限は既にある） |

`delete_journal_entry` / `update_journal_entry` / `execute_sql` / `reopen_period` は
**将来も作らない**（D-014。「Phase 4 以降」ではない）。

あわせて、このスコープに伴う次の事項を確定する。

1. **`search_entries` / `get_ledger` / `get_entry` は Phase 3 に含める**
   （`ROADMAP.md` Phase 3 の成果物「読み取り系ツール」を採る）。read model
   （`kaikei-store/src/query/{search,ledger,entry_detail}.rs`）を Phase 3 で新設し、
   `crates/kaikei-store/src/query/mod.rs` の「Phase 5 で実装する」という注記を
   Phase 3 に直す。
2. **勘定科目マスタの投入は MCP ツールにしない。** `kaikei-app` に専用の
   ユースケースを新設して行う（`ChartRepo` の doc が言う「マスタの編集は本 crate
   のユースケースの対象外」はこの範囲で改める）。現状これが無いため
   `kaikei-e2e/tests/e2e_jp.rs` の `seed_chart` が migrator ロールで生 SQL を
   発行しており、本番の合成ルートには流用できない。
3. **`PolicyNote` を `post_journal_entry` の戻り値に含め、`audit_log.output` にも残す。**
   `post_entry::execute` は現在 `derive_tax_lines(...)?.lines` として `notes` を
   捨てており、戻り値の拡張（`PostEntryOutput { entry, notes }` 相当）が Phase 3 の
   スコープに入る（Phase 2 の申し送りへの回答。D-059）。
4. **audit_log は別コネクションで2回書く**（開始レコード `status='started'` →
   操作 → 結果レコード `status='ok'|'error'`。`request_id` で対応付ける）。
   開始レコードの書き込み失敗は **fail-closed**（操作しない）、結果レコードの
   失敗は **fail-open**（操作は成功として返し警告を添える）。
5. **事業者設定は明示必須。未設定は起動失敗**（既定値にフォールバックしない。D-057）。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `docs/07-mcp-server.md` の22ツールをそのまま Phase 3 の対象として実装する | 半数は依存する crate（`kaikei-import` / `kaikei-blob`）もテーブルも存在せず、`get_statements` / `explain_balance` は D-031 により core の外から `TrialBalance` を構築できず、`StatementPolicy` へ橋を架けるには core の公開 API 拡大（`CLAUDE.md` §9 により人間の承認が要る）か、集約を全部ロードする形（`CLAUDE.md` §6 が戒める）しかない。ROADMAP の「読み取り系ツール全部」と表を併読した実装者が、必ず実装不能なツールに着手する |
| 延期するツールの節・行を設計書から**削除**する | 「なぜ無いのか」が失われ、後の実装者が善意で復活させる。とくに `suggest_journal_entry` の「`reasoning` と `similar_entries` を必須にする」という設計意図は、このプロジェクトの差別化そのものであり、消すと再発明できない。**残したうえで Phase を明記する**方が安全 |
| `close_period` を Phase 3 に入れ、`checksum` は暫定の式で実装しておく | 締めは不可逆操作であり、後から式を変えると**既に締めた記録を検証できなくなる**。`kaikei verify`（Phase 5）と式が一致しない締めスナップショットは、検証機能から見れば「壊れた記録」と区別できない |
| `search_entries` / `get_ledger` を Phase 5 に送り（`query/mod.rs` の現行注記に従う）、Phase 3 は `get_trial_balance` だけにする | Phase 3 は「自分の帳簿を付け始める」ドッグフーディングの起点（`ROADMAP.md`）であり、記帳した仕訳を引けないと記帳の誤りに気づけない。検索と元帳が無い状態で帳簿を付け始めるのは、`post` の誤りを検出する手段を持たないまま運用を始めることを意味する |
| 監査ログを操作と同一トランザクションで1回だけ書く（列を増やさない） | **失敗した操作の記録が rollback で一緒に消える。** 「AI が何をしようとしたか」を最も知りたいのは失敗したときであり、この設計では最も重要な記録だけが残らない |

**理由**: `PROGRESS.md` Phase 2 の教訓1は「設計書が却下済みの設計を指示したまま
になる事故が3回起きた」である。`docs/07-mcp-server.md` は Initial commit 以降
一度も更新されておらず、D-021〜D-069 が1件も反映されていない状態でこの
Phase の実装者に渡ろうとしていた。同じ事故のより大きな版になる。
ツール一覧に **Phase 列**を入れて「いつ作るか」を1件ずつ明示するのが、
文書を読んだ実装者が実装不能なツールに着手することを防ぐ最小の手当てだった。

**トレードオフ**: Phase 3 で新設する層が増える（read model 3本、勘定科目投入
ユースケース、audit_log 用ポートと `kaikei-store` 実装、`post_entry::execute` の
戻り値拡張）。「MCP はユースケースを呼ぶ薄い層」という当初の見積もりより
Phase 3 の作業量は大きい。ただしこれは見積もりが実態に合っていなかっただけで、
どの層に置くかを曖昧にしたまま進めれば MCP 層に SQL やビジネスロジックが
染み出す（`CLAUDE.md` §1・§6 の違反）ため、先に置き場を決める方を選んだ。
また audit_log の2行構成は書き込み回数が倍になるが、単一ユーザー・自己ホスト
前提（D-015）の規模では問題にならないと判断した。

---

## D-071 MCP SDK は `rmcp` 3.x（stdio）を使い、エラーはツール結果エラー（`isError: true`）で返す

**決定**: `kaikei-mcp` の MCP 実装には公式 Rust SDK の **`rmcp` 3.x** を使う。

```toml
rmcp = { version = "3", default-features = false, features = [
    "server", "macros", "transport-io", "schemars",
] }
```

- トランスポートは **stdio**（MCP クライアントが子プロセスとして起動する）。
  サーバはソケットを開かない。
- 入力は `Parameters<T>` で受け、`T` に `#[derive(Deserialize, JsonSchema)]` を
  付けて input_schema を生成する（`schemars` feature）。
- 1ツール1ファイルは、各ファイルの
  `#[tool_router(router = <ツール名>_router, vis = "pub")]` を `server.rs` で
  `+` 合成して実現する。
- **ドメインのエラーはツール結果エラー（`CallToolResult::error`。`isError: true`）
  で返す。** JSON-RPC のプロトコルエラー（rmcp では `Err(ErrorData)`）は、
  ツール呼び出しに到達できない異常（未知のツール名など）に限る。
- `crates/kaikei-mcp` を workspace の `members` に追加し、`rust-version` は
  workspace 既定の `1.80` を継承せず**個別に `1.88` を宣言する**。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| JSON-RPC 2.0 と MCP のハンドシェイクを手書きする（依存を増やさない） | MCP は仕様が更新され続けているプロトコルであり、`initialize` のバージョン折衝・`tools/list` のスキーマ形・通知の扱いを自前で追随し続けることになる。会計の正しさに寄与しない保守負債であり、`CLAUDE.md` §1 の「依存を増やしたくなったら設計を疑う」は**下位層（core）**の話であって、presentation 層でプロトコル SDK を使わない理由にはならない |
| 第三者製の MCP crate を使う | 公式 SDK が存在する領域でメンテナンス主体が不明な実装に依存する理由が無い。7年保存の会計データを扱う基盤で保守されないライブラリを選ばない（`serde_yaml` → `serde_norway` の判断と同じ。D-049） |
| HTTP（SSE / Streamable HTTP）トランスポートにする | Phase 3 の利用形態は「Claude Code がローカルで spawn する」の1つだけであり、ソケットを開くと認証・バインドアドレス・CORS という論点が一式付いてくる（D-015 の留保）。stdio なら信頼境界が「このプロセスを起動できる OS ユーザー」に閉じる。HTTP が要るのは Phase 4 の `kaikei-api` |
| ドメインエラーを JSON-RPC のプロトコルエラー（`error` オブジェクト）で返す | **クライアントが `error.data` をモデルに見せる保証が無い。** 不透明な文言（"Tool result missing due to internal error" 等）に置き換えて描画されると、`CLAUDE.md` §11 が要求する「次の手が分かる文言」が AI に届かず、原則そのものが空文になる。「貸借不一致のとき AI が自己修正できる」は Phase 3 の完了条件（`ROADMAP.md`）である |
| `rust-version` を workspace 既定の `1.80` のままにする | `rmcp` 3.x が要求する MSRV に届かない。workspace 全体を 1.88 に引き上げると、`kaikei-core` 以下の下位 crate まで presentation 層の都合で MSRV が上がる。上位 crate 側で個別に宣言すれば影響を閉じ込められる |
| `default-features = true` のまま使う | 使わないトランスポート（HTTP/SSE 等）とその依存が入る。`cargo-deny`（`supply-chain.yml`）の監査面と、ソケットを開く経路をビルドに含めない意味の両方で、必要な feature だけを明示する |

**理由**: MCP はこのプロジェクトの差別化の本体（`ROADMAP.md` Phase 3）であり、
価値はプロトコル実装そのものではなく「会計操作を安全に AI へ開くこと」にある。
プロトコル部分は公式 SDK に委ね、こちらは `CLAUDE.md` §11 の
エラーメッセージ設計・§10 の表現規律・監査ログに労力を割く。
エラーの返し方を SDK 採用と同じ決定に含めたのは、この2つが実装上分離できない
（`Err(ErrorData)` と `Ok(CallToolResult::error(...))` のどちらを返すかは
rmcp のハンドラのシグネチャで毎回選ぶ）ためである。

**トレードオフ**: `rmcp` の破壊的変更に追随する必要がある（3.x 系のマイナー
更新でマクロの形が変わる可能性がある）。また `#[tool]` がツール説明文を
**関数の doc コメント**から採るため、`CLAUDE.md` §10・§11 の表現規律が
doc コメントにも及ぶ（doc コメントが AI に読まれる文書になる、という
これまでに無い性質が加わる）。stdio を選んだことで、**stdout は JSON-RPC
専用チャネル**になり、`println!` や stdout に出る `tracing` が1行でも
混ざるとプロトコルが壊れるという運用上の制約を負う
（ログは stderr に固定する。`docs/07-mcp-server.md` §4）。

---

## D-072 エラーコードの語彙を `kaikei-app` に1箇所だけ持ち、メッセージから独立した安定識別子にする

**決定**: MCP / HTTP 応答の `error` フィールドと `audit_log.error_code` に載せる
**分類コード**の対応表を `crates/kaikei-app/src/error.rs` に1箇所だけ置く
（`docs/07-mcp-server.md` §6）。

- 入口は4つ: `AppError::code()` / `RepoError::code()` /
  `core_error_code(&CoreError)` / `policy_error_code(&PolicyError)`。
  `CoreError` / `PolicyError` が**自由関数**なのは、定義元 crate が凍結層で
  `impl` を生やせないため（`CLAUDE.md` §1）。
- コードは **snake_case の安定した識別子**であり、日本語のメッセージ
  （`Display`）とは**別物**。**メッセージを変えてもコードは変わらない。**
  逆にコードの変更は、応答・監査ログ・それを読む AI の分岐を同時に壊す
  破壊的変更なので、意味が変わったら付け替えず新しいコードを足す。
- 名前空間は平坦にし、層ごとの接頭辞を付けない。例外は**異なる層に同名の
  バリアントが実在する場合**のみで、現状は `Unsupported` の1件
  （`policy_unsupported` / `repo_unsupported`）。
- `AppError::Repo` / `Policy` / `Core` は**中身へ委譲**する
  （`AppError::Core(CoreError::Unbalanced)` は `unbalanced`）。
- `#[non_exhaustive]` の実測結果は `AppError` のみ該当。したがって
  `AppError::code()` だけが `_ => "internal"` の受け皿を持ち、
  `RepoError` / `CoreError` / `PolicyError` の写像は網羅 `match` にする。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| 対応表を `kaikei-mcp`（presentation 層）に置く | 写像元の `CoreError` / `PolicyError` / `RepoError` はいずれも `kaikei-app` 以下の型であり、コードは「どの層が返したか」ではなく「何が起きたか」の分類である。presentation 層に置くと、`kaikei-api`（Phase 4）が同じ表を再実装し、**同じエラーに違うコードを返す2つのサーバ**ができる。`audit_log.error_code` を横断で読めなくなる |
| コードを持たず、メッセージ本文だけを返す | AI の分岐も監査ログの集計も自然文のパターンマッチになる。`CLAUDE.md` §11 に従って文言を改善するたびに下流が壊れる（「良い文言に直す」ことに罰則が付く設計になる） |
| コードにエラーの層を含める（`core_unbalanced` / `app_already_reversed`） | 呼び出し元にとって「貸借が合っていない」ことは同じで、どの crate を経由したかは対処に影響しない。層を跨いだリファクタでコードが変わってしまう（`PolicyError::Core(..)` を `AppError::Core(..)` に付け替えただけでコードが変わる） |
| `policy_unsupported` / `repo_unsupported` を `unsupported` 1つに潰す | 「その税区分の処理が未実装」と「証憑の紐付けが未実装（`PgTx::insert_entry` の `document_refs`。D-041）」が区別できなくなる。AI は前者なら税区分を変え、後者なら証憑を外すという**異なる次の手**を取る必要がある（`CLAUDE.md` §11） |
| バリアント名を機械的に snake_case 化する derive マクロを書く | コードが**バリアント名のリネームで壊れる**。リネームは内部リファクタとして自由に行われるべきで、そのたびに外部契約が変わるのは逆立ちしている |
| ~~`AppError::code()` の網羅 `match` から `_ =>` を外し、バリアント追加時にコンパイルエラーで気づく設計にする~~ | ~~受け皿が無いと、`kaikei-app` にバリアントが増えたときに下流（`kaikei-mcp` / `kaikei-api`）が**その場でコードを発明する**~~ → **訂正注記1で採用に転じた（下記）** |

**理由**: `docs/07-mcp-server.md` §6 が「対応表を1箇所に持つ」と定めながら
置き場所を決めていなかった。置き場所を決めないまま `kaikei-mcp` の実装に入ると、
ツールごとに `match` が書かれて必ず食い違う。

**トレードオフ**:

- **`kaikei_jp::JpError` はこの表で覆えない。** `kaikei-app` は `kaikei-jp` に
  依存できない（CI が検査）ため、`kaikei-jp` を直接呼ぶ経路
  （`docs/07-mcp-server.md` §4 の経路 (c)。`list_tax_categories` /
  `get_settings` / `suggest_tax_category` / `validate_invoice_number`）の
  エラーは `kaikei-mcp` 側に別表が要る。**既に語彙がある概念には同じコードを
  使う**という規約だけを先に決めた。

### 訂正注記1（PR-B 2巡目）: `AppError::code()` の受け皿を**削除した**

初版は `_ => codes::INTERNAL` + `#[allow(unreachable_patterns)]` を置き、
割り当て漏れの検出をテスト側の重複した網羅 `match`
（`exhaustive_app_error_code`）に任せていた。**これを撤回する。**

- **`#[non_exhaustive]` は crate の外にしか効かない。** 「下流がバリアント追加で
  壊れないように受け皿を置く」という当初の理由は、下流側の `match`
  （そこでは `_` の腕が**必須**）が担っており、`kaikei-app` 自身の `code()` に
  受け皿を置く理由にはなっていなかった。**却下理由が事実誤認だった。**
- 受け皿があると、バリアントを足しても `code()` のコンパイルは壊れず、
  新しいバリアントが黙って `"internal"` になる。それを別の関数で見張るのは
  **同じ一覧を2つ手で維持する**ことであり、`PROGRESS.md` Phase 1 の教訓6
  「手で維持する一覧は必ず腐る。構造で閉じる」に真っ向から反する。
- 受け皿を消すと `#[allow(unreachable_patterns)]` も不要になる
  （この lint は受け皿の腕自身が作り出していた問題だった）。
  `cargo clippy -p kaikei-app --all-targets -- -D warnings` が警告0で通ることを実測。
- 重複していたテスト関数 `exhaustive_app_error_code` は削除した。
  下流のワイルドカードが実際に効くことの検証は
  `crates/kaikei-app/tests/contract_from_downstream.rs`（外部 crate として
  リンクされる統合テスト）に残っている。

`codes::INTERNAL` は**下流の既定値として**残す（`kaikei-app` からは返らない）。

### 訂正注記2（PR-B 2巡目）: 語彙の範囲を「線上に出る表現」全体に広げた

初版は**エラーの分類コード**だけを `kaikei-app` に集めたが、同じく線上に出る
表現が下流の手書き `match` に残っていた。`kaikei-mcp` / 将来の `kaikei-api` /
`audit_log.output` の3箇所で同じ表を手書きすることになり、綴りがずれる
（D-072 の本文が「1箇所に持つ」と決めた理由そのものが、コードにしか
適用されていなかった）。次を追加した。

| 表現 | 置き場 |
|---|---|
| `Side` / `AccountType` / `NoteSeverity` / `FiscalYearRule` ⇄ 機械可読名 | `kaikei_app::wire`（自由関数。定義元が凍結層なので `impl` を生やさない。`core_error_code` と同じ形） |
| `Money` → 区切り無し文字列、整形済み文字列 → 区切り無し | `kaikei_app::amount` |
| UUID 文字列 → `EntryId` | `kaikei_app::id::entry_id_from_uuid_string`（出力側の `entry_id_to_uuid_string` と対。下流に `uuid` 依存を強いないため） |
| `TaxMode` / `RoundMode` / `RoundingUnit` ⇄ 機械可読名 | `kaikei_jp::tax`（`kaikei-app` は `kaikei-jp` に依存できない。語彙は `kaikei-jp-data` の YAML が定める値そのもの） |

**`kaikei-core` / `kaikei-policy` は変更していない。** `Money::minor()` と
`Currency::minor_unit()` はどちらも公開されており、区切り無しの文字列は
その2つから組み立てられる（`Money::parse` とのラウンドトリップもテスト済み）。

コードも追加した:

- `invalid_entry_id`: 送られた仕訳IDが UUID の正準表記として解釈できない。
  **`not_found` と分ける**（「IDを調べ直す」と「表記を直す」で次の手が違う）。
- `audit_log_unavailable`: `docs/07-mcp-server.md` §9 の fail-closed。
  対応する `AppError` のバリアントは**無い**（判定は `with_tx` の外側で起き、
  ユースケースに到達しない）。**`rejected` を借りない**——`rejected` は
  「入力を直せば通る」拒否に使っており、同じコードにすると AI が
  「入力の問題か、サーバ都合か」を区別できない。

### 訂正注記3（PR-B 2巡目）: エラー本文の外向きの入口（`public_message`）

`docs/07-mcp-server.md` §3（「`message` は `Display` を写像したもの」）と
§9（「接続文字列を含みうる下位層のエラー本文をそのまま転記しない」）は
**正面から矛盾しており、初版の契約はどちらも安全に満たせなかった**。
公開されていたのは `code()`（安全）と `Display`（生の DB 文字列入り）の2つだけで、
中間が無かった。`kaikei-store` の `sqlstate::map_sqlstate` は
`reason: format!("...: {message}")` として **DB のメッセージをそのまま
`RepoError` に埋めている**（接続文字列・ロール名が混じりうる）。

→ `AppError::public_message()` / `RepoError::public_message()` を追加した。

| バリアント | `public_message()` |
|---|---|
| ドメインのエラー（`Core` / `Policy` / `AlreadyReversed` / `EmptyReverseReason` / `InvalidEntryId` / `Inconsistent` / `Rejected`） | `Display` と同じ（文言はこのリポジトリが書いている。**言い換えない**。`CLAUDE.md` §10） |
| `RepoError::NotFound` / `Unsupported` | `Display` と同じ（`NotFound` の `reason` は app 層が組み立て、仕訳IDの UUID 正準表記を含む。潰すと MC-14 の要件が消える） |
| `RepoError::AppendOnlyViolation` / `Conflict` / `OutOfRange` / `Corrupt` / `Backend` | **正規化**（`reason` を出さず、分類ごとの汎用文言 + 次の手） |

詳細は `Display` 側に残す（サーバのログには `Display`、応答には
`public_message()`）。**却下**: `Display` そのものを安全側に変える案——
診断情報が失われ、SQLSTATE や制約名からしか分からない障害の切り分けが
できなくなる。`map_sqlstate` が `reason` に DB メッセージを含める設計
（D-032 / D-038）は正しく、出口を分けるのが正解である。

### 訂正注記4（PR-B 3巡目）: 「有効な期間」の文言も1箇所に持つ（`kaikei-jp` 側）

`docs/07-mcp-server.md` §2 は `list_tax_categories` について「該当する年度
マスタが存在しない日付では**空配列ではなくエラー**を返し、**有効期間を示す**」と
定めているのに、`TaxRuleSets` の公開メソッドは `from_embedded` / `new` /
`for_date` の3つだけで、**保持している表を列挙できなかった**
（`TaxCategoryTable::range_display` も `pub(super)`）。消費側は
`no method named iter found for struct TaxRuleSets` を実測した。

このままだと各ツールが「2026-01-01〜…のマスタのみ…」という文言を手書きする。
D-072 本文が潰したのと同じ、**同じ表現が複数箇所に散る**状態である。

→ `kaikei-jp` に列挙用の公開 API を足した（`kaikei-core` は無変更）。

| 入口 | 返すもの |
|---|---|
| `TaxRuleSets::iter` / `len` / `is_empty` | 保持しているマスタそのもの |
| `TaxCategoryTable::range_display`（`pub` 化） | 1件分の適用期間（`"2026-01-01 〜 無期限"`） |
| `TaxRuleSets::available_ranges_display` | 全件を適用開始日の昇順に並べた文字列 |
| `TaxRuleSets::require_for_date` | 該当マスタ、無ければ `JpError::NoApplicableTaxRuleSet`（有効期間入り） |

- **`for_date` は `Option` のまま残す。** 該当なしが `None` なのは正常な
  戻り値であり（D-055）、`JpTaxPolicy` はそれを
  `PolicyError::NoApplicableRuleSet` に写像する。`require_for_date` は
  「該当なしをエラーとして扱う呼び出し元」（経路 (c)）専用の入口で、
  両者は**どちらが正しいかではなく宛先が違う**。
- `available_ranges_display` に**ラベル（YAMLのファイルパス）を含めない**。
  線上に出るのは期間であり、読み手が直すのは取引日である。パスまで要る
  診断は `iter()` + `label()` で組み立てる（§9「下位層の生の文字列を
  そのまま応答に載せない」と同じ向き）。

### 訂正注記5（PR-B 3巡目）: プローブが「検査できないこと」を名乗っていた

`crates/kaikei-app/tests/contract_from_downstream.rs` は冒頭 doc で
「ここでのコンパイル可否は `kaikei-mcp` / `kaikei-api` から見た可否と
**同じになる**」と名乗り、検査項目5で「下流が `uuid` や独自の `match` を
足さずに済むことそのものを検査する」と書いていた。**これは成り立たない。**

統合テストのターゲットには、その crate の `[dependencies]` と
`[dev-dependencies]` が**両方**リンクされる。消費側の実測:
プローブに `Uuid::parse_str` を使う行を1行足しても**コンパイルエラーに
ならず全テストが ok** になる。実在する下流（`kaikei-mcp`）は自分の
`Cargo.toml` に `uuid` を書かない限りその行を書けないので、**同じ意味の
検査ではない**。

効いていない検査が「効いている」と名乗るのは、`PROGRESS.md` Phase 1 の
教訓3（誤診を招く記述は実バグと同格）に該当する。**doc を実態に合わせて
正す**（案 (a)）ことにし、次のように書き分けた。

| | 扱い |
|---|---|
| 公開 API だけで下流の写像が書けること | **検査できる**（外部 crate としてリンクされるので `pub` でないものは見えない）。従来どおりプローブが担う |
| `#[non_exhaustive]` の受け皿が下流で効くこと | **検査できる**（定義元 crate 内では検証できない性質。D-072 訂正注記1 の委譲先） |
| 下流の `Cargo.toml` に依存が増えないこと | **検査できない。** PR-D（`kaikei-mcp` 新設）で CI に置く（`docs/07-mcp-server.md` §4 の申し送り、テストケース MC-30） |

案 (b)（検査を本当に効かせる）は**この PR では採らない**。効かせるには
`kaikei-app` だけに依存する専用 crate を新設することになるが、それは
`kaikei-mcp` の劣化コピーであり、PR-D で本物が来た瞬間に「手で維持する
複製」（D-047）になる。代わりに、**プローブ自身が余計な crate に手を
伸ばしていないこと**を各プローブが自分のソースを読んで検査する
（`the_probe_itself_does_not_reach_for_crates_outside_kaikei_*`）。
これは自制の固定であって下流の依存の検査ではない、と doc に明記した。

同じ理由で `kaikei-jp` 側にもプローブを置いた
（`crates/kaikei-jp/tests/contract_from_downstream.rs`）。経路 (c) と
`tags` 変換は `kaikei-app` を通らないため、app 側のプローブでは1行も
踏めていなかった。

---

## D-073 書き込み系ユースケースの戻り値を出力構造体にし、`PolicyNote` は `post_entry` だけが持つ

**決定**: `post_entry::execute` / `reverse_entry::execute` の戻り値を
`JournalEntry` 単体から専用の出力構造体に変える。

```rust
pub struct PostEntryOutput { pub entry: JournalEntry, pub notes: Vec<PolicyNote> }
pub struct ReverseEntryOutput { pub entry: JournalEntry }   // notes を持たない
```

- `post_entry::execute` は `tax.derive_tax_lines(...)?.lines` として `notes` を
  捨てていた。これを `notes` ごと運ぶ（D-070 の決定3、Phase 2 の申し送り
  「`PolicyNote` が永続化されない」への回答）。
- **`ReverseEntryOutput` には `notes` を置かない。** `reverse_entry` は
  `TaxPolicy` を引数に取らず（明細は貸借を反転して複製するだけで、税額行を
  再導出すると二重計上になる）、注記の発生経路そのものが存在しない。
  MCP の応答でも `policy_notes` は**キーごと出さない**。
- `#[non_exhaustive]` は付けない（構築するのは当該ユースケースのみ。
  読み取り側はフィールド追加で壊れない）。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `(JournalEntry, Vec<PolicyNote>)` のタプルを返す | 呼び出し側が `.0` / `.1` で参照することになり、どちらが何かがコード上に現れない。フィールドを足すときにタプルの要素数が変わり、全呼び出し元が壊れる |
| `notes` を `JournalEntry` に持たせる | `kaikei-core` の変更（人間の承認が必要。`CLAUDE.md` §1）であり、かつ `PolicyNote` は `kaikei-policy` の型なので core → policy の逆依存になる。そもそも注記は仕訳の不変条件ではなく「この記帳をしたときに policy が言い添えたこと」であり、仕訳集約の一部ではない |
| `PolicyNote` を仕訳のタグ（`TagSet`）に入れて永続化する | `CLAUDE.md` §4「`TagSet` はゴミ箱ではない」。タグキーの登録（`tags.yaml`）が要り、集計軸でもない自由文をタグに入れることになる。注記の保存先は `audit_log.output`（D-070 の決定4） |
| `ReverseEntryOutput` にも常に空の `notes` を置いて post と形を揃える | 空配列は「policy を通したが注記が無かった」と区別できない。`PROGRESS.md` Phase 1 の教訓3「MCP 経由で AI が自己修正する前提のシステムでは、誤診は誤値と同じ実害を持つ」。後から足すのは非破壊なので、必要になってから足せばよい |
| `reverse_entry` の戻り値は `JournalEntry` のままにする（構造体で包まない） | 呼び出しの形が post と揃わず、MCP 層で2種類の書き方が要る。将来フィールドを足すときに戻り値の型そのものが変わる（今回と同じ全呼び出し元の修正がもう一度発生する）。★契約凍結点★である PR-B で1回だけ払うべきコスト |

**理由**: 非適格の経過措置（`deduction_ratio < 1`）や簡易課税は、**税額計算に
反映されず注記にしか現れない**（D-059）。`notes` を捨てると、AI も監査ログも
「控除割合の制限があった」ことを知る手段が無くなる。

**トレードオフ**: 既存の呼び出し側（`kaikei-app` の単体テスト、
`kaikei-store/tests/e2e_usecase.rs`、`kaikei-e2e/tests/e2e_jp.rs`）が全て壊れ、
本 PR で修正した。`kaikei-store` 側のヘルパは検証対象が永続化なので
`.map(|output| output.entry)` で `entry` だけを取り出している。
`post_entry` の `notes` は `auto_tax_lines: false` のとき常に空になるため、
**空配列は「注記が無い」を意味しない**（呼び出し元は自分が渡した
`auto_tax_lines` と併せて読む必要がある）。この非対称は doc に明記した。

### 訂正注記1（PR-B 2巡目）: 失敗経路でも `PolicyNote` を運ぶ

初版は `Result<PostEntryOutput, AppError>` だったため、**`notes` を運べるのは
成功時だけ**だった。1巡目の消費側が実測で示したとおり、
`derive_tax_lines` が成功して
`PolicyNote{Info, "税込経理の設定のため税額行を生成していません"}` を返しても、
直後の `JournalEntry::new` が `Unbalanced` で落ちると `notes` はスコープごと
捨てられる。

**注記が最も要るのは失敗時である。** AI に届くのが「貸借不一致: 借方 110,000 /
貸方 100,000」だけだと、原因（税込経理の設定だから税額行が生成されなかった）が
分からず、**金額を書き換える**という誤った修正に進む。
`docs/07-mcp-server.md` §1③「エラーは自己修正可能な形で返す」が空文になる。

→ 専用のエラー型を導入した。

```rust
pub struct PostEntryFailure { pub error: AppError, pub notes: Vec<PolicyNote> }
// execute / preview の戻り値は Result<_, PostEntryFailure>
```

- `code()` / `public_message()` は `error` へ委譲する。
- `From<PostEntryFailure> for AppError` を用意する（`AppError` だけ欲しい
  呼び出し元向け。**剥がすと `notes` は落ちる**ことを doc に明記）。
- 既存の呼び出し側（`kaikei-app` のテスト、`kaikei-store` /
  `kaikei-e2e` の pg-tests、`tests/contract_from_downstream.rs`）は全て修正した。

| 却下した候補 | 却下理由 |
|---|---|
| `AppError` 自身に `notes` を持たせる | `AppError` は試算表・訂正・永続化のエラーも表す。`PolicyNote` は記帳経路にしか存在しない概念で、全バリアントに空の `notes` が付く形になる（D-073 本文が `ReverseEntryOutput` について下した判断と同型: 常に空のフィールドは「policy を通したが注記が無かった」と区別できない） |
| `notes` を `&mut Vec<PolicyNote>` の出力引数で受け取る | Rust で慣用的でなく、**渡し忘れても何も起きない**（黙って注記が消える）。型で強制されない設計に戻ることになる |
| 成功・失敗を1つの enum（`PostEntryOutcome`）にして `Result` を使わない | `with_tx` が `Err` を見て rollback するので、ドメインの失敗が `Ok` になると**失敗した記帳が commit される**。会計データで最も避けるべき事故 |

**`with_tx` の扱い**: `with_tx` は `Result<T, AppError>` 固定のままにし、
エラー型について総称な `with_tx_err`（`E: From<RepoError>`）を**同じ実装本体**に
足した。`with_tx` は `with_tx_err::<_, _, AppError, _>` を呼ぶだけである。
`with_tx` 自体を総称化する案は実測で却下した——`Ok(..)` しか書かない
読み取り専用のクロージャで `E` が推論できなくなり、`kaikei-store` /
`kaikei-e2e` の pg-tests が8箇所コンパイルエラーになった。
**最も多い書き方が最も短く書ける**方を既定にする。

### 訂正注記2（PR-B 2巡目）: dry-run（`preview`）を `kaikei-app` に置いた

`docs/07-mcp-server.md` §3 の `hint.suggested_lines` を組み立てるには
「記帳せずに `derive_tax_lines` の結果を得る」入口が要る。初版はこれを
「`kaikei-mcp` を新設する PR で入れる」と先送りしていた。

1巡目の消費側の指摘: 契約が「書けない」ではなく
**「間違った書き方が通ってしまう」**形で凍結されていた。MCP 層で `with_tx` を
開き `load_posting_context` を呼び `TaxContext` を自前で組み立て `sum_money` で
検算する——同 §4「MCP はビジネスロジックを書かない」に真っ向から反するのに、
**そのままコンパイルが通りテストも通った**。★契約凍結点★でこれを残すのは、
後続 PR の実装者に誤った書き方を許可するのと同じである。

→ `usecase::post_entry::preview` を追加した。

- **記帳しない**: `tx.next_entry_no` も `tx.insert_entry` も呼ばない。
  `id_gen` を引数に取らない（`IdGenerator` の実装によっては呼ぶこと自体が
  状態を進める）。
- **`execute` と同じ手順を通る**: 手順1〜3を `prepare`、手順5（`JournalEntry::new`
  による検証）を `build_entry` という**同じ private 関数**に切り出し、
  両者がそれを呼ぶ。手順を書き写した箇所が無いので、検証の順序が
  乖離しえない。差分は「採番と INSERT をするかどうか」だけであり、
  それがまさに dry-run の定義である。
- 実行時にも突き合わせる: `preview_and_execute_agree_on_the_final_lines`
  （preview の明細をそのまま post すると同じ仕訳になる）と
  `preview_and_execute_agree_on_rejections`（同じ入力に対して同じコード・同じ
  本文のエラーになる。4ケース）。

**置き場**: 別ファイル（`usecase/preview_entry.rs`）にせず
`usecase/post_entry.rs` に同居させた。`CLAUDE.md` §6 の
「1ユースケース = 1ファイル = 1関数」に対しては、**これは別のユースケースでは
なく同じユースケースの dry-run モード**である、という判断。ファイルを分けると
`prepare` / `build_entry` を `pub(crate)` に緩めることになり、
§6 が「集約は1モジュールに収める」で警告している状態そのもの
（＝手順が2箇所から触れるようになり、乖離の余地が生まれる）を作ってしまう。

**戻り値**: `PreviewEntryOutput { lines, notes, debit_total, credit_total }`。
`JournalEntry` は返さない——記帳していない仕訳に仕訳ID・仕訳番号は存在しない。
内部では検証のために仮の値（`EntryId::new(0)` / `EntryNumber::new(0)`）で
`JournalEntry` を組み立てるが、それが関数外へ出ることは無い
（`JournalEntry::new` はこの2つを検証項目に含めない）。

---

## D-074 入力検証と帳簿通貨は `kaikei-app` で必須にする（MCP 層に委ねない）

**決定**: 次の2つを `kaikei-app` の責務として確定する。

1. **訂正理由（`reverse_entry` の `reason`）の非空検証はユースケース層で行う。**
   `reverse_entry::execute` が I/O より前に `input.reason.trim().is_empty()` を
   見て `AppError::EmptyReverseReason`（コード `empty_reverse_reason`）を返す。
   判定は `str::trim` に任せるので全角スペース（U+3000）のみの理由も拒否される。
   **受理した理由は加工せず入力のまま保存する**（トリムした値を保存しない）。
2. **帳簿通貨は `BookSettings::book_currency`（`kaikei_core::Currency`）として
   必須で保持する。** `Option` にせず `Default` も実装しない。
   コード文字列からの解決は `kaikei_app::currency::currency_from_code` を通し、
   **未知のコードは桁数を推測せずエラー**にする方針を迂回しない。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| 訂正理由の非空検証を MCP 層で行う（`docs/07-mcp-server.md` の初版） | MCP は**唯一の呼び出し元ではない**。`ROADMAP.md` は Phase 4 で `kaikei-api`（HTTP）を予定しており、CLI も想定されている。検証を presentation 層に置くと、呼び出し口が増えるたびに同じ検証を書き足す運用になり、1つ忘れた瞬間に**理由が空の赤伝が帳簿に入る**（下位層はどこも空文字を通す）。訂正理由は「訂正削除の履歴」に対応する記録であり、空で通ることに実害がある |
| `kaikei-core` の `JournalEntry::reverse` で弾く | **`kaikei-core` は凍結層**（`CLAUDE.md` §1・§9）。変更には人間の承認が要る。またユースケース層で弾けば `JournalEntry::reverse` に到達しないので、core を触らずに同じ結果が得られる |
| DB の `CHECK (btrim(reverse_reason) <> '')` で弾く | 摘要（`description`）には同種の CHECK があるのに `reverse_reason` には無い、という非対称は実在する。ただし DB で弾くと**採番を消費してから**失敗し、エラーが SQLSTATE 経由の `RepoError` になって「次の手が分かる文言」から遠ざかる。多層防御として後から足す価値はあるが、それは別の作業であり、app 層の検証を省く理由にはならない |
| `reason` を非空を型で保証する newtype にする | `kaikei_core::JournalEntry::reverse` が `String` を受け取る以上、境界で必ず剥がすことになり、検証の置き場が「newtype の構築時」と「core に渡す直前」の2箇所に増える。凍結層を変えられない現状では利得が無い |
| 帳簿通貨を `JpSettings` に持たせる | 帳簿通貨は日本の税制固有の設定ではない。`kaikei-jp` を別の国の実装に差し替えても必要になる、帳簿全体の設定である（`BookSettings` の位置づけそのもの） |
| `book_currency: Option<Currency>` にして `None` なら JPY を使う | D-057 が `is_taxable_business` について下した判断と同型。`Currency` は**コードと小数桁数の組**であり、桁数を1つ間違えると金額が100倍ずれて記帳される（`CLAUDE.md` §8）。「指定し忘れ＝合理的な既定値」を許すと、事故が起きても構築時にエラーにならず気づけない |
| `BookSettings` に `Default` を実装する（テストを書きやすくする） | 上と同じ。テストの都合で本番の事故経路を作らない。テスト用の既定値は `test_support::settings()` のような**テスト専用のヘルパ**に置けば足りる |

**理由**: どちらも「上位層が既定値を補う／検証を肩代わりする」形にすると、
呼び出し口が増えた瞬間に穴になる種類の設定である。★契約凍結点★である
PR-B で app 層に寄せる。

**トレードオフ**: `BookSettings` にフィールドが増えたため、構築している
全箇所（`kaikei-app` の `test_support`、`kaikei-store` / `kaikei-e2e` の
pg-tests）を修正した。`currency_from_code` のホワイトリストは現状 `JPY` /
`USD` の2つだけなので、他の通貨で帳簿を付けるにはまずこの関数に追加する
必要がある（推測させないことの代償として受け入れる）。

### 訂正注記1（PR-B 2巡目）: `AlreadyReversed` に既存赤伝の `EntryId` を持たせた

`ports::JournalRepo::find_reversal_of` は `Option<(EntryId, EntryNumber)>` を
返しているのに、`reverse_entry::execute` が `if let Some((_, reversal_no))` で
**`EntryId` を捨てていた**。呼び出し元（AI）が仕訳を指すのは UUID
（`original_id`）なのに、返るのは会計年度内の通し番号だけになる。
`JournalRepo` には番号から仕訳を引く経路が無い（`find_entry(EntryId)` のみ）ので、
AI は提示された赤伝を開くことすらできない。

→ `AppError::AlreadyReversed { entry_no, reversal_no, reversal_id }` にし、
メッセージにも `entry_id_to_uuid_string` を通した UUID 正準表記を含めた。
番号（人間が帳簿を目で追うための表示）は**両方残す**（どちらかに寄せない）。

### 訂正注記2（PR-B 2巡目）: 帳簿通貨は試算表の応答にも必須である

`BookSettings::book_currency` を「金額入力の既定」としてだけ位置づけていたが、
**出力側でも必要**だった。`TrialBalanceView` は行から通貨を推論していたため、
0行の期間では `totals()` が `Ok(None)` を返し、`get_trial_balance` の応答で
**通貨を名乗れず合計も出せなかった**（「集計対象の通貨が単一であること」を
求める D-042 も、行が無いと検査できない）。

→ `TrialBalanceView::new(rows, currency)` が帳簿通貨を必須で受け取る形にした。

- `TrialBalanceView::currency()` が 0 行でも通貨を返す。
- `totals()` の戻り値から `Option` が消え、0行なら `(0, 0)` を返す。
- 行の通貨が帳簿通貨と食い違えば `CoreError::CurrencyMismatch`
  （D-042 の実効的な検査になった）。
- `usecase::report::execute` は `&BookSettings` を受け取るようになった
  （書き込み系ユースケースと引数の形を揃えた）。

`kaikei-app` が持つ DTO なので `kaikei-core` は無変更
（`kaikei_core::TrialBalance` は外から構築できない。D-031）。

### 訂正注記3（PR-B 2巡目）: 会計年度の区切り規則も文字列から解決できる

`BookSettings` の `book_currency` には `currency::currency_from_code` があるのに、
同じ構造体の `fiscal_year_rule` には文字列からの解決手段が**片方向すら
無かった**。合成ルート（`kaikei-mcp` の `config.rs`）は設定ファイルから
両方を組み立てるので、片方だけ下流の手書き `match` になる。

→ `wire::fiscal_year_rule_code` / `fiscal_year_rule_from_code` を追加した
（D-072 訂正注記2 の表を参照）。`currency_from_code` と同じく
**未知の値は既定値へフォールバックせずエラー**にする。

### 訂正注記4（PR-B 3巡目）: 線上の `tags` → `TagSet` の変換は `kaikei-jp` に置く

訂正注記1〜3 と同じ「入力検証をどの層が持つか」の話が、`tags` に残っていた。

`docs/07-mcp-server.md` §3 の `post_journal_entry` が受け取る `tags` は
**文字列 → 文字列**のマップ（`{"tax_category": "SALES_10"}`）である。一方
`kaikei_core::TagValue` は型付き（`Code` / `Text` / `Decimal` / `Date`）で、
平文から作るにはキーごとの `TagValueType` が要る。消費側が実測した
コンパイルエラーは2つ:

- `no method named value_type found for reference &TagSchema`
  （公開 API は `new` / `empty` / `validate` / `is_aggregatable` の4つだけで、
  **組み立てたスキーマから型を引き戻せない**）
- `cannot find module or crate rust_decimal`
  （`TagValue::Decimal` を作るには小数の crate が要るが、下流は持っていない）

初版の docs/07 は「これは MCP の DTO 変換であり `kaikei-app` の契約には
現れない」としてスコープ外に置いていたが、**書き場所は `kaikei-mcp` ではなく
`kaikei-jp` だった**。型を知っているのはタグスキーマ（`tags.yaml`）を読む層で
あり、`kaikei-app` は `kaikei-jp` に依存できない（`CLAUDE.md` §1）。
MCP 層に置けば `kaikei-api`（Phase 4）が同じ変換を書き直すことになり、
D-072 本文および本決定の「呼び出し口が増えるたびに検証を書き足す運用に
なる」がそのまま当てはまる。

→ `kaikei_jp::tags::TagCatalog` を追加した（**`kaikei-core` は無変更**。
docs/07 §3 が挙げた3案のうち (a)）。

| 入口 | 役割 |
|---|---|
| `TagCatalog::from_embedded` / `from_path` / `from_yaml_str` | ロード（`TagSchema` + キーごとの `TagDef`） |
| `TagCatalog::schema()` / `into_schema()` | `kaikei-core` / `kaikei-app` に渡す `&TagSchema` |
| `TagCatalog::defs()` / `def()` / `value_type()` | キーからの逆引き |
| `TagCatalog::parse_tag_set()` / `parse_value()` | 線上の文字列 → `TagSet` |
| `tags::tag_value_to_string()` | 応答に載せる逆向き |

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| (b) `kaikei-core` に `TagSchema::value_type` を足す | **core の変更は人間の承認事項**（`CLAUDE.md` §1・§9）。同じ結果が凍結層を触らずに得られる以上、承認を求める理由が無い |
| (c) 線上でも D-035 の `{"t":..,"v":..}` 形式を使う | 型を送り手（AI）に書かせることになる。型はスキーマが持っており、送り手が書いた `"t"` とスキーマが食い違ったらどちらを信じるかという論点が新たに増える。冗長でもある |
| 変換を `kaikei-mcp` の DTO 層に置く（初版 docs/07） | 上記のとおり `kaikei-api` が同じ変換を再実装する。`JpError` を返す点でも経路 (c) と同型で、置き場を分ける理由が無い |
| 未登録キーを黙って捨てる／`Text` として通す | `CLAUDE.md` §4「`TagSet` はゴミ箱ではない」。`tax_cat` のような綴り違いが**無言で消える**と、AI は「税区分を付けた」と思ったまま `tax_category` 無しの明細を作る（`required_for` の検証は通ってしまう組み合わせがある） |
| 入力に同じキーが2回来たら後勝ちにする | `tags.yaml` の重複キーを拒否している（D-062）のと同じ理由。送り手が意図した値と保存される値が食い違う。**ただし MCP 経由では現にこの後勝ちが起きている**（D-085 の訂正注記。重複は `rmcp` の stdio トランスポートが JSON をパースする時点で畳み込まれ、`TagCatalog::parse_tag_set` に届く時点で既に1件になっている）。この却下は「MCP 層で後勝ちを**実装しない**」という意味であって、「重複が拒否される」という保証ではない |
| 小数を `Decimal::from_str`（`kaikei-core` の `parse_decimal` と同じ）で読む | 表現できない桁数を**黙って丸める**。`business_ratio` は税務調査時の根拠として記録するもの（`tags.yaml`）で、入口で丸めた値が保存されるのは事故。`from_str_exact` で拒否し、`CLAUDE.md` §11 に従って書式を示す |

**値の文字列表現は D-035 の JSONB の `"v"` と同じ規約にする**（`Decimal` は
`Display`/`FromStr` を経由した文字列、`Date` は ISO 8601）。線上とDBで別の
書き方を発明しないため。number にしない理由は D-013 と同じ。

**トレードオフ**: `compose::Composition` のフィールドを
`tag_schema: TagSchema` から `tag_catalog: TagCatalog` に変えた
（`kaikei-e2e` の呼び出しは `composition.tag_catalog.schema()` になった）。
2つのフィールドで両方を持たせる案は、片方だけ差し替えた組み合わせが
作れてしまうため採らない。`TagCatalog` は `Vec<(TagKey, TagDef)>` を
保持し、`TagSchema` はその同じ `Vec` から構築時に1度だけ組み立てる
（手で維持する2つの一覧にはしない。`PROGRESS.md` Phase 1 の教訓6）。

**残る前工事**: `JpError` の新しい4バリアント
（`UnregisteredTagKey` / `InvalidTagValue` / `DuplicateTagKeyInInput` /
`NoApplicableTaxRuleSet`）に対する分類コードは、`JpError` 用の対応表ごと
`kaikei-mcp` 側に置く（D-072 トレードオフの項、`docs/07-mcp-server.md` §6）。
→ **PR-D で完了**（D-080）。

---

## D-075 audit_log は帳簿とは別のコネクションで2回書き、append-only を帳簿と同じ4点セットで守る

**決定**: `audit_log` を `crates/kaikei-store/migrations/0009_audit_log.sql` で新設し、
D-070 が定めた「開始レコード → 操作 → 結果レコード」の2回書きを、
**帳簿のトランザクション（`PgTx`）を経由しない別コネクション**で行う。

| 層 | 実体 |
|---|---|
| テーブル・トリガ・権限 | `migrations/0009_audit_log.sql` |
| ポート（trait） | `kaikei_app::ports::AuditSink`（`&self`。`Store` / `TxScope` と無関係） |
| 記録する値 | `kaikei_app::audit`（`RequestId` / `AuditCall` / `AuditStart` / `AuditResult` / `AuditOutcome`） |
| PostgreSQL 実装 | `kaikei_store::audit::PgAuditSink`（`execute(&self.pool)`） |

確定した細目:

1. **主キーは `BIGINT GENERATED ALWAYS AS IDENTITY`。`BIGSERIAL` を使わない。**
   `BIGSERIAL` はテーブル権限とは別にシーケンスの `USAGE` を要求し、付与漏れの
   INSERT 失敗は SQLSTATE `42501` になる。`sqlstate.rs` はこれを
   `AppendOnlyViolation` に写像するため、「監査ログが書けない」事象に対して
   「訂正は逆仕訳で行ってください」という**完全に誤った案内**が出る。
2. **append-only は4点セット**（`GRANT`/`REVOKE`・行トリガ・TRUNCATE の
   STATEMENT トリガ・専用 ERRCODE）。トリガ関数は `reject_mutation()` を
   **流用せず** `reject_audit_log_mutation()` を新設し、ERRCODE は
   `P0010`/`P0011` の次の **`P0012`** を割り当てた。理由は 1 と同じで、
   帳簿の文言（逆仕訳）が監査ログに対しては的外れだからである（D-038 の再演を防ぐ）。
3. **`P0012` の写像先は `RepoError::Backend`。**`AppendOnlyViolation` に寄せない。
   専用バリアントを `RepoError` に足すことも却下した（`RepoError` は
   `#[non_exhaustive]` ではなく、バリアント追加は下流の網羅 `match` を壊す
   破壊的変更。一方 `P0012` に到達するのは「アプリが `audit_log` を
   UPDATE/DELETE しようとした」＝実装のバグの場合だけで、
   `Backend`（「入力の問題ではない。ログを添えて管理者へ」）が意味的に最も近い）。
4. **`status` と `error_code` の対応を型と CHECK の二重で守る。**
   `AuditOutcome` は `Succeeded { output_json }` / `Failed { error_code,
   public_message }` の2バリアントで、「成功なのに `error_code` がある」
   「結果レコードに `'started'` を書く」を構築できない。DB 側にも
   `CHECK ((status = 'error') = (error_code IS NOT NULL))` を置く。
5. **`entry_id` に外部キーを張らない。** rollback された操作の仕訳IDは
   `journal_entries` に存在しえない。**存在しないことを記録できること自体に
   価値がある**（D-070 の目的そのもの）。
6. **`occurred_at` に `DEFAULT now()` を付けない。** `Clock`（`AppClock`）から
   取得した値を明示的に渡す（`CLAUDE.md` §7）。DEFAULT があるとテストで
   時刻を固定できず、渡し忘れも検出できない。
7. **入力を理由に fail-closed へ落とさない。格納できない内容は無害化してでも
   記録を残す。** `input` に `{"description":"A\u0000B"}`（JSON としても
   JSON-RPC としても正当）が来ると、`jsonb` は U+0000 を格納できず SQLSTATE
   `22P05` で拒否する。そのまま fail-closed にすると (1) 正常な監査ログを
   「使えない」と誤診し、(2) 同じ入力で再試行する限り永久に成功せず、
   (3) AI には原因（自分が送った1文字）が分からない、という詰みになる
   （`CLAUDE.md` §11。D-038 が潰した誤診クラスの再演）。`PgAuditSink` は
   U+0000 を U+FFFD に置換し、置換したときは `_audit.verbatim = false` と
   置換数を添えた封筒（`{"_audit": {...}, "value": ...}`）に包む——
   **原文と違うものを黙って記録するのは監査ログとして不誠実**なので、
   「記録は原文どおり」と読める形にはしない。置換しても **DB がその文を
   拒否した**場合は `input`/`output` を「記録できなかった旨のプレースホルダ」に
   差し替えて1度だけやり直し、**誰がいつ何のツールを呼んだかは必ず残す**。
   JSON として解釈できない文字列（presentation 層のバグ）も同じ扱いにする。
   結果として fail-closed に落ちるのは「sink が本当に使えない」場合だけになる。
   なお `actor`/`tool` の `CHECK btrim(...) <> ''` 違反（23514 → `Corrupt`）は
   呼び出し側のバグなので fail-closed のままだが、その場合の応答は
   「時間をおいて再試行」ではなく**「同じ内容で再試行しても成功しません」**に
   分岐させた（再試行しても直らない経路で再試行を促さない）。
8. **`42501` を `PgAuditSink` 側で包み直す。** `sqlstate.rs` は関与テーブルを
   見ないため、`REVOKE INSERT ON audit_log FROM kaikei_app`（`docs/07` §9 が
   fail-closed の代表シナリオとして名指ししている状況）は帳簿と同じ
   `RepoError::AppendOnlyViolation` になり、その `public_message()` は
   「訂正は逆仕訳（`reverse_journal_entry`）で行ってください」を返す。
   `AuditLogUnavailable::public_message()` が握り潰すので応答には出ないが、
   **`cause` は pub フィールド**であり、MCP 層が診断のつもりで
   `cause.public_message()` を出せば D-038 と同型の誤案内が復活する。
   `kaikei_store::audit` でこの経路の `42501` を `RepoError::Backend` に
   包み直し、**到達可能な位置に「逆仕訳で訂正を」を残さない**。
   共通写像（`sqlstate.rs`）は帳簿側に波及するので触らない。
9. **やり直すのは「DB が SQLSTATE 付きで拒否した」場合だけ**
   （`retryable_payload_rejection`）。当初は `input`/`output` が載っていれば
   何の失敗でもやり直していたが、これだと (a) 1回目が実はコミット済みで
   応答だけ失われた場合に同じ `request_id` の `started` 行が2行でき、
   (b) コミット前に落ちた場合は生き残る行が `reason:"not_storable_as_jsonb",
   sqlstate:null` という**事実と違う理由**を名乗って実入力を捨てる。
   監査ログは内容の忠実さが本体なのでどちらも許容できない。DB がエラー応答を
   返した場合はその文がアボートしたことが確定しているので重複せず、
   SQLSTATE を添えれば理由も名乗れる。クラス 22（data exception）までは
   絞っていない——DB が payload を拒否しうる経路は `22P05` だけではなく、
   差し替えても直らない失敗（`42501` 等）は2回目も同じ SQLSTATE で失敗して
   結論が変わらないためである。SQLSTATE を取れない失敗では差し替えないので、
   `sqlstate: null` の `not_storable_as_jsonb` は構造的に発生しない
   （`unstorable_placeholder` は `&str` を要求する）。
10. **`tool` / `actor`（TEXT 列）はこの無害化を通らない。**
    上記 7 が閉じたのは JSONB 列（`input`/`output`）だけであり、差し替えの
    やり直しもその2列しか触らない。PR-C の時点では両方ともサーバ側の定数
    なので実害は無いが、`kaikei-mcp` では `tools/call` の `name` が
    **クライアント（AI）由来**になる。受け取った名前をそのまま
    `AuditCall.tool` に載せる実装にすると、`tool` に U+0000 を1文字入れられた
    時点で開始レコードが書けず fail-closed になり、**上記 7 で塞いだ
    「入力1文字で操作が実行できない」が TEXT 側で再発する**。
    そこで不変条件として「**`AuditCall.tool` にはレジストリに登録済みの
    ツール名以外を載せない。未知の名前は audit に載せる前に弾く**」を
    `docs/07-mcp-server.md` §9 と `kaikei_app::audit`（`AuditCall::tool` /
    `with_audit` の doc）に明記した。**実装で満たすのは PR-F
    （`kaikei-mcp` の結線）**であり、PR-C が置いたのは記述だけである
    （`kaikei-mcp` がまだ無いので機械的な検査は置けない）。
    TEXT 列側で同じ無害化を行う案は採らなかった——`tool` は監査の索引であり、
    U+FFFD に置換された名前が並ぶより「知らないツール名は実行も記録もしない」
    方が監査として正しい（`actor` も定数のみ）。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `TxOps` に audit 用メソッドを生やす（`tx.record_audit(..)`） | **必ず帳簿と同一トランザクションになる。** `with_tx` は `Err` で必ず rollback するため、失敗した操作の記録が構造的に消える。最も知りたい記録だけが残らない（D-070） |
| 監査ログ専用の `PgPool` をもう1本張る | 接続数が倍になるだけで、分離の実体は「別プール」ではなく「トランザクションを経由しないこと」。単一ユーザー・自己ホスト前提（D-015）の規模では利点が無い |
| `P0012` を `AppendOnlyViolation` に写像する（写像表を増やさない） | 上記 2・3 と同じ。エラーメッセージが「訂正は逆仕訳で」になり、監査ログに対して誤った対処法を案内する |
| `RepoError` に `AuditLogViolation` バリアントを足す | `#[non_exhaustive]` ではないため下流の網羅 `match` を壊す。得られるのは「実装のバグでしか到達しない経路」の分類精度だけで、割に合わない |
| `input` / `output` を `TEXT` 列にする | JSONB で保持できる検索性（`->>` による絞り込み）を捨てることになる。JSON テキストの受け渡し自体は変えない（`kaikei-app` は serde を持てないので、ポートは JSON テキストで渡し、パースは store 側で行う） |
| 格納できない `input` を `RepoError::Corrupt` にして fail-closed のままにする（当初の実装） | **監査ログが正常なのに「使えない」と誤診し、同じ入力では永久に成功しない。** 上記 7 の理由。D-070 の趣旨（AI が何をしようとしたかを残す）に沿うのは「無害化してでも記録する」側である |
| U+0000 を黙って落として記録する | 原文と違うものを「原文どおり」と読める形で残すのは監査ログとして不誠実。置換したことが記録から分かる形（`_audit.verbatim = false`）にする |
| 置換の有無を新しい列（`input_sanitized BOOLEAN`）で持つ | 注記を JSON の中に持てば、`input` を読む側が同じ値の中で完結して判定できる。列を足すと「列を見ずに `input` だけ読む」経路が残り、黙って加工されたのと同じことになる |
| `error_code` を CHECK で縛らず運用で守る | 「`status='error'` なのに分類コードが無い行」は、後から読む側に「失敗したが理由不明」としか見えない。型（`AuditOutcome`）で塞げるものを運用に落とさない |
| `sqlstate.rs` の `42501` の写像自体を「テーブル名を見て分岐」に変える | 共通写像は帳簿側（`journal_entries` への UPDATE 等）が依存している。監査ログの都合で共通経路を触ると波及範囲が読めない。包み直しは呼び出し側（`PgAuditSink`）に閉じる |

**理由**: 「append-only。」と書くだけでは守られない、という帳簿本体で得た教訓
（D-006）が監査ログにもそのまま当てはまる。とくに ERRCODE を分けることは
`docs/07-mcp-server.md` §9 が明示的に要求しており、これを怠ると D-038 で
潰したはずの「誤診クラス」の欠陥を、監査ログという**事故調査に使う経路**で
再生産することになる。

**トレードオフ**: 1リクエストで DB 接続を（帳簿・監査ログの）計3回
取っては返す。`with_audit` は `record_start` → `with_tx` → `record_result`
の順なので、**同時に2本保持する瞬間は無い**（保持しているのは常に1本）。
同時保持が起きるのは「`with_tx` の内側から監査ログを書く」場合だけで、
それはこの決定が禁じている形である——観測したらプール枯渇より先に退行を疑う。
`connect_app` の `max_connections` は 10 で、単一ユーザー前提では
問題にならないが、`kaikei-api`（Phase 4）で同時実行が現実になったら
接続の取得回数（1リクエストあたり3回）は再評価が要る。

また、DB が拒否した INSERT を1度だけやり直すため、**その入力に限り**
監査ログへの往復が1回増える（U+0000 を含む入力と、JSON として壊れた入力は
往復1回で済む——置換とパースは INSERT 前に終わっているため、やり直しに
入るのは想定外の理由で DB に拒否された場合だけ）。転送レベルの失敗では
やり直さないので、接続断のたびに往復が倍になることは無い。

---

## D-076 fail-closed / fail-open の手順は `kaikei_app::audit::with_audit` に閉じる

**決定**: 「開始レコードを書く → 操作 → 結果レコードを書く」という手順と、
その失敗時の扱い（開始＝fail-closed、結果＝fail-open）を
**`kaikei-app` の1つの関数 `with_audit` に閉じる**。
`kaikei-mcp` の各ツールはこの関数を呼ぶだけで、順序も失敗時の分岐も書かない。

```rust
pub async fn with_audit<T, E, Op, Fut, D>(
    sink: &dyn AuditSink,
    clock: &dyn AppClock,
    call: &AuditCall<'_>,
    operation: Op,          // FnOnce() -> Fut。開始レコードが書けなければ呼ばれない
    describe: D,            // FnOnce(&T) -> AuditSuccess。成功時の entry_id と出力JSON
) -> Result<AuditedCall<T, E>, AuditLogUnavailable>
```

- 開始レコードが書けなければ `Err(AuditLogUnavailable)` を返し、
  **`operation` を一度も呼ばない**（future すら構築されない）。
  コードは `codes::AUDIT_LOG_UNAVAILABLE`、本文は
  「帳簿は変更されていません」まで含める。
- 結果レコードが書けなければ `AuditResultNotRecorded` を載せ、操作の結果は
  そのまま返す。警告文は**再実行を促さない**（二重計上を招く）。
- **警告を握り潰せない形にする。** `AuditedCall` はフィールドを private に
  し、`#[must_use]` を付け、結果を取り出す口を2つに絞った。
  `into_result(&mut notes)`（警告があれば注記に本文を積んでから結果を返す。
  **既定**）と `into_parts_unchecked()`（結果と警告を同時に返す。**逃げ道**）である。
  後者を `into_parts()` から改名したのは、`let (result, _) = ...` が
  **警告ゼロで通る**ため（`AuditResultNotRecorded` の `#[must_use]` は
  タプル分解に効かない）。既定経路（`into_result`）を塞いだ以上この口は
  意図的な逃げ道だが、**名前が逃げ道だと分かる形**にしておかないと
  「2つある取り出し口のうち短い方」として選ばれる。`_unchecked` を使う側は
  警告を自分で応答へ載せる責任を負い、使っている箇所は grep で全て出る。
  `pub result` / `pub warning` のままだと `audited.result` だけを読んで
  警告を捨てるコードが書け、しかも**正常系のテストでは検出できない**
  ——これは D-076 自身が fail-closed について挙げた却下理由と同じ性質であり、
  11ツールに散る前（PR-C）に閉じた。`#[must_use]` だけでは
  「`audited.result` を使って `warning` を捨てる」書き方を止められないため、
  private + `into_*` の組み合わせにしている。
- `AuditLogUnavailable` は `AppError` のバリアントに**しない**。判定は
  `with_tx` の外側で起き、ユースケースには到達しないため
  （`docs/07-mcp-server.md` §6 の「`AppError` のバリアントを持たないコード」）。
- 操作のエラー型は `AuditableError`（`audit_error_code` /
  `audit_public_message`）で抽象化し、`AppError` と
  `usecase::post_entry::PostEntryFailure` の両方を同じ手順で扱う。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| 各ツール（Phase 3 で11個）が `sink.record_start` → `with_tx` → `sink.record_result` を手で書く | **同じ規律が11箇所に複製される。** とくに「開始レコードの失敗時に操作を実行しない」は、書き忘れても正常系のテストが全て緑のまま通る種類の規律であり、11箇所を目で守るのは `PROGRESS.md` Phase 1 の教訓6「手で維持する一覧は必ず腐る。構造で閉じる」に真っ向から反する |
| 手順を `kaikei-mcp` 側のヘルパに置く | `kaikei-api`（Phase 4）が同じ手順を再実装することになる。D-072（線上表現を1箇所に持つ）と同型の問題で、presentation 層ごとに持つと必ずずれる。監査ログの手順は「どの入口から呼ばれても同じでなければ意味が無い」もの |
| `operation` を `Future` で受け取る（クロージャにしない） | 呼び出し側が future を**先に構築**するため、「実行していない」ことがコード上で読み取りにくい。Rust の future は遅延なので実害は無いが、fail-closed は読んで分かる形にする価値がある |
| 成功時の出力 JSON も `kaikei-app` が組み立てる | `kaikei-app` は serde を持てない（`CLAUDE.md` §1）。JSON の組み立ては presentation 層の責務であり、`describe` クロージャで受け取る |
| `AuditedCall` のフィールドを `pub` にして「警告を応答に添える」を規約で守る | 書き忘れが11ツールに散り、**正常系のテストが全て緑のまま通る**。fail-closed について同じ理由で却下したものを fail-open で採るのは筋が通らない |
| `#[must_use]` だけで済ませる | `AuditedCall` 自体を捨てる書き方は止まるが、`audited.result` を使って `warning` を捨てる書き方は止まらない（`#[must_use]` は構造体の**使用**を求めるだけで、フィールドの読み方までは縛れない） |
| 警告を `result` に畳み込む（`Err` にする / 戻り値をタプルにする） | 操作は成功しているので `Err` にはできない（D-070）。タプルは分解時に `_` で捨てられ、`into_result` のように「注記に積む」既定経路を提供できない |
| 失敗時の本文も呼び出し側に組み立てさせる | `err.to_string()`（`Display`）を渡す事故が起きる。`Display` には DB が返した生メッセージ（接続文字列・ロール名）が入りうる。`with_audit` が `AuditableError::audit_public_message()` から自動で埋め、`AuditOutcome::Failed` が `public_message` しか運べない形にすることで、`Display` が `audit_log.output` に届く経路を**構造的に塞いだ** |

**理由**: D-070 の決定は「2回書く」ことではなく「**失敗した操作の記録が残る**」
ことであり、それを壊すのは順序の誤りと fail-closed の書き忘れである。
規律を関数1つに閉じれば、退行はコンパイルエラーかテスト失敗になる。

**トレードオフ**: `with_audit` のシグネチャが長い（型引数5つ・クロージャ2つ）。
1ツールあたり1回だけ書く形なので許容した。`kaikei-mcp` の実装で
書きにくさが目立つようなら、ツール共通のラッパを MCP 層に**1つだけ**
置いて吸収する（手順そのものを再実装するのではなく）。

---

## D-077 「記帳が失敗しても監査ログが残る」ことを実 PostgreSQL の対照実験で実証する

**決定**: D-070 / D-075 の設計が成立していることを、
`crates/kaikei-store/tests/audit_log.rs`（`pg-tests`）で実 PostgreSQL に対して
実証する。要となるのは次の3本で、**対照実験を含める**。

| テスト | 何を示すか |
|---|---|
| `failed_posting_rolls_back_the_ledger_but_keeps_both_audit_rows` | 記帳が失敗して `with_tx` が rollback しても、開始レコードと結果レコード（`status='error'` + `error_code`）が残る。帳簿は0件 |
| `audit_row_written_inside_a_rolled_back_transaction_still_survives` | `with_tx` の**内側**から `AuditSink` で書いた行が、そのトランザクションの rollback で消えない（＝別コネクションであることの直接の証明） |
| `a_row_written_inside_the_same_transaction_does_disappear_on_rollback` | **対照実験。** 同一トランザクションへの素の INSERT は rollback で消える |

あわせて D-075 の 7・8（入力を理由に fail-closed へ落とさない／`42501` の
包み直し）も実 PostgreSQL で実証する。いずれも**フェイクでは観測できない**
（SQLSTATE `22P05` は `jsonb` の実装が返すもので、権限剥奪も同様）。

| テスト | 何を示すか |
|---|---|
| `a_nul_character_in_the_input_is_sanitized_and_never_blocks_the_operation` | U+0000 を含む入力でも**操作が実行され**、監査ログに2行残り、`_audit.verbatim = false` で置換が起きたと分かる |
| `a_nul_character_in_the_error_message_still_records_the_result_row` | 失敗の本文に U+0000 が混じっても結果レコードが残る（fail-open の警告にならない） |
| `an_unparsable_input_is_replaced_by_a_placeholder_instead_of_blocking` | 壊れた JSON でも `actor` / `tool` が残る |
| `start_record_failure_prevents_the_posting_and_leaves_the_ledger_untouched` | `REVOKE INSERT ON audit_log` で fail-closed になり、`cause` の分類が `backend` で、`cause.public_message()` にも「逆仕訳」が現れない |

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| インメモリのフェイク（`RecordingAuditSink`）だけで検証する | フェイクにはトランザクションが無いので、**D-070 の要件を原理的に検証できない**。「別コネクション」は永続化層の性質であり、実 PostgreSQL でしか観測できない。フェイクで検証するのは fail-closed / fail-open の**手順**だけに留める |
| 生き残るテスト（2本目）だけを置き、対照実験を書かない | 「消える条件が成立していないだけ」でも通ってしまう（空虚な真）。3本目が無いと、例えばテストDBが自動コミットになっていても緑になる |
| 失敗した操作を「貸借不一致」で作る | `JournalEntry::new` が構築時に弾くため、そもそも `with_tx` に入らない。rollback を観測したいので、**帳簿に INSERT した後で**失敗する形（`AppError::Rejected` を返す）にした |
| 新しい CI ジョブを追加して実行する | 不要。`database` ジョブの `cargo test -p kaikei-store --features pg-tests` は**テストターゲットを列挙しない**ので、`Cargo.toml` に `[[test]]` を足すだけで対象に入る。clippy も `--all-targets` で拾われる。ブランチ保護の必須チェック（`CLAUDE.md` §13）を触る必要が無い |
| マイグレーションの検証を「`migrations/` の `.sql` 件数 = `_sqlx_migrations` の行数」で行う | **トートロジー。** `#[sqlx::test]` が事前に全件適用するので常に一致し、落ちる経路が存在しない。加えて `.down.sql` を置くと誤検出で赤になる |
| 件数の検証を畳んで `expected_tables_exist_and_documents_tables_do_not` に寄せる | テーブルを作らないマイグレーション（`0001_baseline_privileges.sql` のような権限だけの変更）の増減を検出できない。一覧との突き合わせなら検出できる |

あわせて、`tests/migrations.rs` の「マイグレーションが8件」というリテラルの
期待値を、**バージョン番号と description の一覧**との突き合わせに置き換えた
（`applied_migrations_match_the_expected_list`）。

一度は「`migrations/` の `.sql` を数える」形にしたが、これは**赤になる経路が
無い**（`#[sqlx::test]` は実行前に `migrations/` を全件適用するので、
ファイル数と `_sqlx_migrations` の行数は原理的に常に一致する）。
リテラル `8` が持っていた「マイグレーションが増減したことに人間が気づく」
検出力すら失っており、さらに `0010_x.down.sql` を置くと sqlx は up/down を
1件と数えるのにディレクトリを数える側は2件と数えて**誤検出で赤になる**。
一覧との突き合わせなら、ファイルを足した／改名した瞬間に赤になり
（実際に `0010_probe_tmp.sql` を置いて赤になることを確認した）、
down マイグレーションを置いても誤検出しない。
「手で維持する一覧は必ず腐る」（`PROGRESS.md` Phase 1 の教訓6）が禁じて
いるのは**腐っても誰も気づかない一覧**であり、維持を忘れると赤になる一覧は
その対象ではない。

`docs/03-database.md` §5 のファイル一覧も実態（0009 まで）に合わせ、
「番号を先に予約しない」ことを明記した——旧版は `0008`/`0009` を
documents / imported_transactions 用として予約していたが、その番号は
既に別の用途で埋まっていた。

**理由**: D-070 は「同一トランザクションで書くと最も重要な記録だけが
残らない」という一点のためにある決定であり、その一点は正常系のテストでは
一切観測できない。**この3本が落ちない限り退行に気づけない**ので、
テストの側にも「なぜこの形なのか」を doc コメントで残した。

**トレードオフ**: `pg-tests` の実行時間が約15秒増える（`#[sqlx::test]` は
テストごとに使い捨てDBを作りマイグレーションを再適用する）。
実 DB でしか検証できない性質なので受け入れる。


---

## D-078 `kaikei-mcp` の「してはいけないこと」は許可リストと禁止リストの両方で閉じる

**決定**: Phase 3 PR-D（`kaikei-mcp` の骨組み）で、この層に対する機械的な歯止めを
2つ置く。どちらも**許可リストと禁止リストの両方**から閉じる。

1. **ツールレジストリ**（`docs/07-mcp-server.md` §10 MC-10）
   - 禁止側: `delete_journal_entry` / `update_journal_entry` / `execute_sql` /
     `reopen_period` の4件を**テスト側の定数**にして**ループで**検査する
     （`crates/kaikei-mcp/tests/forbidden_tools.rs`）。
   - 許可側: 登録されているツールが `docs/07-mcp-server.md` §2 の
     **Phase 3 の11件**のいずれかであることも検査する。
   - 検査対象はレジストリ（`ToolRouter`）から導出する。実装側のツール名の
     一覧をテストに書き写さない。
2. **依存の最小性**（同 §10 MC-30）
   - `.github/workflows/architecture.yml` の既存ジョブ `dependency-direction` に
     **ステップを追加**する（新しいジョブは作らない）。
   - **許可リスト**方式にする: `kaikei-core` / `kaikei-app` / `kaikei-jp` /
     `kaikei-store` / `rmcp` / `tokio` / `serde` / `serde_json` / `schemars`。
   - 依存の取得は **`cargo metadata --no-deps`** の
     `packages[].dependencies[].name`（＝ `Cargo.toml` の宣言そのもの）。
   - 許可リストとの照合は `case " $ALLOWED " in *" $d "*)` による**完全一致**。

**訂正注記（レビュー2巡目）**: 当初は `cargo tree`（`--edges` 指定なし）+
`grep -qw` で書いたが、**どちらにも抜け道が残っていた**。

- `cargo tree` は既定で**ホストターゲットに解決される依存しか見ない**。
  CI は ubuntu-latest なので、`[target.'cfg(windows)'.dependencies]` の下に
  `uuid` を置くと検査を素通りする（実測: `cfg(target_os = "linux")` 配下の
  `uuid` は Windows ホストの `cargo tree` から見えず、`cargo metadata` からは
  見える）。この決定が塞いだつもりの「dev-dependency の抜け道」と**同型**。
  `cargo metadata --no-deps` の `dependencies[]` は kind（normal / dev /
  build）・target 指定・optional を問わず全て列挙されるので、`--edges` の
  指定が不要になり、target-cfg の穴も同時に閉じる。
- `grep -qw` の `-w` は `-` を**単語構成文字として扱わない**。そのため
  `kaikei` / `core` / `app` / `jp` / `store` という名前の crate を直接依存に
  足すと、`kaikei-core` 等の**成分名**に一致して緑のまま通る（実測）。
  `case` の完全一致に変えた。

**この訂正の範囲は `kaikei-mcp` のステップだけ**にする。同じジョブの
`kaikei-core` / `kaikei-policy` / `kaikei-jp-data` のステップも `cargo tree` +
`grep -qw` で書かれており同じ性質を持つが、凍結層の検査を同じ PR で
書き換えると影響範囲が広がりすぎる（`CLAUDE.md` §9「1コミット1論点」）。
**別 Issue として起票する。**

**訂正注記2（レビュー3巡目）**: 上の `case` による完全一致は、**Windows の
開発機で手元実行すると健全な状態でも全滅する**。Git Bash の `jq.exe` は
stdout をテキストモードで開くため各依存名の末尾に `\r` が付き、
`" $ALLOWED "` に `"kaikei-app\r"` は含まれないので、7件すべてが
「禁止された依存」になって exit 1 になる（実測）。**push 前に手元で回せない
検査は回されなくなる**ので、`jq` の出力に `tr -d '\r'` を1つ挟んだ。
ubuntu-latest の `jq` は `\r` を出さないため CI 側の挙動は変わらない。

**許可リストと実際の依存は一致していなくてよい**。許可リストは「足してよい
上限」であって「足さねばならない一覧」ではない。PR-D 時点の
`crates/kaikei-mcp/Cargo.toml` からは `tokio` を外してある（この crate の
`src/` から参照が0件で、`docs/07-mcp-server.md` §4 が掲げる「依存は最小限」の
実物として未使用の1件が残るのを避けるため）。実際に要るのは合成ルート
（`#[tokio::main]`）を置く PR-E なので、**使う PR で足す**。許可リストからは
外していない——外すと PR-E が「CI を緩める PR」になり、この決定の最後の
却下理由（`kaikei-store` の行）と同じ問題を招く。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| 禁止4件のうち代表1件だけを検査する | `docs/07-mcp-server.md` §10 が明示的に禁じている。1件だけの検査では他の3件が復活しても緑のまま通る。4件は**理由が同じではない**（D-014 の3件と、締めが Phase 4 以降だから作りようがない `reopen_period`）ので、片方の理由が変わったときに巻き添えで検査が消えるのも避けたい |
| 禁止リストだけで閉じる（許可リストを作らない） | `drop_journal_entries` のような**新しい名前**の破壊的ツールが増えても緑のまま通る。禁止リストは「既に思いついた名前」しか守れない |
| 許可リストだけで閉じる（禁止リストを作らない） | 禁止4件は「なぜ無いのか」の記録そのものである。一覧が消えると実装者が善意で復活させる（`docs/07-mcp-server.md` §10 の「行を消さない」と同じ理由） |
| 実装側のツール名一覧をテストに手で持つ | 「手で維持する一覧は必ず腐る」（`PROGRESS.md` Phase 1 の教訓6）。許可リストは**設計書 §2 の写し**なので手で持つが、**実装側の一覧は必ずレジストリから導出する** |
| MC-10 を実プロセスの stdio ハンドシェイク（`tools/list` の JSON）で検査する | `tools/call` を直接叩くには `RequestContext` が要り、その構築に必要な `rmcp::service::Peer::new` は `pub(crate)` で外部 crate から組み立てられない。実プロセスで見るには起動バイナリ（＝合成ルート。PR-E）と実 DB が要る。`#[tool_handler]` が生成する `list_tools` / `get_tool` は同じ `ToolRouter` を引くので、レジストリを見る検査と結果は同一である |
| 依存検査を**禁止リスト**（`uuid` / `rust_decimal` を弾く）にする | 「uuid と rust_decimal さえ無ければよい」ではない。線上表現を `kaikei-app` / `kaikei-jp` から取る限りこの層に依存が増える理由は無く、増えたこと自体が「MCP は薄い層」（`docs/07-mcp-server.md` §4）を疑うべき兆候である。次に何が生えるかを先に列挙できない |
| 依存検査を `--edges normal` で行う（他のステップと揃える） | `uuid` を **dev-dependency** として足してテストで使う抜け道が残る（D-047 の注記、`kaikei-e2e` のステップと同型の穴）。実測でも `--edges normal` では dev-dependency の `rust_decimal` が見えず、既定の edges では検出できた |
| 依存検査を `cargo tree`（`--edges` 指定なし）のまま続ける | ホストターゲットに解決される依存しか見ないため、`[target.'cfg(windows)'.dependencies]` 配下が ubuntu-latest の CI を素通りする（上の訂正注記。実測）。`--target` を並べて全ターゲットを網羅する案もあるが、「次にどの `cfg` が使われるか」を先に列挙できない点で禁止リスト方式と同じ弱さを持つ |
| 許可リストとの照合を `grep -qw` のまま続ける | `-w` が `-` を単語構成文字として扱わないため、`kaikei` という名前の crate を足すだけで `kaikei-core` に一致して通る（上の訂正注記。実測） |
| 照合を `grep -Fxq`（固定文字列の完全一致）にする | 許可リストが空白区切りの1変数なので、行に分解する前処理が要る。`case` なら追加のプロセスも前処理もなく完全一致になる。**`jq` 以外の外部コマンドを増やさない**方が、CI と手元で挙動が分かれる余地も小さい |
| `cargo metadata` の JSON を `grep` で読む（`jq` を使わない） | 依存名以外の文字列（`description` に crate 名が出る等）に当たる。`jq` は ubuntu-latest に同梱されており、依存の追加にはならない |
| 依存検査を新しい CI ジョブにする | 新しいジョブは必須チェックへの登録（ブランチ保護の変更）が要り、登録を忘れると「失敗してもマージできる」状態になる（`CLAUDE.md` §13）。既存ジョブへのステップ追加なら、その日から `dependency-direction` の一部として必須チェックに含まれる |
| `sqlx` を許可リストに入れる | `kaikei-mcp` は合成ルートなので `PgStore` は要るが、接続の生成（`connect_app`）も含めて `kaikei-store` が公開している。sqlx の型を MCP 層が直接触りたくなったら、それは `kaikei-store` の公開 API が足りていないという**別の問題**であり、ここで許可して隠してはいけない |
| `kaikei-store` を許可リストから外す（PR-E で足させる） | `kaikei-mcp` が本番で最初の合成ルートになることは `docs/07-mcp-server.md` §4 で決まっており、`Arc<PgStore>` を持つのは既定路線である。ここで外すと PR-E が「CI を緩める PR」になり、検査を緩めること自体が普通の作業に見えてしまう。**判断はこの決定で済ませ、PR-E は CI を触らない** |

**理由**: この層で守りたいのは「AI に開いてよい操作の集合」と「薄さ」の2つで、
どちらも**書き忘れではなく書き足しによって壊れる**性質を持つ。
書き足しを検出するには、既知の悪い名前を並べるだけでは足りない。

**トレードオフ**: 許可リストが2箇所（テストの `PHASE_3_TOOLS` と CI の
`ALLOWED`）に増えた。前者は設計書 §2 の写しなので、ツールを増やす PR は
設計書とテストの両方を触ることになる（これは意図した摩擦である）。
後者は依存を増やす PR に説明を強制する。どちらも「増やすとき」にだけ効く
コストで、日常の作業には現れない。

---

## D-079 線上の金額は `wire.rs` の `AmountStr` にし、`serde_json::Value` 経由で number を拒否する

**決定**:

1. 金額の線上型は `crates/kaikei-mcp/src/wire.rs` の **`AmountStr`**（newtype）にする。
   **`kaikei-mcp/src/amount.rs` は作らない**（`docs/07-mcp-server.md` §5）。
   文字列化は `kaikei_app::amount::money_to_plain_string` に委ね、
   この層は詰め替えだけを行う。
2. `Deserialize` は手書きし、**一度 `serde_json::Value` として受けてから**
   文字列かどうかを判定する。文字列以外（number / bool / null / 配列 /
   オブジェクト）はすべて同じ**日本語**のメッセージで拒否する。
3. `JsonSchema` は `#[schemars(with = "String")]` で `"type": "string"` を出す。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| 素の `String` フィールドにする | serde が組み立てる `invalid type: integer 110000, expected a string` という**英語の型エラー**しか AI に届かない。`CLAUDE.md` §11 を満たさない（D-019 が `{:?}` の英語バリアント名を禁じたのと同型） |
| `deserializer.deserialize_str(..)` + 独自 Visitor | number を受け取った時点で **serde 側が** `invalid_type` を組み立ててしまい、Visitor に制御が渡らない。custom メッセージが無視される（実測） |
| `deserialize_any` + 独自 Visitor（`visit_i64` / `visit_f64` …） | 機能的には成立するが、`visit_f64` の引数型としてソースに浮動小数点の型名が現れ、`.github/workflows/architecture.yml` の「f64 が金額に使われていない」ステップ（コメント行以外の該当語を全て落とす）が**必ず赤になる**。この CI は `CLAUDE.md` §8 の中核であり、迂回のために緩めない |
| 整数の number だけ受理する（警告を添える） | D-013 の訂正注記で Phase 3 が却下済み。サーバに届いた時点では、クライアント側で既に倍精度に丸められたかどうかを**サーバから検出できない**。受理すれば「警告付きで壊れた金額を記帳する」ことになる |
| `kaikei-mcp/src/amount.rs` に整形も含めて置く（初版の構成案） | 同じ整形が `kaikei-app` と2箇所に育つ。`kaikei-api`（Phase 4）も同じ形式を返す必要があり、presentation 層ごとに持つと必ずずれる（`docs/07-mcp-server.md` §5・D-072） |
| `Money` に `Serialize` / `Deserialize` を実装する | `kaikei-core` への serde 参照は CI が機械的に禁じている。上位層の都合で凍結層に依存を足さない（`CLAUDE.md` §1） |

**理由**: 「number を弾く」は仕様だが、**どう弾くか**が `CLAUDE.md` §11 と §8 の
両方に触れる。§11（日本語で次の手を示す）を満たそうとして §8 の CI（f64 禁止）を
踏む、という組み合わせが実在したため、両方を満たす形を決定として残す。

**トレードオフ**: `serde_json::Value` を1度経由するぶん、自己記述的でない
シリアライズ形式では使えない。MCP の入力は常に JSON なので実害は無いが、
将来 `kaikei-api` が別形式を受けるようになったらここは再検討になる。
また `AmountStr` は**書式の検証をしない**（`Money::parse` に通すまで
「金額として妥当か」は分からない）。通貨ごとに小数桁が違う以上、通貨を
伴わずに検証はできないので、これは意図した分業である。

---

## D-080 `JpError` の分類コードは既存語彙を最優先で再利用し、ロード失敗は1つに集約する

**決定**: `crates/kaikei-mcp/src/error.rs` に `jp_error_code`（**網羅 `match`**）を置く。
`kaikei-app` は `kaikei-jp` に依存できない（`CLAUDE.md` §1）ため、
`JpError` の写像表はここが唯一の置き場になる（`docs/07-mcp-server.md` §6）。

- **既存語彙を再利用する**: `UnregisteredTagKey` → `unknown_tag_key`、
  `InvalidTagValue` → `tag_type_mismatch`、`NoApplicableTaxRuleSet` →
  `no_applicable_rule_set`、`UnknownTaxCategoryCode` → `unknown_tax_category`、
  `InvalidBusinessRatio` → `invalid_value`、`InvalidHouseholdSplitTotal` →
  `invalid_amount`、`Core(_)` は中身の `CoreError` へ委譲。
- **新しく起こすのは6件だけ**: `invoice_reg_no_missing_prefix` /
  `invoice_reg_no_wrong_length` / `invoice_reg_no_non_digit` /
  `invoice_reg_no_check_digit` / `duplicate_tag_key` / `invalid_setting_code`。
- **`InvalidChart` は既存の `invalid_chart` に寄せる**（下の訂正注記1）。
- **マスタ・設定のロード失敗9件は `invalid_policy_data` に集約する**
  （`YamlParse` / `Io` / `InvalidTaxCategoryTable` / `OverlappingTaxPeriods` /
  `InvalidTagSchema` / `MissingClosingAccount` /
  `NotPostableClosingAccount` / `DuplicateClosingAccount` /
  `ClosingTagSchemaMismatch`）。
- 応答は `ToolError` にまとめ、**`CallToolResult::structured_error`**
  （`isError: true` + `structuredContent` + 同じ JSON のテキスト）で返す。

**訂正注記1（レビュー2巡目）— `InvalidChart` を集約から外す**:
当初は `JpError::InvalidChart` もロード失敗10件の1つとして
`invalid_policy_data` に入れていたが、これは**この決定自身に反していた**。
`kaikei_app::error::codes::INVALID_CHART`（`"invalid_chart"`、doc は
「勘定科目表そのものが不正」）が既にあり、`CoreError::InvalidChart` は
そちらに写像されている。そして `JpError::InvalidChart` は
`ChartOfAccounts::new` が返した `CoreError::InvalidChart` を包み直したものを
**含む**（`crates/kaikei-jp/src/chart.rs` の `from_raw` が `CoreError` の
`Display` を `reason` に詰める）。つまり「親科目が存在しない科目表を読んだ」
という**同一の条件**が、`kaikei-app` 経由なら `invalid_chart`、`kaikei-jp`
直呼び（`docs/07-mcp-server.md` §4 の経路 (c)）なら `invalid_policy_data` に
なっていた。「既に語彙がある概念に別名を作らない」に寄せて `INVALID_CHART` に
統一する。

`InvalidTagSchema` を集約に残すのは、`codes` に「タグスキーマそのものが不正」に
相当する語彙が**無い**ため（`unknown_tag_key` / `tag_type_mismatch` はどちらも
**入力側**の誤りを指す。同じ意味の既存語彙が無いものは集約側に残す）。

**訂正注記2（レビュー2巡目）— `from_jp_error` と `from_app_error` の非対称**:
`ToolError::from_app_error` は `AppError::public_message()` を使い、`Display` を
線上に載せない（`Display` は `RepoError::Backend { reason }` に下位層が返した
生の文字列——接続文字列やロール名が混じりうる——を抱えるため）。一方
`ToolError::from_jp_error` は `JpError` の `Display` を**そのまま**線上に載せる。

**この非対称は意図したものである。** `JpError` の文言は `kaikei-jp` が
AI 向けに自分で書いたもので（有効なキー一覧・有効期間・期待する書式を含む）、
外部クレートのメッセージを抱え込む口を持たない。言い換えは
`docs/07-mcp-server.md` §6 が禁じている。

ただし「口を持たない」は放置すると崩れる性質なので、`kaikei-mcp` 側の
`jp_error_display_is_self_authored` が **`crates/kaikei-jp/src/error.rs` の
ソースを読んで**、`#[error(...)]` に `{source}` を書いている／
`#[error(transparent)]` で丸ごと委ねているバリアントの集合が、理由付きの
明示リスト（`Io` / `YamlParse` / `Core`）と一致することを検査する。

**サーバ側の情報が線上に出る唯一の例外が `JpError::Io`** で、読めなかった
ファイルのパスを含む。運用者が直すための情報であり（`CLAUDE.md` §11）、かつ
マスタ・設定の不備は起動時に検出して**起動を中止する**（同 §7）ため、通常
ツール応答には現れない。

**訂正注記3（レビュー2巡目）— 検査を構造で閉じる**:
初版は次の2つが「主張はあるが検査が無い」状態だった。

1. `into_call_tool_result()` を呼ぶテストが1本も無かった。
   `CallToolResult::structured_error` を隣接する `structured`
   （`is_error: Some(false)`）に取り違えても**全テストが緑のまま通り**、
   結果として**全てのドメインエラーが「成功」として AI に届く**。
   `isError == Some(true)` / `structuredContent` に本文が入ること /
   **`content` のテキストにも本文が載ること**（構造化コンテンツを
   モデルに見せないクライアントにも届く、という上の却下表の主張）の
   3点を検査する。
2. `jp_error_code` の網羅性テストが参照する `all_jp_errors()` が
   **手書きの一覧**で、新しいバリアントに `codes::INTERNAL` を割り当てても
   一覧に載っていない限り発火しなかった（`PROGRESS.md` Phase 1 の教訓6
   「手で維持する一覧は必ず腐る」）。`crates/kaikei-jp/src/error.rs` を
   `include_str!` で読んでバリアント名を抽出し、一覧と突き合わせる
   （`crates/kaikei-jp/tests/chart_drift.rs`／D-051 と同じ手法）。
   値を作れない `YamlParse`（`serde_norway::Error` のコンストラクタが
   非公開）は**理由付きの除外リスト**に置く。抽出パターンが当たらなく
   なったときに「0件を突き合わせて全件一致」で無言で通らないよう、
   件数の下限を番人として置く。

**訂正注記4（レビュー3巡目）— 訂正注記1を散文でなく検査で固定する**:
「`JpError::InvalidChart` と `CoreError::InvalidChart` が同じコードになる」は
訂正注記1と `docs/07-mcp-server.md` §6 の散文にしか無く、将来 `kaikei-app` 側が
`CoreError::InvalidChart` の写像を変えれば**無言で再発**する状態だった。
`crates/kaikei-mcp/src/error.rs` の
`jp_and_core_invalid_chart_resolve_to_the_same_code` で、**両方の写像関数を
実際に通してその結果が一致すること**を検査する。定数どうし
（`codes::INVALID_CHART` と `codes::INVALID_CHART`）を比べても自明に等しい
だけで何も固定できないので、`jp_error_code` と
`kaikei_app::error::core_error_code` の**戻り値**を突き合わせる。
テストは `kaikei-mcp` 側に置く（`kaikei-app` の公開契約は触らない）。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `JpError` にも層ごとの接頭辞付きコードを新しく起こす（`jp_unknown_tag_key` 等） | 層ごとの接頭辞を付けないのが D-072 の規約。AI が読むのは「何が起きたか」であって「どの crate が返したか」ではない。同じ意味に2つの綴りがあると、AI の分岐が両方を知らないと動かなくなる |
| 登録番号の4件を1つのコード（`invalid_invoice_reg_no`）にまとめる | 検証順は「先頭文字 → 桁数 → 文字種 → チェックデジット」に固定されており、**最初に失敗した観点だけ**が返る（D-053）。1つに潰すと、AI は何を直すのかを本文の日本語から推測するしかない。分類コードを機械可読にしている意味が消える |
| `DuplicateTagKeyInInput` を `unknown_tag_key` に寄せる | 次の手が違う（綴りを直すのではなく、重複した指定を1つにまとめる）。`invalid_entry_id` を `not_found` と分けたのと同型の判断（D-072） |
| `InvalidChart` もロード失敗の集約（`invalid_policy_data`）に入れる | 初版はこうしていたが、`invalid_chart` が既にあり `CoreError::InvalidChart` がそちらに写像されている。`JpError::InvalidChart` はその `CoreError::InvalidChart` を包み直したものを含むので、同じ条件が経路によって2つのコードになる（訂正注記1） |
| `from_jp_error` にも `public_message()` 相当を用意して `Display` を言い換える | `JpError` の文言は有効なキー一覧・有効期間・期待する書式を含む「次の手」そのものであり、言い換えると情報が落ちる（`docs/07-mcp-server.md` §6 が明示的に禁じている）。`AppError` と違って下位層の生メッセージを抱える口が無いことは、ソースを読む検査で維持する（訂正注記2） |
| `all_jp_errors()` を手書きの一覧のまま運用ルールで守る | 「手で維持する一覧は必ず腐る」（`PROGRESS.md` Phase 1 教訓6 / D-047）。同じ形で3回失敗している |
| `JpError` に `strum` 等を足してバリアントを列挙する | `kaikei-jp` に依存が1つ増える。凍結層に近い層の依存を、下流のテストの都合で増やさない（`CLAUDE.md` §1）。ソースを読む方式なら依存はゼロで済む |
| マスタ・設定のロード失敗に新しいコード（`master_data_invalid` 等）を起こす | `PolicyError::InvalidPolicyData` が既に「policy が構築時に受け取ったデータが不正」であり、意味が一致する。**既に語彙がある概念に別名を作らない**（`docs/07-mcp-server.md` §6）。なおこれらは通常ツール応答に現れない——設定・マスタの不備は起動時に検出して起動を中止する（同 §7） |
| ロード失敗を `internal` に潰す | `internal` は**下流が知らないバリアントに出会ったときの受け皿**であり、既知のエラーに使う値ではない（`kaikei_app::error::codes::INTERNAL` の doc） |
| ロード失敗を `backend` に寄せる（`Io` は I/O 失敗だから） | `backend` は `RepoError::Backend`＝**永続化層**の失敗である。同梱 YAML が読めないことに対して「時間をおいて再試行するか管理者に連絡」と案内するのは誤診で、D-038 が潰したのと同じクラスの欠陥になる |
| `jp_error_code` にワイルドカードの腕を置く | `JpError` は `#[non_exhaustive]` ではない（意図的。`kaikei-jp/src/error.rs` の doc）。受け皿があるとバリアント追加時にコンパイルが壊れず、新しいバリアントが黙って既定コードになる（D-072 訂正注記と同じ理由） |
| 応答を `CallToolResult::error`（テキストのみ）で返す | 構造化コンテンツを読むクライアントに対して `error` / `message` を機械的に取り出す経路が無くなる。逆に `structured` だけにすると、構造化コンテンツをモデルに見せないクライアントで本文が AI に届かない。`structured_error` は**同じ JSON をテキストとしても**載せるので両方を満たす |
| ドメインのエラーを JSON-RPC のプロトコルエラーで返す | D-071 で却下済み。プロトコルエラーを使うのは未知のツール名など、ツール呼び出しに到達できない異常に限る |

**理由**: 分類コードは AI の分岐に使われる**安定した識別子**であり、
増やすこと自体にコストがある（`audit_log.error_code` を読む側も追随が要る）。
一方で、意味の違うものを同じコードに潰すと AI が誤った次の手に進む。
「既存語彙を最優先で再利用し、次の手が違うものだけ新しく起こす」を判断基準にする。

**トレードオフ**: `invalid_policy_data` に9バリアントが集まったため、
このコードだけを見ても「YAML の構文エラー」なのか「決算科目の設定ミス」なのかは
分からない。ただし区別が要るのは**運用者**であって AI ではなく、運用者向けの
情報は `message`（`JpError` の `Display`。どのファイルの何が悪いかを含む）と
サーバのログに残っている。

---

## D-081 勘定科目マスタの投入は「追加のみ」にし、既存の科目定義を上書きしない

**決定**: 勘定科目マスタの投入経路を `kaikei-app` のユースケース
（`usecase/import_chart.rs` の `execute`）とポート
（`ports::ChartWriteRepo::insert_accounts`）として新設し、合成ルート
（`kaikei-mcp`）が**起動のたびに**呼ぶ。冪等性は次のように定める。

| 投入しようとした科目 | 動作 |
|---|---|
| DB に無い | **追加する** |
| DB にあり、定義が完全に一致 | 何もしない |
| DB にあり、定義が異なる | **既存を残す。上書きしない。** 差異を `ImportChartOutput::kept_existing` として返し、合成ルートが stderr に出す |

つまり**この経路は `UPDATE` も `DELETE` も一切発行しない**。
PostgreSQL 実装は `ON CONFLICT (code) DO NOTHING` の1文で、`DO UPDATE` に
書き換えないことを `crates/kaikei-store/src/chart.rs` の doc と
`crates/kaikei-store/tests/chart_import.rs` で固定する。

あわせて次を決めた。

1. **読み書きのポートを分ける。** `ChartWriteRepo` は `ChartRepo` に
   メソッドを足す形ではなく別 trait にし、`TxOps` の束ねにも**入れない**。
   記帳しかしないユースケースの `Tx` に、マスタを書き換える能力が付いて
   回らないようにする（`where Tx: ChartWriteRepo` と書いた `import_chart`
   以外はマスタに触れないことが型で読める）。
   `kaikei-app/src/ports.rs` の `ChartRepo` doc（「マスタの編集は本 crate の
   ユースケースの対象外」）はこの範囲で改めた。
2. **テスト用シードと本番の投入経路を1本にする。**
   `crates/kaikei-e2e/tests/e2e_jp.rs` の `seed_chart` は、migrator ロールで
   生 SQL（2パスの INSERT + UPDATE）を発行していたものを、この
   ユースケースを呼ぶ形に書き換えた（D-047「手で維持する複製は腐る」）。
3. **投入は `kaikei_app` ロールで行う。** 合成ルートは `APP_DATABASE_URL` で
   繋ぐ（D-048 / `docs/07-mcp-server.md` §8）ので、migrator でしか通らない
   実装は本番で使えない。`accounts` には
   `GRANT SELECT, INSERT, UPDATE`（`0002_accounts.sql`）があり `INSERT` は通る。
4. **親子関係を2パスに分けない。** 自己参照 FK（`accounts.parent_code`）は
   同一文の中で挿入された行同士なら順序を問わない（参照整合性チェックは
   AFTER ROW トリガとして文の終わりに発火する）。1文の `UNNEST` INSERT で
   足りるので2パス目の `UPDATE` が不要になり、上の「UPDATE を発行しない」と
   両立する。この性質に依存していることは実 DB のテスト
   （`insert_accounts_accepts_a_child_listed_before_its_parent`）で固定する。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| 起動のたびにテンプレートで**上書き**する（`ON CONFLICT DO UPDATE`） | 2つの実害がある。(1) D-069 は「ユーザーが科目名を編集する」ことを明示的に前提にしており、上書きすると**編集が起動のたびに消える**。(2) `account_type` が変われば**過去の仕訳の意味が後から変わる**（試算表の残高の符号、決算書の区分）。帳簿本体と違って `accounts` は append-only ではないが、それは「起動処理が黙って書き換えてよい」という意味ではない |
| 既存と定義が異なる科目があったら**起動を中止**する | ユーザーが科目名を1つ編集しただけでサーバが起動しなくなる。「設定が欠けている」（起動を止めるべき。D-057）と「マスタがテンプレートと違う」（利用者の正当な編集でありうる）は、止めるべきかどうかが違う。差異は握り潰さず**診断として出す**方を採った |
| `accounts` が**空のときだけ**投入する（if-empty ブートストラップ） | テンプレートに科目が増えても既存の帳簿には永久に入らない。「差分だけ入れる」なら追加の実害が無い（新しい科目コードを足すことは過去の仕訳の意味を変えない）ので、より弱い条件で足りる |
| 投入を MCP ツールとして公開する | D-070 の決定2で却下済み。AI が呼ぶ操作ではなく、サーバが起動するための前工事である |
| `kaikei-store` に投入処理を直接置く（ユースケースを作らない） | 「どの科目を投入するか」は日本固有のテンプレート由来のデータであり、infra に日本固有の知識が入る（`CLAUDE.md` §1 / §5）。加えて「既存と差異があったらどうするか」は業務判断であって SQL の都合ではない |
| 起動のたびに投入するのをやめ、別バイナリ（`kaikei-seed`）にする | 「サーバは起動するが記帳が1件も通らない」状態を作れてしまう。その状態は AI から見ると「全ツールが `unknown_account` で失敗する」だけで、原因（科目を投入していない）に辿り着けない。追加のみ・冪等なら毎回実行して害が無い |
| `sort_order` / `active` 列もテンプレートから設定する | テンプレートの `sort` はドメイン変換で破棄済み（D-061）で、`kaikei_core::AccountDef` に対応するフィールドが無い。`active` は無効化のために用意された列であり、投入経路が触れば「無効化した科目が起動のたびに復活する」ことになる。**ただし現時点で `active` は無効化として機能していない**（下記の訂正） |

> **訂正（PR-E レビュー）**: 上の行は当初「`active` は無効化のための
> ユーザー操作用の列であり」とだけ書いており、**無効化が既に機能している
> 前提**を置いていた。実際には `load_chart`
> （`crates/kaikei-store/src/chart.rs`）が `active` を `SELECT` しておらず、
> `active = false` にしても記帳は成功する（自動生成される税額行も
> `active = false` の科目に付く）。**`active` は「将来 Phase で無効化を
> 実装するために用意されている列」であって、現在の無効化手段ではない。**
> 投入経路が触らないという結論自体は変わらない（触れば、無効化を実装した
> 後に「無効化した科目が起動のたびに復活する」が現実の不具合になる）。
> 現状と再開地点は `PROGRESS.md` の Phase 3「実装中の申し送り」に置いた。

**理由**: この PR 以前、本番コードに `INSERT INTO accounts` は**1箇所も
存在しなかった**（D-070 の決定2）。`post_entry::execute` は
`tx.load_chart()` で DB を読むため、`compose` が返す `chart` を持っている
だけでは1件も記帳できない。つまり「サーバは起動するのに記帳できない」が
**既定の状態**だった。ここを塞ぐのが Phase 3 の合成ルートの最低条件である。

そのうえで冪等性の方針を「追加のみ」に寄せたのは、**投入経路が既存の
科目定義に触れない限り、起動を何回繰り返しても会計上の意味が変わらない**
という性質が最も強い保証だからである。上書きを許すと「安全に何回でも
起動できる」が「テンプレートを更新しない限り安全」に後退する。

**トレードオフ**: 同梱テンプレートの科目名・科目種別を将来修正しても、
既に投入済みの帳簿には反映されない（差異が起動のたびに stderr に出続ける）。
科目マスタの編集手段は Phase 4 以降（`upsert_counterparty` と同じ枠。D-070）
なので、それまでは DB を直接編集する必要がある。これは「勝手に書き換えない」
ことの裏返しであり、意図した性質として受け入れる。

**関連**: `crates/kaikei-app/src/usecase/import_chart.rs`（方針と単体テスト）、
`crates/kaikei-store/src/chart.rs` / `tests/chart_import.rs`（実 DB での検証）、
`crates/kaikei-e2e/tests/e2e_jp.rs`（投入後に記帳が通ること）。

---

## D-082 事業者設定は `tax_mode` / `rounding` / `rounding_unit` も含めて全て明示必須にし、環境変数で渡す

**決定**: `kaikei-mcp` の起動に必要な設定を**全て必須**にし、1つでも
欠けていたら（あるいは値が不正なら）起動を中止する。読み取り元は
**環境変数**とする。

| 環境変数 | 内容 |
|---|---|
| `APP_DATABASE_URL` | `kaikei_app` ロールの接続文字列（D-048） |
| `KAIKEI_BOOK_CURRENCY` | 帳簿通貨（D-074） |
| `KAIKEI_FISCAL_YEAR_RULE` | 会計年度の区切り規則 |
| `KAIKEI_TAX_MODE` | 経理方式（`exclusive` / `inclusive`） |
| `KAIKEI_ROUNDING` | 端数処理方式（`floor` / `ceil` / `half_up`） |
| `KAIKEI_ROUNDING_UNIT` | 端数処理の単位（`line` / `document`） |
| `KAIKEI_IS_TAXABLE_BUSINESS` | 課税事業者か（D-057） |
| `KAIKEI_SIMPLIFIED_TAXATION` | 簡易課税か（D-057） |
| `KAIKEI_CLOSING_ACCOUNT_CAPITAL` / `_OWNER_DRAWINGS` / `_OWNER_CONTRIBUTIONS` | 決算3科目（D-066） |
| `KAIKEI_CLOSING_TAX_CATEGORY` | 決算振替のゼロ化明細に付ける税区分コード |

**`docs/07-mcp-server.md` §7 の初版からの変更点**は、`tax_mode` /
`rounding` / `rounding_unit` を「省略時はマスタの `settings_defaults`」から
**必須**に変えたことである。`JpSettingsOverrides` の型は変えていない
（3つとも `Option` のまま）。変わったのは合成ルートが常に `Some` を詰める
という点だけで、`kaikei-jp` 側の契約には触れていない。

あわせて次を決めた。

1. **不足・不正は最初の1件で打ち切らず、全部まとめて返す。**
2. **空文字は「設定した」ことにしない**（`"KEY": ""` / `KEY=` は未設定と
   同じく起動を止める。ただし文言では区別する）。
3. **真偽値は `true` / `false` のみ**。`1` / `0` / `yes` / `on` は受けない。
4. **`ServerConfig` の `Debug` は接続文字列を伏せる**
   （`docs/07-mcp-server.md` §8。パスワードが平文で入る）。
5. 値の語彙の解釈は `kaikei_jp::tax` / `kaikei_app::{wire, currency}` の
   `from_code` を通す（同じ綴りの表を MCP 層で作らない。D-072）。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `tax_mode` / `rounding` / `rounding_unit` は省略可のままにする（設計書の初版どおり） | 税抜経理か税込経理かは**税務判断そのもの**であり、「たまたまその年度のマスタが推奨している値」で黙って動くのは D-057 が `is_taxable_business` について避けた事故と同型である。`docs/07-mcp-server.md` §3 が書くとおり、`tax_mode` の取り違えは同じリクエストを貸借不一致にする（あるいは税額行を勝手に足す）。`CLAUDE.md` §10「税務判断を断定しない／確定は人間に残す」の帰結として、値をコードにもマスタ既定にも委ねない |
| 専用の設定ファイル形式（TOML / YAML）を新設する | MCP クライアントはサーバを**子プロセスとして spawn** する（`docs/07-mcp-server.md` §8）ため、設定を渡す標準の経路はクライアント設定の `env` である。ファイルも併用できるようにすると**同じ設定が2箇所に書ける**状態になり、どちらが効いているか分からなくなる。単一ユーザー・自己ホスト前提（D-015）で設定は十数項目なので、環境変数だけで足りる |
| 未設定の項目を警告付きで既定値にする | 警告は stderr にしか出ず、**AI にも利用者にも届かない**（MCP クライアントがサーバの stderr を表示する保証は無い）。「起動しているのに前提が違う」状態が最も危険で、`PROGRESS.md` Phase 1 の教訓「誤診は誤値と同じ実害を持つ」と同型 |
| 不足を1件ずつ返す（最初の1件で `return`） | 12 項目を揃えるのに 12 回起動し直すことになる（`CLAUDE.md` §11「次の手が分かる文言にする」） |
| `1` / `yes` / `on` も真として受ける | 受け入れ方言が増えるほど「設定したつもりで効いていない」事故が増える。既定値へのフォールバックを禁じている以上、綴り違いが黙って `false` になってはならず、エラーにするのが一貫している |
| `defaults_as_of`（`ComposeOptions`）も設定項目にする | 「起動時点の日付」以外を渡す用途が無い（D-057 が言う「合成ルート起動時点の日付」そのもの）。設定項目が1つ増える代わりに、取り違えて過去の日付を書くと古いマスタが選ばれるという新しい事故を作る |

**理由**: D-057 は `JpSettingsOverrides::is_taxable_business` を `Option`
にしないと決めた。設定の読み取り側で省略を許して既定値に落とすと、
**その決定がそっくり無効になる**——型で塞いだ穴を、その型を組み立てる層が
開け直すことになる。「型で必須」と「起動時に必須」は同じ規律の両端であり、
片方だけでは守れない。

**トレードオフ**: 初回セットアップで揃えるべき環境変数が 12 個ある。
`.env.example` と README に一覧と例を置き、起動失敗時のメッセージが
不足項目を全部名指ししてそこへ誘導する形で補う。
検査は `docs/07-mcp-server.md` §10 MC-24（`config.rs` の単体テストと、
**実際にバイナリを起動する**統合テストの両方で、必須項目を1つずつ外して
総当たりする）。

---

## D-083 `kaikei-mcp` は `tracing_subscriber` を持たず、診断は `eprintln!` で stderr に出す

**決定**: `kaikei-mcp` にログライブラリ（`tracing` / `tracing_subscriber`）を
**入れない**。起動時の診断・起動失敗の理由は `eprintln!`（stderr）で出す。
stdout には一切書かない。

この不変条件は3段で機械的に検査する（`docs/07-mcp-server.md` §10 MC-33）。

1. `crates/kaikei-mcp/src/` に `println!` / `print!` / `io::stdout` が
   現れないこと（`tests/stdout_is_json_rpc_only.rs`）
2. 起動に失敗したとき stdout が**1バイトも**出ないこと
   （`tests/startup_config.rs`。実際にバイナリを起動する）
3. 実際に起動して `initialize` を送ったとき、stdout に出た行が
   JSON-RPC のメッセージであること（`tests/startup_pg.rs`）

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `tracing_subscriber` を入れて writer を stderr に固定する | MC-30 の許可リストを広げることになり、広げるにはその PR で理由を説明する必要がある（D-078）。PR-E が出す診断は起動時の数行だけで、レベル分け・フィルタ・構造化ログの必要が無い。**購読者を登録しなければ `tracing` のイベントはどこにも出力されない**ので、下位層（`kaikei-store` の `PgTx` commit 忘れ警告）が stdout を汚す経路も構造上存在しない |
| `.github/workflows/architecture.yml` に grep のステップを足して stdout 書き込みを検査する | 手元で `cargo test` を回すだけでは踏めない検査になる（CI でしか回らない検査は、CI が回るまで気づけない）。テストにすれば `cargo test --workspace` に自動的に乗る |
| 検査はソースの grep だけにする | 「`println!` を書いていない」ことは「stdout が汚れていない」ことを意味しない（下位層の出力・将来のログ購読者・`rmcp` 自身の挙動が残る）。**実際に起動して stdout を見る**検査を併せて置く |
| 起動診断も stdout に出す（人が読む用） | stdio トランスポートでは stdout が MCP のフレーミングそのもの。1行でも混ざるとプロトコルが壊れ、しかも症状が「サーバが応答しない」になるため原因に辿り着きにくい（`docs/07-mcp-server.md` §4） |

**理由**: `println!` を1つ書くだけで壊れ、壊れ方が「無言で応答しない」で
あるという性質は、**規律ではなく構造で守る**べきものである。依存を1つも
足さずに済むなら、この層に入れてよいものも増えない。

**トレードオフ**: 出力のレベル分け（`RUST_LOG` によるフィルタ）ができない。
起動診断は数行なので現状は問題にならないが、ツールが増えて出力量が問題に
なったら、そのとき購読者の導入を検討する（**その際は writer を stderr に
固定すること**。既定は stdout であり、入れた瞬間に壊れる）。
`tests/stdout_is_json_rpc_only.rs` は `eprintln!` を `println!` と
取り違えないことも検査している——取り違えると「stderr に出せ」という指示に
従ったコードが落ちる検査になり、`grep -qw` の単語境界で繰り返し嵌まったのと
同型の問題になる（D-078）。

---

## D-084 MCP のツールは dispatch 層を通してしか登録・実行できない形にする（rmcp のツールマクロを使わない）

**決定**: `kaikei-mcp` に **dispatch 層**（`crates/kaikei-mcp/src/dispatch.rs`）を
置き、**監査ログを通らないツールを書けない形**にする。
各ツールは `dispatch::McpTool` を実装するだけで、監査ログの手順も
`isError` の扱いも一切書かない。

**これは型だけで達成していない**（訂正注記3）。型で閉じているのは
`ToolRegistry` / `ToolContext` / `McpTool::run` の戻り値までで、
「別のルータを新しく作る」ことは `rmcp` を名指しできるファイルの
**許可リスト**（`dispatch.rs` / `error.rs`）が止めている。

```rust
pub trait McpTool: Send + Sync + 'static {
    type Input: DeserializeOwned + JsonSchema + Send + 'static;
    const NAME: &'static str;
    const DESCRIPTION: &'static str;
    fn run(ctx: &ToolContext<'_>, input: Self::Input)
        -> impl Future<Output = Result<ToolSuccess, ToolFailure>> + Send;
}

// rmcp の ToolRouter を private フィールドとして包む（レビュー反映。下記の訂正注記1）
pub struct ToolRegistry { /* rmcp の ToolRouter を包んで隠す */ }
impl ToolRegistry {
    pub fn new() -> Self;
    pub fn with<T: McpTool>(self) -> Self;                              // 唯一の登録経路
}

pub async fn call<T: McpTool>(runtime: &Runtime, arguments: Option<Map<String, Value>>)
    -> CallToolResult;                                                  // 唯一の実行経路
```

閉じ方（**型で閉じる → 残りをソース走査で見張る**の順）:

| # | 塞ぐもの | 実体 |
|---|---|---|
| 1 | ツールが応答を組み立てる | `McpTool::run` の戻り値は `Result<ToolSuccess, ToolFailure>`。`CallToolResult`（`isError` を含む）を作れるのは `dispatch::call` と `ToolError` だけ |
| 2 | ツールが監査ログを書く／書き忘れる | `run` が受け取る `ToolContext` は `AuditSink` を**露出しない**（`Runtime` 自体が渡らない）。監査ログを書く能力がツールに無い |
| 3 | `ToolContext` を自作する | フィールドも `new` も private。作れるのは `dispatch` モジュールだけ |
| 4 | `ToolRegistry` に `McpTool` 以外を載せる | 載せる口は `with::<T: McpTool>` だけで、内側の `ToolRouter` は private フィールド（訂正注記1） |
| 4b | **別のルータ・別の `ServerHandler` を書き足す** | **型では閉じない。** `rmcp` を名指しできるファイルを `dispatch.rs` / `error.rs` の2つに限る**許可リスト**（訂正注記3） |
| 4c | **走査の外にファイルを置く**（`#[path = "../foo.rs"]` / `include!("foo.inc")`） | **走査の穴だった**（訂正注記4）。`src/` 配下を拡張子で絞らず全部読み、さらに `#[path` / `include!(` が現れないことを検査する（`tests/source_scan/mod.rs`） |
| 4d | **上の全部**（ソースの書き方に依らず、監査ログが2行残ること） | **振る舞い検査**（訂正注記4）。実バイナリを stdio で起動して `tools/call` を送り、`journal_entries` と `audit_log` を数える（`crates/kaikei-e2e/tests/mcp_stdio_server.rs`） |
| 5 | fail-open の警告を捨てる | `call` は `AuditedCall::into_result_noting_outcome(&mut notes)`（既定経路）しか使わず、積まれた警告を必ず応答の `warnings` に載せる。`into_parts_unchecked`（逃げ道。D-076）はこの crate に1箇所も無い |
| 6 | クライアント由来のツール名が `audit_log.tool` に載る | `AuditCall` を組み立てるのは `dispatch.rs` の1箇所だけ（**走査で担保**）で、そこは `ToolName::resolve`（`server::is_registered_tool` ＝`tools/list` と同じレジストリを引く）を通す。**型で閉じているのは「登録済み以外から `ToolName` を作れない」ことである**（訂正注記2） |

型で閉じられない残り（同一 crate 内から `kaikei_app::audit::with_audit` を
呼ぶ、`dispatch.rs` の中で `ToolRouter` に別のツールを足す）は
`crates/kaikei-mcp/tests/audit_is_structural.rs` がソースを走査して見張る
（識別子ごとに「現れてよいファイル」と理由を持つ表。対照実験付き）。

**`docs/07-mcp-server.md` §4 の「1ツール1ファイルは `#[tool_router]` で実現する」を
撤回する。** 1ツール1ファイルは維持するが、機構は `rmcp` のツールマクロでは
なく上記の trait + `route` にする（§4 を書き換えた）。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| 各ツールの `#[tool]` 関数が `with_audit` を呼ぶ（設計書の初版） | **書き忘れても正常系のテストが全て緑のまま通る。** D-076 が fail-closed について挙げた却下理由と同じ性質であり、それを11ツールに複製することになる。「検査で落ちる」ではなく「書き忘れる形が存在しない」を選ぶ |
| `#[tool]` で書き、`with_audit` の呼び出しをソース走査だけで見張る | 走査は「書いてある」ことしか見られない。`with_audit` を**呼んでいるが順序が違う**、`describe` に別の JSON を渡している、といった崩れ方を検出できない。型で閉じられるものを走査に任せない |
| `rmcp` の `AsyncTool` / `ToolBase` trait（3.1.0 が「ツールが増えたらこちら」と勧める形）を使う | `AsyncTool::invoke` の失敗値は `Into<ErrorData>`、すなわち **JSON-RPC のプロトコルエラー**になる。D-071 が「ドメインのエラーは全てツール結果エラー（`isError: true`）」と決めており、真っ向から反する。成功値も `Json<Output>` に固定されるため、`warnings` を後から差し込む余地も無い |
| `ToolContext` ではなく `&Runtime` をツールに渡す | `Runtime::audit_sink` が見えるので、ツールが自前で `with_audit` を呼べる。上の 2 が崩れる |
| `Runtime::audit_sink` を private にして dispatch だけに見せる | Rust の可視性はモジュール単位で、`pub(in crate::dispatch)` は**祖先モジュールにしか書けない**（`startup` は `dispatch` の祖先ではない）。`Runtime` を `dispatch` の下に移すと合成ルートの置き場が歪む。「渡さない」（`ToolContext`）で同じ効果が得られる |
| 未知のツール名を `isError: true` + `rejected` で返す（§9 の初版） | `rmcp` の `ToolRouter` は未登録名をハンドラに到達させず、`invalid_params: tool not found` を返す。これは §6 が認めている唯一のプロトコルエラー（「ツール呼び出しに到達できない異常」）であり、PR-D の `server.rs` doc も既にそう書いていた。§9 の記述の方を §6 に合わせる。**§9 が本当に守りたい不変条件**（`audit_log.tool` にクライアント由来の文字列を載せない）は `ToolName` が型で保証しており、そちらは失われない |
| `ToolName` を `&'static str` の定数表にする | 「手で維持する一覧は必ず腐る」（`PROGRESS.md` Phase 1 の教訓6）。レジストリを引く述語1本にすれば、ツールが増えても一覧だけが古くなることが起きない |
| `is_registered_tool` が毎回 `tool_router()` を組み立てる（PR-D のまま） | ツール呼び出しのたびに11ツール分の入力スキーマ生成が走る。`OnceLock` の集合に変えた（**導出元は `tool_router().list_all()` のまま**で、両者が一致することは `server.rs` のテストが `has_route` と突き合わせる） |
| `Runtime::composition` を `Composition` のまま持つ | `with_tx_err` のクロージャは HRTB で `'static` でない借用をキャプチャできない（`crates/kaikei-app/src/tx.rs` の doc）。素の値だと**記帳のたびに税区分マスタ一式を `clone`** することになる。`Arc<Composition>` に変えた |

**理由**: `ROADMAP.md` Phase 3 の完了条件は「全操作が audit_log に記録される」
であり、これは**書き忘れが正常系のテストで検出できない**種類の要件である。
D-076 が `kaikei-app` の中でその手順を1関数に閉じたのと同じ理由で、
presentation 層でも「呼ぶ場所」を1つに閉じる必要がある。

**トレードオフ**: `rmcp` のツールマクロが使えないぶん、ツール定義に
`impl McpTool` の定型（`NAME` / `DESCRIPTION` / `type Input`）を書く。
マクロが doc コメントから説明文を採る機能は使えないので、説明文は
`DESCRIPTION` に書く（`CLAUDE.md` §10 / §11 の規律は同じく及ぶ）。
1ツールあたり数行のコストで、11ツール分の書き忘れ経路が消える。

**訂正注記1（レビュー B-1。この決定の中心的な主張が成立していなかった）**:
初版は「`ToolRoute` を組み立てるのは `dispatch::route` だけ」で登録経路を
閉じたつもりだったが、**閉じていなかった**。`rmcp` の `ToolRouter` は
`with_route` のほかに `with_async_tool::<T>()` / `with_sync_tool::<T>()` を
持ち、`ToolBase` + `AsyncTool` を実装した型はそこからハンドラ本体ごと
ルータに載る。その形は `ToolRoute` / `CallToolResult` / `with_audit` /
`AuditCall` / `audit_sink` / `tool_handler` / `#[tool(` の**どれも書かない**ので、
`tests/audit_is_structural.rs` の走査を全て素通りする
（レビュー2名が独立に再現。`get_settings` を名乗る probe が `tools/list` に
載り、`tools/call` すると `audit_log` に1行も残さずに実 DB へ記帳できた）。
しかも `rmcp` 3.1 の module doc は「1ツール1ファイルならこの形」と
**勧めており**、PR-G / PR-H の実装者が最も踏みやすい経路だった。
この決定の却下表は `AsyncTool` を「`invoke` の失敗値が `ErrorData` 固定」を
理由に却下していたが、**却下しただけでは使えないままにならない**
（読み取り系・成功前提のツールなら普通に書ける）。

そこで **`ToolRouter` を `dispatch.rs` の private フィールドに閉じ込め**、
外には `dispatch::ToolRegistry`（ツールを載せる口が `with::<T: McpTool>` しか
無い型）だけを見せる形に変えた。`server.rs` は `McpTool` の型を並べるだけに
なり、**`ToolRegistry` を経由して** `McpTool` 以外を載せる方法は無くなった。
`route` は private にした。走査（second line）にも `ToolRouter` /
`with_async_tool` / `with_sync_tool` / `AsyncTool` / `SyncTool` / `ToolBase` /
`IntoToolRoute` / `CallToolHandler` を追加した。

**この注記の「型として存在しない」という当初の書き方は誤りだった**
（訂正注記3）。閉じたのは「`ToolRegistry` 経由の登録」であって、
「別のルータを新しく作ること」ではない。

**訂正注記2（レビュー C-6。提供していない保証を書いていた）**:
初版の表 6 は「`AuditCall.tool` に渡せる型を `dispatch::ToolName` に限定した」と
断定していたが、`AuditCall` は凍結済みの `kaikei-app` にあり、そのフィールドは
`pub tool: &'a str` である（この PR で `kaikei-app` のこの型は変えていない）。
`dispatch.rs` の中で `tool: T::NAME` と直接書くことは型としては妨げられて
いない。実際の担保は「`AuditCall` という識別子を `dispatch.rs` に閉じ込める
走査＋その1ファイル内で `ToolName::resolve` を通す慣行」であり、
**型で閉じているのは `ToolName::resolve` が登録済み以外を弾くこと**である。
表と `docs/07-mcp-server.md` §9 の記述を実態に合わせた。

**訂正注記3（レビュー3巡目 B / C-1 / D-4。禁止リストを許可リストへ反転する）**:

訂正注記1 で塞いだつもりの登録経路が、**同じ走査でもう一度破られた**。
`rmcp-macros` 3.1.0 の `#[tool_handler]` は
`if !has_method("call_tool", &item_impl)` で条件付き生成する。つまり
**同じ impl ブロックに `call_tool` を手書きすると、マクロが生成する
dispatch 経路が黙って置き換わる**。実測では `tools/list` は正規の2件のまま
（見た目は正常）、`tools/call` を1回送っただけで `journal_entries` に1件・
`audit_log` に0行で、`cargo build` / `clippy -D warnings` /
`cargo test -p kaikei-mcp` は全緑だった。使う識別子
（`call_tool` / `CallToolRequestParams` / `CallToolResponse` /
`ToolCallContext` / `into_call_tool_result`）は `CONFINED` 15件のどれにも
入っておらず、`tool_handler` 自体は `server.rs` で許可されていた。
しかもその impl は既に `get_info` を手書きしており、「メソッドを自分で書けば
マクロが引き下がる」形が**見本として目の前に置かれていた**。
レビュアーはさらに `ToolRouter::add_route` / `merge`、`IntoToolRoute` の
`WithToolAttr` / `ToolAttrGenerateFunctionAdapter`、`CallToolHandlerExt`
（`mentions` の識別子境界判定により既存の `CallToolHandler` 規則に
**一致しない**）が未了であることも挙げた。

**識別子の禁止リストは原理的に不完全である。** `rmcp` が API を1つ増やす
たび、レビュアーが1つ見落とすたびに穴が開く。2巡続けて破られた時点で、
足りないのは識別子ではなく**方式**だと判断した。

そこで向きを反転し、**`crates/kaikei-mcp/src/` のうち `rmcp` という識別子を
書いてよいファイルを許可リストで限定する**ことにした。

| ファイル | なぜ必要か |
|---|---|
| `dispatch.rs` | ルータ、`ServerHandler` の実装（`call_tool` / `list_tools` / `get_tool` / `get_info`）、stdio トランスポートの起動（`serve_stdio`） |
| `error.rs` | `ToolError::into_call_tool_result`（ツール結果エラーの組み立て。D-071） |

MCP のツールを登録・実行する能力はすべて `rmcp` の API から来るので、
**どの API を使う迂回であっても `rmcp` の名前が必要**であり、迂回は必ず
許可された2ファイルのどちらかに現れる。「`rmcp` が将来 API を増やす」
「レビュアーが見落とす」に対して強い。MC-30（`kaikei-mcp` の依存の許可
リスト）や `tests/forbidden_tools.rs` の
`every_registered_tool_is_one_of_the_eleven_phase_3_tools`（禁止4件だけで
なく許可11件の側からも閉じる）と**同じ形**であり、このプロジェクトが既に
採っている方式である。

あわせて **`#[tool_handler]` を外し、`ServerHandler` の4メソッドを
`dispatch.rs` で手書き**した。生成物と手書きが入れ替わるという事象そのものが
起きなくなり、「`call_tool` を手書きする」形自体が許可リストの内側に入る。
`serve_stdio` も `server.rs` から `dispatch.rs` へ移した（stdio トランス
ポートを名指しするには `rmcp` を書く必要があるため）。
`server.rs` に残るのは「どのツールを登録するか」と「AI に何と名乗るか」だけで、
`rmcp` の型は `dispatch` が再輸出する `ServerInfo` / `Tool` /
`Implementation` / `ServerCapabilities` を借りる（**登録経路に関わる型は
再輸出しない**。識別子の閉じ込め `CONFINED` をその second line として
残してある。**これが「唯一の穴」だという当時の記述は誤りだった**——
訂正注記4 の表を参照）。

**「型で閉じた」という記述をすべて撤回する**（3巡目 C-1）。`rmcp` は
`kaikei-mcp` の直接依存で `ToolRouter` は `pub` であり、**同一 crate の他
モジュールから import を妨げる仕組みは Rust に無い**（レビュアーが実際に
コンパイルを通している）。`dispatch.rs` の「`ToolRouter` を名指しできない
以上、`with_async_tool` を呼ぶ相手が無い」、`server.rs` の「`ToolRouter` は
このモジュールから見えない」、この決定の「型として存在しない」、
`docs/07-mcp-server.md` の「『監査ログを通らないツール』は書けない」は
**いずれも成立していなかった**。止めているのは検査であって型ではない。
訂正注記2（`AuditCall` / `ToolName`）と同じ扱いで、実態どおりに書き直した。

**`every_known_rmcp_registration_path_is_confined` の「1つ残らず」も撤回した**
（3巡目 D-4）。網羅を担うのは許可リストであり、識別子の一覧は再輸出という
穴に対する second line である。テスト名も
`the_second_line_rules_for_rmcp_identifiers_are_still_present` に改めた。

**実測**（4種類の迂回を実際に書いて確かめた。確認後は復元済み）:

| 迂回 | 結果 |
|---|---|
| `#[tool_handler]` の impl に `call_tool` を手書きし、`runtime.store` + `with_tx_err` + `post_entry::execute` で実 DB に記帳する | `cargo build` は通るが `rmcp_is_named_only_in_the_files_allowed_to_name_it` が `server.rs` の3行を挙げて落ちる |
| `ToolRouter::add_route` / `merge` | 同上（`server.rs` の `use rmcp::...` と本文） |
| `CallToolHandlerExt` | 同上。**識別子の閉じ込めはこれを捕まえない**（`CallToolHandlerExt` は `CallToolHandler` 規則に一致しない）。許可リストだけが捕まえた |
| `with_async_tool` / `with_sync_tool` / `(Tool, handler)` タプル（1巡目に塞いだ形の退行） | 同上（`tools/probe_regression.rs`） |

**正規経路が壊れていないことも実 DB で確かめた**（使い捨て DB を作って
`tools/call` を1回送り、確認後に DROP）。`#[tool_handler]` を外して手書きに
した `call_tool` は `dispatch::call` に届いており、`journal_entries` に1件・
`audit_log` に `started` / `ok` の2行（`tool='post_journal_entry'`,
`actor='mcp'`）が残った。2巡目の迂回が示した「1件・0行」と対照になる。
未登録のツール名は `-32602 tool not found`（§6 が認める唯一のプロトコル
エラー）のままである。

**訂正注記4（レビュー4巡目 A / B / C-1 / D。担保の主役を走査から
振る舞いへ移す）**:

訂正注記3 で「どの API を使う迂回であっても `rmcp` の名前が必要なので、
迂回は必ず許可された2ファイルのどちらかに現れる」と書いたが、**それは
走査がそのファイルを読んだ場合の話だった**。3巡目の迂回は

```rust
#[path = "../probe_handler.rs"] mod probe_handler;   // src/ の外
include!("probe_handler.inc");                        // .rs ですらない
```

で、当時の走査（`CARGO_MANIFEST_DIR/src` を再帰し `.rs` だけを読む）は
**その2つのファイルを一度も読まなかった**。監査ログを通らない別の
`ServerHandler` を `main.rs` から**実際に待ち受けさせた**状態で、
`cargo build` / `clippy -D warnings` / `fmt --check` /
`cargo test -p kaikei-mcp` が全緑だった。**独立した2名のレビュアーが同じ穴に
到達している。**

3巡とも同じ失敗である——**ソース走査で「書けない形」を作ろうとし、そのたびに
走査の外側から破られた**。走査は「ソースがどう書かれているか」しか見られない
ので、書き方を変える迂回に対して原理的に後手に回る。

そこで**網羅の担い手を走査から振る舞い検査へ移した**。
`crates/kaikei-e2e/tests/mcp_stdio_server.rs`（`pg-tests`）が

1. `#[sqlx::test]` で使い捨てDBを作り、
2. **実バイナリ**（`cargo build -p kaikei-mcp` の成果物）を子プロセスとして
   起動し、`APP_DATABASE_URL` でそのDBを指させ、
3. stdio で `initialize` → `notifications/initialized` → `tools/call` を送り、
4. `journal_entries` と `audit_log` を **SQL で数える**

という形で、プロトコルの入口から監査ログまでを1本に通す。

| 見るもの | 期待 |
|---|---|
| `post_journal_entry`（成功） | 帳簿1件・`audit_log` に `started` / `ok` の2行・`entry_id` が応答と一致 |
| `post_journal_entry`（貸借不一致） | `isError: true`・帳簿0件・`audit_log` に `started` / `error` の2行 |
| `reverse_journal_entry`（成功・失敗） | 同じ経路を通る（ツールごとに迂回できない） |

識別子が何であれ、ファイルがどこに在ろうと、別の入口から来ようと、
**監査ログが2行無ければ落ちる**。走査は「**書いた瞬間に、DB 無しで、手元で
落ちる**」二線目として残す（`PROGRESS.md` Phase 2 の教訓「手元で回せない
検査は回されなくなる」）。

置き場が `kaikei-e2e` なのは `kaikei-mcp` が `sqlx` を持てない（MC-30）ため
であり、その代償として **`CARGO_BIN_EXE_kaikei-mcp` が使えない**
（同じ package のテストにしか渡らない）。`cargo` を入れ子で起動すると
target ディレクトリのロックで詰まりうるので、テストは target ディレクトリ
から実行ファイルを辿り、**無い場合・`crates/kaikei-mcp/` のどのファイルより
古い場合はその旨を書いて落ちる**（古いバイナリに対して緑になると、この検査は
何も保証しない）。CI は `database.yml` に `cargo build -p kaikei-mcp` の
ステップを足して直前でビルドする。

**走査の穴も塞いだ**（4巡目 B）:

- 走査対象を `src/` 配下の**全ファイル**に広げた（拡張子で絞らない）
- それでも `#[path]` は `src/` の外を指せるので、**`#[path` と `include!(` が
  crate に現れないこと**を別に検査する（空白を落としてから見るので
  `#[ path = ".." ]` / `include !(..)` も捕まる。`include_str!` /
  `include_bytes!` は資源の埋め込みなので落とさない）
- 同じ走査が `audit_is_structural.rs` と `stdout_is_json_rpc_only.rs` に
  **複製**されており、片方の穴を塞いでももう片方に残る形だった。
  `crates/kaikei-mcp/tests/source_scan/mod.rs` に集約し、両方が使う
  （stdio では stdout に1行混ざれば接続ごと壊れるので、`println!` を走査の外の
  ファイルに置ければ同じ事故が起きる）

**「許可リストの唯一の穴」という記述を撤回する**（4巡目 C-1）。
穴は少なくとも3つある。

| 穴 | 何ができるか | 見張り |
|---|---|---|
| `dispatch.rs` が `rmcp` の型を再輸出する | 他のファイルが `rmcp` を名指しせずに登録経路へ届く | `CONFINED`（second line） |
| 走査の外にファイルを置く | crate の一部なのに一度も読まれないファイルに何でも書ける | `assert_no_out_of_tree_inclusion`（今回追加） |
| 許可された `dispatch.rs` の中に**別のプロトコル入口**を足す | `ServerHandler` は `call_tool` 以外にも既定実装を持つ（`read_resource` / `get_prompt` / `complete` / `get_task` / `update_task` / `cancel_task` …）。そこに書けば `tools/call` を通らずに操作を書ける | **無い**（`dispatch.rs` を全面的に許すという判断の帰結。設計上の想定内で diff には出る） |

3つ目は許可リストの内側なので走査では原理的に見張れない。
**網羅は主張しない。網羅を担うのは振る舞い検査である。**

`crates/kaikei-mcp/src/dispatch.rs` 冒頭の表にも「手段」列を足した
（4巡目 D。`docs/07-mcp-server.md` §4 と本項の表には既に足してあったが、
**上から読む読者が最初に読む**その表だけが「呼び忘れる形が存在しない」で
無条件に締めていた）。

**実測**（3巡分の迂回をそれぞれ実際に書き、`cargo build -p kaikei-mcp` し
直したうえで振る舞い検査を走らせた。確認後は復元済み）:

| 迂回 | 走査 | 振る舞い検査 |
|---|---|---|
| `dispatch.rs` の `call_tool` を手書きで差し替え、`with_tx_err` + `post_entry::execute` で直接記帳（2巡目の形。**許可リストの内側**） | **緑のまま**（`dispatch.rs` は `rmcp` を名指してよいファイル） | **落ちる**（帳簿1件・`audit_log` 0行） |
| `#[path = "../probe_handler.rs"] mod probe_handler;` で走査の外に別の `ServerHandler` を置き、`main.rs` からそれを待ち受けさせる（3巡目の形） | 落ちる（`#[path` の検査。**4巡目で追加した分**） | **落ちる**（帳簿1件・`audit_log` 0行） |
| 同じものを `include!("probe_handler.inc")` で取り込む（3巡目の形） | 落ちる（`include!(` の検査。**4巡目で追加した分**） | **落ちる**（帳簿1件・`audit_log` 0行） |

1行目が「走査は緑・振る舞いは赤」になっていることが、**担い手を移した理由
そのもの**である。

---

## D-085 ツールの入力は dispatch 層が操作の中でデシリアライズする（タグの重複キーは MCP 経由では検出できない）

**決定**:

1. **`rmcp` の `Parameters<T>` を使わず、`dispatch::call` が
   `serde_json::from_value::<T::Input>` を `with_audit` の**操作の中**で行う。**
   デシリアライズの失敗（金額を JSON number で渡した等）は
   `codes::REJECTED` のツール結果エラーになり、**開始・結果の2行として
   監査ログに残る**（`docs/07-mcp-server.md` §10 MC-09 の (3)）。
2. 入力 DTO は **`#[serde(deny_unknown_fields)]`** にする。
   `post_journal_entry` に `document_ids` を渡すと拒否される（黙って捨てない）。
3. 線上の `tags` は **`BTreeMap<String, String>`** で受ける。
   **MCP 経由では重複キーを検出できない**（後勝ちになる）ことを制約として
   `docs/07-mcp-server.md` §3 と `inputSchema` の説明文に明記する。
   タグの型付けと未登録キーの判定は
   `kaikei_jp::tags::TagCatalog::parse_tag_set` に委ねる（従来どおり）。

**却下した選択肢**:

| 候補 | 却下理由 |
|---|---|
| `Parameters<T>` で受ける（`rmcp` の標準的な書き方） | デシリアライズが**ツール本体に入る前**に走り、失敗は `dispatch::call` の外側で `CallToolResult::error`（テキストのみ）に変換される。つまり **audit_log に1行も残らない**。MC-09 の (3) は PR-F の担当と設計書に明記されている。構造化コンテンツ（`error` / `message`）で返せなくなるという副次的な損失もある |
| 未知のフィールドを黙って無視する（serde の既定） | `document_ids` を付けて「証憑を紐付けたつもり」で記帳が成功する。証憑の紐付けは Phase 4（`attach_document`）であり、現状 `PgTx::insert_entry` は `document_refs` 非空を拒否する（D-041）。**入力を黙って落とさない**のは `CLAUDE.md` §4 と同じ姿勢である |
| `tags` を出現順のペア列（`TagPairs`）で受けて重複を保持する（初版の決定3） | **本番経路では一度も効いていなかった。** 訂正注記を参照 |
| 重複キーの検出を MCP 層で書く | 同じ判定が2箇所に育つ（D-072）。`TagCatalog::parse_tag_set` が既に持っている。そもそも MCP 層には重複が届かない（訂正注記） |
| 生の JSON テキストを MCP 層まで持ち込んで自前でパースする | `rmcp` の stdio トランスポートを自前に置き換えることになる。プロトコルの取り扱い（フレーミング・初期化の折衝・通知）を自分で持つ代償に対して、得られるのは「タグの重複キーを検出できる」1点だけである |
| デシリアライズのエラー本文も日本語で書き起こす | 金額（`AmountStr`）のように**会計上の実害に直結するもの**は D-079 で日本語にしてある。一方「未知のフィールド」「必須フィールドの欠落」は serde が有効な候補を列挙した英語を返す。これを全て日本語にするには入力 DTO ごとに `Deserialize` を手書きすることになり、DTO が増えるほど腐る。**ツール名と「金額は文字列」を添えた日本語の包み**を付け、詳細は serde の文言のままにした（`CLAUDE.md` §11 の「次の手」は包みと `inputSchema` で満たす） |

**理由**: 「AI が何をしようとしたか」を最も知りたいのは失敗したときである
（D-070）。入力が壊れていた呼び出しこそ記録に残す価値があり、
それを取りこぼす境界が `Parameters<T>` だった。

**トレードオフ**: `input_schema` の生成を `dispatch::route` が自分で行う
（`schema_for_input::<T::Input>()`）。`rmcp` の `ToolBase` が
`Parameters<Self::Parameter>` に対して生成するのと同じ結果になる
（`Parameters<P>` の `JsonSchema` は `P` へ委譲する実装であることを確認済み）。

**訂正注記（レビュー B-2。決定3を撤回する）**:
初版の決定3（`TagPairs` で重複キーを保持し
`JpError::DuplicateTagKeyInInput` に到達させる）は**成立していなかった**。
`rmcp` の `CallToolRequestParams::arguments` は `Option<JsonObject>`
（＝ `serde_json::Map`）で、**JSON-RPC メッセージ全体が `serde_json` で
パースされる時点、つまり `dispatch::call` に入る前に重複が潰れる**。
ツール側の受け型を何にしても、届く時点で既に1件になっている。
実測では、両方とも有効な税区分を重複指定すると後勝ちで黙って採用され
記帳が成功した（`SALES_10` → `SALES_8_REDUCED`。税率が無言で入れ替わる）。
`audit_log.input` にも畳み込み後の値しか残らない。
`wire.rs` の単体テストが緑だったのは `serde_json::from_str` に生のテキストを
渡す**本番と違う入口**を通していたためである（本番経路を再現していない
緑のテスト。**誤診は誤値と同じ実害を持つ**——`PROGRESS.md` Phase 1 の教訓3）。

採った方針は「**提供できない保証を主張している記述と機構を消す**」である
（PR-B が `contract_from_downstream.rs` の doc で提供できない保証を撤回したのと
同じ判断）。

- `crate::wire::TagPairs` を**削除**した。効いていない機構を残すと、
  次の実装者が「重複は防がれている」と誤解する
- `tags` は `BTreeMap<String, String>` で受ける
- 制約（**MCP 経由では `tags` の重複キーを検出できない。後勝ちになる**／
  **`JpError::DuplicateTagKeyInInput` は MCP 経由では到達不能**。他の
  呼び出し元からは到達する）を `docs/07-mcp-server.md` §3 に明記した。
  `inputSchema` の `tags` の説明文にも「同じキーを2回指定した場合、
  後に書いた指定だけが使われます」と書いてある
- `wire.rs` のテストは「重複キーは**この層に届く前に**畳み込まれている」ことを
  固定するものに書き換え、**何を検証していないか**を doc に明記した
