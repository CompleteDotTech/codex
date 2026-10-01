//! Transactional bootstrap of one preprovisioned named namespace.

use crate::BootstrapError;
use crate::NamedNamespace;
use crate::PostgresPool;
use crate::bootstrap::BASE_MIGRATOR;
use crate::bootstrap::BOOTSTRAP_TIMEOUT;
use crate::bootstrap::LOCK_CLASS;
use crate::bootstrap::LOCK_RESOURCE;
use sqlx::Acquire;
use sqlx::AssertSqlSafe;
use sqlx::SqlSafeStr;
use sqlx::migrate::Migration;
use sqlx::migrate::Migrator;
use std::borrow::Cow;
use tokio::time::timeout;

fn classify(error: &sqlx::Error) -> BootstrapError {
    match error {
        sqlx::Error::Database(error) if error.code().as_deref() == Some("42501") => {
            BootstrapError::Privilege
        }
        _ => BootstrapError::Migration,
    }
}

pub(crate) fn namespaced_migrations(
    namespace: &NamedNamespace,
) -> Result<Vec<Migration>, BootstrapError> {
    let [metadata, graph, imports, threads] = BASE_MIGRATOR.migrations.as_ref() else {
        return Err(BootstrapError::Migration);
    };
    let source = metadata.sql.as_str();
    // Version 1 has exactly two schema-qualified identifiers: CREATE and INSERT.
    // A changed SQL layout needs a new review before identifier substitution.
    if metadata.version != 1
        || metadata.no_tx
        || !source.starts_with("CREATE TABLE codex_storage.codex_schema_meta (")
        || !source.contains("\nINSERT INTO codex_storage.codex_schema_meta\n")
        || source.matches("codex_storage.").count() != 2
    {
        return Err(BootstrapError::Migration);
    }
    let graph_source = graph.sql.as_str();
    // Version 2 has only the reviewed table, index target and metadata update.
    if graph.version != 2
        || graph.no_tx
        || !graph_source.starts_with("CREATE TABLE codex_storage.thread_spawn_edges (")
        || !graph_source.contains("\n    ON codex_storage.thread_spawn_edges (")
        || !graph_source.contains("\nUPDATE codex_storage.codex_schema_meta\n")
        || graph_source.matches("codex_storage.").count() != 3
    {
        return Err(BootstrapError::Migration);
    }
    let imports_source = imports.sql.as_str();
    // Version 3 has only the reviewed table, index target and metadata update.
    if imports.version != 3
        || imports.no_tx
        || !imports_source.starts_with("CREATE TABLE codex_storage.external_agent_config_imports (")
        || !imports_source.contains("\n    ON codex_storage.external_agent_config_imports (")
        || !imports_source.contains("\nUPDATE codex_storage.codex_schema_meta\n")
        || imports_source.matches("codex_storage.").count() != 3
    {
        return Err(BootstrapError::Migration);
    }
    let threads_source = threads.sql.as_str();
    // Version 4 has only the reviewed table, index target, and metadata update.
    if threads.version != 4
        || threads.no_tx
        || !threads_source.contains("\nCREATE TABLE codex_storage.threads (\n")
        || !threads_source
            .contains("\n    ON codex_storage.threads (recency_at_ms DESC, id DESC);\n")
        || !threads_source.contains("\nUPDATE codex_storage.codex_schema_meta\n")
        || threads_source.matches("codex_storage.").count() != 3
    {
        return Err(BootstrapError::Migration);
    }
    let qualified_prefix = format!("{}.", namespace.quoted_schema());
    Ok([metadata, graph, imports, threads]
        .into_iter()
        .map(|base| {
            let sql = base
                .sql
                .as_str()
                .replace("codex_storage.", &qualified_prefix);
            Migration::new(
                base.version,
                base.description.clone(),
                base.migration_type,
                AssertSqlSafe(sql).into_sql_str(),
                base.no_tx,
            )
        })
        .collect())
}

/// Bootstrap a distinct preprovisioned schema using its matching migrator login.
/// This only creates metadata/history; it never selects or activates a backend.
pub async fn bootstrap_named_namespace(
    pool: &PostgresPool,
    namespace: &NamedNamespace,
) -> Result<(), BootstrapError> {
    let migrations = namespaced_migrations(namespace)?;
    timeout(BOOTSTRAP_TIMEOUT, async {
        let mut connection = pool.acquire().await.map_err(BootstrapError::Connection)?;
        let mut transaction = connection.begin().await.map_err(|error| classify(&error))?;
        let login: String = sqlx::query_scalar("SELECT session_user")
            .fetch_one(&mut *transaction)
            .await
            .map_err(|error| classify(&error))?;
        if login != namespace.migrator {
            return Err(BootstrapError::Privilege);
        }
        sqlx::query("SELECT pg_advisory_xact_lock($1, $2)")
            .bind(LOCK_CLASS)
            .bind(LOCK_RESOURCE)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|error| classify(&error))?;
        sqlx::query(AssertSqlSafe(format!(
            "SET LOCAL ROLE {}",
            namespace.quoted_owner()
        )))
        .execute(&mut *transaction)
        .await
        .map_err(|error| classify(&error))?;

        let owner: Option<String> = sqlx::query_scalar(
            "SELECT pg_get_userbyid(nspowner) FROM pg_namespace WHERE nspname = $1",
        )
        .bind(&namespace.schema)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| classify(&error))?;
        if owner.as_deref() != Some(namespace.owner.as_str()) {
            return Err(BootstrapError::IncompatibleNamespace);
        }
        let unexpected_objects: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_class WHERE relnamespace = (SELECT oid FROM pg_namespace WHERE nspname = $1) AND relname NOT IN ('_codex_pg_migrations', '_codex_pg_migrations_pkey', 'codex_schema_meta', 'codex_schema_meta_pkey', 'thread_spawn_edges', 'thread_spawn_edges_pkey', 'idx_thread_spawn_edges_parent_status', 'external_agent_config_imports', 'external_agent_config_imports_pkey', 'idx_external_agent_config_imports_history', 'threads', 'threads_pkey', 'idx_threads_recency_id')) OR EXISTS (SELECT 1 FROM pg_proc WHERE pronamespace = (SELECT oid FROM pg_namespace WHERE nspname = $1)) OR EXISTS (SELECT 1 FROM pg_type WHERE typnamespace = (SELECT oid FROM pg_namespace WHERE nspname = $1) AND typtype <> 'b' AND typrelid = 0)",
        )
        .bind(&namespace.schema)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| classify(&error))?;
        if unexpected_objects {
            return Err(BootstrapError::IncompatibleNamespace);
        }
        let history_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_class WHERE relnamespace = (SELECT oid FROM pg_namespace WHERE nspname = $1) AND relname = '_codex_pg_migrations' AND relkind = 'r')",
        )
        .bind(&namespace.schema)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| classify(&error))?;
        if history_exists {
            let metadata = format!(
                "SELECT format_version FROM {}.\"codex_schema_meta\" WHERE singleton = TRUE",
                namespace.quoted_schema()
            );
            let format: Option<i32> = sqlx::query_scalar(AssertSqlSafe(metadata))
                .fetch_optional(&mut *transaction)
                .await
                .map_err(|_| BootstrapError::IncompatibleNamespace)?;
            let history_query = format!(
                "SELECT version, success, checksum FROM {}.\"_codex_pg_migrations\" ORDER BY version",
                namespace.quoted_schema()
            );
            let history = sqlx::query(AssertSqlSafe(history_query))
                .fetch_all(&mut *transaction)
                .await
                .map_err(|_| BootstrapError::IncompatibleNamespace)?;
            if !format.is_some_and(|format| {
                crate::bootstrap::history_matches(&history, &migrations, format)
            }) {
                return Err(BootstrapError::IncompatibleNamespace);
            }
        } else {
            let occupied: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_class WHERE relnamespace = (SELECT oid FROM pg_namespace WHERE nspname = $1)) OR EXISTS (SELECT 1 FROM pg_proc WHERE pronamespace = (SELECT oid FROM pg_namespace WHERE nspname = $1)) OR EXISTS (SELECT 1 FROM pg_type WHERE typnamespace = (SELECT oid FROM pg_namespace WHERE nspname = $1))",
            )
            .bind(&namespace.schema)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|error| classify(&error))?;
            if occupied {
                return Err(BootstrapError::IncompatibleNamespace);
            }
        }

        let qualified_schema = namespace.quoted_schema();
        let history = format!("{qualified_schema}.\"_codex_pg_migrations\"");
        let migrator = Migrator {
            migrations: Cow::Owned(migrations),
            table_name: Cow::Owned(history.clone()),
            locking: false,
            ignore_missing: false,
            ..Migrator::DEFAULT
        };
        migrator
            .run_direct(/*target*/ None, &mut *transaction, /*skip*/ false)
            .await
            .map_err(|_| BootstrapError::Migration)?;
        sqlx::query(AssertSqlSafe(format!(
            "REVOKE ALL ON {qualified_schema}.\"thread_spawn_edges\" FROM {}",
            namespace.quoted_runtime()
        )))
        .execute(&mut *transaction)
        .await
        .map_err(|error| classify(&error))?;
        sqlx::query(AssertSqlSafe(format!(
            "GRANT SELECT, INSERT, UPDATE, DELETE ON {qualified_schema}.\"thread_spawn_edges\" TO {}",
            namespace.quoted_runtime()
        )))
        .execute(&mut *transaction)
        .await
        .map_err(|error| classify(&error))?;
        sqlx::query(AssertSqlSafe(format!(
            "REVOKE ALL ON {qualified_schema}.\"external_agent_config_imports\" FROM {}",
            namespace.quoted_runtime()
        )))
        .execute(&mut *transaction)
        .await
        .map_err(|error| classify(&error))?;
        sqlx::query(AssertSqlSafe(format!(
            "GRANT SELECT, INSERT, UPDATE ON {qualified_schema}.\"external_agent_config_imports\" TO {}",
            namespace.quoted_runtime()
        )))
        .execute(&mut *transaction)
        .await
        .map_err(|error| classify(&error))?;
        sqlx::query(AssertSqlSafe(format!(
            "REVOKE ALL ON {qualified_schema}.\"threads\" FROM {}",
            namespace.quoted_runtime()
        )))
        .execute(&mut *transaction)
        .await
        .map_err(|error| classify(&error))?;
        sqlx::query(AssertSqlSafe(format!(
            "GRANT SELECT, INSERT, UPDATE, DELETE ON {qualified_schema}.\"threads\" TO {}",
            namespace.quoted_runtime()
        )))
        .execute(&mut *transaction)
        .await
        .map_err(|error| classify(&error))?;
        sqlx::query(AssertSqlSafe(format!(
            "REVOKE ALL ON {qualified_schema}.\"codex_schema_meta\" FROM {}",
            namespace.quoted_runtime()
        )))
        .execute(&mut *transaction)
        .await
        .map_err(|error| classify(&error))?;
        sqlx::query(AssertSqlSafe(format!(
            "GRANT SELECT ON {qualified_schema}.\"codex_schema_meta\" TO {}",
            namespace.quoted_runtime()
        )))
        .execute(&mut *transaction)
        .await
        .map_err(|error| classify(&error))?;
        sqlx::query(AssertSqlSafe(format!(
            "REVOKE ALL ON {history} FROM {}",
            namespace.quoted_runtime()
        )))
        .execute(&mut *transaction)
        .await
        .map_err(|error| classify(&error))?;
        transaction.commit().await.map_err(|error| classify(&error))?;
        Ok(())
    })
    .await
    .map_err(|_| BootstrapError::Timeout)?
}
