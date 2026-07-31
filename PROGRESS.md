# PROGRESS.md — 進捗の記録

`ROADMAP.md` 末尾「進捗の記録」の定めに従い、各 Phase 完了時に以下を追記する。

- 完了日
- 実際にかかった時間と見積との差
- 設計変更が必要になった箇所（`DECISIONS.md` に追記したものを参照）
- 次 Phase への申し送り
- 税理士に確認すべき事項として新たに出てきたもの

---

## Phase 0 — kaikei-core

### 完了日

2026-07-31

### 見積との差

`ROADMAP.md` の見積は「1〜2週間」。実際には 2026-07-31 の1日（同日 03:07〜07:12 の間に
関連する全 PR がマージされている）で完了した。

これは AI エージェントによる並列実装によるもので、Phase 1 以降が同じ速度で
進む保証にはならない。見積との差が大きいこと自体を、以降の Phase の
見積もり精度を疑う材料として正直に記録しておく。

マージ済み PR（`gh pr list --state merged`）:

| # | タイトル |
|---|---|
| #1 | Phase 0: kaikei-core に money.rs / error.rs を実装 |
| #2 | コミット/PRのAI帰属表示を禁止するCIを追加 |
| #3 | Phase 0: kaikei-core に account.rs / period.rs / clock.rs を実装 |
| #4 | Phase 0: kaikei-core に tag.rs を実装 |
| #5 | Phase 0: kaikei-core に journal.rs（集約）を実装 |
| #6 | Phase 0 完了: kaikei-core に trial_balance.rs（read model）を実装 |
| #7 | 監査で発見した境界バグ3件を修正（mul_ratio の積オーバーフロー / Currency の小数桁数 / 循環参照の犯人誤認） |

### 設計変更（`DECISIONS.md` 参照）

Phase 0 の途中で以下の3件を `DECISIONS.md` に追記した。いずれもコードレビューまたは
Phase 0 完了後の横断監査で発覚した欠陥への対応。

- **D-018**（`Money::mul_ratio` を `Result` 化）: コードレビューで、初期実装が
  `i128` → `rust_decimal::Decimal` の変換に `unwrap()` 相当を使っており、
  金額が `Decimal` の表現上限を超えると `mul_ratio` が panic することが発覚したため。
- **D-019**（エラーメッセージの列挙型は `label_ja()` で日本語表示）: `{:?}`（derive された
  `Debug`）をそのまま使うと英語のバリアント名がメッセージに混ざり、
  `CLAUDE.md` §11 が求める「AI が自己修正できる文言」の趣旨に反することが
  レビューで指摘されたため。
- **D-020**（`Currency::minor_unit` の上限を18に設定）: 横断監査で
  `Currency::new("XXX", 40)` のような極端な値が無検証で通り、`to_display_string` 内の
  `10u128.pow(minor_unit)` が桁あふれ（debug ビルドで panic、release ビルドでは
  無言に誤った金額を表示）することが発覚したため。

### 次 Phase への申し送り

- **(a) `rehydrate` は検証を行わない**: `JournalEntry::rehydrate` は永続化層からの復元専用で
  一切の検証をしない。壊れたデータが渡されると `currency()` / `debit_total()` /
  `credit_total()` が呼び出し時に panic する。`kaikei-store` 実装時に、DB 行から
  `rehydrate` を呼ぶ経路で追加の防御（行の整合性チェック等）を検討する。
- **(b) `test_chart()` と `sole_proprietor.yaml` の自動乖離検出は未実装**:
  `crates/kaikei-core/tests/common/mod.rs` の `test_chart()` は
  `kaikei-jp-data/chart/sole_proprietor.yaml` の科目コード・名称と手動で
  一致させているが、`kaikei-jp-data` が現時点でワークスペースメンバーでないため
  （ルート `Cargo.toml` の `members` に未登録）、両者の乖離を機械的に検出する仕組みが無い。
  `kaikei-jp-data` をワークスペースに組み込む Phase で検討する。
- **(c) 逆仕訳の `document_refs` は「常に空」で確定させた**: `JournalEntry::reverse` は
  証憑への参照を複製しない設計にしている。訂正の根拠となる証憑が元仕訳と別に
  用意されることが多いという想定によるものだが、`kaikei-app` で逆仕訳のユースケース
  （`reverse_entry`）を実装する際に、この前提が実際の運用と合っているか再検討する。
- **(d) `PeriodGuard` / `Clock` の実装は上位層の責務**: `kaikei-core` は trait のみを
  定義しており、締め状態を持つ `PeriodGuard` 実装、実時刻を返す `Clock` 実装は
  どちらも存在しない。Phase 1（`kaikei-store` / `kaikei-app`）で用意する。

### 教訓

Phase 0 完了後に実施した横断監査で、PR単位のコードレビュー6回（#1〜#6）が
すり抜けた実バグ3件が見つかった（#7 で修正）。

根本原因は共通して「**プロパティテストの生成器のレンジが仕様の許容範囲より狭かった**」こと。

- `mul_ratio` の比率生成器が `Ratio::parse_fraction`（0以上1以下限定）しか使っておらず、
  仕様上は上限の無い `Ratio::parse_rate` の経路（積が表現上限を超える経路）を
  構造的に踏めていなかった
- `Currency` の生成器が `minor_unit` を実在通貨相当の3値（0, 2, 3）でしか作っておらず、
  型（`u8`）が表現できる全域の境界（上限超過）を踏めていなかった
- 勘定科目表の循環参照テストが「純粋な循環」しか扱っておらず、循環に入る前の
  枝（尻尾）を持つケースを踏めていなかった

Phase 1 以降、プロパティテストの生成器は「型が表現できる範囲」ではなく
「仕様が許容する範囲」に合わせて設計することを意識する。

### 税理士に確認すべき事項

Phase 0 は複式簿記エンジンのみで税制を一切含まないため、この段階で新たに
確認すべき事項は無い（該当なし）。

---

## Phase 1 — kaikei-store + kaikei-app

未着手。

---

## Phase 2 — kaikei-jp

未着手。

---

## Phase 3 — kaikei-mcp

未着手。

---

## Phase 4 — kaikei-import + kaikei-blob

未着手。

---

## Phase 5 — kaikei-report

未着手。
