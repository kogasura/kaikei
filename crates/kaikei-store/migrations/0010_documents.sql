-- 0010_documents.sql
--
-- 証憑（docs/06-documents.md §3・§4）。
--
-- ファイルの中身は kaikei-blob が内容の SHA-256 で持つ。このテーブルは
-- **メタデータと帳簿との紐付け**だけを持つ。
--
-- ■ 検索要件の3項目を列にする
--
-- 電子取引データの検索要件は「取引年月日・取引金額・取引先」の3項目である。
-- 基準期間の課税売上高が 5,000万円以下でダウンロードの求めに応じられる場合は
-- 検索機能の確保が不要とされるが、**免除に甘えると売上が伸びた瞬間に作り直しに
-- なる**（§4）。DB に入れておけば検索は自然に付いてくるので最初から列にする。
--
-- 金額と取引先は NULL を許す。契約書のように金額が無い証憑があるため。
-- **0 で埋めない**——「金額が無い」と「0円」は違う。
--
-- ■ append-only
--
-- 帳簿本体（0003 / 0004）・監査ログ（0009）と同じ4点セットで強制する。
--   1. ロール権限の REVOKE
--   2. 行トリガ（UPDATE / DELETE）
--   3. STATEMENT トリガ（TRUNCATE は行トリガを起動しない）
--   4. 専用 ERRCODE と専用のトリガ関数
--
-- 4 が要る理由は 0009 と同じ。reject_mutation() を流用すると例外メッセージが
-- 「訂正は逆仕訳で行ってください」になるが、証憑のメタデータは逆仕訳で直す
-- ものではない。誤った対処法を案内することになる（CLAUDE.md §11）。
--
-- メタデータの訂正が要るようになったら、新しい行を足して旧行に superseded_by を
-- 持たせる（§3。必要になるまで入れない）。

CREATE TABLE documents (
    id             UUID PRIMARY KEY,
    -- 内容の SHA-256（16進64文字・小文字）。ファイル本体は kaikei-blob。
    -- **同じ内容の証憑が複数の行から参照されうる**ので一意にしない
    -- （同じ請求書を別の取引の証憑としても登録することがある）。
    blob_hash      TEXT NOT NULL,
    original_name  TEXT NOT NULL,
    mime_type      TEXT NOT NULL,
    byte_size      BIGINT NOT NULL,

    -- ---- 検索要件の3項目 ----
    doc_date       DATE NOT NULL,
    amount_minor   BIGINT,
    counterparty   TEXT,

    doc_type       TEXT NOT NULL,
    received_via   TEXT NOT NULL,
    received_at    TIMESTAMPTZ NOT NULL,
    note           TEXT,
    created_at     TIMESTAMPTZ NOT NULL,

    -- **値を決め打ちで縛る。** 綴りの揺れ（invoice / Invoice / 請求書）が
    -- 混ざると検索が当たらなくなる。増やすときはマイグレーションで足す。
    CONSTRAINT documents_doc_type_known
        CHECK (doc_type IN ('invoice', 'receipt', 'contract', 'other')),
    CONSTRAINT documents_received_via_known
        CHECK (received_via IN ('email', 'download', 'scan', 'manual')),
    -- 16進64文字の小文字。表記が揺れると blob の場所を引けない。
    CONSTRAINT documents_blob_hash_is_hex
        CHECK (blob_hash ~ '^[0-9a-f]{64}$'),
    -- 空のファイル名は「名前が無い」ことを隠す。
    CONSTRAINT documents_original_name_not_blank
        CHECK (length(btrim(original_name)) > 0),
    CONSTRAINT documents_byte_size_not_negative
        CHECK (byte_size >= 0)
);

CREATE INDEX idx_documents_date ON documents (doc_date);
CREATE INDEX idx_documents_cp   ON documents (counterparty);
CREATE INDEX idx_documents_amt  ON documents (amount_minor);
CREATE INDEX idx_documents_hash ON documents (blob_hash);

-- 帳簿との相互関連性（電子帳簿保存法。証憑から仕訳へ、仕訳から証憑へ辿れる）。
CREATE TABLE entry_documents (
    entry_id    UUID NOT NULL REFERENCES journal_entries(id),
    document_id UUID NOT NULL REFERENCES documents(id),
    PRIMARY KEY (entry_id, document_id)
);

-- 仕訳から証憑を引く索引。主キーは (entry_id, document_id) なので
-- entry_id 側は主キーで足りるが、**証憑から仕訳を引く向き**が無い。
CREATE INDEX idx_entry_documents_document ON entry_documents (document_id);

-- append-only 専用のトリガ関数。P0012（監査ログ）の次の P0013。
CREATE FUNCTION reject_document_mutation() RETURNS trigger AS $$
BEGIN
  RAISE EXCEPTION
    '証憑（%）は追記のみです。記録の訂正は新しい行の追加で行ってください',
    TG_TABLE_NAME
    USING ERRCODE = 'P0013';
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER no_update_documents
  BEFORE UPDATE OR DELETE ON documents
  FOR EACH ROW EXECUTE FUNCTION reject_document_mutation();

CREATE TRIGGER no_truncate_documents
  BEFORE TRUNCATE ON documents
  FOR EACH STATEMENT EXECUTE FUNCTION reject_document_mutation();

-- 紐付けも同じ。**証憑と仕訳の関係を後から消せると、帳簿から証憑への
-- 道筋が黙って切れる。**
CREATE TRIGGER no_update_entry_documents
  BEFORE UPDATE OR DELETE ON entry_documents
  FOR EACH ROW EXECUTE FUNCTION reject_document_mutation();

CREATE TRIGGER no_truncate_entry_documents
  BEFORE TRUNCATE ON entry_documents
  FOR EACH STATEMENT EXECUTE FUNCTION reject_document_mutation();

GRANT SELECT, INSERT ON documents TO kaikei_app;
REVOKE UPDATE, DELETE, TRUNCATE ON documents FROM kaikei_app;

GRANT SELECT, INSERT ON entry_documents TO kaikei_app;
REVOKE UPDATE, DELETE, TRUNCATE ON entry_documents FROM kaikei_app;
