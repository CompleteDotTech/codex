//! Host-only, read-only preflight for a saved PostgreSQL proposal.
//!
//! A successful check is not permission to migrate or select the candidate as
//! the active backend. The caller must be the host that owns the config stack.

use codex_config::ConfigLayerStack;
use codex_postgres_runtime::CompatibilityError;
use codex_postgres_runtime::CompatibilityResult;
use codex_postgres_runtime::ConnectionSettings;
use codex_postgres_runtime::NamedNamespace;
use codex_postgres_runtime::PoolError;
use codex_postgres_runtime::PoolLimits;
use codex_postgres_runtime::PostgresPool;
use codex_postgres_runtime::RequiredAccess;
use codex_postgres_runtime::VerifiedTarget;
use codex_postgres_runtime::check_verified_named_target_compatibility;
use codex_storage_authority::CredentialResolutionError;
use codex_storage_authority::HostCredentialResolver;
use codex_storage_authority::StorageCandidateProfile;
use std::fmt;
use std::time::Duration;

/// Diagnostics deliberately exclude profile values and driver error strings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CandidatePreflightError {
    InvalidConfiguration,
    NoRemoteCandidate,
    UnsupportedNamespace,
    ExplicitCaRequired,
    Credential(CredentialResolutionError),
    Connection(PoolError),
    Compatibility(CompatibilityError),
}

impl fmt::Display for CandidatePreflightError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "storage candidate preflight: {self:?}")
    }
}

impl std::error::Error for CandidatePreflightError {}

/// Check the trusted host candidate without creating tables, changing config,
/// switching backend, or falling back to the local SQLite store.
pub async fn preflight_trusted_candidate(
    stack: &ConfigLayerStack,
    resolver: &HostCredentialResolver<'_>,
    target: &VerifiedTarget,
) -> Result<CompatibilityResult, CandidatePreflightError> {
    let candidate = stack
        .storage_candidate()
        .map_err(|_| CandidatePreflightError::InvalidConfiguration)?;
    let Some(StorageCandidateProfile::RemotePostgres(profile)) = candidate else {
        return Err(CandidatePreflightError::NoRemoteCandidate);
    };
    let namespace = NamedNamespace::new(profile.namespace())
        .map_err(|_| CandidatePreflightError::UnsupportedNamespace)?;
    let ca_certificate = profile
        .ca_certificate()
        .ok_or(CandidatePreflightError::ExplicitCaRequired)?;
    let credential = resolver
        .resolve(profile.credential())
        .map_err(CandidatePreflightError::Credential)?;
    let settings = ConnectionSettings {
        host: profile.endpoint().to_owned(),
        port: profile.port(),
        database: profile.database().to_owned(),
        username: namespace.migrator_login().to_owned(),
        password: credential.into_zeroizing().into(),
        ca_certificate: ca_certificate.to_path_buf(),
        limits: PoolLimits {
            connect_timeout: Duration::from_secs(u64::from(profile.connect_timeout_seconds())),
            acquire_timeout: Duration::from_secs(u64::from(profile.pool_acquire_timeout_seconds())),
            max_connections: u32::from(profile.max_connections()),
        },
    };
    let pool = PostgresPool::connect(settings)
        .await
        .map_err(CandidatePreflightError::Connection)?;
    let result = check_verified_named_target_compatibility(
        &pool,
        &namespace,
        target,
        RequiredAccess::ReadWrite,
    )
    .await
    .map_err(CandidatePreflightError::Compatibility);
    let close = pool.close().await;
    match (result, close) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(CandidatePreflightError::Connection(error)),
        (Ok(value), Ok(())) => Ok(value),
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
