use codex_postgres_runtime::PoolError;
use codex_postgres_runtime::RuntimeSchemaError;
use codex_storage_authority::CredentialResolutionError;
use std::fmt;

/// Why a remote store could not be used. Diagnostics never carry profile values, credentials or
/// driver messages.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteStorageError {
    /// The profile names a namespace this client cannot address.
    UnsupportedNamespace,
    /// Remote connections verify the server against an explicit certificate authority.
    CaCertificateRequired,
    /// The credential could not be resolved.
    Credential(CredentialResolutionError),
    /// The profile has no schema-owner credential, so it cannot initialize or upgrade a dataset.
    MigratorCredentialMissing,
    /// The connection could not be established.
    Connection(PoolError),
    /// The store's schema does not match this build.
    Schema(RuntimeSchemaError),
    /// A migration holds the store, so writes are refused until it finishes.
    Migrating,
    /// The dataset was handed back to local storage; this client must not use it.
    Retired,
    /// The dataset was activated again after this handle connected. The handle is stale and the
    /// host must reconnect before it resumes any work.
    GenerationChanged,
}

impl RemoteStorageError {
    /// Whether the same request can succeed later without anyone changing configuration or
    /// upgrading software.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Connection(PoolError::Timeout | PoolError::Unavailable | PoolError::Closed) => {
                true
            }
            Self::Schema(RuntimeSchemaError::Unavailable) | Self::Migrating => true,
            Self::Credential(CredentialResolutionError::StoreUnavailable) => true,
            Self::UnsupportedNamespace
            | Self::CaCertificateRequired
            | Self::MigratorCredentialMissing
            | Self::Credential(_)
            | Self::Connection(_)
            | Self::Schema(_)
            | Self::GenerationChanged
            | Self::Retired => false,
        }
    }
}

impl fmt::Display for RemoteStorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "remote storage: {self:?}")
    }
}

impl std::error::Error for RemoteStorageError {}
