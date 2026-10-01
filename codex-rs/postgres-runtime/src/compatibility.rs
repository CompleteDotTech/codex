//! Read-only compatibility preflight for the fixed PostgreSQL metadata schema.

use crate::PoolError;
use crate::PostgresPool;
use crate::bootstrap::BASE_MIGRATOR;
use crate::bootstrap::BOOTSTRAP_TIMEOUT;
use crate::bootstrap::BootstrapError;
use crate::bootstrap::LOCK_CLASS;
use crate::bootstrap::LOCK_RESOURCE;
use crate::bootstrap::KNOWN_RELATIONS;
use crate::bootstrap::namespace_has_unexpected_objects;
use crate::bootstrap::require_safe_protected_privileges;
use sqlx::Acquire;
use sqlx::Postgres;
use sqlx::Row;
use sqlx::Transaction;
use std::fmt;
use tokio::time::Instant;
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
        sqlx::Error::Database(error)
            if matches!(error.code().as_deref(), Some("55P03" | "25P04"))
                || error.code().as_deref() == Some("57014")
                    && error.message().contains("statement timeout") =>
        {
            CompatibilityError::Timeout
        }
        _ => CompatibilityError::Unavailable,
    }
}

fn classify_schema(error: &sqlx::Error, incompatible: CompatibilityError) -> CompatibilityError {
    match error {
        sqlx::Error::Database(error)
            if matches!(
                error.code().as_deref(),
                Some("42P01" | "42703" | "42804" | "42809")
            ) =>
        {
            incompatible
        }
        _ => classify(error),
    }
}

fn classify_bootstrap(error: BootstrapError) -> CompatibilityError {
    match error {
        BootstrapError::Privilege => CompatibilityError::Privilege,
        BootstrapError::IncompatibleNamespace => CompatibilityError::IncompatibleNamespace,
        BootstrapError::Timeout => CompatibilityError::Timeout,
        BootstrapError::Connection(error) => CompatibilityError::Connection(error),
        BootstrapError::Unavailable => CompatibilityError::Unavailable,
        BootstrapError::Migration => CompatibilityError::Unavailable,
    }
}

async fn validate_relation(
    transaction: &mut Transaction<'_, Postgres>,
    relation: &str,
    columns: &[(&str, &str)],
    incompatible: CompatibilityError,
) -> Result<(), CompatibilityError> {
    let names: Vec<_> = columns.iter().map(|(name, _)| *name).collect();
    let types: Vec<_> = columns.iter().map(|(_, kind)| *kind).collect();
    let valid: Option<bool> = sqlx::query_scalar(
        "SELECT c.relkind = 'r' AND c.relpersistence = 'p' AND NOT c.relrowsecurity
           AND NOT c.relispartition AND NOT EXISTS (
             SELECT 1 FROM pg_inherits WHERE inhparent = c.oid
           )
           AND c.relowner = 'codex_owner'::regrole AND NOT EXISTS (
             SELECT 1 FROM (
               SELECT * FROM pg_attribute WHERE attrelid = c.oid AND attnum > 0 AND NOT attisdropped
             ) a FULL JOIN unnest($2::text[], $3::text[]) expected(name, type_name)
               ON a.attname = expected.name
             WHERE a.attname IS NULL OR expected.name IS NULL
                OR a.atttypid <> expected.type_name::regtype OR NOT a.attnotnull
                OR a.attgenerated <> '' OR a.attidentity <> '' OR a.attinhcount <> 0
           ) AND EXISTS (
             SELECT 1 FROM pg_index i JOIN pg_attribute a
               ON a.attrelid = c.oid AND a.attnum = i.indkey[0]
             WHERE i.indrelid = c.oid AND i.indisprimary AND i.indisvalid
               AND i.indnatts = 1 AND a.attname = ($2::text[])[1]
           ) FROM pg_class c WHERE c.oid = to_regclass($1)",
    )
    .bind(relation)
    .bind(names)
    .bind(types)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| classify_schema(&error, incompatible))?;
    if valid != Some(true) {
        return Err(incompatible);
    }
    Ok(())
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
    let deadline = Instant::now() + BOOTSTRAP_TIMEOUT;
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
        // Bound the whole PostgreSQL 17 transaction within the outer deadline.
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .saturating_sub(10000);
        if remaining == 0 {
            return Err(CompatibilityError::Timeout);
        }
        sqlx::query("SELECT set_config('transaction_timeout', $1, true)")
            .bind(format!("{remaining}ms"))
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

        let unexpected_objects = namespace_has_unexpected_objects(
            &mut transaction,
            KNOWN_RELATIONS,
        ).await.map_err(classify_bootstrap)?;
        if unexpected_objects {
            return Err(CompatibilityError::IncompatibleNamespace);
        }

        validate_relation(&mut transaction, "codex_storage.codex_schema_meta", &[
            ("singleton", "bool"), ("format_version", "int4"),
            ("min_reader_version", "int4"), ("min_writer_version", "int4"),
        ], CompatibilityError::MissingMetadata).await?;
        validate_relation(&mut transaction, "codex_storage._codex_pg_migrations", &[
            ("version", "int8"), ("description", "text"), ("installed_on", "timestamptz"),
            ("success", "bool"), ("checksum", "bytea"), ("execution_time", "int8"),
        ], CompatibilityError::IncompatibleHistory).await?;
        let valid_checks: bool = sqlx::query_scalar(
            "WITH expected(name, definition) AS (VALUES
                ('codex_schema_meta_singleton_check', 'CHECK (singleton)'),
                ('codex_schema_meta_format_version_check', 'CHECK ((format_version > 0))'),
                ('codex_schema_meta_min_reader_version_check', 'CHECK ((min_reader_version > 0))'),
                ('codex_schema_meta_min_writer_version_check', 'CHECK ((min_writer_version > 0))'))
             SELECT (SELECT count(*) FROM pg_constraint WHERE conrelid = 'codex_storage.codex_schema_meta'::regclass) = 5
                AND (SELECT pg_get_expr(d.adbin, d.adrelid) FROM pg_attrdef d JOIN pg_attribute a
                     ON a.attrelid = d.adrelid AND a.attnum = d.adnum WHERE d.adrelid = 'codex_storage.codex_schema_meta'::regclass AND a.attname = 'singleton') IS NOT DISTINCT FROM 'true'
                AND NOT EXISTS (SELECT 1 FROM expected WHERE NOT EXISTS (
                    SELECT 1 FROM pg_constraint c WHERE c.conrelid = 'codex_storage.codex_schema_meta'::regclass
                      AND c.conname = expected.name AND pg_get_constraintdef(c.oid) = expected.definition))",
        )
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| classify_schema(&error, CompatibilityError::MissingMetadata))?;
        if !valid_checks {
            return Err(CompatibilityError::MissingMetadata);
        }
        let valid_history_constraints: bool = sqlx::query_scalar(
            "SELECT count(*) = 1 AND COALESCE(bool_and(contype = 'p'
                AND conname = '_codex_pg_migrations_pkey'
                AND conindid = 'codex_storage._codex_pg_migrations_pkey'::regclass), FALSE)
             FROM pg_constraint WHERE conrelid = 'codex_storage._codex_pg_migrations'::regclass",
        )
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| classify_schema(&error, CompatibilityError::IncompatibleHistory))?;
        if !valid_history_constraints {
            return Err(CompatibilityError::IncompatibleHistory);
        }
        require_safe_protected_privileges(&mut transaction)
            .await
            .map_err(classify_bootstrap)?;

        let metadata = sqlx::query(
            "SELECT singleton, format_version, min_reader_version, min_writer_version FROM ONLY codex_storage.codex_schema_meta LIMIT 2",
        )
        .fetch_all(&mut *transaction)
        .await
        .map_err(|error| classify_schema(&error, CompatibilityError::MissingMetadata))?;
        let [metadata] = metadata.as_slice() else {
            return Err(CompatibilityError::MissingMetadata);
        };
        if !metadata.try_get::<bool, _>("singleton").map_err(|_| CompatibilityError::MissingMetadata)? {
            return Err(CompatibilityError::MissingMetadata);
        }
        let schema_format: i32 = metadata.try_get("format_version").map_err(|_| CompatibilityError::MissingMetadata)?;
        let min_reader: i32 = metadata.try_get("min_reader_version").map_err(|_| CompatibilityError::MissingMetadata)?;
        let min_writer: i32 = metadata.try_get("min_writer_version").map_err(|_| CompatibilityError::MissingMetadata)?;
        if min_reader <= 0 || min_writer <= 0 {
            return Err(CompatibilityError::MissingMetadata);
        }

        let versions: Vec<_> = BASE_MIGRATOR.migrations.iter().map(|migration| migration.version).collect();
        let checksums: Vec<_> = BASE_MIGRATOR.migrations.iter().map(|migration| migration.checksum.as_ref()).collect();
        // Compare checksums on the server: even an oversized bytea never becomes
        // a client allocation. One extra row is sufficient to reject extra history.
        let history = sqlx::query(
            "SELECT history.version, history.success, COALESCE(history.checksum = expected.checksum, FALSE) AS checksum_matches
             FROM ONLY codex_storage._codex_pg_migrations history
             LEFT JOIN unnest($1::bigint[], $2::bytea[]) expected(version, checksum) ON history.version = expected.version
             ORDER BY history.version LIMIT $3",
        )
        .bind(versions)
        .bind(checksums)
        .bind(BASE_MIGRATOR.migrations.len() as i64 + 1)
        .fetch_all(&mut *transaction)
        .await
        .map_err(|error| classify_schema(&error, CompatibilityError::IncompatibleHistory))?;
        for row in &history {
            if !row.try_get::<bool, _>("success").map_err(|_| CompatibilityError::IncompatibleHistory)? {
                return Err(CompatibilityError::DirtyMigration);
            }
        }
        // The recorded format equals the number of applied migrations.
        match usize::try_from(schema_format) {
            Ok(applied) if (1..=BASE_MIGRATOR.migrations.len()).contains(&applied) => {
                if history.len() != applied {
                    return Err(CompatibilityError::IncompatibleHistory);
                }
            }
            _ => return Err(CompatibilityError::UnsupportedSchema),
        }
        for (row, migration) in history.iter().zip(BASE_MIGRATOR.migrations.iter()) {
            let version: i64 = row.try_get("version").map_err(|_| CompatibilityError::IncompatibleHistory)?;
            let checksum_matches: bool = row.try_get("checksum_matches").map_err(|_| CompatibilityError::IncompatibleHistory)?;
            if version != migration.version || !checksum_matches {
                return Err(CompatibilityError::IncompatibleHistory);
            }
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
