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
use serde_json::Value;
use sqlx::Acquire;
use std::path::Path;
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
    sqlx::query(sql)
        .execute(&mut *transaction)
        .await
        .expect("execute owner fixture SQL");
    transaction
        .commit()
        .await
        .expect("commit owner fixture SQL");
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
    let migrator_b = PostgresPool::connect(settings(state, "migrator"))
        .await
        .expect("second migrator pool");
    let runtime = PostgresPool::connect(settings(state, "runtime"))
        .await
        .expect("runtime pool");

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
        min_schema_format: 3,
        max_schema_format: 3,
        reader_version: 3,
        writer_version: 3,
    };
    let compatible = Ok(CompatibilityResult {
        schema_format: 3,
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
        "UPDATE codex_storage.codex_schema_meta SET min_writer_version = 4",
    )
    .await;
    assert_eq!(
        check_codex_storage_compatibility(&migrator_a, capabilities, RequiredAccess::ReadOnly)
            .await,
        compatible
    );
    assert_eq!(
        check_codex_storage_compatibility(&migrator_a, capabilities, RequiredAccess::ReadWrite)
            .await,
        Err(CompatibilityError::WriterTooOld)
    );
    owner_query(
        &migrator_a,
        "UPDATE codex_storage.codex_schema_meta SET min_reader_version = 4",
    )
    .await;
    assert_eq!(
        check_codex_storage_compatibility(&migrator_a, capabilities, RequiredAccess::ReadOnly)
            .await,
        Err(CompatibilityError::ReaderTooOld)
    );
    owner_query(
        &migrator_a,
        "UPDATE codex_storage.codex_schema_meta SET format_version = 4, min_reader_version = 3, min_writer_version = 3",
    )
    .await;
    assert_eq!(
        check_codex_storage_compatibility(&migrator_a, capabilities, RequiredAccess::ReadOnly)
            .await,
        Err(CompatibilityError::UnsupportedSchema)
    );
    owner_query(
        &migrator_a,
        "UPDATE codex_storage.codex_schema_meta SET format_version = 3",
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
    owner_query(
        &migrator_a,
        "INSERT INTO codex_storage._codex_pg_migrations (version, description, success, checksum, execution_time) SELECT 999, description, TRUE, checksum, execution_time FROM codex_storage._codex_pg_migrations WHERE version = 1",
    )
    .await;
    assert_eq!(
        check_codex_storage_compatibility(&migrator_a, capabilities, RequiredAccess::ReadOnly)
            .await,
        Err(CompatibilityError::IncompatibleHistory)
    );
    owner_query(
        &migrator_a,
        "DELETE FROM codex_storage._codex_pg_migrations WHERE version = 999",
    )
    .await;

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
