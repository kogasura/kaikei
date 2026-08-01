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
