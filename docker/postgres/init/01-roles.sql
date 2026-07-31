-- docker/postgres/init/01-roles.sql
--
-- ロール作成・パスワード設定・権限付与。docker-compose
-- （docker-entrypoint-initdb.d 経由で初回起動時に自動実行）と CI
-- （.github/workflows/database.yml が明示的に `psql -f` で実行）の両方から
-- 同一のこのファイルを流す。ロール作成と権限付与の記述箇所はここだけにする
-- （真実の点を1つにする）。
--
-- 前提: 環境変数 KAIKEI_MIGRATOR_PASSWORD / KAIKEI_APP_PASSWORD が
-- 設定されていること。`-v` オプションではなく `\getenv` で読む
-- （docker-entrypoint-initdb.d は *.sql を素の `psql -f` で実行するため、
-- 呼び出し側が `-v` を渡す余地が無い。`\getenv` なら環境変数さえ設定されていれば
-- 呼び出し方法を問わず同じ結果になる）。
--
-- ロール構成（docs/03-database.md §1、DECISIONS.md D-006）:
--   kaikei_migrator: マイグレーション実行用ロール。CREATEDB を持つ
--     （`#[sqlx::test]` がテストのたびに新しいデータベースを作成するために必要。
--     sqlx-core/src/testing/mod.rs の実装を確認済み）。
--     テーブル/スキーマの所有者にする（R5: 所有者はトリガ以外の全てを
--     バイパスできるため、これ以外の防御はトリガのみになる。
--     この前提は crates/kaikei-store/migrations/0001_baseline_privileges.sql と
--     tests/privileges.rs が検証する）。
--   kaikei_app: アプリ実行用ロール。SUPERUSER/CREATEDB/CREATEROLE/BYPASSRLS を
--     一切持たない。帳簿本体（journal_entries/journal_lines）への
--     UPDATE/DELETE/TRUNCATE 権限を持たない（append-only の強制）。

\set ON_ERROR_STOP on

\getenv migrator_password KAIKEI_MIGRATOR_PASSWORD
\getenv app_password KAIKEI_APP_PASSWORD

\if :{?migrator_password}
\else
  \echo '環境変数 KAIKEI_MIGRATOR_PASSWORD が設定されていません。.env.example を参照してください。'
  \quit 1
\endif

\if :{?app_password}
\else
  \echo '環境変数 KAIKEI_APP_PASSWORD が設定されていません。.env.example を参照してください。'
  \quit 1
\endif

DO $do$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = 'kaikei_migrator') THEN
    CREATE ROLE kaikei_migrator
      LOGIN CREATEDB NOSUPERUSER NOCREATEROLE NOBYPASSRLS
      PASSWORD 'placeholder--overwritten-below';
  END IF;

  IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = 'kaikei_app') THEN
    CREATE ROLE kaikei_app
      LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS
      PASSWORD 'placeholder--overwritten-below';
  END IF;
END
$do$;

-- パスワードは DO ブロックの外側（プレーンな SQL 文）で設定する。
-- psql の `:'var'` 展開はドル引用文字列（$do$ ... $do$）の中でも行内テキスト置換として
-- 効いてしまう場合があり挙動が読みにくいため、ここでは避ける。
ALTER ROLE kaikei_migrator PASSWORD :'migrator_password';
ALTER ROLE kaikei_app      PASSWORD :'app_password';

-- kaikei_migrator をスキーマ所有者にし、kaikei_app には USAGE のみを与える。
-- 現在接続中のデータベース（docker-entrypoint-initdb.d 経由なら $POSTGRES_DB、
-- CI ならジョブが指定したデータベース）に適用する。
ALTER SCHEMA public OWNER TO kaikei_migrator;
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
GRANT USAGE ON SCHEMA public TO kaikei_app;

-- データベース自体への CREATE 権限（`CREATE SCHEMA` に必要。スキーマの CREATE とは
-- 別のACLで、既定では PUBLIC にも付与されない。データベースの所有者のみが持つ）。
--
-- なぜ必要か: `#[sqlx::test]` はテストのたびに、`DATABASE_URL` が指すデータベース
-- 自身に管理用の `_sqlx_test` スキーマ（テスト用DB名の記録簿）を作成する
-- （sqlx-postgres-0.8.6/src/testing/mod.rs で確認済み）。`kaikei` / `postgres` の
-- どちらの db もコンテナ初期化用スーパーユーザー（kaikei_root）の所有であり
-- kaikei_migrator は所有者ではないため、これが無いと `_sqlx_test` スキーマの
-- 作成時点で `permission denied for database <db>` となり `#[sqlx::test]` を使う
-- 全テストが失敗する（実測確認済み: 一時的にこの GRANT を外すと全滅し、戻すと復帰する）。
--
-- 権限拡大にならない理由: `kaikei_migrator` は本ファイルで既に `public` スキーマの
-- 所有者にしており（トリガ以外の全ての制約をバイパスできる = R5）、テーブルの
-- CREATE/ALTER/DROP は元々自由に行える完全信頼ロールである。データベースへの
-- CREATE を追加してもこの脅威モデルは変わらない（`kaikei_app` には一切関係しない）。
--
-- 現在のデータベース名は動的に取得する（docker-compose では "kaikei"、
-- CI では "postgres" のように呼び出し環境で異なるため）。
DO $$
BEGIN
  EXECUTE format('GRANT CREATE ON DATABASE %I TO kaikei_migrator', current_database());
END
$$;

-- `#[sqlx::test]` はテストのたびに `CREATE DATABASE`（テンプレート省略 = template1）で
-- テスト用データベースを新規作成する（sqlx-postgres-0.8.6/src/testing/mod.rs で確認済み）。
-- そのテスト用DBでも kaikei_migrator がマイグレーションを実行できるよう、
-- template1 の public スキーマにも同じ変更を適用しておく。これをしないと、
-- 2件目以降のテスト用データベースで最初の CREATE TABLE が権限不足で失敗する
-- （新しいデータベースの public スキーマは作成元テンプレートの所有者/ACLを
-- そのまま引き継ぐため）。
\connect template1
ALTER SCHEMA public OWNER TO kaikei_migrator;
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
GRANT USAGE ON SCHEMA public TO kaikei_app;
