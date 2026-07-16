\set ON_ERROR_STOP on

-- Fixed NOLOGIN group roles. Deployment tooling creates credential-bearing
-- LOGIN roles separately and grants exactly one of these groups.
SELECT 'CREATE ROLE manchester_arcana_migration NOLOGIN'
WHERE NOT EXISTS (
    SELECT 1 FROM pg_roles WHERE rolname = 'manchester_arcana_migration'
) \gexec
SELECT 'CREATE ROLE manchester_arcana_app NOLOGIN'
WHERE NOT EXISTS (
    SELECT 1 FROM pg_roles WHERE rolname = 'manchester_arcana_app'
) \gexec
SELECT 'CREATE ROLE manchester_arcana_backup NOLOGIN'
WHERE NOT EXISTS (
    SELECT 1 FROM pg_roles WHERE rolname = 'manchester_arcana_backup'
) \gexec
SELECT 'CREATE ROLE manchester_arcana_operator NOLOGIN'
WHERE NOT EXISTS (
    SELECT 1 FROM pg_roles WHERE rolname = 'manchester_arcana_operator'
) \gexec

ALTER ROLE manchester_arcana_migration
    NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
ALTER ROLE manchester_arcana_app
    NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
ALTER ROLE manchester_arcana_backup
    NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
ALTER ROLE manchester_arcana_operator
    NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;

REVOKE CREATE ON SCHEMA public FROM PUBLIC;
GRANT CONNECT ON DATABASE :DBNAME TO
    manchester_arcana_migration,
    manchester_arcana_app,
    manchester_arcana_backup,
    manchester_arcana_operator;
GRANT USAGE ON SCHEMA public TO
    manchester_arcana_app,
    manchester_arcana_backup,
    manchester_arcana_operator;
GRANT USAGE, CREATE ON SCHEMA public TO manchester_arcana_migration;

-- Run this bootstrap after the initial migrations. Ownership transfer lets a
-- future credential-bearing migration member alter the existing schema after
-- `SET ROLE manchester_arcana_migration`.
SELECT format('ALTER TABLE %I.%I OWNER TO manchester_arcana_migration', schemaname, tablename)
FROM pg_tables
WHERE schemaname = 'public'
ORDER BY tablename
\gexec
SELECT format('ALTER SEQUENCE %I.%I OWNER TO manchester_arcana_migration', sequence_schema, sequence_name)
FROM information_schema.sequences
WHERE sequence_schema = 'public'
ORDER BY sequence_name
\gexec

GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public
    TO manchester_arcana_app;
GRANT USAGE, SELECT, UPDATE ON ALL SEQUENCES IN SCHEMA public
    TO manchester_arcana_app;
GRANT SELECT ON ALL TABLES IN SCHEMA public TO manchester_arcana_backup;

-- Operational collection sees only body-free queue/migration/recovery state
-- plus PostgreSQL's pg_monitor views. It cannot read campaign/source tables.
GRANT pg_monitor TO manchester_arcana_operator;
GRANT SELECT ON _sqlx_migrations, generation_jobs, operator_recovery_status
    TO manchester_arcana_operator;
GRANT UPDATE (
    last_backup_completed_at,
    last_backup_vault_digest,
    last_restore_test_completed_at,
    last_restore_test_result,
    last_restore_source_digest,
    updated_at
) ON operator_recovery_status TO manchester_arcana_operator;

-- The ordinary application cannot rewrite operator recovery evidence.
REVOKE INSERT, UPDATE, DELETE ON operator_recovery_status
    FROM manchester_arcana_app;

ALTER DEFAULT PRIVILEGES FOR ROLE manchester_arcana_migration IN SCHEMA public
    GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO manchester_arcana_app;
ALTER DEFAULT PRIVILEGES FOR ROLE manchester_arcana_migration IN SCHEMA public
    GRANT USAGE, SELECT, UPDATE ON SEQUENCES TO manchester_arcana_app;
ALTER DEFAULT PRIVILEGES FOR ROLE manchester_arcana_migration IN SCHEMA public
    GRANT SELECT ON TABLES TO manchester_arcana_backup;

-- Narrow the operator again after broad backup/app defaults are established.
ALTER ROLE manchester_arcana_app SET default_transaction_isolation = 'read committed';
ALTER ROLE manchester_arcana_app SET statement_timeout = '30s';
ALTER ROLE manchester_arcana_app SET lock_timeout = '5s';
ALTER ROLE manchester_arcana_app SET idle_in_transaction_session_timeout = '15s';
