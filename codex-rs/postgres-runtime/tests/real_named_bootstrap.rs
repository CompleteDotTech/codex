#![expect(
    clippy::expect_used,
    reason = "isolated PostgreSQL fixture failures should identify their source"
)]

use codex_postgres_runtime::BootstrapError;
use codex_postgres_runtime::ConnectionSettings;
use codex_postgres_runtime::NamedNamespace;
use codex_postgres_runtime::PoolLimits;
use codex_postgres_runtime::PostgresPool;
use codex_postgres_runtime::bootstrap_named_namespace;
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

async fn owner_sql(pool: &PostgresPool, sql: &'static str) {
    let mut connection = pool.acquire().await.expect("acquire named migrator");
    let mut transaction = connection.begin().await.expect("begin owner transaction");
    sqlx::query("SET LOCAL ROLE codex_isolation_owner")
        .execute(&mut *transaction)
        .await
        .expect("assume named owner");
    sqlx::query(sql)
        .execute(&mut *transaction)
        .await
        .expect("run owner fixture SQL");
    transaction
        .commit()
        .await
        .expect("commit owner fixture SQL");
}

#[tokio::test]
async fn real_named_namespace_bootstrap_is_isolated_and_rejects_wrong_roles() {
    let Ok(state) = std::env::var("CODEX_TEST_POSTGRES_ISOLATION_STATE") else {
        return;
    };
    let state = Path::new(&state);
    let namespace = NamedNamespace::new("codex_storage_isolation").expect("valid named schema");
    assert!(NamedNamespace::new("codex_storage_isolation\";DROP SCHEMA public").is_err());
    let first = PostgresPool::connect(settings(state, "isolation_migrator"))
        .await
        .expect("first named migrator");
    let second = PostgresPool::connect(settings(state, "isolation_migrator"))
        .await
        .expect("second named migrator");
    let runtime = PostgresPool::connect(settings(state, "isolation_runtime"))
        .await
        .expect("named runtime");
    let default_migrator = PostgresPool::connect(settings(state, "migrator"))
        .await
        .expect("default migrator");

    assert_eq!(
        bootstrap_named_namespace(&runtime, &namespace).await,
        Err(BootstrapError::Privilege)
    );
    assert_eq!(
        bootstrap_named_namespace(&default_migrator, &namespace).await,
        Err(BootstrapError::Privilege)
    );
    owner_sql(
        &first,
        "CREATE TYPE codex_storage_isolation.occupied_type AS ENUM ('x')",
    )
    .await;
    assert_eq!(
        bootstrap_named_namespace(&first, &namespace).await,
        Err(BootstrapError::IncompatibleNamespace)
    );
    owner_sql(&first, "DROP TYPE codex_storage_isolation.occupied_type").await;

    let (a, b) = tokio::join!(
        bootstrap_named_namespace(&first, &namespace),
        bootstrap_named_namespace(&second, &namespace)
    );
    assert_eq!((a, b), (Ok(()), Ok(())));
    assert_eq!(bootstrap_named_namespace(&first, &namespace).await, Ok(()));

    let mut owner = first.acquire().await.expect("read named ledger");
    let mut transaction = owner.begin().await.expect("begin named ledger read");
    sqlx::query("SET LOCAL ROLE codex_isolation_owner")
        .execute(&mut *transaction)
        .await
        .expect("assume named owner for ledger");
    let (format, history): (i32, i64) = sqlx::query_as(
        "SELECT (SELECT format_version FROM codex_storage_isolation.codex_schema_meta), (SELECT COUNT(*) FROM codex_storage_isolation._codex_pg_migrations)",
    )
    .fetch_one(&mut *transaction)
    .await
    .expect("read named metadata and history");
    assert_eq!((format, history), (1, 1));
    transaction
        .rollback()
        .await
        .expect("finish named ledger read");
    drop(owner);
    let mut runtime_connection = runtime.acquire().await.expect("read named metadata");
    let visible: i32 =
        sqlx::query_scalar("SELECT format_version FROM codex_storage_isolation.codex_schema_meta")
            .fetch_one(&mut *runtime_connection)
            .await
            .expect("runtime reads named metadata");
    assert_eq!(visible, 1);
    let denied = sqlx::query("SELECT version FROM codex_storage_isolation._codex_pg_migrations")
        .execute(&mut *runtime_connection)
        .await
        .expect_err("runtime must not read named migration history");
    assert_eq!(
        denied
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some("42501")
    );
}
