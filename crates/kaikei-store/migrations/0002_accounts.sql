-- 0002_accounts.sql
--
-- マスタ（可変）。過去の仕訳が参照しているため物理削除しない（active フラグで無効化する）。

CREATE TABLE accounts (
    code         TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    account_type SMALLINT NOT NULL,   -- 1=Asset .. 5=Expense
    parent_code  TEXT REFERENCES accounts(code),
    postable     BOOLEAN NOT NULL DEFAULT TRUE,
    sort_order   INTEGER NOT NULL DEFAULT 0,
    active       BOOLEAN NOT NULL DEFAULT TRUE
);

GRANT SELECT, INSERT, UPDATE ON accounts TO kaikei_app;
