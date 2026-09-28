//! Read-only compatibility preflight for a preprovisioned named namespace.

use crate::ClientCapabilities;
use crate::CompatibilityError;
use crate::CompatibilityResult;
use crate::NamedNamespace;
use crate::PostgresPool;
use crate::RequiredAccess;
use crate::bootstrap::BOOTSTRAP_TIMEOUT;
use crate::bootstrap::LOCK_CLASS;
use crate::bootstrap::LOCK_RESOURCE;
use crate::bootstrap::history_matches;
use crate::named_bootstrap::namespaced_migrations;
use sqlx::Acquire;
use sqlx::AssertSqlSafe;
use sqlx::Row;
use tokio::time::timeout;

fn classify(error: &sqlx::Error) -> CompatibilityError {
    match error {
        sqlx::Error::Database(error) if error.code().as_deref() == Some("42501") => {
            CompatibilityError::Privilege
        }
        _ => CompatibilityError::Unavailable,
    }
}

/// Check named metadata/history using the matching migrator login. A passing
/// result does not qualify the native backend or permit its activation.
pub async fn check_named_namespace_compatibility(
    migrator: &PostgresPool,
    namespace: &NamedNamespace,
    capabilities: ClientCapabilities,
    access: RequiredAccess,
) -> Result<CompatibilityResult, CompatibilityError> {
    if capabilities.min_schema_format <= 0
        || capabilities.max_schema_format < capabilities.min_schema_format
        || capabilities.reader_version <= 0
        || capabilities.writer_version <= 0
    {
        return Err(CompatibilityError::InvalidCapabilities);
    }
    let migrations =
        namespaced_migrations(namespace).map_err(|_| CompatibilityError::IncompatibleHistory)?;
    timeout(BOOTSTRAP_TIMEOUT, async {
        let mut connection = migrator
            .acquire()
            .await
            .map_err(CompatibilityError::Connection)?;
        let mut transaction = connection.begin().await.map_err(|error| classify(&error))?;
        sqlx::query("SET TRANSACTION READ ONLY")
            .execute(&mut *transaction)
            .await
            .map_err(|error| classify(&error))?;
        let login: String = sqlx::query_scalar("SELECT session_user")
            .fetch_one(&mut *transaction)
            .await
            .map_err(|error| classify(&error))?;
        if login != namespace.migrator {
            return Err(CompatibilityError::Privilege);
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
            return Err(CompatibilityError::IncompatibleNamespace);
        }
        let unexpected_objects: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_class WHERE relnamespace = (SELECT oid FROM pg_namespace WHERE nspname = $1) AND relname NOT IN ('_codex_pg_migrations', '_codex_pg_migrations_pkey', 'codex_schema_meta', 'codex_schema_meta_pkey', 'thread_spawn_edges', 'thread_spawn_edges_pkey', 'idx_thread_spawn_edges_parent_status', 'external_agent_config_imports', 'external_agent_config_imports_pkey', 'idx_external_agent_config_imports_history')) OR EXISTS (SELECT 1 FROM pg_proc WHERE pronamespace = (SELECT oid FROM pg_namespace WHERE nspname = $1)) OR EXISTS (SELECT 1 FROM pg_type WHERE typnamespace = (SELECT oid FROM pg_namespace WHERE nspname = $1) AND typtype <> 'b' AND typrelid = 0)",
        )
        .bind(&namespace.schema)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| classify(&error))?;
        if unexpected_objects {
            return Err(CompatibilityError::IncompatibleNamespace);
        }
        let qualified_schema = namespace.quoted_schema();
        let metadata = format!(
            "SELECT format_version, min_reader_version, min_writer_version FROM {qualified_schema}.\"codex_schema_meta\" WHERE singleton = TRUE"
        );
        let metadata = sqlx::query(AssertSqlSafe(metadata))
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|_| CompatibilityError::MissingMetadata)?
            .ok_or(CompatibilityError::MissingMetadata)?;
        let schema_format: i32 = metadata.get("format_version");
        let min_reader: i32 = metadata.get("min_reader_version");
        let min_writer: i32 = metadata.get("min_writer_version");
        let history = format!(
            "SELECT version, success, checksum FROM {qualified_schema}.\"_codex_pg_migrations\" ORDER BY version"
        );
        let history = sqlx::query(AssertSqlSafe(history))
            .fetch_all(&mut *transaction)
            .await
            .map_err(|_| CompatibilityError::IncompatibleHistory)?;
        if history
            .iter()
            .any(|row| row.try_get::<bool, _>("success").ok() == Some(false))
        {
            return Err(CompatibilityError::DirtyMigration);
        }
        if !matches!(schema_format, 1..=3) {
            return Err(CompatibilityError::UnsupportedSchema);
        }
        if !history_matches(&history, &migrations, schema_format) {
            return Err(CompatibilityError::IncompatibleHistory);
        }
        if schema_format < capabilities.min_schema_format
            || schema_format > capabilities.max_schema_format
        {
            return Err(CompatibilityError::UnsupportedSchema);
        }
        if capabilities.reader_version < min_reader {
            return Err(CompatibilityError::ReaderTooOld);
        }
        if access == RequiredAccess::ReadWrite && capabilities.writer_version < min_writer {
            return Err(CompatibilityError::WriterTooOld);
        }
        transaction.rollback().await.map_err(|error| classify(&error))?;
        Ok(CompatibilityResult {
            schema_format,
            activation_permitted: false,
        })
    })
    .await
    .map_err(|_| CompatibilityError::Timeout)?
}
