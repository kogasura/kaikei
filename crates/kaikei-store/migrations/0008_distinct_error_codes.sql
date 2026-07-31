-- 0008_distinct_error_codes.sql
--
-- 0004_append_only_triggers.sql の reject_mutation() と assert_entry_is_balanced()
-- は、どちらも ERRCODE を明示しない RAISE EXCEPTION を使っており、結果として
-- 両方とも PostgreSQL の既定コード P0001（raise_exception）を返す。
--
-- これにより「貸借不一致」（store層のバグによって JournalEntry::new の検証を
-- 経ずに journal_lines へ書き込まれた場合にのみ起こりうる、遅延制約トリガに
-- よる検出）が、SQLSTATE だけを見るアプリ層のコード（kaikei-store::sqlstate）
-- からは「append-only 違反」と区別できない。区別できないと、アプリ層は
-- 「訂正は逆仕訳で行ってください」という完全に誤った対処法を提示してしまう
-- （CLAUDE.md §11違反。Phase 0 の循環参照バグ「無関係の科目を犯人として
-- 名指しし、破壊的で無駄な修正に誘導する」と同じ欠陥クラス。DECISIONS.md
-- D-038）。
--
-- 0004 は適用済みマイグレーションであり、sqlx の `_sqlx_migrations` が
-- checksum を検証するため編集できない（CLAUDE.md §2 マイグレーションの掟）。
-- そのため、両関数を CREATE OR REPLACE FUNCTION で置き換え、それぞれに
-- 異なる ERRCODE を明示的に与える（メッセージ本文は変更しない）。
--
-- ERRCODE の選定理由:
-- PostgreSQL のエラーコード一覧（errcodes-appendix）でクラス P0 は
-- 「PL/pgSQL Error Codes」用に予約されている。このクラスのうち
-- P0000（plpgsql_error）/ P0001（raise_exception）/ P0002（no_data_found）/
-- P0003（too_many_rows）/ P0004（assign_incompatible_datatypes）は
-- PL/pgSQL 本体が使う組み込みの擬似エラーとして既に割り当て済みである。
-- それ以外の P0005〜P0999 はどの組み込みコードにも割り当てられていない。
-- 本マイグレーションでは将来 PostgreSQL 本体が P0 クラスに新しい組み込み
-- コードを追加する可能性に備えて P0005〜P0009 を空けたうえで、
--   - P0010: append-only 違反（reject_mutation）
--   - P0011: 貸借不一致（assert_entry_is_balanced。store層のバグ検出）
-- を割り当てる。両方とも PL/pgSQL の RAISE EXCEPTION から発生させる
-- アプリケーション固有のエラーであるため、意味的に一致する P0 クラスを
-- 引き続き使う（既存の標準 SQLSTATE とは衝突しない）。

CREATE OR REPLACE FUNCTION reject_mutation() RETURNS trigger AS $$
BEGIN
  RAISE EXCEPTION 'append-only table: % は変更できません（訂正は逆仕訳で行ってください）',
    TG_TABLE_NAME
    USING ERRCODE = 'P0010';
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION assert_entry_is_balanced() RETURNS trigger AS $$
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
      v_entry_id, v_debit, v_credit, abs(v_debit - v_credit)
      USING ERRCODE = 'P0011';
  END IF;

  RETURN NULL;
END;
$$ LANGUAGE plpgsql;
