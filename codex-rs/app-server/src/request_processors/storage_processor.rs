use super::*;
use codex_analytics::AppServerRpcTransport;
use codex_app_server_protocol::StorageActivateParams;
use codex_app_server_protocol::StorageActivateResponse;
use codex_app_server_protocol::StorageAuthority;
use codex_app_server_protocol::StorageBackend;
use codex_app_server_protocol::StorageBlocker;
use codex_app_server_protocol::StorageCancelParams;
use codex_app_server_protocol::StorageCancelResponse;
use codex_app_server_protocol::StorageCheckParams;
use codex_app_server_protocol::StorageCheckResponse;
use codex_app_server_protocol::StorageCheckStage;
use codex_app_server_protocol::StorageConnectionReport;
use codex_app_server_protocol::StorageCopiedDomain;
use codex_app_server_protocol::StorageInitializeParams;
use codex_app_server_protocol::StorageInitializeResponse;
use codex_app_server_protocol::StorageOperation;
use codex_app_server_protocol::StorageOperationListParams;
use codex_app_server_protocol::StorageOperationListResponse;
use codex_app_server_protocol::StorageOperationReadParams;
use codex_app_server_protocol::StorageOperationReadResponse;
use codex_app_server_protocol::StorageOperationState;
use codex_app_server_protocol::StoragePlan;
use codex_app_server_protocol::StoragePlanAction;
use codex_app_server_protocol::StoragePlanParams;
use codex_app_server_protocol::StoragePlanResponse;
use codex_app_server_protocol::StorageRecoverParams;
use codex_app_server_protocol::StorageRecoverResponse;
use codex_app_server_protocol::StorageRecoveryOutcome;
use codex_app_server_protocol::StorageRemoteSummary;
use codex_app_server_protocol::StorageSourceEstimate;
use codex_app_server_protocol::StorageStartParams;
use codex_app_server_protocol::StorageStartResponse;
use codex_app_server_protocol::StorageStatus;
use codex_app_server_protocol::StorageStatusParams;
use codex_app_server_protocol::StorageStatusResponse;
use codex_keyring_store::DefaultKeyringStore;
use codex_storage_authority::StorageCandidateProfile;
use codex_storage_service as service;
use codex_storage_service::StorageService;
use codex_storage_service::StorageServiceInputs;
use uuid::Uuid;

/// Serves the `storage/*` methods for the machine this app-server runs on.
///
/// Reads work for every client. Methods that change storage run only for clients that reached
/// the server through its own host (stdio, the in-process transport or a local socket), because
/// the server holds the credentials and the files they would change.
#[derive(Clone)]
pub(crate) struct StorageRequestProcessor {
    config_manager: ConfigManager,
    rpc_transport: AppServerRpcTransport,
}

fn blocker(code: service::BlockerCode) -> StorageBlocker {
    use service::BlockerCode as Code;
    match code {
        Code::NoCandidateProfile => StorageBlocker::NoCandidateProfile,
        Code::MigratorCredentialMissing => StorageBlocker::MigratorCredentialMissing,
        Code::CredentialUnavailable => StorageBlocker::CredentialUnavailable,
        Code::CaCertificateRequired => StorageBlocker::CaCertificateRequired,
        Code::UnsupportedNamespace => StorageBlocker::UnsupportedNamespace,
        Code::ConnectionFailed => StorageBlocker::ConnectionFailed,
        Code::ConnectionTimedOut => StorageBlocker::ConnectionTimedOut,
        Code::SchemaNeedsUpgrade => StorageBlocker::SchemaNeedsUpgrade,
        Code::SchemaTooNew => StorageBlocker::SchemaTooNew,
        Code::SchemaInvalid => StorageBlocker::SchemaInvalid,
        Code::TargetNotEmpty => StorageBlocker::TargetNotEmpty,
        Code::DatasetNotActivated => StorageBlocker::DatasetNotActivated,
        Code::DatasetMismatch => StorageBlocker::DatasetMismatch,
        Code::DatasetMigrating => StorageBlocker::DatasetMigrating,
        Code::DatasetRetired => StorageBlocker::DatasetRetired,
        Code::CutoverInProgress => StorageBlocker::CutoverInProgress,
        Code::AuthorityInvalid => StorageBlocker::AuthorityInvalid,
        Code::AlreadyRemote => StorageBlocker::AlreadyRemote,
        Code::NotRemote => StorageBlocker::NotRemote,
        Code::SqliteHomeDiffersFromCodexHome => StorageBlocker::SqliteHomeDiffersFromCodexHome,
        Code::StalePlan => StorageBlocker::StalePlan,
        Code::NotConfirmed => StorageBlocker::NotConfirmed,
        Code::OperationConflict => StorageBlocker::OperationConflict,
        Code::OperationNotFound => StorageBlocker::OperationNotFound,
        Code::SourceUnreadable => StorageBlocker::SourceUnreadable,
        Code::StagingFailed => StorageBlocker::StagingFailed,
        Code::VerificationFailed => StorageBlocker::VerificationFailed,
        Code::Internal => StorageBlocker::Internal,
    }
}

/// A refusal clients can switch on: the stable code and whether trying again can help.
fn storage_error(code: service::BlockerCode) -> JSONRPCErrorError {
    let mut error = invalid_request(format!("storage operation blocked: {}", code.as_str()));
    error.data = Some(serde_json::json!({
        "blocker": code.as_str(),
        "retryable": code.is_retryable(),
    }));
    error
}

fn storage_error_from(error: service::StorageError) -> JSONRPCErrorError {
    storage_error(error.0)
}

fn admin_required() -> JSONRPCErrorError {
    let mut error = invalid_request("storage changes are only available to clients of this host");
    error.data = Some(serde_json::json!({
        "blocker": "storage_admin_required",
        "retryable": false,
    }));
    error
}

fn parse_id(value: &str) -> Result<Uuid, JSONRPCErrorError> {
    Uuid::parse_str(value).map_err(|_| invalid_params("expected a UUID"))
}

fn backend(value: service::BackendName) -> StorageBackend {
    match value {
        service::BackendName::LocalSqlite => StorageBackend::LocalSqlite,
        service::BackendName::RemotePostgres => StorageBackend::RemotePostgres,
    }
}

fn authority(value: service::AuthorityLabel) -> StorageAuthority {
    match value {
        service::AuthorityLabel::Unmanaged => StorageAuthority::Unmanaged,
        service::AuthorityLabel::Local => StorageAuthority::Local,
        service::AuthorityLabel::Remote => StorageAuthority::Remote,
        service::AuthorityLabel::CutoverInProgress => StorageAuthority::CutoverInProgress,
        service::AuthorityLabel::Invalid => StorageAuthority::Invalid,
    }
}

fn stage(value: service::CheckStage) -> StorageCheckStage {
    match value {
        service::CheckStage::Profile => StorageCheckStage::Profile,
        service::CheckStage::Credential => StorageCheckStage::Credential,
        service::CheckStage::Connect => StorageCheckStage::Connect,
        service::CheckStage::Schema => StorageCheckStage::Schema,
        service::CheckStage::Dataset => StorageCheckStage::Dataset,
        service::CheckStage::Ready => StorageCheckStage::Ready,
    }
}

fn action(value: service::PlanAction) -> StoragePlanAction {
    match value {
        service::PlanAction::Migrate => StoragePlanAction::Migrate,
        service::PlanAction::Return => StoragePlanAction::Return,
    }
}

fn plan_action(value: StoragePlanAction) -> service::PlanAction {
    match value {
        StoragePlanAction::Migrate => service::PlanAction::Migrate,
        StoragePlanAction::Return => service::PlanAction::Return,
    }
}

fn state(value: service::OperationState) -> StorageOperationState {
    match value {
        service::OperationState::Planned => StorageOperationState::Planned,
        service::OperationState::Copying => StorageOperationState::Copying,
        service::OperationState::Verifying => StorageOperationState::Verifying,
        service::OperationState::Ready => StorageOperationState::Ready,
        service::OperationState::Committing => StorageOperationState::Committing,
        service::OperationState::Active => StorageOperationState::Active,
        service::OperationState::Failed => StorageOperationState::Failed,
        service::OperationState::Cancelled => StorageOperationState::Cancelled,
    }
}

fn count(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn connection(report: service::ConnectionReport) -> StorageConnectionReport {
    StorageConnectionReport {
        stage: stage(report.stage),
        blocker: report.blocker.map(blocker),
        schema_format: report.schema_format,
        dataset_state: report.dataset_state,
        dataset_id: report.dataset_id.map(|id| id.to_string()),
        generation: report.generation,
        empty: report.empty,
    }
}

fn operation(record: service::OperationRecord) -> StorageOperation {
    StorageOperation {
        operation_id: record.operation_id.to_string(),
        action: action(record.action),
        plan_digest: record.plan_digest,
        state: state(record.state),
        run_id: record.run_id.map(|id| id.to_string()),
        created_at: record.created_at_ms.div_euclid(1000),
        updated_at: record.updated_at_ms.div_euclid(1000),
        blocker: record.blocker.map(blocker),
        copied: record
            .copied
            .into_iter()
            .map(|(domain, rows)| StorageCopiedDomain {
                domain,
                rows: count(rows),
            })
            .collect(),
    }
}

fn plan(plan: service::StoragePlan) -> StoragePlan {
    StoragePlan {
        plan_id: plan.plan_id.to_string(),
        action: action(plan.action),
        digest: plan.digest,
        host: plan.host,
        destination: plan.destination,
        local_generation: plan.local_generation.map(count),
        estimate: plan.estimate.map(|estimate| StorageSourceEstimate {
            threads: count(estimate.threads),
            sections: count(estimate.sections),
            projects: count(estimate.projects),
            attachments: count(estimate.attachments),
            queued_items: count(estimate.queued_items),
            goals: count(estimate.goals),
            logs: count(estimate.logs),
            memory_outputs: count(estimate.memory_outputs),
            board_posts: count(estimate.board_posts),
            rollout_files: count(estimate.rollout_files),
            rollout_bytes: count(estimate.rollout_bytes),
        }),
        connection: connection(plan.connection),
        blockers: plan.blockers.into_iter().map(blocker).collect(),
        requires_pause: plan.requires_pause,
    }
}

fn host_label() -> String {
    ["COMPUTERNAME", "HOSTNAME"]
        .iter()
        .find_map(|name| std::env::var(name).ok())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

impl StorageRequestProcessor {
    pub(crate) fn new(config_manager: ConfigManager, rpc_transport: AppServerRpcTransport) -> Self {
        Self {
            config_manager,
            rpc_transport,
        }
    }

    /// The service for this host's current configuration. Changing storage is refused for
    /// clients that did not reach the server through its own host.
    async fn service(
        &self,
        changes_storage: bool,
    ) -> Result<Arc<StorageService>, JSONRPCErrorError> {
        if changes_storage && matches!(self.rpc_transport, AppServerRpcTransport::Websocket) {
            return Err(admin_required());
        }
        let config = self
            .config_manager
            .load_latest_config(/*fallback_cwd*/ None)
            .await
            .map_err(|err| internal_error(format!("failed to load config: {err}")))?;
        let candidate = match config
            .config_layer_stack
            .storage_candidate()
            .map_err(|_| invalid_request("the saved storage profile is invalid"))?
        {
            Some(StorageCandidateProfile::RemotePostgres(profile)) => Some(profile),
            Some(StorageCandidateProfile::LocalSqlite) | None => None,
        };
        Ok(Arc::new(StorageService::new(StorageServiceInputs {
            codex_home: config.codex_home.to_path_buf(),
            sqlite: config.sqlite_config().clone(),
            candidate,
            default_model_provider_id: config.model_provider_id.clone(),
            host_label: host_label(),
            keyring: Arc::new(DefaultKeyringStore),
        })))
    }

    pub(crate) async fn status(
        &self,
        params: StorageStatusParams,
    ) -> Result<StorageStatusResponse, JSONRPCErrorError> {
        let service = self.service(/*changes_storage*/ false).await?;
        let status = service.status(params.probe_remote).await;
        Ok(StorageStatusResponse {
            status: StorageStatus {
                host: service.host_label().to_string(),
                active_backend: backend(status.active_backend),
                authority: authority(status.authority),
                local_generation: status.local_generation.map(count),
                dataset_id: status.dataset_id.map(|id| id.to_string()),
                remote_ever_activated: status.remote_ever_activated,
                candidate_configured: status.candidate_configured,
                remote: status.remote.map(|remote| StorageRemoteSummary {
                    reachable: remote.reachable,
                    state: remote.state,
                    generation: remote.generation,
                    dataset_id: remote.dataset_id.map(|id| id.to_string()),
                    dataset_matches: remote.dataset_matches,
                    blocker: remote.blocker.map(blocker),
                }),
                blockers: status.blockers.into_iter().map(blocker).collect(),
            },
        })
    }

    pub(crate) async fn check(
        &self,
        _params: StorageCheckParams,
    ) -> Result<StorageCheckResponse, JSONRPCErrorError> {
        let service = self.service(/*changes_storage*/ false).await?;
        Ok(StorageCheckResponse {
            report: connection(service.check_connection().await),
        })
    }

    pub(crate) async fn initialize(
        &self,
        _params: StorageInitializeParams,
    ) -> Result<StorageInitializeResponse, JSONRPCErrorError> {
        let service = self.service(/*changes_storage*/ true).await?;
        let schema_format = service
            .initialize_schema()
            .await
            .map_err(storage_error_from)?;
        Ok(StorageInitializeResponse { schema_format })
    }

    pub(crate) async fn plan(
        &self,
        params: StoragePlanParams,
    ) -> Result<StoragePlanResponse, JSONRPCErrorError> {
        let service = self.service(/*changes_storage*/ false).await?;
        let built = service
            .plan(plan_action(params.action))
            .await
            .map_err(storage_error_from)?;
        Ok(StoragePlanResponse { plan: plan(built) })
    }

    pub(crate) async fn start(
        &self,
        params: StorageStartParams,
    ) -> Result<StorageStartResponse, JSONRPCErrorError> {
        let service = self.service(/*changes_storage*/ true).await?;
        let plan_id = parse_id(&params.plan_id)?;
        let operation_id = match &params.operation_id {
            Some(value) => parse_id(value)?,
            None => Uuid::new_v4(),
        };
        let confirmation = service::Confirmation {
            writers_stopped: params.writers_stopped,
        };
        let migrate = matches!(params.action, StoragePlanAction::Migrate);
        let (record, created) = if migrate {
            service
                .prepare_migration(operation_id, plan_id, confirmation)
                .await
        } else {
            service
                .prepare_return(operation_id, plan_id, confirmation)
                .await
        }
        .map_err(storage_error_from)?;
        if created {
            let background = Arc::clone(&service);
            let record = record.clone();
            let activate = params.activate;
            // Copying can take a long time: the request is answered now and the operation record
            // carries the progress. Failures are recorded on the operation, not lost.
            tokio::spawn(async move {
                let finished = if migrate {
                    background.run_migration(record).await
                } else {
                    background.run_return(record).await
                };
                if activate
                    && let Ok(ready) = finished
                    && ready.state == service::OperationState::Ready
                {
                    let _ = background.activate(ready.operation_id).await;
                }
            });
        }
        Ok(StorageStartResponse {
            operation: operation(record),
        })
    }

    pub(crate) async fn activate(
        &self,
        params: StorageActivateParams,
    ) -> Result<StorageActivateResponse, JSONRPCErrorError> {
        let service = self.service(/*changes_storage*/ true).await?;
        let record = service
            .activate(parse_id(&params.operation_id)?)
            .await
            .map_err(storage_error_from)?;
        Ok(StorageActivateResponse {
            operation: operation(record),
        })
    }

    pub(crate) async fn recover(
        &self,
        _params: StorageRecoverParams,
    ) -> Result<StorageRecoverResponse, JSONRPCErrorError> {
        let service = self.service(/*changes_storage*/ true).await?;
        let report = service.recover().await.map_err(storage_error_from)?;
        Ok(StorageRecoverResponse {
            outcome: match report.outcome {
                service::RecoveryKind::Idle => StorageRecoveryOutcome::Idle,
                service::RecoveryKind::RolledForward => StorageRecoveryOutcome::RolledForward,
                service::RecoveryKind::RolledBack => StorageRecoveryOutcome::RolledBack,
            },
            operations: report.operations.into_iter().map(operation).collect(),
        })
    }

    pub(crate) async fn cancel(
        &self,
        params: StorageCancelParams,
    ) -> Result<StorageCancelResponse, JSONRPCErrorError> {
        let service = self.service(/*changes_storage*/ true).await?;
        let record = service
            .cancel(parse_id(&params.operation_id)?)
            .await
            .map_err(storage_error_from)?;
        Ok(StorageCancelResponse {
            operation: operation(record),
        })
    }

    pub(crate) async fn operation_read(
        &self,
        params: StorageOperationReadParams,
    ) -> Result<StorageOperationReadResponse, JSONRPCErrorError> {
        let service = self.service(/*changes_storage*/ false).await?;
        let record = service
            .operation_progress(parse_id(&params.operation_id)?)
            .await
            .map_err(storage_error_from)?;
        Ok(StorageOperationReadResponse {
            operation: operation(record),
        })
    }

    pub(crate) async fn operation_list(
        &self,
        _params: StorageOperationListParams,
    ) -> Result<StorageOperationListResponse, JSONRPCErrorError> {
        let service = self.service(/*changes_storage*/ false).await?;
        Ok(StorageOperationListResponse {
            operations: service.operations().into_iter().map(operation).collect(),
        })
    }
}
