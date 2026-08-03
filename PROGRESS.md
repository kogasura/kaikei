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
  → **Phase 2 PR-1 で消化**。`kaikei-jp-data` をワークスペースに組み込んだうえで、
  `crates/kaikei-jp/tests/chart_drift.rs` に乖離検出テストを追加した
  （`kaikei-core` は依存を増やせないため、検出は `kaikei-jp` 側でのみ行える。
  `DECISIONS.md` D-051）。
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

### 完了日

2026-08-01

### 見積との差

`ROADMAP.md` の見積は「2〜3週間」。実際には 2026-08-01 08:14Z（PR-1 マージ）〜
2026-08-01（本 PR-8 マージ）の1日で完了した。Phase 0・Phase 1 と同じく
AI エージェントによる並列実装によるもので、見積との差が Phase を重ねても
縮まっていないこと自体を、以降の Phase の見積もり精度を疑う材料として
正直に記録しておく。

Phase 2 は「ここが最も泥臭い」（`ROADMAP.md`）と明記されていた Phase だが、
体感の泥臭さは実装速度よりも**設計書と実装の整合を保つコスト**（教訓を参照）
に現れた。

マージ済み PR（`gh pr list --state merged`）:

| # | タイトル |
|---|---|
| #18 | Phase 2 PR-1: kaikei-jp / kaikei-jp-data の骨組みと YAML ローダ基盤 |
| #19 | Phase 2 PR-2+PR-3: インボイス登録番号と税区分マスタ（適用期間による選択） |
| #20 | Phase 2 PR-4: JpTaxPolicy（消費税行の自動生成） |
| #21 | Phase 2 PR-5+PR-6: 科目表/TagSchema のロードと家事按分 |
| #22 | Phase 2 PR-7: 決算振替仕訳と財務諸表（ClosingPolicy / StatementPolicy） |
| （本PR） | Phase 2 PR-8: 結線 + E2E + PROGRESS |

### ROADMAP 完了条件の充足

証跡は `crates/kaikei-e2e/tests/e2e_jp.rs`（PR-8 で新設。`kaikei-store` と
`kaikei-jp` は互いを知れないため、両方を知ってよい合成ルートを模した専用
crate を作った。`DECISIONS.md` D-068）の各テスト関数。すべて `JpTaxPolicy` /
`JpSoleProprietorClosingPolicy` を実際に `post_entry::execute` /
`report::execute` へ注入し、実 PostgreSQL に対して記帳・読み戻し・SQL集計を
行うことで検証している（コード内で組み立てた `TrialBalance` に対する
ユニットテストではない）。

| 完了条件 | 証跡 |
|---|---|
| 税抜経理で消費税行が自動生成される | `condition_1_exclusive_accounting_generates_tax_line_on_taxable_sale`（10%課税売上を記帳し、仮受消費税(330)の行がDBに保存・読み戻せることを確認） |
| 8% 軽減税率、非課税、不課税が扱える | `condition_2_reduced_rate_tax_free_and_out_of_scope_categories_are_handled`（10%・8%・非課税・不課税を1仕訳に混在させ、税額行が10%分・8%分の2行のみ生成されることを確認） |
| 非適格の経過措置が YAML で表現できている | `condition_3_non_qualified_transitional_measure_is_expressed_in_yaml`（`PURCHASE_10_NON_QUALIFIED`の`deduction_ratio: "0.80"`がYAMLから読み込まれ、記帳では`rate`のみで計算されつつ`PolicyNote`で断定せず伝えられることを確認） |
| 家事按分の 3 行仕訳が生成される | `condition_4_household_split_produces_a_three_line_entry`（`household_split`の出力をそのまま`post_entry::execute`へ入力として渡し、3行がDBに保存されることを確認） |
| 年度別 YAML の切り替えが取引日で行われる | `condition_5_yearly_master_switch_is_based_on_entry_date`（同一の税区分コードでも取引日により異なるマスタ・異なる税率が適用され、実際に異なる税額でDBへ記帳されることを確認） |

加えて `phase2_end_to_end_scenario_posts_and_closes_the_books` が、Phase 2 の
実装が**通しで動く**ことを示す（**決算振替仕訳が実際に記帳できることの実証。
PR-8 の最大の価値**）: 科目表をDBに投入 → 課税売上・課税仕入・家事按分を
記帳 → `report::execute` で試算表を出す → DBから読み戻した仕訳から
`kaikei_core::TrialBalance` を組み立てて `closing_entries` に決算振替仕訳を
提案させる → **その `ProposedEntry` を `post_entry::execute` で実際に記帳する**
→ 決算後の試算表で収益・費用の残高が0になることを確認 → `JpStatementPolicy`
でBS/PLを組み立てる。PR-7 で「構築は通るが記帳できない」欠陥を3件踏んでいた
ため、この一連の記帳が実際に成功することが Phase 2 の実質的なゴールだった。

### 設計変更（`DECISIONS.md` 参照）

D-049〜D-069。とくに後続 Phase に影響するもの:

- **D-050**（税区分マスタは「暦年 → マスタ1件」ではなく適用期間で選ぶ全件リスト）:
  日本の消費税率改正が年度途中に起きる（2019年10月の軽減税率導入等）ため、
  暦年会計の個人事業主には1つの暦年に2つのマスタが適用される期間が実際に
  生じる。データが1件しかないうちに直しておかないと、複数年度が並んだ後の
  キー型変更は全ファイルのリネームを伴う。
- **D-054/D-055**（適用期間の重なりはロード時エラー、`for_date`は`None`を
  返すのみでエラーにしない）: 「一意に決まらない」を静かに解決しない方針。
- **D-057**（`JpSettings`は構築時に一度だけ合成し、`ctx.as_of`で再合成しない）:
  事業者設定は「その事業者が今どう記帳したいか」であり、年度マスタの
  `settings_defaults`はあくまで初期値の提案に留める。
- **D-059**（非適格の経過措置は税額計算に反映せず`PolicyNote`に留める）:
  控除できない部分の帳簿処理は税務判断そのもの。税理士確認前に実装で
  既成事実化しない。
- **D-064**（`household_split`は`TaxPolicy`のメソッドにせず独立関数にし、
  合成ルート経由でのみ呼べる）: 初版の`DECISIONS.md`が「`kaikei-app`が直接
  importすることは許容範囲」と誤って書いており、CIが実際に落とすことが
  レビューで発覚して訂正した（教訓を参照）。
- **D-065/D-066**（`opening_entries`は実装しない。決算科目3つの実在・
  記帳可否・タグスキーマ適合を構築時に検証する）: 「構築は通るが記帳
  できない」欠陥3件（教訓を参照）を踏まえた最終形。
- **D-068**（`kaikei-e2e`を新設）: `kaikei-store`も`kaikei-jp`も互いを知れない
  ため、両方を知ってよい最上位の層をテスト専用crateとして用意した。
- **D-069**（`JpStatementPolicy`の`chart`は決算書生成の直前に都度読み直した
  ものから構築し、起動時に長期保持しない）: `JpTaxPolicy`/
  `JpSoleProprietorClosingPolicy`と異なり`chart`はDBから頻繁に読み直される
  可変データであるため、同じ「構築時保持」パターンを機械的に適用すると
  「科目名を変更したのに決算書には古い名前が出る」というバグになる。
  PR-7のレビューで積み残された論点を PR-8 で決めた。

### 次 Phase への申し送り

- **`PolicyNote`（`TaxDerivation.notes`）が永続化されない**。
  `post_entry::execute`は`tax.derive_tax_lines(...)?.lines`のみを使い、
  `.notes`を捨てている。経過措置・簡易課税の注記（D-059）は現状、記帳した
  仕訳をDBから読み戻しても再現できない。MCP経由でAIに「この記帳には確認
  すべき注記がある」と伝えるには、Phase 3 で`notes`をどこかに保存する
  （監査ログ、または仕訳への付帯情報として）設計が必要。**未着手**であり、
  Phase 2 時点ではテスト内で`derive_tax_lines`を直接呼んで確認するに留めた
  （`crates/kaikei-e2e/tests/e2e_jp.rs`の`condition_3_*`を参照）。
  → **Phase 3 PR-B で半分消化**。`post_entry::execute`の戻り値を
  `PostEntryOutput { entry, notes }`に拡張し、`notes`が呼び出し元まで
  届くようにした（`DECISIONS.md` D-073）。`condition_3_*`は同じpolicyへ
  直接問い合わせ直すのではなく、**post_entryの戻り値**を検証する形に
  書き換えてある。**残りは永続化**（`audit_log.output`への保存。D-070の
  決定4）で、audit_log用ポート・テーブルごと`kaikei-mcp`のPRの担当。
- **`opening_entries`（期首の振替）は未実装のまま**（D-065）。事業主貸・
  事業主借の期首リセット方式が税理士確認事項として未解決のため。
- **簡易課税のみなし仕入率による計算は未実装**（設定を保持するのみ）。
- **`kaikei-e2e`は「他のどのcrateからも依存されない」制約を今後も維持する
  こと**。Phase 3 の`kaikei-mcp`（本番の合成ルート）を実装する際、
  `kaikei-e2e`のコードを流用・依存させたくなる誘惑が起きうるが、それは
  この制約の存在意義を壊す（`DECISIONS.md` D-068）。
- **`JpStatementPolicy`の`chart`は呼び出し都度構築する方針（D-069）を
  Phase 3以降の合成ルート実装でも守ること**。`Composition`のような
  長期保持する構造体に`JpStatementPolicy`を含めない。

### 教訓

Phase 2 で実際に起きたことを、脚色せずに記録する。

**1. 設計書が却下済みの設計を指示したままになる事故が3回起きた。**
`docs/04-jp-tax.md`は実装が進むたびにレビューで確定した決定と食い違って
いく傾向があり、後続PRの実装者が「設計書だけを見て却下済みの設計を
再実装しかねない」状態を3回作った。

- PR-19: §2 が「暦年 → マスタ1件」（D-050 が明示的に却下した設計）の
  ままだった
- PR-20: §7 が非適格の経過措置について「仮払消費税を減らして本体に足す」
  （D-059 が却下した処理方針）を指示したままだった
- PR-21: §8 の擬似コードが `tax_category: Option<TaxCategoryCode>` の
  ままで、`TaxCategoryCode` という型はどこにも存在しなかった（D-064 の
  実装が採用したのは `Option<String>`）

3回とも「レビューで発覚 → 該当箇所を実態に合わせて書き換え」で対処した。
**設計書は一度書いたら終わりではなく、決定が変わるたびに直さないと
「却下済みの設計を指示する文書」に劣化する。**

**2. 「構築は通るが記帳できない」欠陥が PR-7 で3件出た。**
`JpSoleProprietorClosingPolicy::new`は「決算処理の実行時ではなく構築時に
検出する」ことを設計目標に掲げていたが、その未達部分が3件、いずれも
**実際に`JournalEntry::new`に通してみて初めて**発覚した。

1. 生成した収益・費用のゼロ化明細に`tax_category`タグを付けられず、
   同梱`tags.yaml`の`required_for`制約で`MissingRequiredTag`になった
2. 見出し科目（`postable: false`）を決算科目に指定できてしまい、
   `closing_entries`は明細を作れる一方`CoreError::NotPostable`で落ちた
3. 元入金のタグ検証が`AccountType::Equity`決め打ちで、元入金を誤って
   別種別で登録した科目表では構築時は通るのに記帳時に落ちた
   （**これは1の修正で自分が作り込んだバグ**）

**「構築時に検証する」と謳うコードは、実際にその構築物を最後まで
使い切ってみるテスト（`JournalEntry::new`に通す・記帳する）を用意しない
限り、検証漏れに自分で気づけない。** PR-8 の E2E テスト
（`phase2_end_to_end_scenario_posts_and_closes_the_books`）が「決算振替
仕訳が実際に記帳できる」ことを最大の価値としているのは、この教訓への
直接の回答である。

**3. CI の grep がコメント中の記述で誤検知する事故が Phase 0/1 から
数えて2回目、再発した。**
`rehydrate`の呼び出しを1箇所に限定するCIチェックが、「このAPIはCIが
禁じているので使わない」と**説明しているdocコメント**に反応して落ちる、
という同型の誤検知を再び踏んだ（1回目はPhase 1 PR-4の
`kaikei-app/src/error.rs`、2回目がPhase 2 PR-7の
`kaikei-jp/src/test_support.rs`）。1回目は該当箇所の文言を書き換えて
個別に回避しただけで、検査側の根本原因（コメント行を除外していない）を
直していなかったため、Phase 2 で同じ構造の別ファイルにまた当たった。
2回目でようやく検査側を構造的に直した（行頭が`//`/`///`/`//!`/`*`の行を
除外する。`.github/workflows/architecture.yml`）。**「次に同じ形の
コードを書いたら再発する」個別対処ではなく、検査ロジック自体を直すこと
を優先すべきだった。**

### 税理士に確認すべき事項

`docs/08-compliance.md` §9 の質問リスト（1〜7）に加えて、Phase 2 の実装で
新たに8〜14が追加されている（一覧は同ファイルを参照）。PR-8 では新しい
論点は見つからなかったが、既存の未解決事項のうち2件を E2E テストで
実際に踏んで再確認した:

- 項目8・9（`condition_3_non_qualified_transitional_measure_is_expressed_in_yaml`
  で確認）: `PURCHASE_10_NON_QUALIFIED`の`deduction_ratio: "0.80"`は
  引き続き**未確認の値**のまま。実装は`rate`のみで税額を計算し、
  控除できない部分の処理は行っていないことを`PolicyNote`で伝えるに
  留めている（値そのものの正しさ・控除できない部分の帳簿処理方法は
  依然として税理士確認が必要）

---

## Phase 3 — kaikei-mcp

実装中（PR-F まで完了）。完了時に他 Phase と同じ節構成で記録する。

### 実装中の申し送り

- **【PR-G / PR-H への申し送り】ツールは `dispatch::McpTool` を実装して
  `dispatch::ToolRegistry::with` で登録すること。** `rmcp` の `#[tool]` /
  `#[tool_router]` マクロは使わない（`DECISIONS.md` D-084、
  `docs/07-mcp-server.md` §4）。ツールに渡る `ToolContext` は
  `AuditSink` を露出しないので、監査ログを自分で書くことも書き忘れることも
  できない。`crates/kaikei-mcp/tests/audit_is_structural.rs` が
  型で閉じられない残り（`ToolRoute` を直に組み立てる等）を見張っている。

- **応答本文で `warnings` キーを使わないこと。** fail-open の警告
  （監査ログの結果記録に失敗したときの注記）を載せるために dispatch 層が
  予約している。`dispatch::call` が `debug_assert!` で毎回検査するので、
  使うと `cargo test` が落ちる（release では値を捨てずに併合する）。

- **`tags` の重複キーは MCP 経由では検出できない**（後勝ちになる）。
  `rmcp` のトランスポートが JSON をパースする時点で畳み込まれるためで、
  受け型を工夫しても直らない。`JpError::DuplicateTagKeyInInput` は
  MCP 経由では到達不能である（`DECISIONS.md` D-085 の訂正注記）。
  タグを受け取るツールを増やすときに「重複は弾かれる」と前提しないこと。

- **読み取り系ツールも `dispatch::call` を通す。** §9 は「読み取り系も含めて
  全て記録する」と定めている。読み取り系は `ToolSuccess::with_entry_id` を
  付けないだけで、経路は書き込み系と同じである。

- **MC-11 の「全11ツール総当たり」は書き込み系2件まで済み。**
  残り9件は PR-G / PR-H で `crates/kaikei-e2e/tests/mcp_write_tools.rs` と
  同じ形（`dispatch::call` を直接呼び、`audit_log` を SELECT する）で足す。
  読み取り専用のツールなら `kaikei-mcp` 側の `pg-tests` でも書けるが、
  `audit_log` を読むには SQL が要るので `kaikei-e2e` 側になる。

- **`accounts.active` / `accounts.sort_order` は現在どこからも読まれていない。**
  `kaikei-store` の `load_chart`（`crates/kaikei-store/src/chart.rs`）が
  `SELECT` するのは `code / name / account_type / parent_code / postable` の
  5列だけで、`active` は問い合わせてすらいない。したがって
  **`active = false` にしても何も起きない**——その科目に対する記帳は成功し、
  自動生成される税額行も `active = false` の科目に付く（レビューで実測）。
  `sort_order` も同様に読まれていない（テンプレートの `sort` は
  `kaikei_core::AccountDef` に対応するフィールドが無く、ロード時に破棄
  される。`DECISIONS.md` D-061）。

  **科目の無効化は Phase 4 以降。** 列は `0002_accounts.sql` に存在するが、
  「無効化された科目には記帳できない」という振る舞いは1行も実装されて
  いない。実装する場合の置き場は `kaikei-core`（`ChartOfAccounts` が
  `postable` と同じ扱いで判定する）か `kaikei-app`（読み込み時に除外する）
  かの選択から始まる論点であり、PR-E の範囲では決めていない。

  この申し送りが要るのは、PR-E の README が
  「テンプレート側を採用したい場合は勘定科目マスタを直接編集してください」と
  **DB の直接編集を正規の手段として案内し始めた**ためである。
  `active = false` にして科目を引退させたつもりになる経路が新しくできた。
  README にも同じ注意を書いてある。

- **勘定科目マスタとテンプレートの食い違い（`ImportChartOutput::kept_existing`）の
  出口が起動時の stderr しか無い。** `docs/07-mcp-server.md` §7 の
  「PR-G への申し送り」を参照（`get_settings` に載せる）。

---

## Phase 4 — kaikei-import + kaikei-blob

未着手。

---

## Phase 5 — kaikei-report

未着手。
