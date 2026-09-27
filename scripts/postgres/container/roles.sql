SET log_statement = 'none';
SET log_min_error_statement = 'panic';
CREATE ROLE codex_owner NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
SELECT format('CREATE ROLE %I LOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS PASSWORD %L',
    'codex_' || role, pg_read_file('/run/codex-pg/' || role || '.password'))
FROM (VALUES ('runtime'), ('migrator'), ('backup')) AS credentials(role)
\gexec
GRANT codex_owner TO codex_migrator WITH INHERIT FALSE, SET TRUE;
REVOKE ALL ON DATABASE codex FROM PUBLIC;
GRANT CONNECT ON DATABASE codex TO codex_runtime, codex_migrator, codex_backup;
GRANT CREATE ON DATABASE codex TO codex_owner;
REVOKE ALL ON SCHEMA public FROM PUBLIC;
CREATE SCHEMA codex_storage AUTHORIZATION codex_owner;
GRANT USAGE ON SCHEMA codex_storage TO codex_runtime, codex_backup;
ALTER DEFAULT PRIVILEGES FOR ROLE codex_owner IN SCHEMA codex_storage
    GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO codex_runtime;
ALTER DEFAULT PRIVILEGES FOR ROLE codex_owner IN SCHEMA codex_storage
    GRANT USAGE, SELECT ON SEQUENCES TO codex_runtime;
ALTER DEFAULT PRIVILEGES FOR ROLE codex_owner IN SCHEMA codex_storage
    GRANT SELECT ON TABLES TO codex_backup;
ALTER DEFAULT PRIVILEGES FOR ROLE codex_owner IN SCHEMA codex_storage
    GRANT SELECT ON SEQUENCES TO codex_backup;
-- Function EXECUTE defaults are global; a schema-scoped revoke is insufficient.
ALTER DEFAULT PRIVILEGES FOR ROLE codex_owner REVOKE EXECUTE ON FUNCTIONS FROM PUBLIC;
ALTER ROLE codex_runtime IN DATABASE codex SET search_path = pg_catalog, codex_storage;
ALTER ROLE codex_backup IN DATABASE codex SET search_path = pg_catalog, codex_storage;
ALTER ROLE codex_migrator IN DATABASE codex SET search_path = pg_catalog, codex_storage;
CREATE SCHEMA codex_service AUTHORIZATION postgres;
REVOKE ALL ON SCHEMA codex_service FROM PUBLIC;
CREATE TABLE codex_service.bootstrap (instance text PRIMARY KEY, format integer NOT NULL);
\getenv instance CODEX_PG_INSTANCE
INSERT INTO codex_service.bootstrap VALUES (:'instance', 1);
