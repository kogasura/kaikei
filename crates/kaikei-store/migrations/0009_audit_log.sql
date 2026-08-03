-- 0009_audit_log.sql
--
-- 監査ログ（docs/07-mcp-server.md §9、DECISIONS.md D-070 / D-075）。
--
-- 1リクエスト＝2行。開始レコード（status='started'）を操作の前に、
-- 結果レコード（status='ok' | 'error'）を操作の後に、**帳簿とは別の
-- コネクション**で書く。同一トランザクションで1回だけ書く実装にすると、
-- 失敗した操作の記録が rollback で一緒に消える（kaikei_app::tx::with_tx は
-- Err で必ず rollback する）。「AI が何をしようとしたか」を最も知りたいのは
-- 失敗したときであり、その記録だけが残らない設計になってしまう。
--
-- append-only の強制は帳簿本体（0003 / 0004）と同じ4点セットで行う:
--   1. ロール権限の REVOKE（本ファイル末尾）
--   2. 行トリガ（UPDATE / DELETE）
--   3. STATEMENT トリガ（TRUNCATE は行トリガを起動しないため）
--   4. 専用 ERRCODE（P0012）と専用のトリガ関数
-- 4 が要る理由: reject_mutation() を流用すると例外メッセージが
-- 「訂正は逆仕訳で行ってください」になる。監査ログは逆仕訳で直すものでは
-- ないため、これは D-038 が潰したのと同じ「誤った対処法を案内する」欠陥に
-- なる（CLAUDE.md §11）。

CREATE TABLE audit_log (
    -- ★ BIGSERIAL を使わない（docs/07-mcp-server.md §9）。
    -- BIGSERIAL の既定値 nextval にはテーブル権限とは別にシーケンスの USAGE が
    -- 必要で、付与し忘れると kaikei_app の INSERT が
    -- `permission denied for sequence`（SQLSTATE 42501）で失敗する。
    -- 42501 は crates/kaikei-store/src/sqlstate.rs が
    -- RepoError::AppendOnlyViolation に写像するため、「監査ログが書けない」と
    -- いう事象に対して「訂正は逆仕訳で行ってください」という完全に誤った
    -- 案内が出る。GENERATED ALWAYS AS IDENTITY ならテーブル権限だけで
    -- INSERT できる。
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,

    -- ツール呼び出しごとにサーバが採番する UUID。JSON-RPC の id は number にも
    -- なりうるので流用しない。開始レコードと結果レコードの突き合わせが
    -- 読み取りの基本操作なのでインデックスを張る。
    request_id  UUID NOT NULL,

    -- ★ DEFAULT now() を付けない（CLAUDE.md §7）。kaikei_app::clock::SystemClock
    -- （Clock trait）から取得した時刻を明示的に渡す。DEFAULT があるとテストで
    -- 時刻を固定できず、渡し忘れも検出できない。UTC 保存。
    occurred_at TIMESTAMPTZ NOT NULL,

    -- "mcp" / "cli" / "api"（kaikei_app::audit::actor の定数）。
    actor       TEXT NOT NULL CHECK (btrim(actor) <> ''),
    tool        TEXT NOT NULL CHECK (btrim(tool) <> ''),

    status      TEXT NOT NULL CHECK (status IN ('started', 'ok', 'error')),

    -- input は開始レコードのみ、output は結果レコードのみ。
    -- どちらも帳簿本体と同等の機微度として扱う（自由記述欄に個人情報が
    -- 入る前提。docs/07-mcp-server.md §9「個人情報」）。
    input       JSONB,
    output      JSONB,

    -- 分類コードだけを入れる（kaikei_app::error::codes の値。AppError::code()）。
    -- AI に返した本文は output に入れる。
    error_code  TEXT,

    -- ★ 外部キーを張らない。別トランザクションで書くため、rollback された
    -- 操作の entry_id は journal_entries に存在しえない。そして
    -- 「存在しないことを記録できること」自体に価値がある。
    entry_id    UUID,

    -- status と error_code の対応（結果レコードの構造を DB 側でも保つ）。
    -- 開始レコードに error_code は無く、失敗の結果レコードには必ずある。
    CHECK ((status = 'error') = (error_code IS NOT NULL)),
    -- 開始レコードに output は無い（入力だけを記録する）。
    CHECK (status <> 'started' OR output IS NULL)
);

CREATE INDEX idx_audit_log_request  ON audit_log (request_id);
CREATE INDEX idx_audit_log_occurred ON audit_log (occurred_at);

-- append-only 専用のトリガ関数。reject_mutation() を流用しない（上記4を参照）。
-- ERRCODE は P0010（append-only 違反）/ P0011（貸借不一致）の次の P0012。
-- P0 クラスの割り当て理由は 0008_distinct_error_codes.sql を参照。
CREATE FUNCTION reject_audit_log_mutation() RETURNS trigger AS $$
BEGIN
  RAISE EXCEPTION
    '監査ログ（%）は追記のみです。記録の訂正は新しい行の追加で行ってください',
    TG_TABLE_NAME
    USING ERRCODE = 'P0012';
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER no_update_audit_log
  BEFORE UPDATE OR DELETE ON audit_log
  FOR EACH ROW EXECUTE FUNCTION reject_audit_log_mutation();

-- TRUNCATE は行トリガを起動しない（空テーブルでも通ってしまう）。
CREATE TRIGGER no_truncate_audit_log
  BEFORE TRUNCATE ON audit_log
  FOR EACH STATEMENT EXECUTE FUNCTION reject_audit_log_mutation();

GRANT SELECT, INSERT ON audit_log TO kaikei_app;
REVOKE UPDATE, DELETE, TRUNCATE ON audit_log FROM kaikei_app;
