#![expect(
    clippy::expect_used,
    reason = "isolated PostgreSQL fixture failures should identify their source"
)]

use codex_postgres_runtime::ConnectionSettings;
use codex_postgres_runtime::PoolError;
use codex_postgres_runtime::PoolLimits;
use codex_postgres_runtime::PostgresPool;
use pretty_assertions::assert_eq;
use serde_json::Value;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

fn settings(state: &Path) -> ConnectionSettings {
    let receipt: Value = serde_json::from_slice(
        &std::fs::read(state.join("receipt.json")).expect("read isolated PostgreSQL receipt"),
    )
    .expect("parse PostgreSQL receipt");
    ConnectionSettings {
        host: "localhost".to_string(),
        port: receipt["port"].as_u64().expect("PostgreSQL port") as u16,
        database: "codex".to_string(),
        username: "codex_runtime".to_string(),
        password: std::fs::read_to_string(state.join("secrets/runtime.password"))
            .expect("read private runtime credential")
            .trim()
            .to_string(),
        ca_certificate: state.join("secrets/ca.crt"),
        limits: PoolLimits {
            connect_timeout: Duration::from_secs(5),
            acquire_timeout: Duration::from_secs(1),
            max_connections: 1,
        },
    }
}

#[tokio::test]
async fn real_postgres_pool_bounds_waits_and_rejects_bad_credentials() {
    let Ok(state) = std::env::var("CODEX_TEST_POSTGRES_STATE") else {
        return;
    };
    let state = Path::new(&state);
    let pool = Arc::new(
        PostgresPool::connect(settings(state))
            .await
            .expect("connect with verified TLS"),
    );
    pool.health().await.expect("healthy PostgreSQL connection");
    let held = pool.acquire().await.expect("hold sole connection");
    let waiting = tokio::spawn({
        let pool = Arc::clone(&pool);
        async move { pool.acquire().await }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    waiting.abort();
    assert!(waiting.await.expect_err("cancelled waiter").is_cancelled());
    assert_eq!(pool.health().await, Err(PoolError::Timeout));
    drop(held);
    pool.health().await.expect("pool recovers after release");
    pool.close().await.expect("bounded shutdown");
    assert_eq!(pool.health().await, Err(PoolError::Closed));

    let mut invalid = settings(state);
    invalid.password = "wrong-password".to_string();
    assert_eq!(
        PostgresPool::connect(invalid).await.err(),
        Some(PoolError::Authentication)
    );
}

#[tokio::test]
async fn real_postgres_acquisition_can_outwait_initial_connection_timeout() {
    let Ok(state) = std::env::var("CODEX_TEST_POSTGRES_STATE") else {
        return;
    };
    let mut connection_settings = settings(Path::new(&state));
    connection_settings.limits.connect_timeout = Duration::from_secs(1);
    connection_settings.limits.acquire_timeout = Duration::from_secs(5);
    let pool = PostgresPool::connect(connection_settings)
        .await
        .expect("connect with verified TLS");
    let held = pool.acquire().await.expect("hold sole connection");
    let (acquired, ()) = tokio::join!(pool.acquire(), async {
        tokio::time::sleep(Duration::from_secs(2)).await;
        drop(held);
    });
    let mut acquired = acquired.expect("acquisition honors its longer timeout");
    assert_eq!(
        sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(&mut *acquired)
            .await
            .expect("reused connection works"),
        1
    );
    drop(acquired);
    pool.close().await.expect("bounded shutdown");
}

#[tokio::test]
async fn real_postgres_startup_can_outwait_acquisition_timeout() {
    let Ok(state) = std::env::var("CODEX_TEST_POSTGRES_STATE") else {
        return;
    };
    let mut connection_settings = settings(Path::new(&state));
    let upstream_port = connection_settings.port;
    let relay = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind isolated TLS relay");
    connection_settings.port = relay.local_addr().expect("relay address").port();
    let forwarding = tokio::spawn(async move {
        let (mut client, _) = relay.accept().await.expect("accept pool connection");
        tokio::time::sleep(Duration::from_secs(2)).await;
        let mut upstream = tokio::net::TcpStream::connect(("localhost", upstream_port))
            .await
            .expect("connect relay to real PostgreSQL");
        tokio::io::copy_bidirectional(&mut client, &mut upstream)
            .await
            .expect("forward encrypted PostgreSQL traffic");
    });
    let pool = PostgresPool::connect(connection_settings)
        .await
        .expect("startup honors its longer timeout");
    pool.health()
        .await
        .expect("TLS connection works through relay");
    pool.close().await.expect("bounded shutdown");
    tokio::time::timeout(Duration::from_secs(5), forwarding)
        .await
        .expect("relay shuts down")
        .expect("relay completed");
}

#[tokio::test]
async fn real_postgres_rejects_untrusted_ca() {
    let Ok(state) = std::env::var("CODEX_TEST_POSTGRES_STATE") else {
        return;
    };
    let state = Path::new(&state);
    let mut invalid_ca = settings(state);
    let unrelated_ca = tempfile::tempdir().expect("isolated CA fixture");
    let unrelated_cert = unrelated_ca.path().join("unrelated.crt");
    let unrelated_key = unrelated_ca.path().join("unrelated.key");
    let result = Command::new("openssl")
        .args(["req", "-x509", "-newkey", "rsa:2048", "-nodes"])
        .arg("-keyout")
        .arg(&unrelated_key)
        .arg("-out")
        .arg(&unrelated_cert)
        .args(["-subj", "/CN=unrelated.test", "-days", "1"])
        .output()
        .expect("generate unrelated CA certificate");
    assert!(result.status.success());
    invalid_ca.ca_certificate = unrelated_cert;
    assert_eq!(
        PostgresPool::connect(invalid_ca).await.err(),
        Some(PoolError::Tls)
    );
}
