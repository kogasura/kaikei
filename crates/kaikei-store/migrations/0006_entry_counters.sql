-- 0006_entry_counters.sql
--
-- 会計年度ごとの仕訳番号カウンタ。採番は仕訳 INSERT と同一トランザクションで
-- 行うため、検証失敗時はカウンタの増分も一緒に巻き戻り、欠番は原理的に発生しない
-- （欠番が出るのは Postgres の SEQUENCE を使った場合。docs/03-database.md §4、
-- DECISIONS.md D-023）。`skipped` は意図的に飛ばした番号の記録専用で、
-- Phase 1 では書き込みを実装しない。

CREATE TABLE entry_counters (
    fiscal_year INTEGER PRIMARY KEY,
    next_no     INTEGER NOT NULL,
    skipped     JSONB NOT NULL DEFAULT '[]'
);

GRANT SELECT, INSERT, UPDATE ON entry_counters TO kaikei_app;
