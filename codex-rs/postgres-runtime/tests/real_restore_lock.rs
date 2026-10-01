#![expect(
    clippy::expect_used,
    reason = "isolated PostgreSQL fixture failures should identify their source"
)]

use codex_postgres_runtime::ClientCapabilities;
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
use tokio::time::sleep;
use tokio::time::timeout;

fn settings(state: &Path) -> ConnectionSettings {
    let receipt: Value = serde_json::from_slice(
        &std::fs::read(state.join("receipt.json")).expect("read isolated PostgreSQL receipt"),
    )
    .expect("parse PostgreSQL receipt");
    ConnectionSettings {
        host: "localhost".to_string(),
        port: receipt["port"].as_u64().expect("PostgreSQL port") as u16,
        database: "codex".to_string(),
        username: "codex_migrator".to_string(),
        password: std::fs::read_to_string(state.join("secrets/migrator.password"))
            .expect("read private migrator credential")
            .trim()
            .to_string()
            .into(),
        ca_certificate: state.join("secrets/ca.crt"),
        limits: PoolLimits {
            connect_timeout: Duration::from_secs(5),
            acquire_timeout: Duration::from_secs(5),
            max_connections: 4,
        },
    }
}

#[tokio::test]
async fn real_restore_lock_serializes_bootstrap_and_preflight() {
    let Ok(state) = std::env::var("CODEX_TEST_POSTGRES_LOCK_STATE") else {
        return;
    };
    let migrator = Arc::new(
        PostgresPool::connect(settings(Path::new(&state)))
            .await
            .expect("connect migrator"),
    );
    bootstrap_codex_storage(&migrator)
        .await
        .expect("bootstrap fixture before contention");

    // The restore guard takes this exact schema-wide transaction lock.
    let mut connection = migrator.acquire().await.expect("acquire lock holder");
    let mut restore = connection.begin().await.expect("begin simulated restore");
    sqlx::query("SET LOCAL ROLE codex_owner")
        .execute(&mut *restore)
        .await
        .expect("assume owner for restore lock");
    sqlx::query("SELECT pg_advisory_xact_lock($1, $2)")
        .bind(1_414_676_819_i32)
        .bind(1_i32)
        .fetch_one(&mut *restore)
        .await
        .expect("hold restore schema lock");

    let preflight_pool = Arc::clone(&migrator);
    let preflight = tokio::spawn(async move {
        check_codex_storage_compatibility(
            &preflight_pool,
            ClientCapabilities {
                min_schema_format: 10,
                max_schema_format: 10,
                reader_version: 10,
                writer_version: 10,
            },
            RequiredAccess::ReadWrite,
        )
        .await
    });
    let bootstrap_pool = Arc::clone(&migrator);
    let bootstrap = tokio::spawn(async move { bootstrap_codex_storage(&bootstrap_pool).await });

    let mut observer = migrator.acquire().await.expect("acquire lock observer");
    timeout(Duration::from_secs(5), async {
        loop {
            let waiting: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_locks WHERE locktype = 'advisory' AND classid = 1414676819::oid AND objid = 1::oid AND NOT granted",
            )
            .fetch_one(&mut *observer)
            .await
            .expect("inspect advisory waiters");
            if waiting == 2 {
                break;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("both calls wait for the restore lock");
    assert!(!preflight.is_finished(), "preflight bypassed restore lock");
    assert!(!bootstrap.is_finished(), "bootstrap bypassed restore lock");
    restore.commit().await.expect("release restore lock");

    assert_eq!(
        timeout(Duration::from_secs(10), preflight)
            .await
            .expect("preflight unblocked")
            .expect("preflight task completed"),
        Ok(CompatibilityResult {
            schema_format: 10,
            activation_permitted: false,
        })
    );
    assert_eq!(
        timeout(Duration::from_secs(10), bootstrap)
            .await
            .expect("bootstrap unblocked")
            .expect("bootstrap task completed"),
        Ok(())
    );
}
