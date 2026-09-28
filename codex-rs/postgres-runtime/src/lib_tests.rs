use super::*;

#[test]
fn only_the_qualified_server_release_is_accepted() {
    assert_eq!(require_qualified_server_version("170011"), Ok(()));
    for version in ["", "17.11", "160011", "170010", "170012", "180000"] {
        assert_eq!(
            require_qualified_server_version(version),
            Err(PoolError::UnsupportedServer)
        );
    }
    assert_eq!(
        PoolError::UnsupportedServer.to_string(),
        "PostgreSQL pool error: UnsupportedServer"
    );
}

#[test]
fn settings_debug_redacts_credentials() {
    let settings = ConnectionSettings {
        host: "hidden.example".to_string(),
        port: 5432,
        database: "hidden_database".to_string(),
        username: "hidden_user".to_string(),
        password: "hidden_password".to_string(),
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
        password: "secret".to_string(),
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
        password: "secret".to_string(),
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
        password: "secret".to_string(),
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
