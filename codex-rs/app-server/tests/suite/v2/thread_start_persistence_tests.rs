//! A failed persist-on-start request must not leave a live thread behind.

use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use app_test_support::create_mock_responses_server_repeating_assistant;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadLoadedListParams;
use codex_app_server_protocol::ThreadLoadedListResponse;
use codex_app_server_protocol::ThreadStartParams;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::time::timeout;

const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::test]
async fn failed_start_persistence_removes_loaded_thread_and_allows_retry() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri()).write(codex_home.path())?;
    let mut client = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;

    let sessions = codex_home.path().join("sessions");
    std::fs::write(&sessions, "block rollout directory creation")?;
    let request_id = client
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            persist_on_start: true,
            ..Default::default()
        })
        .await?;
    let response = timeout(
        READ_TIMEOUT,
        client.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_eq!(response.error.code, -32603);
    assert!(response.error.message.contains("failed to persist thread"));

    let request_id = client
        .send_thread_loaded_list_request(ThreadLoadedListParams::default())
        .await?;
    let loaded: ThreadLoadedListResponse =
        timeout(READ_TIMEOUT, client.read_response(request_id)).await??;
    assert_eq!(
        loaded,
        ThreadLoadedListResponse {
            data: Vec::new(),
            next_cursor: None,
        }
    );

    std::fs::remove_file(sessions)?;
    let started = client
        .start_thread(ThreadStartParams {
            persist_on_start: true,
            ..Default::default()
        })
        .await?;
    assert!(started.persisted_on_start);
    assert!(started.thread.path.expect("durable rollout path").is_file());
    Ok(())
}
