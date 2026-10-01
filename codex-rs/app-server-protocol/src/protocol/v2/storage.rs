use crate::JsonSchema;
use crate::TS;
use serde::Deserialize;
use serde::Serialize;

/// Which backend holds the authoritative history.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum StorageBackend {
    LocalSqlite,
    RemotePostgres,
}

/// What the home's authority records say.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum StorageAuthority {
    Unmanaged,
    Local,
    Remote,
    CutoverInProgress,
    Invalid,
}

/// Why a storage operation cannot proceed. The set is stable; clients may switch on it.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum StorageBlocker {
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
    HomeAlreadyManaged,
    SqliteHomeDiffersFromCodexHome,
    StalePlan,
    NotConfirmed,
    OperationConflict,
    OperationNotFound,
    SourceUnreadable,
    StagingFailed,
    VerificationFailed,
    StorageAdminRequired,
    Internal,
}

/// Where the connection test stopped.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum StorageCheckStage {
    Profile,
    Credential,
    Connect,
    Schema,
    Dataset,
    Ready,
}

/// What a plan or operation moves.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum StoragePlanAction {
    /// Copy this host's history into the remote dataset and make it authoritative.
    Migrate,
    /// Copy the remote dataset back into this host and make local files authoritative.
    Return,
    /// Join a dataset that already exists. Nothing is copied or merged.
    Attach,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum StorageOperationState {
    Planned,
    Copying,
    Verifying,
    /// Verified and waiting for the explicit step that makes it authoritative.
    Ready,
    Committing,
    Active,
    Failed,
    Cancelled,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum StorageRecoveryOutcome {
    Idle,
    RolledForward,
    RolledBack,
}

/// What the remote dataset reported when it was reachable.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct StorageRemoteSummary {
    pub reachable: bool,
    /// `open`, `migrating` or `retired`.
    pub state: Option<String>,
    #[ts(type = "number | null")]
    pub generation: Option<i64>,
    pub dataset_id: Option<String>,
    pub dataset_matches: Option<bool>,
    pub blocker: Option<StorageBlocker>,
}

/// The active backend of the machine that runs this app-server.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct StorageStatus {
    /// Names the machine whose storage this describes, so a client never mistakes its own host
    /// for the server's.
    pub host: String,
    pub active_backend: StorageBackend,
    pub authority: StorageAuthority,
    #[ts(type = "number | null")]
    pub local_generation: Option<i64>,
    pub dataset_id: Option<String>,
    pub remote_ever_activated: bool,
    pub candidate_configured: bool,
    pub remote: Option<StorageRemoteSummary>,
    pub blockers: Vec<StorageBlocker>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct StorageConnectionReport {
    pub stage: StorageCheckStage,
    pub blocker: Option<StorageBlocker>,
    pub schema_format: Option<i32>,
    pub dataset_state: Option<String>,
    pub dataset_id: Option<String>,
    #[ts(type = "number | null")]
    pub generation: Option<i64>,
    pub empty: Option<bool>,
}

/// Row and file counts of a home, read without copying anything.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct StorageSourceEstimate {
    #[ts(type = "number")]
    pub threads: i64,
    #[ts(type = "number")]
    pub sections: i64,
    #[ts(type = "number")]
    pub projects: i64,
    #[ts(type = "number")]
    pub attachments: i64,
    #[ts(type = "number")]
    pub queued_items: i64,
    #[ts(type = "number")]
    pub goals: i64,
    #[ts(type = "number")]
    pub logs: i64,
    #[ts(type = "number")]
    pub memory_outputs: i64,
    #[ts(type = "number")]
    pub board_posts: i64,
    #[ts(type = "number")]
    pub rollout_files: i64,
    #[ts(type = "number")]
    pub rollout_bytes: i64,
}

/// A preview that is either startable or lists exactly what blocks it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct StoragePlan {
    pub plan_id: String,
    pub action: StoragePlanAction,
    /// Changes whenever any fact behind the plan changes.
    pub digest: String,
    pub host: String,
    /// `endpoint:port/database/namespace`; never a credential.
    pub destination: String,
    #[ts(type = "number | null")]
    pub local_generation: Option<i64>,
    pub estimate: Option<StorageSourceEstimate>,
    pub connection: StorageConnectionReport,
    pub blockers: Vec<StorageBlocker>,
    /// Local writers must be stopped before the operation starts.
    pub requires_pause: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct StorageCopiedDomain {
    pub domain: String,
    #[ts(type = "number")]
    pub rows: i64,
}

/// One durable operation as it is stored and reported.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct StorageOperation {
    pub operation_id: String,
    pub action: StoragePlanAction,
    pub plan_digest: String,
    pub state: StorageOperationState,
    pub run_id: Option<String>,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub updated_at: i64,
    pub blocker: Option<StorageBlocker>,
    pub copied: Vec<StorageCopiedDomain>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct StorageStatusParams {
    /// Also connect to the saved remote profile and report the dataset.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub probe_remote: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct StorageStatusResponse {
    pub status: StorageStatus,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct StorageCheckParams {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct StorageCheckResponse {
    pub report: StorageConnectionReport,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct StorageInitializeParams {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct StorageInitializeResponse {
    pub schema_format: i32,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct StoragePlanParams {
    pub action: StoragePlanAction,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct StoragePlanResponse {
    pub plan: StoragePlan,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct StorageStartParams {
    pub action: StoragePlanAction,
    /// The previewed plan; a changed world makes it stale.
    pub plan_id: String,
    /// Names the operation. Repeating a request with the same id never starts a second copy.
    #[ts(optional = nullable)]
    pub operation_id: Option<String>,
    /// The dataset the operator means to join; required for `attach`, and refused if it is not
    /// the dataset the previewed plan names.
    #[ts(optional = nullable)]
    pub dataset_id: Option<String>,
    /// The operator confirms that every process writing this host's Codex home is stopped.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub writers_stopped: bool,
    /// Also make the verified copy authoritative as soon as it is ready.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub activate: bool,
}

/// The operation as it was recorded when the request was accepted. Copying continues on the
/// server; read the operation to follow it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct StorageStartResponse {
    pub operation: StorageOperation,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct StorageActivateParams {
    pub operation_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct StorageActivateResponse {
    pub operation: StorageOperation,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct StorageRecoverParams {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct StorageRecoverResponse {
    pub outcome: StorageRecoveryOutcome,
    pub operations: Vec<StorageOperation>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct StorageCancelParams {
    pub operation_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct StorageCancelResponse {
    pub operation: StorageOperation,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct StorageOperationReadParams {
    pub operation_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct StorageOperationReadResponse {
    pub operation: StorageOperation,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct StorageOperationListParams {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct StorageOperationListResponse {
    pub operations: Vec<StorageOperation>,
}
