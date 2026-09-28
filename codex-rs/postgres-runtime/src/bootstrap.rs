//! Transactional metadata bootstrap for the preprovisioned `codex_storage` schema.

use crate::PoolError;
use crate::PostgresPool;
use sqlx::Acquire;
use sqlx::PgConnection;
use sqlx::migrate::Migrator;
use std::borrow::Cow;
use std::fmt;
use std::time::Duration;
use tokio::time::timeout;

const BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(30);
// The schema-wide lock must match scripts/postgres/container/restore-guard.sql.
const LOCK_CLASS: i32 = 1_414_676_819;
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
        _ => BootstrapError::Migration,
    }
}

async fn namespace_has_unexpected_objects(
    connection: &mut PgConnection,
    permitted_relations: &[&str],
) -> Result<bool, BootstrapError> {
    sqlx::query_scalar(
        "WITH expected_relations AS (
            SELECT oid FROM pg_class
            WHERE relnamespace = 'codex_storage'::regnamespace AND relname = ANY($1)
        ), expected_types AS (
            SELECT oid FROM pg_type WHERE typrelid IN (SELECT oid FROM expected_relations)
            UNION SELECT typarray FROM pg_type WHERE typrelid IN (SELECT oid FROM expected_relations)
        ), namespace_objects AS (
            SELECT 'pg_class'::regclass AS classid, oid AS objid FROM pg_class
            WHERE relnamespace = 'codex_storage'::regnamespace
            UNION SELECT 'pg_type'::regclass, oid FROM pg_type
            WHERE typnamespace = 'codex_storage'::regnamespace
            UNION SELECT classid, objid FROM pg_depend
            WHERE refclassid = 'pg_namespace'::regclass
              AND refobjid = 'codex_storage'::regnamespace
              AND NOT (classid = 'pg_default_acl'::regclass AND objid IN (
                  SELECT oid FROM pg_default_acl
                  WHERE defaclrole = 'codex_owner'::regrole AND defaclobjtype IN ('r', 'S')
              ))
        ) SELECT EXISTS (
            SELECT 1 FROM namespace_objects
            WHERE NOT (classid = 'pg_class'::regclass AND objid IN (SELECT oid FROM expected_relations))
              AND NOT (classid = 'pg_type'::regclass AND objid IN (SELECT oid FROM expected_types))
        )",
    )
    .bind(permitted_relations)
    .fetch_one(connection)
    .await
    .map_err(|error| classify_sqlx(&error))
}

async fn require_safe_runtime_privileges(
    connection: &mut PgConnection,
) -> Result<(), BootstrapError> {
    // Check effective access, including column ACLs, PUBLIC, inherited roles,
    // and roles the runtime could assume explicitly.
    let unsafe_runtime_privileges: bool = sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1 FROM pg_roles
            WHERE (pg_has_role('codex_runtime', oid, 'USAGE') OR pg_has_role('codex_runtime', oid, 'SET'))
              AND (
                has_schema_privilege(oid, 'codex_storage', 'CREATE')
                OR has_table_privilege(oid, 'codex_storage.codex_schema_meta', 'INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')
                OR has_any_column_privilege(oid, 'codex_storage.codex_schema_meta', 'INSERT,UPDATE,REFERENCES')
                OR has_table_privilege(oid, 'codex_storage._codex_pg_migrations', 'SELECT,INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')
                OR has_any_column_privilege(oid, 'codex_storage._codex_pg_migrations', 'SELECT,INSERT,UPDATE,REFERENCES')
              )
        )",
    )
    .fetch_one(connection)
    .await
    .map_err(|error| classify_sqlx(&error))?;
    if unsafe_runtime_privileges {
        return Err(BootstrapError::Privilege);
    }
    Ok(())
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

        // pg_depend covers namespace-scoped objects across catalogs, including
        // operators, collations, and text search objects. The fixture's known
        // default ACLs are policy; effective table privileges are checked below.
        let unexpected_objects = namespace_has_unexpected_objects(
            &mut transaction,
            &["_codex_pg_migrations", "_codex_pg_migrations_pkey", "codex_schema_meta", "codex_schema_meta_pkey"],
        ).await?;
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
            .map_err(|_| BootstrapError::IncompatibleNamespace)?;
            if format != Some(1) {
                return Err(BootstrapError::IncompatibleNamespace);
            }
            // Refuse privilege drift on an existing namespace before any ACL
            // changes, so bootstrap cannot silently repair caller-owned policy.
            require_safe_runtime_privileges(&mut transaction).await?;
        } else {
            let occupied = namespace_has_unexpected_objects(&mut transaction, &[]).await?;
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
            .map_err(|_| BootstrapError::Migration)?;
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
        require_safe_runtime_privileges(&mut transaction).await?;
        transaction.commit().await.map_err(|error| classify_sqlx(&error))?;
        Ok(())
    })
    .await
    .map_err(|_| BootstrapError::Timeout)?
}
