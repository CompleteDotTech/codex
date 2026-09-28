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

fn namespaced_migration(namespace: &NamedNamespace) -> Result<Migration, BootstrapError> {
    let [base] = BASE_MIGRATOR.migrations.as_ref() else {
        return Err(BootstrapError::Migration);
    };
    let source = base.sql.as_str();
    // Version 1 has exactly two schema-qualified identifiers: CREATE and INSERT.
    // A changed SQL layout needs a new review before identifier substitution.
    if base.version != 1
        || base.no_tx
        || !source.starts_with("CREATE TABLE codex_storage.codex_schema_meta (")
        || !source.contains("\nINSERT INTO codex_storage.codex_schema_meta\n")
        || source.matches("codex_storage.").count() != 2
    {
        return Err(BootstrapError::Migration);
    }
    let qualified_prefix = format!("{}.", namespace.quoted_schema());
    let sql = source.replace("codex_storage.", &qualified_prefix);
    Ok(Migration::new(
        base.version,
        base.description.clone(),
        base.migration_type,
        AssertSqlSafe(sql).into_sql_str(),
        base.no_tx,
    ))
}

/// Bootstrap a distinct preprovisioned schema using its matching migrator login.
/// This only creates metadata/history; it never selects or activates a backend.
pub async fn bootstrap_named_namespace(
    pool: &PostgresPool,
    namespace: &NamedNamespace,
) -> Result<(), BootstrapError> {
    let migration = namespaced_migration(namespace)?;
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
            "SELECT EXISTS (SELECT 1 FROM pg_class WHERE relnamespace = (SELECT oid FROM pg_namespace WHERE nspname = $1) AND relname NOT IN ('_codex_pg_migrations', '_codex_pg_migrations_pkey', 'codex_schema_meta', 'codex_schema_meta_pkey')) OR EXISTS (SELECT 1 FROM pg_proc WHERE pronamespace = (SELECT oid FROM pg_namespace WHERE nspname = $1)) OR EXISTS (SELECT 1 FROM pg_type WHERE typnamespace = (SELECT oid FROM pg_namespace WHERE nspname = $1) AND typtype <> 'b' AND typrelid = 0)",
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
            if format != Some(1) {
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
            migrations: Cow::Owned(vec![migration]),
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
