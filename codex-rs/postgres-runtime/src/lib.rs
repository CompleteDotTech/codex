//! A bounded PostgreSQL connection pool for host-resolved credentials.
//!
//! This crate does not create tables, select the active storage backend, or
//! grant authority to a candidate configuration.

#![expect(
    clippy::disallowed_methods,
    reason = "this is the centralized PostgreSQL connection shim"
)]

use sqlx::ConnectOptions;
use sqlx::PgPool;
use sqlx::Postgres;
use sqlx::pool::PoolConnection;
use sqlx::postgres::PgConnectOptions;
use sqlx::postgres::PgPoolOptions;
use sqlx::postgres::PgSslMode;
use std::fmt;
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::timeout;

mod bootstrap;
pub use bootstrap::BootstrapError;
pub use bootstrap::bootstrap_codex_storage;
mod compatibility;
pub use compatibility::ClientCapabilities;
pub use compatibility::CompatibilityError;
pub use compatibility::CompatibilityResult;
pub use compatibility::RequiredAccess;
pub use compatibility::check_codex_storage_compatibility;
mod namespace;
pub use namespace::InvalidNamespace;
pub use namespace::NamedNamespace;
mod named_bootstrap;
pub use named_bootstrap::bootstrap_named_namespace;

const MAX_WAIT: Duration = Duration::from_secs(30);
const MAX_CONNECTIONS: u32 = 32;

/// Resolved by the owning host. The password must not be logged or persisted.
pub struct ConnectionSettings {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    pub password: String,
    pub ca_certificate: PathBuf,
    pub limits: PoolLimits,
}

impl fmt::Debug for ConnectionSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ConnectionSettings([redacted])")
    }
}

/// Every operation has a finite wait; the host supplies validated limits.
#[derive(Clone, Copy, Debug)]
pub struct PoolLimits {
    pub connect_timeout: Duration,
    pub acquire_timeout: Duration,
    pub max_connections: u32,
}

/// Safe to show to a caller; no SQLx or driver error text is exposed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PoolError {
    InvalidSettings,
    Timeout,
    Authentication,
    Tls,
    Unavailable,
    Closed,
}

impl fmt::Display for PoolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PostgreSQL pool error: {self:?}")
    }
}

impl std::error::Error for PoolError {}

fn classify(error: &sqlx::Error) -> PoolError {
    match error {
        sqlx::Error::PoolTimedOut => PoolError::Timeout,
        sqlx::Error::PoolClosed => PoolError::Closed,
        sqlx::Error::Database(error) if error.code().as_deref() == Some("28P01") => {
            PoolError::Authentication
        }
        sqlx::Error::Tls(_) => PoolError::Tls,
        sqlx::Error::Io(error) if matches!(error.get_ref(), Some(source) if source.is::<rustls::Error>()) => {
            PoolError::Tls
        }
        _ => PoolError::Unavailable,
    }
}

/// A standalone pool that cannot mutate SQLite or Codex storage selection.
pub struct PostgresPool {
    pool: PgPool,
    acquire_timeout: Duration,
}

impl PostgresPool {
    pub async fn connect(settings: ConnectionSettings) -> Result<Self, PoolError> {
        let limits = settings.limits;
        let valid_host = settings.host.parse::<IpAddr>().is_ok()
            || (settings.host.len() <= 253
                && settings.host.split('.').all(|label| {
                    !label.is_empty()
                        && label.len() <= 63
                        && label
                            .as_bytes()
                            .first()
                            .is_some_and(u8::is_ascii_alphanumeric)
                        && label
                            .as_bytes()
                            .last()
                            .is_some_and(u8::is_ascii_alphanumeric)
                        && label
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                }));
        if !valid_host
            || settings.database.is_empty()
            || settings.username.is_empty()
            || settings.password.is_empty()
            || !settings.ca_certificate.is_absolute()
            || settings.port == 0
            || limits.max_connections == 0
            || limits.max_connections > MAX_CONNECTIONS
            || limits.connect_timeout.is_zero()
            || limits.acquire_timeout.is_zero()
            || limits.connect_timeout > MAX_WAIT
            || limits.acquire_timeout > MAX_WAIT
        {
            return Err(PoolError::InvalidSettings);
        }
        let options = PgConnectOptions::new()
            .host(&settings.host)
            .port(settings.port)
            .database(&settings.database)
            .username(&settings.username)
            .password(&settings.password)
            .ssl_mode(PgSslMode::VerifyFull)
            .ssl_root_cert(&settings.ca_certificate)
            .disable_statement_logging();
        let pool = timeout(
            limits.connect_timeout,
            PgPoolOptions::new()
                .max_connections(limits.max_connections)
                .acquire_timeout(limits.connect_timeout)
                .connect_with(options),
        )
        .await
        .map_err(|_| PoolError::Timeout)?
        .map_err(|error| classify(&error))?;
        Ok(Self {
            pool,
            acquire_timeout: limits.acquire_timeout,
        })
    }

    pub async fn health(&self) -> Result<(), PoolError> {
        let mut connection = self.acquire().await?;
        timeout(
            self.acquire_timeout,
            sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&mut *connection),
        )
        .await
        .map_err(|_| PoolError::Timeout)?
        .map_err(|error| classify(&error))?;
        Ok(())
    }

    pub async fn close(&self) -> Result<(), PoolError> {
        timeout(self.acquire_timeout, self.pool.close())
            .await
            .map_err(|_| PoolError::Timeout)
    }

    pub async fn acquire(&self) -> Result<PoolConnection<Postgres>, PoolError> {
        timeout(self.acquire_timeout, self.pool.acquire())
            .await
            .map_err(|_| PoolError::Timeout)?
            .map_err(|error| classify(&error))
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
