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
