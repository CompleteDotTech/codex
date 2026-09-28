#![expect(
    clippy::expect_used,
    reason = "isolated PostgreSQL fixture failures should identify their source"
)]

use codex_postgres_runtime::BootstrapError;
use codex_postgres_runtime::ClientCapabilities;
use codex_postgres_runtime::CompatibilityError;
use codex_postgres_runtime::CompatibilityResult;
use codex_postgres_runtime::ConnectionSettings;
use codex_postgres_runtime::PoolLimits;
use codex_postgres_runtime::PostgresPool;
use codex_postgres_runtime::RequiredAccess;
use codex_postgres_runtime::bootstrap_codex_storage;
use codex_postgres_runtime::check_codex_storage_compatibility;
use pretty_assertions::assert_eq;
use serde_json::Value;
use sqlx::Acquire;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

fn settings(state: &Path, role: &str) -> ConnectionSettings {
    let receipt: Value = serde_json::from_slice(
        &std::fs::read(state.join("receipt.json")).expect("read isolated PostgreSQL receipt"),
    )
    .expect("parse PostgreSQL receipt");
    ConnectionSettings {
        host: "localhost".to_string(),
        port: receipt["port"].as_u64().expect("PostgreSQL port") as u16,
        database: "codex".to_string(),
        username: format!("codex_{role}"),
        password: std::fs::read_to_string(state.join(format!("secrets/{role}.password")))
            .expect("read private role credential")
            .trim()
            .to_string(),
        ca_certificate: state.join("secrets/ca.crt"),
        limits: PoolLimits {
            connect_timeout: Duration::from_secs(5),
            acquire_timeout: Duration::from_secs(5),
            max_connections: 2,
        },
    }
}

async fn owner_query(pool: &PostgresPool, sql: &'static str) {
    let mut connection = pool.acquire().await.expect("acquire migrator connection");
    let mut transaction = connection.begin().await.expect("begin owner transaction");
    sqlx::query("SET LOCAL ROLE codex_owner")
        .execute(&mut *transaction)
        .await
        .expect("assume schema owner role");
    sqlx::raw_sql(sql)
        .execute(&mut *transaction)
        .await
        .expect("execute owner fixture SQL");
    transaction
        .commit()
        .await
        .expect("commit owner fixture SQL");
}

async fn reject_namespace_objects(pool: &PostgresPool) {
    for (create, drop) in [
        (
            "CREATE COLLATION codex_storage.occupied_collation FROM pg_catalog.\"C\"",
            "DROP COLLATION IF EXISTS codex_storage.occupied_collation",
        ),
        (
            "CREATE OPERATOR codex_storage.=== (FUNCTION = pg_catalog.int4eq, LEFTARG = integer, RIGHTARG = integer)",
            "DROP OPERATOR IF EXISTS codex_storage.=== (integer, integer)",
        ),
        (
            "CREATE TEXT SEARCH CONFIGURATION codex_storage.occupied_search (COPY = pg_catalog.simple)",
            "DROP TEXT SEARCH CONFIGURATION IF EXISTS codex_storage.occupied_search",
        ),
    ] {
        owner_query(pool, drop).await;
        owner_query(pool, create).await;
        assert_eq!(
            bootstrap_codex_storage(pool).await,
            Err(BootstrapError::IncompatibleNamespace)
        );
        owner_query(pool, drop).await;
    }
}

#[tokio::test]
async fn real_postgres_bootstrap_is_atomic_role_scoped_and_idempotent() {
    let Ok(state) = std::env::var("CODEX_TEST_POSTGRES_STATE") else {
        return;
    };
    let state = Path::new(&state);
    let migrator_a = PostgresPool::connect(settings(state, "migrator"))
        .await
        .expect("first migrator pool");
    let migrator_b = Arc::new(
        PostgresPool::connect(settings(state, "migrator"))
            .await
            .expect("second migrator pool"),
    );
    let runtime = PostgresPool::connect(settings(state, "runtime"))
        .await
        .expect("runtime pool");
    let backup = PostgresPool::connect(settings(state, "backup"))
        .await
        .expect("backup pool");

    owner_query(
        &migrator_a,
        "DROP TABLE IF EXISTS codex_storage.occupied_probe",
    )
    .await;
    owner_query(
        &migrator_a,
        "DROP TYPE IF EXISTS codex_storage.occupied_type",
    )
    .await;
    reject_namespace_objects(&migrator_a).await;
    owner_query(
        &migrator_a,
        "CREATE TABLE codex_storage.occupied_probe(id BIGINT)",
    )
    .await;
    assert_eq!(
        bootstrap_codex_storage(&migrator_a).await,
        Err(BootstrapError::IncompatibleNamespace)
    );
    let mut owner = migrator_a.acquire().await.expect("inspect occupied schema");
    let mut owner_transaction = owner.begin().await.expect("begin owner inspection");
    sqlx::query("SET LOCAL ROLE codex_owner")
        .execute(&mut *owner_transaction)
        .await
        .expect("assume owner for inspection");
    let exists: bool =
        sqlx::query_scalar("SELECT to_regclass('codex_storage.occupied_probe') IS NOT NULL")
            .fetch_one(&mut *owner_transaction)
            .await
            .expect("occupied object was preserved");
    assert!(exists);
    owner_transaction
        .rollback()
        .await
        .expect("finish inspection");
    drop(owner);
    owner_query(&migrator_a, "DROP TABLE codex_storage.occupied_probe").await;

    let (first, second) = tokio::join!(
        bootstrap_codex_storage(&migrator_a),
        bootstrap_codex_storage(&migrator_b)
    );
    assert_eq!(first, Ok(()));
    assert_eq!(second, Ok(()));
    assert_eq!(bootstrap_codex_storage(&migrator_a).await, Ok(()));
    reject_namespace_objects(&migrator_a).await;

    // Match the actual restore transaction's lock, not the implementation's
    // constant, and observe bootstrap waiting before allowing it to continue.
    let mut restore_connection = migrator_a.acquire().await.expect("restore connection");
    let mut restore = restore_connection
        .begin()
        .await
        .expect("restore transaction");
    sqlx::query("SELECT pg_advisory_xact_lock(1414676819, 1)")
        .execute(&mut *restore)
        .await
        .expect("hold restore schema lock");
    let bootstrap = tokio::spawn({
        let pool = Arc::clone(&migrator_b);
        async move { bootstrap_codex_storage(&pool).await }
    });
    let mut observer = migrator_a.acquire().await.expect("lock observer");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_locks WHERE locktype = 'advisory' AND classid = 1414676819 AND objid = 1 AND objsubid = 2 AND NOT granted)")
                .fetch_one(&mut *observer).await.expect("observe bootstrap lock wait");
            if waiting { break; }
            assert!(!bootstrap.is_finished(), "bootstrap bypassed restore lock");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("bootstrap waits for restore");
    restore.rollback().await.expect("release restore lock");
    assert_eq!(bootstrap.await.expect("bootstrap task"), Ok(()));
    drop(observer);
    drop(restore_connection);

    for (grant, revoke, privilege_query) in [
        (
            "GRANT UPDATE (format_version) ON codex_storage.codex_schema_meta TO codex_runtime",
            "REVOKE UPDATE (format_version) ON codex_storage.codex_schema_meta FROM codex_runtime",
            "SELECT has_column_privilege('codex_runtime', 'codex_storage.codex_schema_meta', 'format_version', 'UPDATE')",
        ),
        (
            "GRANT UPDATE ON codex_storage.codex_schema_meta TO PUBLIC",
            "REVOKE UPDATE ON codex_storage.codex_schema_meta FROM PUBLIC",
            "SELECT has_table_privilege('codex_runtime', 'codex_storage.codex_schema_meta', 'UPDATE')",
        ),
        (
            "GRANT SELECT (version) ON codex_storage._codex_pg_migrations TO codex_runtime",
            "REVOKE SELECT (version) ON codex_storage._codex_pg_migrations FROM codex_runtime",
            "SELECT has_column_privilege('codex_runtime', 'codex_storage._codex_pg_migrations', 'version', 'SELECT')",
        ),
        (
            "GRANT SELECT ON codex_storage._codex_pg_migrations TO PUBLIC",
            "REVOKE SELECT ON codex_storage._codex_pg_migrations FROM PUBLIC",
            "SELECT has_table_privilege('codex_runtime', 'codex_storage._codex_pg_migrations', 'SELECT')",
        ),
        (
            "GRANT UPDATE (format_version) ON codex_storage.codex_schema_meta TO codex_backup",
            "REVOKE UPDATE (format_version) ON codex_storage.codex_schema_meta FROM codex_backup",
            "SELECT has_column_privilege('codex_backup', 'codex_storage.codex_schema_meta', 'format_version', 'UPDATE')",
        ),
        (
            "GRANT UPDATE (version) ON codex_storage._codex_pg_migrations TO codex_backup",
            "REVOKE UPDATE (version) ON codex_storage._codex_pg_migrations FROM codex_backup",
            "SELECT has_column_privilege('codex_backup', 'codex_storage._codex_pg_migrations', 'version', 'UPDATE')",
        ),
        (
            "GRANT CREATE ON SCHEMA codex_storage TO codex_backup",
            "REVOKE CREATE ON SCHEMA codex_storage FROM codex_backup",
            "SELECT has_schema_privilege('codex_backup', 'codex_storage', 'CREATE')",
        ),
    ] {
        owner_query(&migrator_a, grant).await;
        assert_eq!(
            bootstrap_codex_storage(&migrator_a).await,
            Err(BootstrapError::Privilege)
        );
        let mut connection = runtime.acquire().await.expect("privilege observer");
        let preserved: bool = sqlx::query_scalar(privilege_query)
            .fetch_one(&mut *connection)
            .await
            .expect("refusal retains original grants");
        assert!(preserved);
        drop(connection);
        owner_query(&migrator_a, revoke).await;
        assert_eq!(bootstrap_codex_storage(&migrator_a).await, Ok(()));
    }

    for (raise_minimum, reset_minimum) in [
        (
            "UPDATE codex_storage.codex_schema_meta SET min_reader_version = 2",
            "UPDATE codex_storage.codex_schema_meta SET min_reader_version = 1",
        ),
        (
            "UPDATE codex_storage.codex_schema_meta SET min_writer_version = 2",
            "UPDATE codex_storage.codex_schema_meta SET min_writer_version = 1",
        ),
    ] {
        owner_query(&migrator_a, raise_minimum).await;
        assert_eq!(
            bootstrap_codex_storage(&migrator_a).await,
            Err(BootstrapError::IncompatibleNamespace)
        );
        owner_query(&migrator_a, reset_minimum).await;
        assert_eq!(bootstrap_codex_storage(&migrator_a).await, Ok(()));
    }

    owner_query(
        &migrator_a,
        "REVOKE SELECT ON codex_storage.codex_schema_meta FROM codex_backup",
    )
    .await;
    assert_eq!(bootstrap_codex_storage(&migrator_a).await, Ok(()));
    let mut backup_reader = backup.acquire().await.expect("backup metadata reader");
    let backup_can_read: i64 =
        sqlx::query_scalar("SELECT count(*) FROM codex_storage.codex_schema_meta")
            .fetch_one(&mut *backup_reader)
            .await
            .expect("backup can read metadata");
    assert_eq!(backup_can_read, 1);
    drop(backup_reader);

    for (drop_key, restore_key) in [
        (
            "ALTER TABLE codex_storage.codex_schema_meta DROP CONSTRAINT codex_schema_meta_pkey",
            "ALTER TABLE codex_storage.codex_schema_meta ADD PRIMARY KEY (singleton)",
        ),
        (
            "ALTER TABLE codex_storage._codex_pg_migrations DROP CONSTRAINT _codex_pg_migrations_pkey",
            "ALTER TABLE codex_storage._codex_pg_migrations ADD PRIMARY KEY (version)",
        ),
    ] {
        owner_query(&migrator_a, drop_key).await;
        assert_eq!(
            bootstrap_codex_storage(&migrator_a).await,
            Err(BootstrapError::IncompatibleNamespace)
        );
        owner_query(&migrator_a, restore_key).await;
        assert_eq!(bootstrap_codex_storage(&migrator_a).await, Ok(()));
    }

    owner_query(
        &migrator_a,
        "ALTER TABLE codex_storage.codex_schema_meta DROP CONSTRAINT codex_schema_meta_singleton_check",
    )
    .await;
    owner_query(
        &migrator_a,
        "ALTER TABLE codex_storage.codex_schema_meta ADD CONSTRAINT codex_schema_meta_singleton_check CHECK (TRUE)",
    )
    .await;
    assert_eq!(
        bootstrap_codex_storage(&migrator_a).await,
        Err(BootstrapError::IncompatibleNamespace)
    );
    owner_query(
        &migrator_a,
        "ALTER TABLE codex_storage.codex_schema_meta DROP CONSTRAINT codex_schema_meta_singleton_check",
    )
    .await;
    owner_query(
        &migrator_a,
        "ALTER TABLE codex_storage.codex_schema_meta ADD CONSTRAINT codex_schema_meta_singleton_check CHECK (singleton)",
    )
    .await;
    assert_eq!(bootstrap_codex_storage(&migrator_a).await, Ok(()));

    owner_query(
        &migrator_a,
        "REVOKE USAGE ON SCHEMA codex_storage FROM codex_runtime, codex_backup",
    )
    .await;
    assert_eq!(bootstrap_codex_storage(&migrator_a).await, Ok(()));
    let mut schema_observer = migrator_a.acquire().await.expect("inspect schema grants");
    let schema_usage: (bool, bool) = sqlx::query_as(
        "SELECT has_schema_privilege('codex_runtime', 'codex_storage', 'USAGE'), has_schema_privilege('codex_backup', 'codex_storage', 'USAGE')",
    )
    .fetch_one(&mut *schema_observer)
    .await
    .expect("reader roles regain schema access");
    assert_eq!(schema_usage, (true, true));
    drop(schema_observer);

    for (damage, repair) in [
        (
            "ALTER TABLE codex_storage.codex_schema_meta DROP CONSTRAINT codex_schema_meta_pkey; ALTER TABLE codex_storage.codex_schema_meta ADD CONSTRAINT codex_schema_meta_pkey PRIMARY KEY (format_version)",
            "ALTER TABLE codex_storage.codex_schema_meta DROP CONSTRAINT codex_schema_meta_pkey; ALTER TABLE codex_storage.codex_schema_meta ADD CONSTRAINT codex_schema_meta_pkey PRIMARY KEY (singleton)",
        ),
        (
            "ALTER TABLE codex_storage._codex_pg_migrations DROP CONSTRAINT _codex_pg_migrations_pkey; ALTER TABLE codex_storage._codex_pg_migrations ADD CONSTRAINT _codex_pg_migrations_pkey PRIMARY KEY (description)",
            "ALTER TABLE codex_storage._codex_pg_migrations DROP CONSTRAINT _codex_pg_migrations_pkey; ALTER TABLE codex_storage._codex_pg_migrations ADD CONSTRAINT _codex_pg_migrations_pkey PRIMARY KEY (version)",
        ),
    ] {
        owner_query(&migrator_a, damage).await;
        assert_eq!(
            bootstrap_codex_storage(&migrator_a).await,
            Err(BootstrapError::IncompatibleNamespace)
        );
        owner_query(&migrator_a, repair).await;
    }
    owner_query(
        &migrator_a,
        "ALTER TABLE codex_storage.codex_schema_meta ENABLE ROW LEVEL SECURITY",
    )
    .await;
    assert_eq!(
        bootstrap_codex_storage(&migrator_a).await,
        Err(BootstrapError::IncompatibleNamespace)
    );
    owner_query(
        &migrator_a,
        "ALTER TABLE codex_storage.codex_schema_meta DISABLE ROW LEVEL SECURITY",
    )
    .await;

    owner_query(
        &migrator_a,
        "CREATE TYPE codex_storage.occupied_type AS ENUM ('x')",
    )
    .await;
    assert_eq!(
        bootstrap_codex_storage(&migrator_a).await,
        Err(BootstrapError::IncompatibleNamespace)
    );
    owner_query(&migrator_a, "DROP TYPE codex_storage.occupied_type").await;

    let mut reader = runtime.acquire().await.expect("runtime connection");
    let readable: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM codex_storage.codex_schema_meta)")
            .fetch_one(&mut *reader)
            .await
            .expect("runtime reads schema metadata");
    assert!(readable);
    for forbidden in [
        "UPDATE codex_storage.codex_schema_meta SET format_version = 99",
        "CREATE TABLE codex_storage.forbidden_probe(id BIGINT)",
        "SELECT version FROM codex_storage._codex_pg_migrations",
    ] {
        let error = sqlx::query(forbidden)
            .execute(&mut *reader)
            .await
            .expect_err("runtime must not alter or read migration history");
        assert_eq!(
            error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .as_deref(),
            Some("42501")
        );
    }
    drop(reader);
    assert_eq!(
        bootstrap_codex_storage(&runtime).await,
        Err(BootstrapError::Privilege)
    );

    let capabilities = ClientCapabilities {
        min_schema_format: 1,
        max_schema_format: 1,
        reader_version: 1,
        writer_version: 1,
    };
    let compatible = Ok(CompatibilityResult {
        schema_format: 1,
        activation_permitted: false,
    });
    assert_eq!(
        check_codex_storage_compatibility(&migrator_a, capabilities, RequiredAccess::ReadOnly)
            .await,
        compatible
    );
    assert_eq!(
        check_codex_storage_compatibility(&runtime, capabilities, RequiredAccess::ReadOnly).await,
        Err(CompatibilityError::Privilege)
    );

    owner_query(
        &migrator_a,
        "UPDATE codex_storage.codex_schema_meta SET min_reader_version = 2",
    )
    .await;
    assert_eq!(
        check_codex_storage_compatibility(&migrator_a, capabilities, RequiredAccess::ReadOnly)
            .await,
        Err(CompatibilityError::ReaderTooOld)
    );
    owner_query(
        &migrator_a,
        "UPDATE codex_storage.codex_schema_meta SET format_version = 2, min_reader_version = 1, min_writer_version = 1",
    )
    .await;
    assert_eq!(
        check_codex_storage_compatibility(&migrator_a, capabilities, RequiredAccess::ReadOnly)
            .await,
        Err(CompatibilityError::UnsupportedSchema)
    );
    owner_query(
        &migrator_a,
        "UPDATE codex_storage.codex_schema_meta SET format_version = 1",
    )
    .await;

    owner_query(
        &migrator_a,
        "UPDATE codex_storage._codex_pg_migrations SET success = FALSE WHERE version = 1",
    )
    .await;
    assert_eq!(
        check_codex_storage_compatibility(&migrator_a, capabilities, RequiredAccess::ReadOnly)
            .await,
        Err(CompatibilityError::DirtyMigration)
    );
    owner_query(
        &migrator_a,
        "UPDATE codex_storage._codex_pg_migrations SET success = TRUE WHERE version = 1",
    )
    .await;
    let mut connection = migrator_a.acquire().await.expect("save migration checksum");
    let mut transaction = connection.begin().await.expect("begin checksum read");
    sqlx::query("SET LOCAL ROLE codex_owner")
        .execute(&mut *transaction)
        .await
        .expect("assume owner for checksum read");
    let checksum: Vec<u8> = sqlx::query_scalar(
        "SELECT checksum FROM codex_storage._codex_pg_migrations WHERE version = 1",
    )
    .fetch_one(&mut *transaction)
    .await
    .expect("read checksum");
    transaction.rollback().await.expect("finish checksum read");
    drop(connection);
    owner_query(
        &migrator_a,
        "UPDATE codex_storage._codex_pg_migrations SET checksum = '\\x00'::bytea WHERE version = 1",
    )
    .await;
    assert_eq!(
        check_codex_storage_compatibility(&migrator_a, capabilities, RequiredAccess::ReadOnly)
            .await,
        Err(CompatibilityError::IncompatibleHistory)
    );
    owner_query(
        &migrator_a,
        "UPDATE codex_storage._codex_pg_migrations SET checksum = repeat('x', 1048576)::bytea WHERE version = 1",
    )
    .await;
    assert_eq!(
        check_codex_storage_compatibility(&migrator_a, capabilities, RequiredAccess::ReadOnly)
            .await,
        Err(CompatibilityError::IncompatibleHistory)
    );
    let mut connection = migrator_a
        .acquire()
        .await
        .expect("restore migration checksum");
    let mut transaction = connection.begin().await.expect("begin checksum restore");
    sqlx::query("SET LOCAL ROLE codex_owner")
        .execute(&mut *transaction)
        .await
        .expect("assume owner for checksum restore");
    sqlx::query("UPDATE codex_storage._codex_pg_migrations SET checksum = $1 WHERE version = 1")
        .bind(checksum)
        .execute(&mut *transaction)
        .await
        .expect("restore checksum");
    transaction.commit().await.expect("commit checksum restore");
    drop(connection);
    let mut connection = migrator_a
        .acquire()
        .await
        .expect("begin interrupted migration");
    let mut transaction = connection
        .begin()
        .await
        .expect("begin interrupted transaction");
    sqlx::query("SET LOCAL ROLE codex_owner")
        .execute(&mut *transaction)
        .await
        .expect("assume owner for interrupted transaction");
    sqlx::query("UPDATE codex_storage._codex_pg_migrations SET success = FALSE WHERE version = 1")
        .execute(&mut *transaction)
        .await
        .expect("write uncommitted dirty marker");
    drop(transaction);
    drop(connection);
    assert_eq!(
        check_codex_storage_compatibility(&migrator_a, capabilities, RequiredAccess::ReadOnly)
            .await,
        compatible
    );
}
