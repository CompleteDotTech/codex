//! Transactional metadata bootstrap for the preprovisioned `codex_storage` schema.

use crate::PoolError;
use crate::PostgresPool;
use sqlx::Acquire;
use sqlx::migrate::MigrateError;
use sqlx::migrate::Migrator;
use std::borrow::Cow;
use std::fmt;
use std::time::Duration;
use tokio::time::timeout;

const BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(30);
const LOCK_CLASS: i32 = 0x4344_5850; // CDXP; transaction-scoped, independent of SQLx's session lock.
const LOCK_RESOURCE: i32 = 1; // Fixed codex_storage metadata namespace.
const MIGRATIONS_TABLE: &str = "codex_storage._codex_pg_migrations";
static BASE_MIGRATOR: Migrator = sqlx_macros::migrate!("./migrations");

/// A redacted bootstrap outcome; SQLx diagnostics may contain server details.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootstrapError {
    Timeout,
    Connection(PoolError),
    Privilege,
    IncompatibleNamespace,
    Unavailable,
    Migration,
}

impl fmt::Display for BootstrapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PostgreSQL bootstrap error: {self:?}")
    }
}

impl std::error::Error for BootstrapError {}

fn classify_sqlx(error: &sqlx::Error) -> BootstrapError {
    match error {
        sqlx::Error::Database(error) if error.code().as_deref() == Some("42501") => {
            BootstrapError::Privilege
        }
        sqlx::Error::Database(error)
            if error.code().is_some_and(|code| {
                code.starts_with("08") || matches!(code.as_ref(), "57P01" | "57P02" | "57P03")
            }) =>
        {
            BootstrapError::Unavailable
        }
        sqlx::Error::Io(_)
        | sqlx::Error::Tls(_)
        | sqlx::Error::Protocol(_)
        | sqlx::Error::PoolTimedOut
        | sqlx::Error::PoolClosed
        | sqlx::Error::WorkerCrashed
        | sqlx::Error::BeginFailed => BootstrapError::Unavailable,
        _ => BootstrapError::Migration,
    }
}

fn classify_migration(error: MigrateError) -> BootstrapError {
    match error {
        MigrateError::Execute(error) | MigrateError::ExecuteMigration(error, _) => {
            classify_sqlx(&error)
        }
        _ => BootstrapError::Migration,
    }
}

fn classify_namespace_validation(error: sqlx::Error) -> BootstrapError {
    match &error {
        sqlx::Error::Database(error)
            if matches!(
                error.code().as_deref(),
                Some("42P01" | "42703" | "42804" | "42809")
            ) =>
        {
            BootstrapError::IncompatibleNamespace
        }
        sqlx::Error::Database(error) if error.code().as_deref() == Some("42501") => {
            BootstrapError::Privilege
        }
        _ => classify_sqlx(&error),
    }
}

/// Bootstrap only the preprovisioned schema, using a separate migrator login.
/// The caller must supply `codex_migrator`; this never selects an active backend.
pub async fn bootstrap_codex_storage(pool: &PostgresPool) -> Result<(), BootstrapError> {
    timeout(BOOTSTRAP_TIMEOUT, async {
        let mut connection = pool.acquire().await.map_err(BootstrapError::Connection)?;
        let mut transaction = connection.begin().await.map_err(|error| classify_sqlx(&error))?;
        sqlx::query("SET LOCAL ROLE codex_owner")
            .execute(&mut *transaction)
            .await
            .map_err(|error| classify_sqlx(&error))?;
        sqlx::query("SELECT pg_advisory_xact_lock($1, $2)")
            .bind(LOCK_CLASS)
            .bind(LOCK_RESOURCE)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|error| classify_sqlx(&error))?;

        let owner: Option<String> = sqlx::query_scalar(
            "SELECT pg_get_userbyid(nspowner) FROM pg_namespace WHERE nspname = 'codex_storage'",
        )
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| classify_sqlx(&error))?;
        if owner.as_deref() != Some("codex_owner") {
            return Err(BootstrapError::IncompatibleNamespace);
        }

        let unexpected_objects: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_class WHERE relnamespace = 'codex_storage'::regnamespace AND relname NOT IN ('_codex_pg_migrations', '_codex_pg_migrations_pkey', 'codex_schema_meta', 'codex_schema_meta_pkey')) OR EXISTS (SELECT 1 FROM pg_proc WHERE pronamespace = 'codex_storage'::regnamespace) OR EXISTS (SELECT 1 FROM pg_type WHERE typnamespace = 'codex_storage'::regnamespace AND typtype <> 'b' AND typrelid = 0)",
        )
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| classify_sqlx(&error))?;
        if unexpected_objects {
            return Err(BootstrapError::IncompatibleNamespace);
        }

        let history_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_class WHERE relnamespace = 'codex_storage'::regnamespace AND relname = '_codex_pg_migrations' AND relkind = 'r')",
        )
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| classify_sqlx(&error))?;
        if history_exists {
            let format: Option<i32> = sqlx::query_scalar(
                "SELECT format_version FROM codex_storage.codex_schema_meta WHERE singleton = TRUE",
            )
            .fetch_optional(&mut *transaction)
            .await
            .map_err(classify_namespace_validation)?;
            if format != Some(1) {
                return Err(BootstrapError::IncompatibleNamespace);
            }
        } else {
            let occupied: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_class WHERE relnamespace = 'codex_storage'::regnamespace) OR EXISTS (SELECT 1 FROM pg_proc WHERE pronamespace = 'codex_storage'::regnamespace) OR EXISTS (SELECT 1 FROM pg_type WHERE typnamespace = 'codex_storage'::regnamespace)",
            )
            .fetch_one(&mut *transaction)
            .await
            .map_err(|error| classify_sqlx(&error))?;
            if occupied {
                return Err(BootstrapError::IncompatibleNamespace);
            }
        }

        let migrator = Migrator {
            migrations: Cow::Borrowed(BASE_MIGRATOR.migrations.as_ref()),
            table_name: Cow::Borrowed(MIGRATIONS_TABLE),
            locking: false,
            ignore_missing: false,
            ..Migrator::DEFAULT
        };
        migrator
            .run_direct(/*target*/ None, &mut *transaction, /*skip*/ false)
            .await
            .map_err(classify_migration)?;
        // The fixture's default grants are broad; metadata and history must be immutable to runtime.
        sqlx::query("REVOKE ALL ON codex_storage.codex_schema_meta FROM codex_runtime")
            .execute(&mut *transaction)
            .await
            .map_err(|error| classify_sqlx(&error))?;
        sqlx::query("GRANT SELECT ON codex_storage.codex_schema_meta TO codex_runtime")
            .execute(&mut *transaction)
            .await
            .map_err(|error| classify_sqlx(&error))?;
        sqlx::query("REVOKE ALL ON codex_storage._codex_pg_migrations FROM codex_runtime, codex_backup")
            .execute(&mut *transaction)
            .await
            .map_err(|error| classify_sqlx(&error))?;
        sqlx::query("GRANT SELECT ON codex_storage._codex_pg_migrations TO codex_backup")
            .execute(&mut *transaction)
            .await
            .map_err(|error| classify_sqlx(&error))?;
        transaction.commit().await.map_err(|error| classify_sqlx(&error))?;
        Ok(())
    })
    .await
    .map_err(|_| BootstrapError::Timeout)?
}

#[cfg(test)]
#[path = "bootstrap_tests.rs"]
mod tests;
