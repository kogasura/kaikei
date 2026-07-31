-- 0004_append_only_triggers.sql
--
-- 補助的にトリガでも防ぐ（多層防御）。0003 の REVOKE がロール権限による防御なのに
-- 対し、こちらはテーブル所有者（kaikei_migrator）自身が UPDATE/DELETE/TRUNCATE を
-- 発行した場合の最後の防御線になる（phase1計画 R5: 所有者はロール権限を
-- バイパスできるため、トリガだけが所有者に対して有効な防御）。

CREATE FUNCTION reject_mutation() RETURNS trigger AS $$
BEGIN
  RAISE EXCEPTION 'append-only table: % は変更できません（訂正は逆仕訳で行ってください）',
    TG_TABLE_NAME;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER no_update_journal_entries
  BEFORE UPDATE OR DELETE ON journal_entries
  FOR EACH ROW EXECUTE FUNCTION reject_mutation();

CREATE TRIGGER no_update_journal_lines
  BEFORE UPDATE OR DELETE ON journal_lines
  FOR EACH ROW EXECUTE FUNCTION reject_mutation();

-- TRUNCATE は行トリガを起動しない（空テーブルでも通ってしまう。phase1計画 R6）。
-- STATEMENT トリガを別途張る。
CREATE TRIGGER no_truncate_journal_entries
  BEFORE TRUNCATE ON journal_entries
  FOR EACH STATEMENT EXECUTE FUNCTION reject_mutation();

CREATE TRIGGER no_truncate_journal_lines
  BEFORE TRUNCATE ON journal_lines
  FOR EACH STATEMENT EXECUTE FUNCTION reject_mutation();

-- 遅延制約トリガ: 仕訳がコミットされる時点で貸借が一致していることを検証する。
-- アプリ層の JournalEntry::new が既に検証しているが、DB側でも最後の防御として
-- 二重に確認する（phase1計画 R4）。DEFERRABLE INITIALLY DEFERRED にすることで、
-- entry 行を先に INSERT し、その後 lines を複数行 INSERT する通常の手順で、
-- entry 単独 INSERT 直後の一時的な不整合を誤検知しない。
CREATE FUNCTION assert_entry_is_balanced() RETURNS trigger AS $$
DECLARE
  v_entry_id uuid;
  v_debit    bigint;
  v_credit   bigint;
BEGIN
  v_entry_id := NEW.entry_id;

  SELECT
    COALESCE(SUM(amount_minor) FILTER (WHERE side = 1), 0),
    COALESCE(SUM(amount_minor) FILTER (WHERE side = 2), 0)
    INTO v_debit, v_credit
  FROM journal_lines
  WHERE entry_id = v_entry_id;

  IF v_debit <> v_credit THEN
    RAISE EXCEPTION
      '貸借不一致: entry_id=% の借方合計(%) と貸方合計(%) が一致しません（差額 %）。'
      'アプリ層の検証（JournalEntry::new）を経ずに journal_lines へ書き込まれた'
      '可能性があります（store層のバグ）。',
      v_entry_id, v_debit, v_credit, abs(v_debit - v_credit);
  END IF;

  RETURN NULL;
END;
$$ LANGUAGE plpgsql;

CREATE CONSTRAINT TRIGGER assert_entry_is_balanced
  AFTER INSERT ON journal_lines
  DEFERRABLE INITIALLY DEFERRED
  FOR EACH ROW EXECUTE FUNCTION assert_entry_is_balanced();
