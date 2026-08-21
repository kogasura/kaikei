# CLAUDE.md — 作業規律

このプロジェクトで作業する際、以下を厳守すること。
迷ったら実装を止めて確認を求める。**会計データは間違うと実害が出る。**

---

## 1. 依存方向の鉄則

```
kaikei-core   ← 何にも依存しない
     ↑
kaikei-policy ← trait定義のみ。coreの型を使う
     ↑
kaikei-jp     ← policyを実装。jp-dataを読む
     ↑
kaikei-app    ← core + policy(trait) に依存。jpは注入される
     ↑
kaikei-store / kaikei-blob / kaikei-import / kaikei-api / kaikei-mcp
```

### 禁止事項

- `kaikei-core/Cargo.toml` に依存を追加すること
  （許可されるのは `rust_decimal`, `thiserror` のみ。増やす場合は人間の承認が必要）
- `kaikei-core` から `sqlx` / `tokio` / `serde_json` / `chrono` の型を参照すること
- `kaikei-core` に「消費税」「軽減税率」「青色申告」「勘定科目の日本語名」を書くこと
- `kaikei-policy` の trait を `async fn` にすること（理由は §3）
- `kaikei-import` から `kaikei-core` に依存すること（別コンテキスト。§5参照）

依存を追加したくなったら、それは設計を疑うべきサイン。
CI (`.github/workflows/architecture.yml`) がこれを機械的に検査する。CIを無効化しないこと。

---

## 2. append-only の絶対性

帳簿本体（`journal_entries`, `journal_lines`）に対して：

- `UPDATE` / `DELETE` を発行するコードを書かない
- 更新系のメソッド（`update`, `delete`, `edit`, `modify`）を `JournalEntry` に生やさない
- 訂正は `JournalEntry::reverse()` による逆仕訳（赤伝）のみ

これは実装の都合ではなく、電子帳簿保存法の「訂正削除の履歴」要件を
構造的に満たすための設計。破ると本プロジェクトの存在意義が消える。

### マイグレーションの掟

1. **既存の仕訳行を書き換えるマイグレーションを書かない**
2. カラム追加は NULL 許容のみ
3. 開発中の壊れたデータは DB を丸ごと作り直す。1行だけ UPDATE して直そうとしない
4. タグの意味が変わる場合、新キーを作って旧キーは残す（`tax_category` → `tax_category_v2`）

「1件だけ UPDATE すれば直る」という誘惑が何度も来る。DB権限で塞いであるのはそのため。

---

## 3. policy trait は純関数を保つ

```rust
// ✅ 正しい
fn validate_tag(&self, ctx: &TaxContext<'_>, tag: &TagSet, account: &AccountDef)
    -> Result<(), PolicyError>;

// ❌ 禁止
async fn validate_tag(&self, tag: &TagSet, repo: &dyn CounterpartyRepo)
    -> Result<(), PolicyError>;
```

必要なデータは呼び出し側（`kaikei-app`）が事前にロードし、`TaxContext` に詰めて渡す。

**I/O は application 層だけが行う。**
これを崩すとテストが重くなり、core の純粋性が段階的に失われる。
一度崩れると取り戻せない類の規律。

---

## 4. TagSet はゴミ箱ではない

`TagSet` は core が意味を解釈しない不透明な袋だが、**スキーマ検証は core が行う**。

- `JournalEntry::new` は `&TagSchema` を受け取り、未登録キーを拒否する
- 新しいタグキーが必要になったら `kaikei-jp-data/tags.yaml` に登録する
- **金額に影響する情報をタグに入れない**（貸借一致の検証を迂回できてしまう）
- 集計軸に使うキーは `aggregatable: true` を宣言する

---

## 5. 境界づけられたコンテキストは2つある

| コンテキスト | 語彙 | crate |
|---|---|---|
| 記帳 | 仕訳、勘定科目、借方/貸方、試算表 | core, policy, jp, store |
| 取引明細取込 | 取引、入金/出金、摘要、未処理 | import |

`ImportedTransaction` と `JournalEntry` は別の言語圏の住人。
「入金/出金」と「借方/貸方」は似ているが同じではない（借方は資産増加も費用発生も表す）。

両者を直接変換せず、`kaikei-app/usecase/journalize.rs` を翻訳層とする。
`kaikei-import` が `kaikei-core` に依存していないことがこの分離の証拠。

---

## 6. フォルダ分割の原則

### crate 内は「ドメイン概念」で切る

```
✅ journal.rs, account.rs, money.rs, period.rs
❌ entities/, value_objects/, repositories/, services/
```

DDDのパターン名でフォルダを切るのはアンチパターン。
`entities/` と `value_objects/` は技術的分類でありユビキタス言語ではない。
「これはエンティティか値オブジェクトか」という生産性ゼロの議論を招く。

### 集約は1モジュールに収める

Rust の可視性はモジュール単位。`JournalEntry` の private フィールドを守るには
同一モジュールに閉じる必要がある。ファイルを分けて `pub(crate)` に緩めてはいけない。

1000行を超えたら `journal/mod.rs` + `journal/line.rs` + `journal/validate.rs` に分割し、
private フィールドを触るコードは `mod.rs` に集める。

### application 層はユースケース単位で縦に切る

```
usecase/post_entry.rs, reverse_entry.rs, import_csv.rs, journalize.rs, ...
```

`AccountingService` のような巨大な構造体を作らない。ユースケース1つ = 1ファイル = 1関数。
構造体のメソッドではなく関数にすることで、依存が引数に全部現れる。

### read model は物理的に分離する

`kaikei-store/src/query/` に置く。Repository を通さず SQL から DTO へ直行する。
書き込みはドメインモデル経由、読み取りは SQL 集計。混ぜない。

---

## 7. 日付と時刻

- **取引日（`entry_date`）は `DATE`。タイムゾーンを持たせない**
- **記帳時刻（`recorded_at`）は `TIMESTAMPTZ`。UTC保存**
- 混ぜると年度跨ぎで必ず事故る
- 現在時刻は `Clock` trait 経由で注入する。`Utc::now()` を core / policy 内で直接呼ばない
- 年度別データの選択は**取引日**で行う。記帳日ではない

---

## 8. 金額

- `f64` を金額に使わない。例外なし
- 内部表現は最小通貨単位の整数（`i128`）+ 通貨
- 通貨ごとに小数桁が違う（JPY=0, USD=2, KWD=3）。「金額=セント」の前提を置かない
- 異通貨の加算は `Result` で弾く（型パラメータ化はしない。理由は `DECISIONS.md`）
- 端数処理は `TaxPolicy::round` 経由。切捨/切上/四捨五入は設定可能

---

## 9. 実装の進め方

- **Phase 0 (`kaikei-core`) が完成し全テストが通るまで、他の crate に着手しない**
- テストは `docs/02-test-cases.md` の一覧を先に全部書く（失敗する状態でよい）
- 1コミット1論点。「型を追加」と「テストを追加」を混ぜない
- 仕様が曖昧な箇所は実装せず、`docs/` に疑問として書き出して人間に返す

---

## 10. 表現に関する禁止事項

コード内コメント、ドキュメント、エラーメッセージにおいて：

- 「電子帳簿保存法に準拠」「法令対応済み」「JIIMA認証相当」と書かない
- 書いてよいのは「〜の機能要件を意識した設計」まで
- 税務判断を断定するメッセージを出さない（「この経費は損金です」等）
- 提案系の機能は候補と根拠を返し、確定は人間に残す

---

## 11. エラーメッセージの設計

MCP 経由で AI が自己修正できる形にする。

```
❌ "Unbalanced entry"
✅ "貸借不一致: 借方 110,000 / 貸方 100,000（差額 10,000）。
    仮受消費税の計上漏れの可能性があります。"
```

次の手が分かる文言にすること。これは MCP サーバーの品質を左右する。

---

## 12. コミット・PR に AI 帰属表示を含めない

このリポジトリでは、コミットメッセージや PR 本文に以下のような AI 帰属表示を含めない:

- `Co-Authored-By: Claude ...` トレーラー
- `🤖 Generated with Claude Code` 等のフッター
- `noreply@anthropic.com`
- `claude.com/claude-code` へのリンク

`.github/workflows/commit-hygiene.yml` が PR のコミットメッセージと本文を検査し、
該当パターンがあれば CI を失敗させる（マージ不可になる）。コミットする際は
このトレーラー・フッターを付けないこと。

---

## 13. CI が通らないものはマージしない

`main` はブランチ保護下にあり、以下が強制される。**この設定を緩めないこと。**

- PR 経由でしかマージできない（直 push は拒否される）
- 必須チェックが**全て成功**するまでマージできない（1つでも失敗・実行中ならブロック）
- `enforce_admins` が有効。リポジトリオーナーもバイパスできない
- force push とブランチ削除は禁止。履歴は線形に保つ

### 必須チェックに登録されているジョブ

| ジョブ | 定義元 | 検査内容 |
|---|---|---|
| `dependency-direction` | `architecture.yml` | core の依存方向、append-only、f64 禁止 |
| `quality` | `architecture.yml` | fmt / clippy -D warnings / test |
| `cargo-deny` | `supply-chain.yml` | 脆弱性・ライセンス・依存元 |
| `no-ai-attribution` | `commit-hygiene.yml` | §12 の AI 帰属表示 |
| `database` | `database.yml` | マイグレーション適用、`.sqlx` の陳腐化、append-only の権限・トリガ実効性（`pg-tests`） |
| `no-real-data` | `no-real-data.yml` | §14 / §15。形式で分かる機密と、「実帳簿」＋具体的な数の同居 |

> `no-real-data.yml` が `.github/workflows-staged/` にある間は**検査が動かない**。
> `.github/workflows/` へ移して、上の `gh api` で必須チェックに登録すること。
> 移し終えたらこの注記も消す。

### 新しい CI ジョブを追加したときの掟

**必須チェックのリストは手動管理であり、ジョブを増やしても自動では追加されない。**
登録を忘れると、そのジョブが失敗してもマージできてしまい、CI を置いた意味が消える。

新しいジョブを追加したら、**同じ PR の中で**必須チェックにも登録すること:

```bash
gh api repos/kogasura/kaikei/branches/main/protection/required_status_checks/contexts \
  -X POST -f 'contexts[]=<新しいジョブ名>'
```

登録後は上の表にも行を追加する。`gh api repos/kogasura/kaikei/branches/main/protection --jq '.required_status_checks.contexts'`
で現在の登録状況を確認できる。

---

## 14. 実データを公開リポジトリに持ち込まない

`kogasura/kaikei` は public であり、`main` は force push を禁じている（§13）。
**一度 push した実データは取り消せない。** 扱う対象が帳簿である以上、これは最も
戻せない種類の事故になる。

PR を出す前に、次の2点を確認する（`.github/pull_request_template.md` の
「公開してよい内容か」欄にチェックを入れる）:

1. **個人情報・機密情報を載せていないか**
   PR 本文、コミットメッセージ、コード・コメント、テストデータ、貼り付けたログや
   スクリーンショットのすべてが対象
2. **具体的な金額・取引先がコミットに含まれていないか**
   `git log -p origin/main..HEAD` で目視する

### 実データで確認したときの書き方

動作確認に検証帳簿を使うのは構わない。**その出力をそのまま貼るのが駄目である。**
架空の値に置き換えるか、金額と取引先名を伏せてから貼る。挙動を示すのに実額は要らない。

```
❌ 検証帳簿を動かした出力をそのまま貼る（仕訳件数・金額・取引先名がそのまま載る）
✅ 検証用の帳簿で同じ状態を再現し、その出力を貼る
```

**この規律の例示に実データを使わないこと**（説明のためなら許される、とはならない）。

テストデータの取引先名・摘要も架空のものを使う。実在の社名を「たまたま知っているから」
という理由で書かない。

---

## 15. 既に入っている実データの書き換え方

§14 は「持ち込まない」ための規律である。**既に入っているものを取り除くとき**は、
消し方を揃えないと同じ数字が別のファイルに残る。

### 置き換えの型

| 対象 | 置き換え方 |
|---|---|
| 事業者 | 名前を付けず「検証帳簿」と呼ぶ |
| 取引先 | `株式会社ABC` / `DEF株式会社` のような明らかな架空名 |
| 摘要 | `カ)サンプル シヨウジ` のような架空の店名 |
| 取り込み元 | `example_bank` / `example_card`（実在の銀行・カード名を使わない） |
| 金額 | **桁と演算関係だけを本物に似せた架空値** |
| 件数 | 「大半が」「数百件」のような規模の言い方 |

### 金額は演算関係ごと差し替える

貸借一致・按分の端数・税額の割戻し・償却の残高——**テストが確かめたい性質は
架空値でも再現できる。** 数字を1つだけ変えると合計や差額と食い違い、テストが
落ちるか、落ちないまま説明と食い違う。同じ額が複数のファイルに出ることがあるので、
リポジトリ全体で同じ対応表を当てる。

### 根拠は残す

消すのは「どの帳簿の、いくらの話か」であって、「実運用でこの誤りが起きた」という
事実そのものではない。**なぜその検査を置いたか**が消えると、後から読んだ人が
検査を不要と判断して外してしまう。

### CI が見ているもの（`no-real-data`）

**「実データを検出する」検査は作れない。** `105,600` が架空値か実額かは、値
そのものからは判定できない。**除去済みの値を並べた denylist も置かない**——
それを public リポジトリに置くのは本末転倒である（ハッシュ化しても、社名や
金額の探索空間は総当たりで戻せる）。

代わりに、これまでの漏洩がすべて持っていた**2つの形**を見る。

| 検査 | 何を見るか | 落ちたときの直し方 |
|---|---|---|
| 形式で分かる機密 | 鍵・トークン・JWT・メール・電話・郵便番号・住所・ローカルパス | 消す。意図したダミーなら `ci-allow: secret-shaped` |
| 書き方の型 | 「実帳簿」の語と具体的な数が、その行の**前1行・後3行**に同居している | 架空値なら「検証帳簿」に言い換える。帳簿の数でないなら `ci-allow: real-ledger-mention`（**印もその窓の中に置く**） |

**2つ目が本命である。** 値ではなく書き方を見るので、まだ知らない実額にも効く。
逆に言えば、`実帳簿` を `検証帳簿` に書き換えるのは「この数字は架空値だ」と
**人が宣言する**行為である。数字を直さずに語だけ言い換えてはいけない。

加えて、PR が持ち込んだ非まるめの数値をジョブ要約に一覧する（落としはしない）。
`.github/pull_request_template.md` の「具体的な金額・取引先がコミットに含まれて
いない」を、目視ではなく**その一覧で**確かめるためのもの。

手元でも同じものを走らせられる。**CI で初めて落ちるのを避けること。**

```bash
python3 .github/scripts/no_real_data.py                    # 木全体
python3 .github/scripts/no_real_data.py --diff origin/main # 差分の数値も一覧する
```

### もう1つの層（`real-data-review`。**門番ではない**）

正規表現は**固有名詞を拾えない**。「株式会社ビーテック」が実在かどうかを denylist
無しにパターンで判定する方法は無く、denylist は置かない（上記）。ところが実際に
見つかった漏洩は、種類でいえば大半が固有名詞だった——事業者名・取引先名・契約
サービス名・OS のユーザー名。そこを埋めるために、PR の差分を Claude に読ませ、
**指摘があるときだけ** PR にコメントするジョブを別に置いている。

**これは必須チェックに登録しない。** 判定がブレるものを門番に据えると、落ちても
「またか」と再実行されるようになり、本物の検出まで一緒に無視される。だから

- 指摘が無ければ**黙る**（毎回「問題ありません」とは言わない）
- 鍵が無くても API が落ちていても、**終了コードは 0**
- 断定しない。**確かめる価値がある箇所を人に渡すところまで**が仕事

という設計にしてある。担当も分けてある——固有名詞は `real-data-review`、鍵・メール・
住所・「実帳簿」＋数の同居は `no-real-data`。

### 何を見るかはスキルに置く

判断の基準は `.claude/skills/real-data-review/SKILL.md` にある。ワークフローは
`anthropics/claude-code-action@v1` にそれを `/real-data-review` として渡すだけ。
**同じものを手元でも走らせられる**（Claude Code で `/real-data-review`）ので、
基準を変えたいときは PR を出す前に手元で確かめられる。

投稿の機構だけは `.github/scripts/post_review_comment.sh` に固定してある。
**判断と機構を分ける。** 何を指摘するかはモデルが決めてよいが、コメントを1つに
保つ手順は決定的でよい——モデルに `gh api` を組み立てさせると、引数を1つ間違えた
だけで黙って別のことをする。

### 動かすのに要るもの

サブスクリプション（Pro / Max / Team / Enterprise）のトークンを使う。API の
従量課金ではない。

```bash
claude setup-token                      # 手元で長期トークンを作る
gh secret set CLAUDE_CODE_OAUTH_TOKEN   # そのトークンを貼る
```

あわせて Claude GitHub App（https://github.com/apps/claude）をこのリポジトリに
入れておくこと。fork からの PR では走らない（シークレットが渡らないため）。
