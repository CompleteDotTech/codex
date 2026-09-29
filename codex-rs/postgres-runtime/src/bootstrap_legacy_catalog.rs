use sqlx::PgConnection;

// Versions 1-4 predate the protected-table ACL tightening. Before changing
// those ACLs, require the exact catalog shape produced by their migrations.
// The qualified server is PostgreSQL 17, whose pg_get_* definitions are stable
// for these migration statements.
const LEGACY_CATALOG_SQL: &str = "
WITH expected_relations(version, name, kind, definition) AS (
    VALUES
      (1, 'codex_schema_meta', 'r', NULL),
      (1, 'codex_schema_meta_pkey', 'i', 'CREATE UNIQUE INDEX codex_schema_meta_pkey ON codex_storage.codex_schema_meta USING btree (singleton)'),
      (1, '_codex_pg_migrations', 'r', NULL),
      (1, '_codex_pg_migrations_pkey', 'i', 'CREATE UNIQUE INDEX _codex_pg_migrations_pkey ON codex_storage._codex_pg_migrations USING btree (version)'),
      (2, 'thread_spawn_edges', 'r', NULL),
      (2, 'thread_spawn_edges_pkey', 'i', 'CREATE UNIQUE INDEX thread_spawn_edges_pkey ON codex_storage.thread_spawn_edges USING btree (child_thread_id)'),
      (2, 'idx_thread_spawn_edges_parent_status', 'i', 'CREATE INDEX idx_thread_spawn_edges_parent_status ON codex_storage.thread_spawn_edges USING btree (parent_thread_id, status, child_thread_id)'),
      (3, 'external_agent_config_imports', 'r', NULL),
      (3, 'external_agent_config_imports_pkey', 'i', 'CREATE UNIQUE INDEX external_agent_config_imports_pkey ON codex_storage.external_agent_config_imports USING btree (import_id)'),
      (3, 'idx_external_agent_config_imports_history', 'i', 'CREATE INDEX idx_external_agent_config_imports_history ON codex_storage.external_agent_config_imports USING btree (completed_at_ms DESC, import_id)'),
      (4, 'threads', 'r', NULL),
      (4, 'threads_pkey', 'i', 'CREATE UNIQUE INDEX threads_pkey ON codex_storage.threads USING btree (id)'),
      (4, 'idx_threads_recency_id', 'i', 'CREATE INDEX idx_threads_recency_id ON codex_storage.threads USING btree (recency_at_ms DESC, id DESC)')
), relations AS (
    SELECT relation.relname::text AS name, relation.relkind::text AS kind,
           CASE WHEN relation.relkind = 'i' THEN pg_get_indexdef(relation.oid) END AS definition,
           relation.relowner = 'codex_owner'::regrole
             AND relation.relpersistence = 'p' AND NOT relation.relrowsecurity
             AND NOT relation.relforcerowsecurity AS safe
    FROM pg_class relation
    WHERE relation.relnamespace = 'codex_storage'::regnamespace
), expected_columns(version, relation, name, type, required, default_expr) AS (
    VALUES
      (2, 'thread_spawn_edges', 'parent_thread_id', 'uuid', TRUE, NULL),
      (2, 'thread_spawn_edges', 'child_thread_id', 'uuid', TRUE, NULL),
      (2, 'thread_spawn_edges', 'status', 'text', TRUE, NULL),
      (3, 'external_agent_config_imports', 'import_id', 'text', TRUE, NULL),
      (3, 'external_agent_config_imports', 'provider_id', 'text', FALSE, NULL),
      (3, 'external_agent_config_imports', 'completed_at_ms', 'bigint', TRUE, NULL),
      (3, 'external_agent_config_imports', 'successes', 'text', TRUE, NULL),
      (3, 'external_agent_config_imports', 'failures', 'text', TRUE, NULL),
      (4, 'threads', 'id', 'uuid', TRUE, NULL),
      (4, 'threads', 'originator', 'text', FALSE, NULL),
      (4, 'threads', 'creator_user_id', 'text', FALSE, NULL),
      (4, 'threads', 'creator_account_id', 'text', FALSE, NULL),
      (4, 'threads', 'origin_rollout_path', 'text', TRUE, NULL),
      (4, 'threads', 'created_at_ms', 'bigint', TRUE, NULL),
      (4, 'threads', 'updated_at_ms', 'bigint', TRUE, NULL),
      (4, 'threads', 'recency_at_ms', 'bigint', TRUE, NULL),
      (4, 'threads', 'source', 'text', TRUE, NULL),
      (4, 'threads', 'history_mode', 'text', TRUE, NULL),
      (4, 'threads', 'thread_source', 'text', FALSE, NULL),
      (4, 'threads', 'agent_nickname', 'text', FALSE, NULL),
      (4, 'threads', 'agent_role', 'text', FALSE, NULL),
      (4, 'threads', 'agent_path', 'text', FALSE, NULL),
      (4, 'threads', 'model_provider', 'text', TRUE, NULL),
      (4, 'threads', 'model', 'text', FALSE, NULL),
      (4, 'threads', 'reasoning_effort', 'text', FALSE, NULL),
      (4, 'threads', 'origin_cwd', 'text', TRUE, NULL),
      (4, 'threads', 'cli_version', 'text', TRUE, NULL),
      (4, 'threads', 'title', 'text', TRUE, NULL),
      (4, 'threads', 'name', 'text', FALSE, NULL),
      (4, 'threads', 'preview', 'text', FALSE, NULL),
      (4, 'threads', 'sandbox_policy', 'text', TRUE, NULL),
      (4, 'threads', 'approval_mode', 'text', TRUE, NULL),
      (4, 'threads', 'tokens_used', 'bigint', TRUE, '0'),
      (4, 'threads', 'first_user_message', 'text', FALSE, NULL),
      (4, 'threads', 'archived_at_s', 'bigint', FALSE, NULL),
      (4, 'threads', 'thread_section_id', 'text', FALSE, NULL),
      (4, 'threads', 'section_position', 'bigint', FALSE, NULL),
      (4, 'threads', 'section_entered_at_ms', 'bigint', FALSE, NULL),
      (4, 'threads', 'project_id', 'text', FALSE, NULL),
      (4, 'threads', 'daybreak_enabled', 'boolean', FALSE, NULL),
      (4, 'threads', 'git_sha', 'text', FALSE, NULL),
      (4, 'threads', 'git_branch', 'text', FALSE, NULL),
      (4, 'threads', 'git_origin_url', 'text', FALSE, NULL)
), columns AS (
    SELECT relation.relname::text AS relation, attribute.attname::text AS name,
           attribute.atttypid::regtype::text AS type, attribute.attnotnull AS required,
           pg_get_expr(definition.adbin, definition.adrelid) AS default_expr,
           NOT attribute.attisdropped AND attribute.attgenerated = ''
             AND attribute.attidentity = ''
             AND (attribute.atttypid <> 'text'::regtype
                  OR attribute.attcollation = 'default'::regcollation) AS safe
    FROM pg_class relation
    JOIN pg_attribute attribute ON attribute.attrelid = relation.oid AND attribute.attnum > 0
    LEFT JOIN pg_attrdef definition ON definition.adrelid = attribute.attrelid
      AND definition.adnum = attribute.attnum
    WHERE relation.relnamespace = 'codex_storage'::regnamespace
      AND relation.relname IN ('thread_spawn_edges', 'external_agent_config_imports', 'threads')
), expected_constraints(version, relation, name, definition) AS (
    VALUES
      (2, 'thread_spawn_edges', 'thread_spawn_edges_pkey', 'PRIMARY KEY (child_thread_id)'),
      (2, 'thread_spawn_edges', 'thread_spawn_edges_status_check', 'CHECK ((status = ANY (ARRAY[''open''::text, ''closed''::text])))'),
      (3, 'external_agent_config_imports', 'external_agent_config_imports_pkey', 'PRIMARY KEY (import_id)'),
      (4, 'threads', 'threads_pkey', 'PRIMARY KEY (id)')
), constraints AS (
    SELECT relation.relname::text AS relation, cst.conname::text AS name,
           pg_get_constraintdef(cst.oid) AS definition
    FROM pg_constraint cst JOIN pg_class relation ON relation.oid = cst.conrelid
    WHERE relation.relnamespace = 'codex_storage'::regnamespace
      AND relation.relname IN ('thread_spawn_edges', 'external_agent_config_imports', 'threads')
)
SELECT NOT EXISTS (
    SELECT 1 FROM (SELECT name, kind, definition FROM expected_relations WHERE version <= $1) expected
    FULL JOIN relations actual USING (name)
    WHERE expected.kind IS DISTINCT FROM actual.kind
       OR expected.definition IS DISTINCT FROM actual.definition
       OR actual.safe IS DISTINCT FROM TRUE
) AND NOT EXISTS (
    SELECT 1 FROM (SELECT relation, name, type, required, default_expr
                   FROM expected_columns WHERE version <= $1) expected
    FULL JOIN columns actual USING (relation, name)
    WHERE expected.type IS DISTINCT FROM actual.type
       OR expected.required IS DISTINCT FROM actual.required
       OR expected.default_expr IS DISTINCT FROM actual.default_expr
       OR actual.safe IS DISTINCT FROM TRUE
) AND NOT EXISTS (
    SELECT 1 FROM (SELECT relation, name, definition FROM expected_constraints WHERE version <= $1) expected
    FULL JOIN constraints actual USING (relation, name)
    WHERE expected.definition IS DISTINCT FROM actual.definition
) AND NOT EXISTS (
    SELECT 1 FROM pg_trigger trigger
    JOIN pg_class relation ON relation.oid = trigger.tgrelid
    WHERE relation.relnamespace = 'codex_storage'::regnamespace
      AND NOT trigger.tgisinternal
) AND NOT EXISTS (
    SELECT 1 FROM pg_rewrite rewrite
    JOIN pg_class relation ON relation.oid = rewrite.ev_class
    WHERE relation.relnamespace = 'codex_storage'::regnamespace
) AND NOT EXISTS (
    SELECT 1 FROM pg_inherits inheritance
    JOIN pg_class relation ON relation.oid IN (inheritance.inhrelid, inheritance.inhparent)
    WHERE relation.relnamespace = 'codex_storage'::regnamespace
) AND NOT EXISTS (
    SELECT 1 FROM pg_constraint cst
    JOIN pg_class referenced ON referenced.oid = cst.confrelid
    WHERE cst.contype = 'f'
      AND referenced.relnamespace = 'codex_storage'::regnamespace
) AND NOT EXISTS (
    SELECT 1 FROM pg_policy policy
    JOIN pg_class relation ON relation.oid = policy.polrelid
    WHERE relation.relnamespace = 'codex_storage'::regnamespace
) AND NOT EXISTS (
    SELECT 1 FROM pg_index index_catalog
    JOIN pg_class relation ON relation.oid = index_catalog.indexrelid
    WHERE relation.relnamespace = 'codex_storage'::regnamespace
      AND (NOT index_catalog.indisvalid OR NOT index_catalog.indisready)
) AND NOT EXISTS (
    SELECT 1 FROM pg_class relation
    JOIN expected_relations expected ON expected.name = relation.relname
    WHERE relation.relnamespace = 'codex_storage'::regnamespace
      AND expected.version BETWEEN 2 AND $1 AND expected.kind = 'r'
      AND NOT (
          cardinality(relation.relacl) = 3
          AND relation.relacl::text[] @> ARRAY[
              'codex_owner=arwdDxtm/codex_owner',
              'codex_backup=r/codex_owner'
          ]::text[]
          AND (
              relation.relacl::text[] @> ARRAY['codex_runtime=arwd/codex_owner']::text[]
              OR (relation.relname = 'external_agent_config_imports'
                  AND relation.relacl::text[] @> ARRAY['codex_runtime=arw/codex_owner']::text[])
          )
      )
) AND NOT EXISTS (
    SELECT 1 FROM pg_attribute attribute
    JOIN pg_class relation ON relation.oid = attribute.attrelid
    JOIN expected_relations expected ON expected.name = relation.relname
    WHERE relation.relnamespace = 'codex_storage'::regnamespace
      AND expected.version BETWEEN 2 AND $1 AND expected.kind = 'r'
      AND attribute.attnum > 0 AND attribute.attacl IS NOT NULL
)
";

pub(super) async fn matches(
    connection: &mut PgConnection,
    version: i32,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(LEGACY_CATALOG_SQL)
        .bind(version)
        .fetch_one(connection)
        .await
}
