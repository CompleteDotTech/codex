GRANT USAGE ON SCHEMA codex_storage TO codex_runtime, codex_backup;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA codex_storage TO codex_runtime;
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA codex_storage TO codex_runtime;
GRANT SELECT ON ALL TABLES IN SCHEMA codex_storage TO codex_backup;
GRANT SELECT ON ALL SEQUENCES IN SCHEMA codex_storage TO codex_backup;
-- Archives omit ACLs. Restore the protected bootstrap policy in the same
-- transaction as the schema, before any runtime connection can observe it.
DO $restore_access$
BEGIN
    IF to_regclass('codex_storage.codex_schema_meta') IS NOT NULL THEN
        REVOKE ALL ON codex_storage.codex_schema_meta FROM codex_runtime;
        GRANT SELECT ON codex_storage.codex_schema_meta TO codex_runtime;
    END IF;
    IF to_regclass('codex_storage._codex_pg_migrations') IS NOT NULL THEN
        REVOKE ALL ON codex_storage._codex_pg_migrations FROM codex_runtime;
    END IF;
END
$restore_access$;
ALTER DEFAULT PRIVILEGES FOR ROLE codex_owner IN SCHEMA codex_storage
    GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO codex_runtime;
ALTER DEFAULT PRIVILEGES FOR ROLE codex_owner IN SCHEMA codex_storage
    GRANT USAGE, SELECT ON SEQUENCES TO codex_runtime;
ALTER DEFAULT PRIVILEGES FOR ROLE codex_owner IN SCHEMA codex_storage
    GRANT SELECT ON TABLES TO codex_backup;
ALTER DEFAULT PRIVILEGES FOR ROLE codex_owner IN SCHEMA codex_storage
    GRANT SELECT ON SEQUENCES TO codex_backup;
