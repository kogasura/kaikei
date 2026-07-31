-- 0003_journal.sql
--
-- 帳簿本体（append-only）。UPDATE/DELETE/TRUNCATE の禁止は、本ファイル末尾の
-- ロール権限 REVOKE と、0004_append_only_triggers.sql のトリガの二重で強制する
-- （CLAUDE.md §2、DECISIONS.md D-006）。
--
-- `entry_date` は DATE、`recorded_at` は TIMESTAMPTZ。取引日にタイムゾーンを
-- 持たせると年度跨ぎで必ず事故るため、ここは絶対に混ぜない（CLAUDE.md §7）。

CREATE TABLE journal_entries (
    id             UUID PRIMARY KEY,
    fiscal_year    INTEGER NOT NULL,
    entry_no       INTEGER NOT NULL,
    entry_date     DATE NOT NULL,
    description    TEXT NOT NULL CHECK (btrim(description) <> ''),
    reverses       UUID REFERENCES journal_entries(id),
    reverse_reason TEXT,
    recorded_at    TIMESTAMPTZ NOT NULL,
    UNIQUE (fiscal_year, entry_no),
    CHECK ((reverses IS NULL) = (reverse_reason IS NULL))
);

CREATE INDEX idx_entries_date ON journal_entries (entry_date);
CREATE INDEX idx_entries_fy   ON journal_entries (fiscal_year, entry_no);
CREATE INDEX idx_entries_rev  ON journal_entries (reverses) WHERE reverses IS NOT NULL;

CREATE TABLE journal_lines (
    entry_id            UUID     NOT NULL REFERENCES journal_entries(id),
    line_no             SMALLINT NOT NULL,
    account_code        TEXT     NOT NULL,
    side                SMALLINT NOT NULL CHECK (side IN (1, 2)),  -- 1=借方, 2=貸方
    amount_minor        BIGINT   NOT NULL CHECK (amount_minor > 0),
    currency            CHAR(3)  NOT NULL,
    -- ★ 人間承認済みの決定（B-1）: DEFAULT を付けない。
    -- currency だけ指定して currency_minor_unit が既定0になると、金額が100倍
    -- ズレて保存されても下のCHECKには引っかからないため。上限18は
    -- Currency::MAX_MINOR_UNIT（kaikei-core、DECISIONS.md D-020）と一致させる。
    currency_minor_unit SMALLINT NOT NULL CHECK (currency_minor_unit BETWEEN 0 AND 18),
    tags                JSONB    NOT NULL DEFAULT '{}',
    memo                TEXT,
    PRIMARY KEY (entry_id, line_no)
);

CREATE INDEX idx_lines_account ON journal_lines (account_code);
CREATE INDEX idx_lines_tags    ON journal_lines USING GIN (tags);

GRANT SELECT, INSERT ON journal_entries, journal_lines TO kaikei_app;
REVOKE UPDATE, DELETE, TRUNCATE ON journal_entries, journal_lines FROM kaikei_app;
