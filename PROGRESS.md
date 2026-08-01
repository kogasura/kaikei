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

### 完了日

2026-08-01

### 見積との差

`ROADMAP.md` の見積は「1〜2週間」。実際には 2026-07-31 09:15Z（PR #9 マージ）〜
2026-08-01（PR #16 マージ）の約1日で完了した。

Phase 0 と同じく AI エージェントによる並列実装だが、**並列化の切り方が違う**。
Phase 0 は「同一 crate 内の独立モジュール（`account` / `period` / `clock`）」で
切れたのに対し、Phase 1 は crate 間に契約（`kaikei-app` の `ports.rs`）があるため
同じ切り方ができない。**契約を直列化し、その前後を並列化する**形にした。

```
PR-1 ワークスペース + CI 拡張
      ├────────────┬──────────────┐
   PR-2 DB      PR-3 policy
                     ↓
                PR-4 app ports  ★契約凍結（ここで直列化）
      └──────┬──────┴──────┬──────────┐
        PR-5 store書込  PR-6 query  PR-7 usecase
                    ↓
              PR-8 結線 + E2E + docs
```

マージ済み PR:

| # | タイトル | 差分 |
|---|---|---|
| #9 | Phase 1 PR-1: ワークスペース骨組みと依存方向CIの拡張 | +2140/-193 |
| #10 | Phase 1 PR-3: kaikei-policy に5 trait を定義（純粋層） | +1994/-14 |
| #11 | Phase 1 PR-2: PostgreSQL基盤とappend-onlyのDB権限強制 | +1431/-45 |
| #12 | Phase 1 PR-4: kaikei-app のポート層を定義（★契約凍結点） | +1993/-3 |
| #13 | Phase 1 PR-5先行: kaikei-store の共有基盤（sqlstate / tags / convert） | +1461/-9 |
| #14 | Phase 1 PR-7: kaikei-app のユースケース3本 | +1578/-31 |
| #15 | Phase 1 PR-5本体+PR-6: kaikei-store の書き込み側と read model | +3324/-11 |
| #16 | Phase 1 PR-8: 結線と E2E、設計ドキュメントの改訂 | +1603/-127 |

### ROADMAP 完了条件の充足

証跡はすべて `crates/kaikei-store/tests/` 配下。E2E-xx は
`tests/e2e_usecase.rs`（PR-8 で追加。`kaikei-app` のユースケース関数を
実 PostgreSQL に繋いだ `PgStore` に通す）のテストID。

| 完了条件 | 証跡 |
|---|---|
| 再起動してもデータが残る | **E2E-01** `posted_entry_is_readable_from_a_freshly_reconnected_pool`（プールを張り直して読めることによる代理検証。docker volume 自体の永続性はテスト対象外である旨をコードと `README.md` に明記） |
| `kaikei_app` ロールで UPDATE を試みると失敗することをテストで確認 | `tests/append_only.rs` / `tests/privileges.rs` |
| 逆仕訳が正しく記録される | **E2E-03**（試算表上で元仕訳と相殺され、`reverses`/`reverse_reason` が往復する）/ **E2E-04**（二重訂正の拒否と副作用の不在） |
| 試算表が SQL 集計で出る | **E2E-02** / **E2E-03** / **E2E-09**、`tests/trial_balance_differential.rs`（ドメインモデル経由の集計との差分検証） |
| `UnitOfWork` を実際に使ってみて、借用チェッカとの相性を評価する | **E2E-10**（`with_tx` のロールバックで仕訳・採番が揃って巻き戻る）、`DECISIONS.md` D-029、`docs/03-database.md` §7（実際に踏んだ痛点2件と回避策） |

完了条件そのものではないが、Phase 1 の設計判断を実 DB 上で裏付けるテストも
入れてある: **E2E-05**（税額行を自動生成しても貸借が壊れない）/ **E2E-06**
（締め済み期間への記帳が DB に到達しない）/ **E2E-07**（貸借不一致は core で
止まり、DB のトリガ P0011 まで到達しない）/ **E2E-08**（D-023。失敗した記帳が
採番を消費しない）。

### 設計変更（`DECISIONS.md` 参照）

D-021 〜 D-048（D-024 は欠番。並列作業で番号が衝突し採番し直した跡）。
とくに後続 Phase に影響するもの:

- **D-025**（`TaxContext` を国非依存の4項目に限定）: `docs/04-jp-tax.md` の当初定義
  （`TaxCategoryTable` / `JpSettings` を含む）をそのまま `kaikei-policy` に置くと
  policy → jp の循環依存になり、`kaikei-app` まで jp を知る必要が出て
  `CLAUDE.md` §1 が壊れる。年度別税区分マスタと事業者設定は `JpTaxPolicy` が
  **構築時**に保持し、年度選択は `ctx.as_of` で行う形にした。
- **D-029**（トランザクション境界は `&mut Tx` を引数で引き回す）: `UnitOfWork` +
  `Box<dyn Tx>` の当初案を却下。ROADMAP 完了条件の「借用チェッカとの相性を評価する」
  への回答。詳細は `docs/03-database.md` §7。
- **D-038**（append-only 違反と貸借不一致の SQLSTATE を分離）: D-037 を覆した決定。
  両トリガが `P0001` を共有していたため、貸借不一致（store 層のバグ）を
  「append-only 違反 → 逆仕訳で訂正してください」と**誤診**していた。
- **D-046**（試算表 read model の全件走査を承知で Phase 1 完了とする）: 人間承認済み。
- **D-047**（`kaikei-app` が `kaikei-policy` の型を再エクスポートする）: 当初は
  「公開シグネチャに現れる型を手で列挙する」運用ルールで防ごうとしたが、
  **その対応表を書いたコミット自身が2型を落としていた**ためレビューで露見し、
  `pub mod policy` という構造で塞ぎ直した。
- **D-048**（`DATABASE_URL` は sqlx ツール専用とし、アプリ接続は `APP_DATABASE_URL`）:
  `.env.example` どおりに設定すると pg-tests が全滅する状態だった。

### 次 Phase への申し送り

Phase 0 からの申し送り (a)〜(d) の消化状況:

- **(a) `rehydrate` は検証を行わない** → **消化**。`kaikei-store/src/journal/mapper.rs` を
  `rehydrate` を呼ぶ唯一の場所とし、9項目の検証を入れた。他の場所からの呼び出しは
  CI（`architecture.yml`）が禁じている。
- **(b) `test_chart()` と `sole_proprietor.yaml` の乖離検出** → **未消化**。
  `kaikei-jp-data` はまだワークスペースメンバーでない。Phase 2 で消化する。
- **(c) 逆仕訳の `document_refs` は常に空** → **再検討して維持**。`reverse_entry`
  ユースケースの実装時に見直したが、訂正の根拠証憑は元仕訳と別に用意される想定が
  妥当と判断した。
- **(d) `PeriodGuard` / `Clock` の実装** → **消化**（`kaikei-app::period_guard::ClosedPeriodGuard`
  / `kaikei-app::clock::SystemClock`）。

Phase 1 で新たに出た申し送り:

- **試算表の全件走査**（D-046）。`journal_lines` に日付列が無く、期間で絞り込めない。
  実測 9年分で約90ms。体感できる遅さとして顕在化した段階で、`entry_date` の非正規化
  または `fiscal_year` によるパーティショニングを検討する。
- **`InMemoryTx::next_entry_no` は行ロックをエミュレートしない**。fake に対する
  ユースケーステストは採番の競合を検出できない（実 DB の pg-tests でのみ検証される）。
- **`kaikei-store/src/chart.rs` / `period.rs` にユニットテストが無い**。
  pg-tests 経由でのみ間接的に検証されている。
- **`tests/append_only.rs` が既定の並列度で不安定**。原因未特定。
  関連する観測として、**同じ PostgreSQL インスタンスに対して `cargo test --features
  pg-tests` を2プロセス同時に走らせると、`#[sqlx::test]` のセットアップが
  SQLSTATE `55006`（`database "_sqlx_test_..." is being accessed by other users`）で
  失敗する**ことを PR-8 で確認した（sqlx が古いテストDBを掃除する際、他プロセスの
  セッションが張っているものを DROP しようとするため）。単一プロセス内の並列度と
  同じ原因かは未検証。CI は1ジョブ1インスタンスなので現状は顕在化しない。
- **`entry_counters.skipped` は書き込み未実装**（意図的な欠番の記録。`README.md` 参照）。

### 教訓

**1. ツールチェーンを固定していなかった。**
CI は `dtolnay/rust-toolchain@stable` で常に最新 stable を引く一方、ローカルは
`rustup update` を打つまで古いまま。1.97 で追加された lint を踏み、「ローカルで緑・
CI だけ赤」が発生した。`rust-toolchain.toml` で 1.97.1 に固定して解消した。
**CI とローカルでバージョンが違いうるツールは、最初に固定する。**

**2. 契約凍結点のレビューは「実装者を演じさせる」と効く。**
PR-4（`ports.rs` の凍結）では、後続 PR-5/6/7 の実装者を演じるエージェントに
**実際にコンパイルが通るスクラッチコードを書かせる**レビューを行い、机上レビューでは
見つからなかったブロッカー3件を発見した（`kaikei-store` から `CounterpartyIndex` を
名指しできない E0433、`tests/` から `kaikei_app::testing` が見えない E0432、
CI の grep に引っかかる doc 文字列）。**契約を凍結する前に、その契約を使う側の
コードを実際に書いてみる。**

**3. 「誤診を招くエラー」を実バグと同格に扱う方針が Phase 1 でも機能した。**
Phase 1 で見つかった欠陥のうち複数が「値は間違っていないが、診断が人・AI を
誤った方向に導く」型だった: `P0001` の共有による append-only 違反と貸借不一致の
取り違え（D-038）、期間を逆に指定したときに「貸借一致した空の試算表」が成功で返る、
孤児の科目コードが試算表から無言で消える（D-044）。
**MCP 経由で AI が自己修正する前提のシステムでは、誤診は誤値と同じ実害を持つ。**

**4. 並列作業では採番の衝突が起きる。**
PR-2 と PR-3 を並列で走らせたところ、両方が `DECISIONS.md` に同じ D-028 を使った
（オーケストレーション側のミス）。以降は並列作業の開始前に D 番号のレンジを
事前割当する運用にした（PR-5: D-039〜041、PR-6: D-042〜044、PR-7: D-045〜047）。

**5. Phase 0 の教訓（プロパティテストの生成器レンジ）は運用に組み込んだ。**
`.github/pull_request_template.md` にチェック項目として入れ、PR ごとに
「生成器のレンジは仕様の許容範囲と一致しているか」を確認する形にした。

**6. 「気をつける」で防ぐルールは、それを書いた本人がその場で破る。**
D-047（`kaikei-app` が policy 型を再エクスポートする）は、当初「公開シグネチャに
現れる型の対応表を `lib.rs` の doc に置き、型を追加したら表と `pub use` の両方に
足す」という**運用ルール**で漏れを防ごうとした。ところが**その対応表を書いた
コミット自身が `PolicyNote` / `NoteSeverity` を落としており**、同じ PR の
レビューで実際のコンパイルエラーとして検出された。同じ穴を3回踏んだ計算になる
（PR-4 の `CounterpartyIndex`、PR-8 の `TaxPolicy`/`PolicyError`、そして今回）。
最終的に `pub mod policy { pub use kaikei_policy::*; }` という**構造**で塞いだ。
**手で維持する一覧は必ず腐る。構造か CI で機械的に閉じられないか先に考える。**

**7. ドキュメントどおりに一度も実行していなかった。**
`.env.example` の `DATABASE_URL` が `kaikei_app` を指していたため、
`README.md` の手順どおりに設定した開発者は pg-tests が全滅する状態だった
（D-048）。CI は独自に migrator を入れていたので気づけず、
`tests/common/mod.rs` の doc は migrator を前提と書いており、
**同じ変数について3箇所が違うことを言っていた**。
PR-8 の E2E テストを「新規開発者と同じ手順」で走らせて初めて露見した。
**セットアップ手順は、書いた後に一度そのとおり実行する。**

### 税理士に確認すべき事項

Phase 1 も税制を一切含まない（`kaikei-policy` は trait 定義のみで、実装は Phase 2）
ため、この段階で新たに確認すべき事項は無い（該当なし）。

Phase 2（`kaikei-jp`）で確認が必要になる論点として、`docs/04-jp-tax.md` の
税区分マスタ・インボイス経過措置・簡易課税の各項目が挙がっている。

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
