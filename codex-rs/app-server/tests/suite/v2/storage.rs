use anyhow::Result;
use app_test_support::TestAppServer;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::StorageAuthority;
use codex_app_server_protocol::StorageBackend;
use codex_app_server_protocol::StorageBlocker;
use codex_app_server_protocol::StorageCheckParams;
use codex_app_server_protocol::StorageCheckResponse;
use codex_app_server_protocol::StorageCheckStage;
use codex_app_server_protocol::StorageOperationListParams;
use codex_app_server_protocol::StorageOperationListResponse;
use codex_app_server_protocol::StoragePlanAction;
use codex_app_server_protocol::StoragePlanParams;
use codex_app_server_protocol::StoragePlanResponse;
use codex_app_server_protocol::StorageStatusParams;
use codex_app_server_protocol::StorageStatusResponse;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::Duration;
use tokio::time::timeout;

const RPC_TIMEOUT: Duration = Duration::from_secs(10);

/// An error response names the blocker in a field clients can switch on.
async fn blocker_of(
    app_server: &mut TestAppServer,
    method: &str,
    params: serde_json::Value,
) -> Result<(String, bool)> {
    let request_id = app_server.send_raw_request(method, Some(params)).await?;
    let error = timeout(
        RPC_TIMEOUT,
        app_server.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;
    let data = error.error.data.expect("storage errors carry data");
    Ok((
        data["blocker"].as_str().expect("blocker code").to_string(),
        data["retryable"].as_bool().expect("retryable flag"),
    ))
}

#[tokio::test]
async fn storage_methods_describe_a_local_home_and_refuse_unconfirmed_changes() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;

    // A home that predates storage authority is plainly local and has nothing to block it.
    let status: StorageStatusResponse = app_server
        .request(|request_id| ClientRequest::StorageStatus {
            request_id,
            params: StorageStatusParams { probe_remote: true },
        })
        .await?;
    assert_eq!(status.status.active_backend, StorageBackend::LocalSqlite);
    assert_eq!(status.status.authority, StorageAuthority::Unmanaged);
    assert!(!status.status.candidate_configured);
    assert_eq!(status.status.remote, None);
    assert!(!status.status.host.is_empty());

    // Without a saved profile the connection test and the plan stop at the profile.
    let check: StorageCheckResponse = app_server
        .request(|request_id| ClientRequest::StorageCheck {
            request_id,
            params: StorageCheckParams {},
        })
        .await?;
    assert_eq!(check.report.stage, StorageCheckStage::Profile);
    assert_eq!(check.report.blocker, Some(StorageBlocker::NoCandidateProfile));
    let plan: StoragePlanResponse = app_server
        .request(|request_id| ClientRequest::StoragePlan {
            request_id,
            params: StoragePlanParams {
                action: StoragePlanAction::Migrate,
            },
        })
        .await?;
    assert!(plan.plan.blockers.contains(&StorageBlocker::NoCandidateProfile));
    assert!(plan.plan.requires_pause);

    // Starting needs the operator's promise, and the refusal is a stable code.
    let (blocker, retryable) = blocker_of(
        &mut app_server,
        "storage/start",
        json!({
            "action": "migrate",
            "planId": plan.plan.plan_id,
            "writersStopped": false,
        }),
    )
    .await?;
    assert_eq!((blocker.as_str(), retryable), ("not_confirmed", false));
    let (blocker, _) = blocker_of(
        &mut app_server,
        "storage/operation/read",
        json!({"operationId": "0194e0a0-0000-7000-8000-000000000001"}),
    )
    .await?;
    assert_eq!(blocker, "operation_not_found");

    // Nothing was recorded, and there is nothing to recover.
    let operations: StorageOperationListResponse = app_server
        .request(|request_id| ClientRequest::StorageOperationList {
            request_id,
            params: StorageOperationListParams {},
        })
        .await?;
    assert_eq!(operations.operations, Vec::new());
    let (blocker, _) = blocker_of(&mut app_server, "storage/recover", json!({})).await?;
    assert_eq!(blocker, "no_candidate_profile");
    Ok(())
}
