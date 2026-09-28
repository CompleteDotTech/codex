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
    IF (metadata IS NULL) <> (history IS NULL) THEN
        RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'unrecognized protected metadata';
    END IF;
    IF metadata IS NOT NULL THEN
        IF (SELECT count(*) FROM pg_attribute WHERE attrelid = metadata AND attnum > 0 AND NOT attisdropped) <> 4
          OR (SELECT count(*) FROM pg_attribute WHERE attrelid = history AND attnum > 0 AND NOT attisdropped) <> 6
          OR EXISTS (
            SELECT 1 FROM (VALUES
                (metadata, 'singleton', 'boolean'::regtype, 'true'),
                (metadata, 'format_version', 'integer'::regtype, NULL),
                (metadata, 'min_reader_version', 'integer'::regtype, NULL),
                (metadata, 'min_writer_version', 'integer'::regtype, NULL),
                (history, 'version', 'bigint'::regtype, NULL),
                (history, 'description', 'text'::regtype, NULL),
                (history, 'installed_on', 'timestamptz'::regtype, 'now()'),
                (history, 'success', 'boolean'::regtype, NULL),
                (history, 'checksum', 'bytea'::regtype, NULL),
                (history, 'execution_time', 'bigint'::regtype, NULL)
            ) required(relation_oid, name, type_oid, expression)
            WHERE NOT EXISTS (
                SELECT 1 FROM pg_attribute attribute
                LEFT JOIN pg_attrdef definition ON definition.adrelid = attribute.attrelid
                  AND definition.adnum = attribute.attnum
                WHERE attribute.attrelid = required.relation_oid
                  AND attribute.attnum > 0 AND NOT attribute.attisdropped
                  AND attribute.attname = required.name
                  AND attribute.atttypid = required.type_oid
                  AND attribute.attnotnull
                  AND attribute.attidentity = '' AND attribute.attgenerated = ''
                  AND pg_get_expr(definition.adbin, definition.adrelid) IS NOT DISTINCT FROM required.expression
            )
        ) THEN
            RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'unrecognized protected columns';
        END IF;
        IF EXISTS (
            SELECT 1 FROM (VALUES (metadata), (history)) required(relation_oid)
            LEFT JOIN pg_class relation ON relation.oid = required.relation_oid
            WHERE relation.relkind <> 'r'
               OR relation.relowner <> 'codex_owner'::regrole
               OR relation.relpersistence <> 'p'
               OR relation.reloftype <> 0
               OR relation.relrowsecurity
        ) OR EXISTS (
            SELECT 1 FROM pg_inherits inheritance
            WHERE inheritance.inhrelid IN (metadata, history)
               OR inheritance.inhparent IN (metadata, history)
        ) OR EXISTS (
            SELECT 1 FROM pg_trigger row_trigger
            WHERE row_trigger.tgrelid IN (metadata, history)
              AND NOT row_trigger.tgisinternal
        ) OR EXISTS (
            SELECT 1 FROM pg_rewrite rewrite
            WHERE rewrite.ev_class IN (metadata, history)
        ) OR NOT EXISTS (
            SELECT 1 FROM pg_constraint constraint_row
            WHERE constraint_row.conrelid = metadata
              AND constraint_row.contype = 'p'
              AND constraint_row.conindid = to_regclass('codex_storage.codex_schema_meta_pkey')
              AND constraint_row.conkey = ARRAY[(SELECT attnum FROM pg_attribute WHERE attrelid = metadata AND attname = 'singleton')]
        ) OR NOT EXISTS (
            SELECT 1 FROM pg_constraint constraint_row
            WHERE constraint_row.conrelid = history
              AND constraint_row.contype = 'p'
              AND constraint_row.conindid = to_regclass('codex_storage._codex_pg_migrations_pkey')
              AND constraint_row.conkey = ARRAY[(SELECT attnum FROM pg_attribute WHERE attrelid = history AND attname = 'version')]
        ) OR (SELECT count(*) FROM pg_constraint WHERE conrelid = history) <> 1
          OR (SELECT count(*) FROM pg_constraint WHERE conrelid = metadata) <> 5
          OR EXISTS (
            SELECT 1 FROM (VALUES
                ('codex_schema_meta_format_version_check', 'CHECK ((format_version > 0))'),
                ('codex_schema_meta_min_reader_version_check', 'CHECK ((min_reader_version > 0))'),
                ('codex_schema_meta_min_writer_version_check', 'CHECK ((min_writer_version > 0))')
            ) required(name, definition)
            WHERE NOT EXISTS (
                SELECT 1 FROM pg_constraint constraint_row
                WHERE constraint_row.conrelid = metadata
                  AND constraint_row.contype = 'c'
                  AND constraint_row.conname = required.name
                  AND pg_get_constraintdef(constraint_row.oid) = required.definition
            )
        )
          OR NOT EXISTS (
            SELECT 1 FROM pg_constraint constraint_row
            WHERE constraint_row.conrelid = metadata
              AND constraint_row.contype = 'c'
              AND constraint_row.conname = 'codex_schema_meta_singleton_check'
              AND pg_get_constraintdef(constraint_row.oid) = 'CHECK (singleton)'
        ) THEN
            RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'unrecognized protected metadata';
        END IF;
        -- Views and rules can supply base-table authority without a caller
        -- holding its ACL. Follow every dependent rule relation across schemas.
        IF EXISTS (
            WITH RECURSIVE protected_dependents(relation_oid) AS (
                SELECT metadata UNION SELECT history
                UNION
                SELECT rewrite.ev_class FROM pg_rewrite rewrite
                JOIN pg_depend dependency ON dependency.classid = 'pg_rewrite'::regclass
                  AND dependency.objid = rewrite.oid
                  AND dependency.refclassid = 'pg_class'::regclass
                JOIN protected_dependents parent ON parent.relation_oid = dependency.refobjid
            )
            SELECT 1 FROM protected_dependents dependent
            WHERE dependent.relation_oid NOT IN (metadata, history)
        ) THEN
            RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'unsupported protected rewrite dependency';
        END IF;
        IF NOT EXISTS (
            SELECT 1 FROM codex_storage.codex_schema_meta
            WHERE singleton IS TRUE AND format_version = 1
              AND min_reader_version = 1 AND min_writer_version = 1
        ) THEN
            RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'unrecognized protected metadata version';
        END IF;
    END IF;
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
        SELECT 1 FROM pg_roles candidate
        WHERE (pg_has_role('codex_runtime', candidate.oid, 'USAGE')
               OR pg_has_role('codex_runtime', candidate.oid, 'SET'))
          AND (
            has_schema_privilege(candidate.oid, 'codex_storage', 'CREATE')
            OR (metadata IS NOT NULL AND (
                has_table_privilege(candidate.oid, metadata, 'INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')
                OR has_any_column_privilege(candidate.oid, metadata, 'INSERT,UPDATE,REFERENCES')
            ))
            OR (history IS NOT NULL AND (
                has_table_privilege(candidate.oid, history, 'SELECT,INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')
                OR has_any_column_privilege(candidate.oid, history, 'SELECT,INSERT,UPDATE,REFERENCES')
            ))
            OR EXISTS (
                SELECT 1 FROM pg_class sequence
                WHERE sequence.relnamespace = 'codex_storage'::regnamespace
                  AND CASE WHEN sequence.relkind = 'S'
                    THEN has_sequence_privilege(candidate.oid, sequence.oid, 'UPDATE')
                    ELSE FALSE END
            )
          )
    ) THEN
        RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'unsafe runtime privileges';
    END IF;
    IF EXISTS (
        SELECT 1 FROM pg_roles candidate
        WHERE (candidate.rolname = 'codex_backup'
               OR pg_has_role('codex_backup', candidate.oid, 'USAGE')
               OR pg_has_role('codex_backup', candidate.oid, 'SET'))
          AND (
            has_schema_privilege(candidate.oid, 'codex_storage', 'CREATE')
            OR EXISTS (
                SELECT 1 FROM pg_class relation
                WHERE relation.relnamespace = 'codex_storage'::regnamespace
                  AND relation.relkind IN ('r', 'p', 'v', 'm', 'f')
                  AND (
                    has_table_privilege(candidate.oid, relation.oid, 'INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')
                    OR has_any_column_privilege(candidate.oid, relation.oid, 'INSERT,UPDATE,REFERENCES')
                  )
            )
            OR EXISTS (
                SELECT 1 FROM pg_class sequence
                WHERE sequence.relnamespace = 'codex_storage'::regnamespace
                  AND CASE WHEN sequence.relkind = 'S'
                    THEN has_sequence_privilege(candidate.oid, sequence.oid, 'USAGE,UPDATE')
                    ELSE FALSE END
            )
          )
    ) THEN
        RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'unsafe backup privileges';
    END IF;
    IF EXISTS (
        SELECT 1 FROM pg_roles candidate
        WHERE (candidate.rolname = 'codex_backup'
               OR pg_has_role('codex_backup', candidate.oid, 'USAGE')
               OR pg_has_role('codex_backup', candidate.oid, 'SET'))
          AND (pg_has_role(candidate.oid, 'codex_runtime', 'USAGE')
               OR pg_has_role(candidate.oid, 'codex_runtime', 'SET'))
    ) THEN
        RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'unsafe backup role escalation';
    END IF;
    -- Backup remains read-only for ordinary application archives too. Refuse
    -- role administration even when the target is outside the runtime graph.
    IF EXISTS (
        SELECT 1 FROM pg_auth_members membership
        WHERE membership.admin_option
          AND (pg_has_role('codex_backup', membership.member, 'USAGE')
               OR pg_has_role('codex_backup', membership.member, 'SET'))
    ) THEN
        RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'unsafe backup role administration';
    END IF;
    IF EXISTS (
        SELECT 1 FROM pg_roles candidate
        WHERE candidate.rolname NOT IN ('codex_owner', 'codex_migrator')
          AND left(candidate.rolname, 3) <> 'pg_'
          AND NOT candidate.rolsuper
          AND (
            pg_has_role(candidate.oid, 'codex_owner', 'USAGE')
            OR pg_has_role(candidate.oid, 'codex_owner', 'SET')
            OR has_schema_privilege(candidate.oid, 'codex_storage', 'CREATE')
            OR (metadata IS NOT NULL AND (
                has_table_privilege(candidate.oid, metadata, 'INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')
                OR has_any_column_privilege(candidate.oid, metadata, 'INSERT,UPDATE,REFERENCES')
            ))
            OR (history IS NOT NULL AND (
                has_table_privilege(candidate.oid, history, 'INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')
                OR has_any_column_privilege(candidate.oid, history, 'INSERT,UPDATE,REFERENCES')
            ))
          )
    ) THEN
        RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'unsafe foreign role privileges';
    END IF;
    IF EXISTS (
        WITH RECURSIVE owner_roles(roleid) AS (
            SELECT 'codex_owner'::regrole
            UNION
            SELECT membership.member
            FROM pg_auth_members membership
            JOIN owner_roles parent ON parent.roleid = membership.roleid
            WHERE membership.inherit_option OR membership.set_option OR membership.admin_option
        )
        SELECT 1 FROM pg_auth_members membership
        JOIN owner_roles parent ON parent.roleid = membership.roleid
        WHERE membership.admin_option
          AND membership.member <> 'codex_migrator'::regrole
    ) THEN
        RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'unsafe owner role administration';
    END IF;
    -- String and dynamic routine bodies need not record table dependencies.
    -- Refuse executable definer authority capable of crossing the store policy.
    IF EXISTS (
        WITH callers AS (
            SELECT candidate.oid, audience.roleid
            FROM pg_roles candidate
            CROSS JOIN (VALUES ('codex_runtime'::regrole), ('codex_backup'::regrole)) audience(roleid)
            WHERE pg_has_role(audience.roleid, candidate.oid, 'USAGE')
               OR pg_has_role(audience.roleid, candidate.oid, 'SET')
        )
        SELECT 1 FROM pg_proc routine
        JOIN callers caller ON has_function_privilege(caller.oid, routine.oid, 'EXECUTE')
        WHERE routine.prosecdef
          AND routine.pronamespace NOT IN ('pg_catalog'::regnamespace, 'information_schema'::regnamespace)
          AND has_schema_privilege(caller.oid, routine.pronamespace, 'USAGE')
          AND (
            has_schema_privilege(routine.proowner, 'codex_storage', 'CREATE')
            OR (metadata IS NOT NULL AND (
                has_table_privilege(routine.proowner, metadata, 'INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')
                OR has_any_column_privilege(routine.proowner, metadata, 'INSERT,UPDATE,REFERENCES')
            ))
            OR (history IS NOT NULL AND (
                has_table_privilege(routine.proowner, history, 'INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')
                OR has_any_column_privilege(routine.proowner, history, 'INSERT,UPDATE,REFERENCES')
                OR (caller.roleid = 'codex_runtime'::regrole AND (
                    has_table_privilege(routine.proowner, history, 'SELECT')
                    OR has_any_column_privilege(routine.proowner, history, 'SELECT')
                ))
            ))
            OR (caller.roleid = 'codex_backup'::regrole AND EXISTS (
                SELECT 1 FROM pg_class relation
                WHERE relation.relnamespace = 'codex_storage'::regnamespace
                  AND relation.relkind IN ('r', 'p', 'v', 'm', 'f')
                  AND (
                    has_table_privilege(routine.proowner, relation.oid, 'INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')
                    OR has_any_column_privilege(routine.proowner, relation.oid, 'INSERT,UPDATE,REFERENCES')
                  )
            ))
            OR EXISTS (
                SELECT 1 FROM pg_class sequence
                WHERE sequence.relnamespace = 'codex_storage'::regnamespace
                  AND CASE WHEN sequence.relkind = 'S' THEN has_sequence_privilege(
                    routine.proowner, sequence.oid,
                    CASE WHEN caller.roleid = 'codex_backup'::regrole THEN 'USAGE,UPDATE' ELSE 'UPDATE' END
                  ) ELSE FALSE END
            )
          )
    ) THEN
        RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'unsafe security definer routine';
    END IF;
    IF EXISTS (
        SELECT 1 FROM pg_class relation,
             LATERAL aclexplode(coalesce(relation.relacl, acldefault('r', relation.relowner))) acl
        WHERE relation.oid = history
          AND acl.privilege_type = 'SELECT'
          AND acl.is_grantable
          AND acl.grantee NOT IN ('codex_owner'::regrole, 'codex_migrator'::regrole)
    ) OR EXISTS (
        SELECT 1 FROM pg_attribute attribute,
             LATERAL aclexplode(attribute.attacl) acl
        WHERE attribute.attrelid = history
          AND acl.privilege_type = 'SELECT'
          AND acl.is_grantable
          AND acl.grantee NOT IN ('codex_owner'::regrole, 'codex_migrator'::regrole)
    ) THEN
        RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'unsafe history grant options';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_default_acl defaults
        WHERE defaults.defaclrole = 'codex_owner'::regrole
          AND defaults.defaclnamespace = 0
          AND defaults.defaclobjtype = 'f'
          AND NOT EXISTS (
              SELECT 1 FROM aclexplode(defaults.defaclacl) acl
              WHERE acl.grantee <> 'codex_owner'::regrole
                AND acl.privilege_type = 'EXECUTE'
          )
    ) OR EXISTS (
        SELECT 1 FROM pg_default_acl defaults,
             LATERAL aclexplode(defaults.defaclacl) acl
        WHERE defaults.defaclrole = 'codex_owner'::regrole
          AND defaults.defaclobjtype = 'f'
          AND defaults.defaclnamespace = 'codex_storage'::regnamespace
          AND acl.grantee <> 'codex_owner'::regrole
          AND acl.privilege_type = 'EXECUTE'
    ) THEN
        RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'unsafe function defaults';
    END IF;
    IF EXISTS (
        SELECT 1 FROM pg_default_acl defaults,
             LATERAL aclexplode(defaults.defaclacl) acl
        WHERE defaults.defaclrole = 'codex_owner'::regrole
          AND defaults.defaclobjtype = 'S'
          AND defaults.defaclnamespace IN (0, 'codex_storage'::regnamespace)
          AND acl.grantee NOT IN ('codex_owner'::regrole, 'codex_runtime'::regrole)
          AND acl.privilege_type IN ('USAGE', 'UPDATE')
    ) THEN
        RAISE EXCEPTION USING ERRCODE = '42501', MESSAGE = 'unsafe sequence defaults';
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
