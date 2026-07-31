-- 0007_period_snapshots.sql
--
-- 締めスナップショット。性能のためではなく意味のために作る。append-only なので
-- 改変は起きないが、checksum があると帳簿が改変されていないことを証明できる。

CREATE TABLE period_snapshots (
    fiscal_year         INTEGER NOT NULL,
    period_end          DATE    NOT NULL,
    closed_at           TIMESTAMPTZ NOT NULL,
    balances            JSONB   NOT NULL,   -- 締め時点の全科目残高
    currency            CHAR(3) NOT NULL,
    -- ★ 人間承認済みの決定（B-1）。journal_lines と同じ理由でDEFAULTを付けない。
    -- balances の金額をどの通貨・何桁の最小単位で解釈するかは currency /
    -- currency_minor_unit の組で決まるため、journal_lines と対の構造にする。
    currency_minor_unit SMALLINT NOT NULL CHECK (currency_minor_unit BETWEEN 0 AND 18),
    entry_count         INTEGER NOT NULL,
    last_entry_no       INTEGER NOT NULL,
    checksum            TEXT    NOT NULL,   -- 対象仕訳のハッシュ連鎖
    PRIMARY KEY (fiscal_year, period_end)
);

GRANT SELECT, INSERT ON period_snapshots TO kaikei_app;
