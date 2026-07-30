# 07 — MCP サーバー（kaikei-mcp）

**このプロジェクトの差別化の本体。**
AI エージェントが会計操作を安全に行うための標準インタフェース。

---

## 1. 設計の 4 原則

### ① 削除ツールを作らない

API に存在しないので、AI が暴走しても帳簿は壊れない。
訂正は `reverse_journal_entry` のみ。

### ② 提案と確定を分ける

`suggest_*` 系は候補と根拠を返すだけ。帳簿は変更しない。
確定は人間が明示的に行う（`post_journal_entry`）。

### ③ エラーは自己修正可能な形で返す

```
❌ "Unbalanced entry"
✅ "貸借不一致: 借方 110,000 / 貸方 100,000（差額 10,000）。
    仮受消費税の計上漏れの可能性があります。
    tax_category が SALES_10 の明細に対する税額行を追加してください。"
```

**次の手が分かる文言にする。** これが MCP サーバーの品質を左右する。

### ④ 不可逆操作は確認を要求する

`close_period` は `confirm: true` を明示的に渡さないと実行しない。

---

## 2. ツール定義

### 読み取り系

| ツール | 説明 |
|---|---|
| `list_accounts` | 勘定科目一覧。種別・階層を含む |
| `search_entries` | 日付・金額・科目・取引先・摘要で仕訳検索 |
| `get_entry` | 仕訳 1 件の詳細（明細・タグ・証憑リンク） |
| `get_trial_balance` | 試算表。`group_by` 指定可 |
| `get_ledger` | 総勘定元帳（科目別の明細） |
| `get_statements` | B/S・P/L |
| `list_pending_transactions` | 未仕訳の取込明細 |
| `list_tax_categories` | 有効な税区分一覧（指定日時点） |
| `search_documents` | 証憑検索（日付・金額・取引先） |
| `get_settings` | 税抜/税込、端数処理、課税事業者かなど |

### 書き込み系

| ツール | 説明 | 備考 |
|---|---|---|
| `post_journal_entry` | 仕訳を起こす | 貸借不一致は必ずエラー |
| `reverse_journal_entry` | 赤伝を起こす | 理由が必須 |
| `journalize_transaction` | 取込明細を仕訳化 | 取込明細 ID と仕訳内容を指定 |
| `ignore_transaction` | 取込明細を無視 | 理由が必須 |
| `attach_document` | 証憑を仕訳に紐付け | |
| `upsert_counterparty` | 取引先マスタ更新 | |
| `upsert_journalize_rule` | 仕訳化ルール更新 | |
| `close_period` | 期間を締める | `confirm: true` 必須。不可逆 |

### 提案系（帳簿を変更しない）

| ツール | 説明 |
|---|---|
| `suggest_journal_entry` | 取込明細から仕訳案を生成。根拠付き |
| `suggest_tax_category` | 取引内容から税区分を提案。根拠付き |
| `validate_invoice_number` | 登録番号の形式検証（実在確認はしない） |
| `explain_balance` | ある科目の残高の内訳を説明 |

### 存在させないツール

```
delete_journal_entry
update_journal_entry
execute_sql
reopen_period          ← 締めの取り消しは CLI からのみ（人間の操作）
```

---

## 3. 主要ツールの入出力

### post_journal_entry

```json
{
  "entry_date": "2026-04-15",
  "description": "A社への請求",
  "lines": [
    { "account": "135", "side": "debit",  "amount": "110000" },
    { "account": "500", "side": "credit", "amount": "100000",
      "tags": { "tax_category": "SALES_10", "counterparty": "CP0001" } }
  ],
  "auto_tax_lines": true,
  "document_ids": []
}
```

`auto_tax_lines: true` なら `TaxPolicy::derive_tax_lines` を通す。
上記の例では仮受消費税 10,000 が自動追加され、貸借が一致する。

成功時：

```json
{
  "entry_id": "0192...",
  "entry_no": 42,
  "fiscal_year": 2026,
  "lines": [ /* 税行を含む確定後の明細 */ ],
  "debit_total": "110000",
  "credit_total": "110000"
}
```

**確定後の明細を必ず返す。** AI が「何が記録されたか」を確認できるようにする。

失敗時：

```json
{
  "error": "unbalanced",
  "message": "貸借不一致: 借方 110,000 / 貸方 100,000（差額 10,000）。tax_category が SALES_10 の明細に対する税額行がありません。auto_tax_lines: true を指定するか、仮受消費税（330）10,000 を貸方に追加してください。",
  "debit_total": "110000",
  "credit_total": "100000",
  "difference": "10000",
  "hint": { "suggested_line": { "account": "330", "side": "credit", "amount": "10000" } }
}
```

`hint` に修正案を入れると AI の自己修正が一段速くなる。

### suggest_journal_entry

```json
{
  "imported_tx_id": "0192...",
  "max_candidates": 3
}
```

```json
{
  "transaction": {
    "occurred_on": "2026-04-20",
    "amount": "1980",
    "direction": "out",
    "description": "ｶ)ｱﾏｿﾞﾝ ｼﾞﾔﾊﾞﾝ"
  },
  "candidates": [
    {
      "confidence": "high",
      "lines": [
        { "account": "609", "side": "debit",  "amount": "1980",
          "tags": { "tax_category": "PURCHASE_10_QUALIFIED" } },
        { "account": "100", "side": "credit", "amount": "1980" }
      ],
      "reasoning": "摘要が仕訳化ルール #3（'ｱﾏｿﾞﾝ' を含む → 消耗品費）にマッチ。過去12ヶ月で同摘要の取引が8件あり、いずれも消耗品費で処理されています。",
      "similar_entries": ["0191...", "0190..."]
    }
  ],
  "warnings": [
    "Amazon は適格請求書発行事業者ですが、購入内容により税区分が異なる場合があります。領収書をご確認ください。"
  ]
}
```

**`reasoning` と `similar_entries` が必須。** これが既存の会計ソフトとの差。
「なぜその科目か」を説明できる。

### close_period

```json
{
  "fiscal_year": 2026,
  "period_end": "2026-12-31",
  "confirm": true
}
```

`confirm` が `false` または省略の場合、実行せずに影響範囲を返す。

```json
{
  "status": "dry_run",
  "message": "この操作は不可逆です。締め後、2026-12-31 以前の日付で仕訳を追加できなくなります。",
  "entry_count": 342,
  "last_entry_no": 342,
  "unbalanced_check": "ok",
  "pending_transactions": 5,
  "warnings": ["未仕訳の取込明細が 5 件あります。締める前に処理を検討してください。"]
}
```

**未処理があれば警告する。** 締めは取り消せないので、事前確認を厚くする。

---

## 4. 実装方針

- MCP サーバーは `kaikei-app` のユースケースを呼ぶ薄い層にする
- ビジネスロジックを MCP 層に書かない
- ツール定義は `kaikei-mcp/src/tools/*.rs` に 1 ツール 1 ファイル
- JSON スキーマは構造体から生成する（`schemars` 等）

```
kaikei-mcp/src/
├── main.rs
├── server.rs
├── error.rs              AppError → MCP エラー JSON への変換
└── tools/
    ├── post_entry.rs
    ├── reverse_entry.rs
    ├── search_entries.rs
    ├── trial_balance.rs
    ├── suggest_entry.rs
    └── ...
```

---

## 5. 金額の受け渡し

**JSON では金額を文字列で扱う。**

```json
{ "amount": "110000" }     ✅
{ "amount": 110000 }       ❌ JSON の number は倍精度浮動小数点
```

JSON の number は IEEE 754 倍精度なので、大きな整数や小数で誤差が出る可能性がある。
会計データでこれは許容できない。**入出力とも文字列で統一。**

ドキュメントとツール説明文に明記し、AI が number を渡してきた場合は
エラーで「文字列で渡してください」と返す（あるいは整数なら受理して警告する）。

---

## 6. 認証

Phase 4 までは認証なし（ローカル・単一ユーザー・自己ホスト前提）。

**早すぎる認証は開発を遅くする。** ただし：

- ネットワークにバインドしない（`127.0.0.1` のみ）ことをデフォルトにする
- 外部公開する場合の注意を README に明記
- Phase 5 でトークン認証を追加する余地を残す

---

## 7. 監査ログ

MCP 経由の操作は全て記録する。

```sql
CREATE TABLE audit_log (
    id          BIGSERIAL PRIMARY KEY,
    occurred_at TIMESTAMPTZ NOT NULL,
    actor       TEXT NOT NULL,        -- "mcp" / "cli" / "api"
    tool        TEXT NOT NULL,
    input       JSONB NOT NULL,
    result      TEXT NOT NULL,        -- ok / error
    error_code  TEXT,
    entry_id    UUID
);
```

append-only。
「AI が何をしたか」を後から追えることは、AI に会計を触らせる前提条件。

**入力にパスワードや個人情報が入らないよう注意する**（現状の設計では入らない）。

---

## 8. テストケース

| # | ケース | 期待 |
|---|---|---|
| MC-01 | 貸借一致の仕訳を post | 成功。確定後明細が返る |
| MC-02 | 貸借不一致の仕訳を post | エラー。差額と hint が含まれる |
| MC-03 | `auto_tax_lines: true` で税行が自動追加 | 貸借一致する |
| MC-04 | 存在しない科目コード | エラー。有効な科目一覧を hint に |
| MC-05 | 締め済み期間への post | エラー |
| MC-06 | `close_period` を confirm なし | dry_run が返る。帳簿は変わらない |
| MC-07 | `close_period` を confirm あり | 締まる。以後 post が失敗する |
| MC-08 | `suggest_journal_entry` | reasoning が空でない |
| MC-09 | 金額を number で渡す | エラーまたは警告 |
| MC-10 | `delete_journal_entry` を呼ぶ | ツールが存在しない |
| MC-11 | 全ツール呼び出しが audit_log に記録される | — |
| MC-12 | `reverse_journal_entry` で理由なし | エラー |
