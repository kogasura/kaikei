# kaikei-policy

可変部（税制・決算・様式・採番規則）の抽象を trait として定義するだけの crate。
**実装は持たない。** 実装は Phase 2 の `kaikei-jp` が担う。

## 依存方向

```
kaikei-core   ← 何にも依存しない
     ↑
kaikei-policy ← このcrate。trait定義のみ、coreの型を使う
     ↑
kaikei-jp     ← policyを実装（Phase 2）
```

`Cargo.toml` の依存は `kaikei-core` と `thiserror` のみ。CI
(`.github/workflows/architecture.yml`) がこれを機械的に検査する。

## この crate が定義する5つの trait

`docs/04-jp-tax.md` §2 が言う「変わる部分の全リスト」に対応する。

| trait | 役割 |
|---|---|
| `TaxPolicy` | 税額計算と税区分の妥当性 |
| `ClosingPolicy` | 決算振替仕訳の生成 |
| `StatementPolicy` | 財務諸表の様式 |
| `EntryValidator` | 追加検証 |
| `Numbering` | 仕訳番号の採番規則 |

## 全メソッドは純関数

`async fn` にしない・`async_trait` を使わない・`sqlx` / `tokio` 等の I/O
クレートに依存しない（`CLAUDE.md` §3）。必要なデータ（勘定科目表、
タグスキーマ、取引先の索引等）は呼び出し側（`kaikei-app`）が事前に
ロードし、[`TaxContext`] のような引数に詰めて渡す。

年度別税区分マスタや事業者設定（`kaikei-jp` の型）はこの crate に置かない。
実装（例: `JpTaxPolicy`）が構築時に保持し、年度の選択は
`TaxContext::as_of`（取引日）で行う（`DECISIONS.md` D-025）。

## ここに置かない2つの trait

- **`FxPolicy` は定義しない。** 外貨は `Currency` として型だけ用意し、
  換算ポリシーは Phase 後半で導入する（`DECISIONS.md` D-016）。
- **`ChartPolicy` は作らない。** 勘定科目体系はユーザーが YAML で自由に
  定義・編集する**データ**であり、「税制ごとに変わるロジック」ではない
  （`ARCHITECTURE.md` §9 R6）。

## テスト用ダミー実装

`test-doubles` feature を有効にすると、`testing` モジュールで
最小限のダミー実装（`NoTaxPolicy` 等）が使える。他 crate のテストから
参照できるよう feature で切っており、`#[cfg(test)]` ではない。

```toml
kaikei-policy = { path = "...", features = ["test-doubles"] }
```

実際の税制ロジックの代替にはならない。
