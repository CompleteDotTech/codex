GRANT USAGE ON SCHEMA codex_storage TO codex_runtime, codex_backup;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA codex_storage TO codex_runtime;
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA codex_storage TO codex_runtime;
GRANT SELECT ON ALL TABLES IN SCHEMA codex_storage TO codex_backup;
GRANT SELECT ON ALL SEQUENCES IN SCHEMA codex_storage TO codex_backup;
-- Archives omit ACLs. Restore the protected bootstrap policy in the same
-- transaction as the schema, before any runtime connection can observe it.
DO $restore_access$
DECLARE
    metadata regclass := to_regclass('codex_storage.codex_schema_meta');
    history regclass := to_regclass('codex_storage._codex_pg_migrations');
BEGIN
    IF metadata IS NOT NULL THEN
        REVOKE ALL ON codex_storage.codex_schema_meta FROM codex_runtime;
        GRANT SELECT ON codex_storage.codex_schema_meta TO codex_runtime;
    END IF;
    IF history IS NOT NULL THEN
        REVOKE ALL ON codex_storage._codex_pg_migrations FROM codex_runtime;
    END IF;
    -- Destination-wide default ACLs can grant access through PUBLIC or another
    -- role as archive objects are recreated. Refuse this drift transactionally;
    -- changing the operator's role memberships or global defaults is not repair.
    IF EXISTS (
        SELECT 1 FROM pg_roles
        WHERE (pg_has_role('codex_runtime', oid, 'USAGE')
               OR pg_has_role('codex_runtime', oid, 'SET'))
          AND (
            has_schema_privilege(oid, 'codex_storage', 'CREATE')
            OR (metadata IS NOT NULL AND (
                has_table_privilege(oid, metadata, 'INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')
                OR has_any_column_privilege(oid, metadata, 'INSERT,UPDATE,REFERENCES')
            ))
            OR (history IS NOT NULL AND (
                has_table_privilege(oid, history, 'SELECT,INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')
                OR has_any_column_privilege(oid, history, 'SELECT,INSERT,UPDATE,REFERENCES')
            ))
            OR EXISTS (
                SELECT 1 FROM pg_class sequence
                WHERE sequence.relnamespace = 'codex_storage'::regnamespace
                  AND sequence.relkind = 'S'
                  AND has_sequence_privilege(oid, sequence.oid, 'UPDATE')
            )
          )
    ) THEN
        RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'unsafe runtime privileges';
    END IF;
    IF EXISTS (
        SELECT 1 FROM pg_roles
        WHERE (rolname = 'codex_backup'
               OR pg_has_role('codex_backup', oid, 'USAGE')
               OR pg_has_role('codex_backup', oid, 'SET'))
          AND (
            has_table_privilege(oid, metadata, 'INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')
            OR has_any_column_privilege(oid, metadata, 'INSERT,UPDATE,REFERENCES')
            OR has_table_privilege(oid, history, 'INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')
            OR has_any_column_privilege(oid, history, 'INSERT,UPDATE,REFERENCES')
            OR EXISTS (
                SELECT 1 FROM pg_class sequence
                WHERE sequence.relnamespace = 'codex_storage'::regnamespace
                  AND sequence.relkind = 'S'
                  AND has_sequence_privilege(oid, sequence.oid, 'USAGE,UPDATE')
            )
          )
    ) THEN
        RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'unsafe backup privileges';
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
