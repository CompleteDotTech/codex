//! The vocabulary shared by every front end of the storage service: one set of blocker codes,
//! one status shape and one operation record, so the CLI, the API and the TUI report the same
//! facts.

use codex_remote_storage::RemoteStorageError;
use codex_storage_authority::CredentialResolutionError;
use codex_storage_migration::CutoverError;
use codex_storage_migration::MigrationError;
use serde::Deserialize;
use serde::Serialize;
use std::fmt;
use uuid::Uuid;

/// Why an operation cannot proceed. The spelling of each code is part of the public contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockerCode {
    NoCandidateProfile,
    MigratorCredentialMissing,
    CredentialUnavailable,
    CaCertificateRequired,
    UnsupportedNamespace,
    ConnectionFailed,
    ConnectionTimedOut,
    SchemaNeedsUpgrade,
    SchemaTooNew,
    SchemaInvalid,
    TargetNotEmpty,
    DatasetNotActivated,
    DatasetMismatch,
    DatasetMigrating,
    DatasetRetired,
    CutoverInProgress,
    AuthorityInvalid,
    AlreadyRemote,
    NotRemote,
    SqliteHomeDiffersFromCodexHome,
    StalePlan,
    NotConfirmed,
    OperationConflict,
    OperationNotFound,
    SourceUnreadable,
    StagingFailed,
    VerificationFailed,
    Internal,
}

impl BlockerCode {
    /// The stable spelling used in JSON, logs and exit reports.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoCandidateProfile => "no_candidate_profile",
            Self::MigratorCredentialMissing => "migrator_credential_missing",
            Self::CredentialUnavailable => "credential_unavailable",
            Self::CaCertificateRequired => "ca_certificate_required",
            Self::UnsupportedNamespace => "unsupported_namespace",
            Self::ConnectionFailed => "connection_failed",
            Self::ConnectionTimedOut => "connection_timed_out",
            Self::SchemaNeedsUpgrade => "schema_needs_upgrade",
            Self::SchemaTooNew => "schema_too_new",
            Self::SchemaInvalid => "schema_invalid",
            Self::TargetNotEmpty => "target_not_empty",
            Self::DatasetNotActivated => "dataset_not_activated",
            Self::DatasetMismatch => "dataset_mismatch",
            Self::DatasetMigrating => "dataset_migrating",
            Self::DatasetRetired => "dataset_retired",
            Self::CutoverInProgress => "cutover_in_progress",
            Self::AuthorityInvalid => "authority_invalid",
            Self::AlreadyRemote => "already_remote",
            Self::NotRemote => "not_remote",
            Self::SqliteHomeDiffersFromCodexHome => "sqlite_home_differs_from_codex_home",
            Self::StalePlan => "stale_plan",
            Self::NotConfirmed => "not_confirmed",
            Self::OperationConflict => "operation_conflict",
            Self::OperationNotFound => "operation_not_found",
            Self::SourceUnreadable => "source_unreadable",
            Self::StagingFailed => "staging_failed",
            Self::VerificationFailed => "verification_failed",
            Self::Internal => "internal",
        }
    }

    /// Whether the same request can succeed later without anyone changing configuration.
    pub fn is_retryable(self) -> bool {
        matches!(
            self,
            Self::ConnectionFailed
                | Self::ConnectionTimedOut
                | Self::DatasetMigrating
                | Self::OperationConflict
        )
    }
}

impl fmt::Display for BlockerCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A refused or failed operation. It carries a code and nothing else, so it can never leak a
/// credential, a path or a row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StorageError(pub BlockerCode);

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "storage operation blocked: {}", self.0)
    }
}

impl std::error::Error for StorageError {}

impl From<BlockerCode> for StorageError {
    fn from(code: BlockerCode) -> Self {
        Self(code)
    }
}

impl From<RemoteStorageError> for BlockerCode {
    fn from(error: RemoteStorageError) -> Self {
        use codex_postgres_runtime::PoolError;
        use codex_postgres_runtime::RuntimeSchemaError;
        match error {
            RemoteStorageError::UnsupportedNamespace => Self::UnsupportedNamespace,
            RemoteStorageError::CaCertificateRequired => Self::CaCertificateRequired,
            RemoteStorageError::MigratorCredentialMissing => Self::MigratorCredentialMissing,
            RemoteStorageError::Credential(CredentialResolutionError::StoreUnavailable) => {
                Self::CredentialUnavailable
            }
            RemoteStorageError::Credential(_) => Self::CredentialUnavailable,
            RemoteStorageError::Connection(PoolError::Timeout) => Self::ConnectionTimedOut,
            RemoteStorageError::Connection(_) => Self::ConnectionFailed,
            RemoteStorageError::Schema(RuntimeSchemaError::NeedsUpgrade) => {
                Self::SchemaNeedsUpgrade
            }
            RemoteStorageError::Schema(RuntimeSchemaError::ClientTooOld) => Self::SchemaTooNew,
            RemoteStorageError::Schema(RuntimeSchemaError::Invalid) => Self::SchemaInvalid,
            RemoteStorageError::Schema(RuntimeSchemaError::Unavailable) => Self::ConnectionFailed,
            RemoteStorageError::Migrating => Self::DatasetMigrating,
            RemoteStorageError::Retired => Self::DatasetRetired,
            RemoteStorageError::GenerationChanged => Self::DatasetMismatch,
        }
    }
}

impl From<MigrationError> for BlockerCode {
    fn from(error: MigrationError) -> Self {
        match error {
            MigrationError::Source(_) => Self::SourceUnreadable,
            MigrationError::Target(_) => Self::ConnectionFailed,
            MigrationError::TargetBusy => Self::OperationConflict,
            MigrationError::Mismatch { .. } => Self::VerificationFailed,
            MigrationError::Interrupted => Self::OperationConflict,
            MigrationError::TargetNotEmpty => Self::TargetNotEmpty,
            MigrationError::NotVerified => Self::VerificationFailed,
            MigrationError::GenerationNotAdvancing => Self::DatasetMismatch,
            MigrationError::NotActivated => Self::DatasetNotActivated,
            MigrationError::Staging(_) => Self::StagingFailed,
        }
    }
}

impl From<CutoverError> for BlockerCode {
    fn from(error: CutoverError) -> Self {
        match error {
            CutoverError::Authority(_) => Self::AuthorityInvalid,
            CutoverError::Destination(error) => error.into(),
            CutoverError::Conflict => Self::DatasetMismatch,
            CutoverError::Uncertain => Self::CutoverInProgress,
            CutoverError::AlreadyActivated => Self::AlreadyRemote,
        }
    }
}

/// Which backend holds the authoritative history.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendName {
    LocalSqlite,
    RemotePostgres,
}

/// What the home's authority records say.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityLabel {
    /// No records: a home from before storage authority, which stays local.
    Unmanaged,
    Local,
    Remote,
    CutoverInProgress,
    Invalid,
}

/// What the remote dataset reports when it was reachable.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct RemoteSummary {
    pub reachable: bool,
    /// `open`, `migrating` or `retired`.
    pub state: Option<String>,
    pub generation: Option<i64>,
    pub dataset_id: Option<Uuid>,
    /// Whether the dataset is the one this home published, when this home has published one.
    pub dataset_matches: Option<bool>,
    pub blocker: Option<BlockerCode>,
}

/// The active backend and everything an operator needs to decide what to do next.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct StorageStatus {
    pub active_backend: BackendName,
    pub authority: AuthorityLabel,
    pub local_generation: Option<u64>,
    pub dataset_id: Option<Uuid>,
    pub remote_ever_activated: bool,
    pub candidate_configured: bool,
    pub remote: Option<RemoteSummary>,
    pub blockers: Vec<BlockerCode>,
}

/// Where the connection test stopped.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStage {
    Profile,
    Credential,
    Connect,
    Schema,
    Dataset,
    Ready,
}

/// The outcome of testing the saved profile without changing anything.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ConnectionReport {
    pub stage: CheckStage,
    pub blocker: Option<BlockerCode>,
    pub schema_format: Option<i32>,
    pub dataset_state: Option<String>,
    pub dataset_id: Option<Uuid>,
    pub generation: Option<i64>,
    /// Whether the dataset holds no history, so a migration could write into it.
    pub empty: Option<bool>,
}

/// What a plan moves.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanAction {
    /// Copy this home's history into the empty remote dataset and make it authoritative.
    Migrate,
    /// Copy the remote dataset back into this home and make local files authoritative.
    Return,
}
