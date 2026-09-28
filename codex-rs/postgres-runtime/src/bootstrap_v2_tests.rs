use super::*;
use crate::ClientCapabilities;
use crate::CompatibilityError;
use crate::CompatibilityResult;
use crate::ConnectionSettings;
use crate::PoolLimits;
use crate::RequiredAccess;
use crate::check_codex_storage_compatibility;
use serde_json::Value;
use std::path::Path;

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

#[tokio::test]
async fn real_v1_upgrade_to_graph_schema_is_atomic_and_role_scoped() {
    let Ok(state) = std::env::var("CODEX_TEST_POSTGRES_UPGRADE_STATE") else {
        return;
    };
    let state = Path::new(&state);
    let first = PostgresPool::connect(settings(state, "migrator"))
        .await
        .expect("first migrator pool");
    let second = PostgresPool::connect(settings(state, "migrator"))
        .await
        .expect("second migrator pool");
    let runtime = PostgresPool::connect(settings(state, "runtime"))
        .await
        .expect("runtime pool");
    let v1 = Migrator {
        migrations: Cow::Borrowed(&BASE_MIGRATOR.migrations[..1]),
        table_name: Cow::Borrowed(MIGRATIONS_TABLE),
        locking: false,
        ignore_missing: false,
        ..Migrator::DEFAULT
    };
    let mut connection = first.acquire().await.expect("acquire migrator");
    let mut transaction = connection
        .begin()
        .await
        .expect("begin v1 fixture transaction");
    sqlx::query("SET LOCAL ROLE codex_owner")
        .execute(&mut *transaction)
        .await
        .expect("assume owner");
    v1.run_direct(/*target*/ None, &mut *transaction, /*skip*/ false)
        .await
        .expect("materialize exact embedded v1 migration");
    transaction.commit().await.expect("commit v1 fixture");
    drop(connection);

    let old = ClientCapabilities {
        min_schema_format: 1,
        max_schema_format: 1,
        reader_version: 1,
        writer_version: 1,
    };
    assert_eq!(
        check_codex_storage_compatibility(&first, old, RequiredAccess::ReadWrite).await,
        Ok(CompatibilityResult {
            schema_format: 1,
            activation_permitted: false,
        })
    );

    let mut connection = first.acquire().await.expect("acquire interrupted migrator");
    let mut transaction = connection.begin().await.expect("begin interrupted upgrade");
    sqlx::query("SET LOCAL ROLE codex_owner")
        .execute(&mut *transaction)
        .await
        .expect("assume owner for interrupted upgrade");
    let all = Migrator {
        migrations: Cow::Borrowed(BASE_MIGRATOR.migrations.as_ref()),
        table_name: Cow::Borrowed(MIGRATIONS_TABLE),
        locking: false,
        ignore_missing: false,
        ..Migrator::DEFAULT
    };
    all.run_direct(/*target*/ None, &mut *transaction, /*skip*/ false)
        .await
        .expect("run v2 inside interrupted transaction");
    transaction
        .rollback()
        .await
        .expect("roll back interrupted upgrade");
    drop(connection);
    let mut observer = first.acquire().await.expect("observe rolled-back schema");
    let mut inspection = observer.begin().await.expect("begin v1 inspection");
    sqlx::query("SET LOCAL ROLE codex_owner")
        .execute(&mut *inspection)
        .await
        .expect("assume owner for v1 inspection");
    let (format, history, table): (i32, i64, bool) = sqlx::query_as(
        "SELECT (SELECT format_version FROM codex_storage.codex_schema_meta), (SELECT COUNT(*) FROM codex_storage._codex_pg_migrations), to_regclass('codex_storage.thread_spawn_edges') IS NOT NULL",
    )
    .fetch_one(&mut *inspection)
    .await
    .expect("read rolled-back v1 schema");
    assert_eq!((format, history, table), (1, 1, false));
    inspection.rollback().await.expect("finish v1 inspection");
    drop(observer);

    let (a, b) = tokio::join!(
        bootstrap_codex_storage(&first),
        bootstrap_codex_storage(&second)
    );
    assert_eq!((a, b), (Ok(()), Ok(())));
    assert_eq!(bootstrap_codex_storage(&first).await, Ok(()));
    let mut observer = first.acquire().await.expect("read v2 history");
    let mut transaction = observer.begin().await.expect("begin history inspection");
    sqlx::query("SET LOCAL ROLE codex_owner")
        .execute(&mut *transaction)
        .await
        .expect("assume owner for history");
    let versions: (i32, i32, i32) = sqlx::query_as(
        "SELECT format_version, min_reader_version, min_writer_version FROM codex_storage.codex_schema_meta WHERE singleton = TRUE",
    )
    .fetch_one(&mut *transaction)
    .await
    .expect("read v2 format");
    let rows = sqlx::query(
        "SELECT version, success, checksum FROM codex_storage._codex_pg_migrations ORDER BY version",
    )
    .fetch_all(&mut *transaction)
    .await
    .expect("read v2 history");
    assert_eq!(versions, (2, 2, 2));
    assert!(history_matches(
        &rows,
        BASE_MIGRATOR.migrations.as_ref(),
        versions.0
    ));
    transaction
        .rollback()
        .await
        .expect("finish history inspection");
    drop(observer);
    assert_eq!(
        check_codex_storage_compatibility(&first, old, RequiredAccess::ReadWrite).await,
        Err(CompatibilityError::UnsupportedSchema)
    );
    let current = ClientCapabilities {
        min_schema_format: 2,
        max_schema_format: 2,
        reader_version: 2,
        writer_version: 2,
    };
    assert_eq!(
        check_codex_storage_compatibility(&first, current, RequiredAccess::ReadWrite).await,
        Ok(CompatibilityResult {
            schema_format: 2,
            activation_permitted: false,
        })
    );

    let mut runtime_connection = runtime.acquire().await.expect("acquire runtime");
    sqlx::query("INSERT INTO codex_storage.thread_spawn_edges VALUES ('00000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000002', 'open')")
        .execute(&mut *runtime_connection)
        .await
        .expect("runtime inserts graph edge");
    let status: String = sqlx::query_scalar(
        "SELECT status FROM codex_storage.thread_spawn_edges WHERE child_thread_id = '00000000-0000-0000-0000-000000000002'",
    )
    .fetch_one(&mut *runtime_connection)
    .await
    .expect("runtime reads graph edge");
    assert_eq!(status, "open");
    sqlx::query("UPDATE codex_storage.thread_spawn_edges SET status = 'closed' WHERE child_thread_id = '00000000-0000-0000-0000-000000000002'")
        .execute(&mut *runtime_connection)
        .await
        .expect("runtime updates graph edge");
    sqlx::query("DELETE FROM codex_storage.thread_spawn_edges WHERE child_thread_id = '00000000-0000-0000-0000-000000000002'")
        .execute(&mut *runtime_connection)
        .await
        .expect("runtime deletes graph edge");
    let denied = sqlx::query("CREATE TABLE codex_storage.forbidden_graph_probe(id BIGINT)")
        .execute(&mut *runtime_connection)
        .await
        .expect_err("runtime cannot create schema objects");
    assert_eq!(
        denied
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some("42501")
    );
}
