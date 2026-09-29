use super::*;
use pretty_assertions::assert_eq;

#[test]
fn only_the_qualified_server_release_is_accepted() {
    assert_eq!(require_qualified_server_version("170011"), Ok(()));
    for version in ["", "17.11", "160011", "170010", "170012", "180000"] {
        assert_eq!(
            require_qualified_server_version(version),
            Err(PoolError::UnsupportedServer)
        );
    }
}

#[tokio::test]
async fn real_postgres_validates_grown_and_replacement_connections() {
    let Ok(state) = std::env::var("CODEX_TEST_POSTGRES_STATE") else {
        return;
    };
    let state = PathBuf::from(state);
    let receipt: serde_json::Value = serde_json::from_slice(
        &std::fs::read(state.join("receipt.json")).expect("read isolated PostgreSQL receipt"),
    )
    .expect("parse PostgreSQL receipt");
    let pool = PostgresPool::connect(ConnectionSettings {
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
            acquire_timeout: Duration::from_secs(5),
            max_connections: 2,
        },
    })
    .await
    .expect("connect qualified observer");
    let mut observer = pool.acquire().await.expect("hold initial connection");
    let observer_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *observer)
        .await
        .expect("observer backend ID");
    let mut backend_ids = Vec::new();
    for generation in 0..2 {
        let application_name = format!("codex-version-gate-{observer_pid}-{generation}");
        pool.pool.set_connect_options(
            (*pool.pool.connect_options())
                .clone()
                .application_name(&application_name),
        );
        let connection = pool.acquire().await.expect("open qualified connection");
        // Do not issue a query on this connection: its last command must be the
        // version gate even when the pool grows or replaces a closed backend.
        let rows: Vec<(i32, String)> =
            sqlx::query_as("SELECT pid, query FROM pg_stat_activity WHERE application_name = $1")
                .bind(&application_name)
                .fetch_all(&mut *observer)
                .await
                .expect("inspect new backend's last statement");
        assert_eq!(rows.len(), 1);
        let (backend_id, last_query) = &rows[0];
        assert_eq!(last_query, "SHOW server_version_num");
        backend_ids.push(*backend_id);
        connection.close().await.expect("close checked backend");
    }
    assert_ne!(backend_ids[0], backend_ids[1]);
    drop(observer);
    pool.close().await.expect("close observer pool");
}

#[test]
fn settings_debug_redacts_credentials() {
    let settings = ConnectionSettings {
        host: "hidden.example".to_string(),
        port: 5432,
        database: "hidden_database".to_string(),
        username: "hidden_user".to_string(),
        password: "hidden_password".to_string().into(),
        ca_certificate: PathBuf::from("/hidden/ca.crt"),
        limits: PoolLimits {
            connect_timeout: Duration::from_secs(1),
            acquire_timeout: Duration::from_secs(1),
            max_connections: 1,
        },
    };
    assert_eq!(format!("{settings:?}"), "ConnectionSettings([redacted])");
}

#[tokio::test]
async fn invalid_settings_fail_before_connecting() {
    let settings = ConnectionSettings {
        host: String::new(),
        port: 5432,
        database: "codex".to_string(),
        username: "codex_runtime".to_string(),
        password: "secret".to_string().into(),
        ca_certificate: PathBuf::from("relative.crt"),
        limits: PoolLimits {
            connect_timeout: Duration::from_secs(1),
            acquire_timeout: Duration::from_secs(1),
            max_connections: 1,
        },
    };
    assert_eq!(
        PostgresPool::connect(settings).await.err(),
        Some(PoolError::InvalidSettings)
    );
}

#[tokio::test]
async fn unix_socket_host_cannot_bypass_tls() {
    let settings = ConnectionSettings {
        host: "/var/run/postgresql".to_string(),
        port: 5432,
        database: "codex".to_string(),
        username: "codex_runtime".to_string(),
        password: "secret".to_string().into(),
        ca_certificate: PathBuf::from("/trusted/ca.crt"),
        limits: PoolLimits {
            connect_timeout: Duration::from_secs(1),
            acquire_timeout: Duration::from_secs(1),
            max_connections: 1,
        },
    };
    assert_eq!(
        PostgresPool::connect(settings).await.err(),
        Some(PoolError::InvalidSettings)
    );
}

#[tokio::test]
async fn excessive_pool_limits_are_rejected() {
    let settings = ConnectionSettings {
        host: "localhost".to_string(),
        port: 5432,
        database: "codex".to_string(),
        username: "codex_runtime".to_string(),
        password: "secret".to_string().into(),
        ca_certificate: PathBuf::from("/trusted/ca.crt"),
        limits: PoolLimits {
            connect_timeout: Duration::from_secs(31),
            acquire_timeout: Duration::from_secs(1),
            max_connections: 33,
        },
    };
    assert_eq!(
        PostgresPool::connect(settings).await.err(),
        Some(PoolError::InvalidSettings)
    );
}
