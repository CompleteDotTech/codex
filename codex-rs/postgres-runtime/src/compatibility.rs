//! Read-only compatibility preflight for the fixed PostgreSQL metadata schema.

use crate::PoolError;
use crate::PostgresPool;
use crate::bootstrap::BASE_MIGRATOR;
use crate::bootstrap::BOOTSTRAP_TIMEOUT;
use crate::bootstrap::LOCK_CLASS;
use crate::bootstrap::LOCK_RESOURCE;
use sqlx::Acquire;
use sqlx::Row;
use std::fmt;
use tokio::time::timeout;

/// Host-verified package/build capabilities, never target-supplied.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClientCapabilities {
    pub min_schema_format: i32,
    pub max_schema_format: i32,
    pub reader_version: i32,
    pub writer_version: i32,
}

/// The storage operation the caller intends to perform after preflight.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequiredAccess {
    ReadOnly,
    ReadWrite,
}

/// Preflight only; this cannot qualify a native runtime or authorize activation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompatibilityResult {
    pub schema_format: i32,
    pub activation_permitted: bool,
}

/// Redacted compatibility failure; database details and credentials stay private.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompatibilityError {
    InvalidCapabilities,
    Timeout,
    Connection(PoolError),
    Privilege,
    IncompatibleNamespace,
    MissingMetadata,
    DirtyMigration,
    IncompatibleHistory,
    UnsupportedSchema,
    ReaderTooOld,
    WriterTooOld,
    Unavailable,
}

impl fmt::Display for CompatibilityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PostgreSQL compatibility error: {self:?}")
    }
}

impl std::error::Error for CompatibilityError {}

fn classify(error: &sqlx::Error) -> CompatibilityError {
    match error {
        sqlx::Error::Database(error) if error.code().as_deref() == Some("42501") => {
            CompatibilityError::Privilege
        }
        _ => CompatibilityError::Unavailable,
    }
}

/// Check the preprovisioned namespace through a separate `codex_migrator` pool.
/// The ordinary runtime role has no migration-history read permission. A passing
/// result is a prefilter only; independent native qualification still applies.
/// Dirty or inconsistent committed history is rejected without auto-repair.
pub async fn check_codex_storage_compatibility(
    migrator: &PostgresPool,
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
        sqlx::query("SET LOCAL ROLE codex_owner")
            .execute(&mut *transaction)
            .await
            .map_err(|error| classify(&error))?;
        sqlx::query("SELECT pg_advisory_xact_lock($1, $2)")
            .bind(LOCK_CLASS)
            .bind(LOCK_RESOURCE)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|error| classify(&error))?;

        let owner: Option<String> = sqlx::query_scalar(
            "SELECT pg_get_userbyid(nspowner) FROM pg_namespace WHERE nspname = 'codex_storage'",
        )
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| classify(&error))?;
        if owner.as_deref() != Some("codex_owner") {
            return Err(CompatibilityError::IncompatibleNamespace);
        }

        let unexpected_objects: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_class WHERE relnamespace = 'codex_storage'::regnamespace AND relname NOT IN ('_codex_pg_migrations', '_codex_pg_migrations_pkey', 'codex_schema_meta', 'codex_schema_meta_pkey')) OR EXISTS (SELECT 1 FROM pg_proc WHERE pronamespace = 'codex_storage'::regnamespace) OR EXISTS (SELECT 1 FROM pg_type WHERE typnamespace = 'codex_storage'::regnamespace AND typtype <> 'b' AND typrelid = 0)",
        )
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| classify(&error))?;
        if unexpected_objects {
            return Err(CompatibilityError::IncompatibleNamespace);
        }

        let metadata = sqlx::query(
            "SELECT format_version, min_reader_version, min_writer_version FROM codex_storage.codex_schema_meta WHERE singleton = TRUE",
        )
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| CompatibilityError::MissingMetadata)?
        .ok_or(CompatibilityError::MissingMetadata)?;
        let schema_format: i32 = metadata.get("format_version");
        let min_reader: i32 = metadata.get("min_reader_version");
        let min_writer: i32 = metadata.get("min_writer_version");

        let history = sqlx::query(
            "SELECT version, success, checksum FROM codex_storage._codex_pg_migrations ORDER BY version",
        )
        .fetch_all(&mut *transaction)
        .await
        .map_err(|_| CompatibilityError::IncompatibleHistory)?;
        if history.iter().any(|row| !row.get::<bool, _>("success")) {
            return Err(CompatibilityError::DirtyMigration);
        }
        if history.len() != BASE_MIGRATOR.migrations.len()
            || history.iter().zip(BASE_MIGRATOR.migrations.iter()).any(|(row, migration)| {
                row.get::<i64, _>("version") != migration.version
                    || row.get::<Vec<u8>, _>("checksum").as_slice() != migration.checksum.as_ref()
            })
        {
            return Err(CompatibilityError::IncompatibleHistory);
        }

        // This binary only embeds format 1, regardless of claimed host capability.
        if schema_format != 1
            || schema_format < capabilities.min_schema_format
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
