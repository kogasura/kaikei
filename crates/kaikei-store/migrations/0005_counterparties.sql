-- 0005_counterparties.sql
--
-- マスタ（可変）。過去の仕訳が参照しているため物理削除しない（active フラグ）。

CREATE TABLE counterparties (
    code            TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    invoice_reg_no  TEXT,             -- T + 13桁
    is_qualified    BOOLEAN,          -- 適格請求書発行事業者か
    verified_at     DATE,
    note            TEXT,
    active          BOOLEAN NOT NULL DEFAULT TRUE
);

GRANT SELECT, INSERT, UPDATE ON counterparties TO kaikei_app;
