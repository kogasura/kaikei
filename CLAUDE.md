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
