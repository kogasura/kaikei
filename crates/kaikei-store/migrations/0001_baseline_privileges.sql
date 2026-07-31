-- 0001_baseline_privileges.sql
--
-- append-only を DB 権限で強制するための前提条件を検証する（DECISIONS.md D-006）。
-- ロール作成そのものはここでは行わない（docker/postgres/init/01-roles.sql の責務）。
-- ロールはクラスタ単位のオブジェクトであり、`#[sqlx::test]` はテストのたびに
-- 新しいデータベースを作成してこのマイグレーションを再実行するため、
-- ロール作成をここに書くと2件目以降のテストが全て失敗する
-- （phase1計画 R11 / DECISIONS.md D-024）。

DO $$
DECLARE
  app_exists          boolean;
  app_is_super        boolean;
  app_bypasses_rls    boolean;
  app_can_createdb    boolean;
  app_can_createrole  boolean;
  app_is_migrator_mem boolean;
BEGIN
  SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = 'kaikei_app')
    INTO app_exists;

  IF NOT app_exists THEN
    RAISE EXCEPTION
      'kaikei_app ロールが存在しません。docker/postgres/init/01-roles.sql を先に実行してください。';
  END IF;

  SELECT rolsuper, rolbypassrls, rolcreatedb, rolcreaterole
    INTO app_is_super, app_bypasses_rls, app_can_createdb, app_can_createrole
    FROM pg_catalog.pg_roles
   WHERE rolname = 'kaikei_app';

  IF app_is_super OR app_bypasses_rls OR app_can_createdb OR app_can_createrole THEN
    RAISE EXCEPTION
      'kaikei_app が特権属性を持っています（SUPERUSER=% BYPASSRLS=% CREATEDB=% CREATEROLE=%）。'
      'append-only の DB権限による強制（DECISIONS.md D-006）が成立しません。',
      app_is_super, app_bypasses_rls, app_can_createdb, app_can_createrole;
  END IF;

  SELECT pg_has_role('kaikei_app', 'kaikei_migrator', 'USAGE') INTO app_is_migrator_mem;
  IF app_is_migrator_mem THEN
    RAISE EXCEPTION
      'kaikei_app が kaikei_migrator のメンバーです。ロールのメンバーシップ付与を'
      '取り消してください（メンバーだとテーブル所有者の権限を継承でき、REVOKEが無効化されます）。';
  END IF;
END
$$;

-- スキーマへの CREATE 権限は kaikei_migrator（所有者）のみが持つ。
-- docker/postgres/init/01-roles.sql が既に kaikei_migrator をスキーマ所有者に
-- しているため、ここでの REVOKE/GRANT は idempotent な再確認（多層防御）。
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
GRANT USAGE ON SCHEMA public TO kaikei_app;
