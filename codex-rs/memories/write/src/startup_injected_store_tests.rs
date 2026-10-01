//! Exercises the inactive memory backend seam through the real startup agents.

use super::*;
use codex_state::VersionedMemoryStores;
use core_test_support::responses::sse_response;
use pretty_assertions::assert_eq;
use wiremock::Mock;
use wiremock::matchers::method;
use wiremock::matchers::path_regex;

#[tokio::test]
async fn injected_memory_store_receives_phase_one_and_phase_two_agent_writes() -> anyhow::Result<()>
{
    let server = start_mock_server().await;
    let home = Arc::new(TempDir::new()?);
    let backend_home = Arc::new(TempDir::new()?);
    let backend = init_state_db(&backend_home).await?;
    let stores = VersionedMemoryStores {
        v1: Arc::new(
            backend
                .memories_for_version(codex_protocol::MemoryVersion::V1)
                .await?,
        ),
        v2: Arc::new(
            backend
                .memories_for_version(codex_protocol::MemoryVersion::V2)
                .await?,
        ),
    };
    let local = codex_state::StateRuntime::init_with_memory_stores(
        codex_state::SqliteConfig::new_for_testing(home.path().abs()),
        "test-provider".to_string(),
        stores,
    )
    .await?;
    local
        .mark_backfill_complete(/*last_watermark*/ None)
        .await?;

    let memories = startup_test_memories_config();
    let test = test_codex()
        .with_home(Arc::clone(&home))
        .with_state_db(Arc::clone(&local))
        .with_config(move |config| {
            config
                .features
                .enable(Feature::Sqlite)
                .expect("test config should allow feature update");
            config.memories = memories;
        })
        .build(&server)
        .await?;
    let source = seed_stage1_candidate(
        local.as_ref(),
        home.path(),
        chrono::Utc::now() - chrono::Duration::hours(2),
        "injected",
    )
    .await?;
    let metadata = local.get_thread(source).await?.expect("seeded source");
    backend.upsert_thread(&metadata).await?;
    backend.set_thread_memory_mode(source, "enabled").await?;

    let memory_root = home.path().join("memories");
    seed_required_memory_artifacts(&memory_root).await?;
    Mock::given(method("POST"))
        .and(path_regex(".*/responses$"))
        .respond_with(|request: &wiremock::Request| {
            let body: serde_json::Value = request.body_json().expect("response request");
            let output = if body["text"]["format"]["schema"]["properties"]
                .get("raw_memory")
                .is_some()
            {
                r#"{"raw_memory":"injected fact","rollout_summary":"injected rollout","rollout_slug":"injected"}"#
            } else {
                "consolidation complete"
            };
            sse_response(sse(vec![
                ev_response_created("response"),
                ev_assistant_message("message", output),
                ev_completed("response"),
            ]))
        })
        .mount(&server)
        .await;

    trigger_memories_startup(&test).await;
    let backend_store = backend
        .memories_for_version(codex_protocol::MemoryVersion::V1)
        .await?;
    let deadline = Instant::now() + Duration::from_secs(60);
    while backend_store.max_consolidated_thread_count().await? == 0 {
        anyhow::ensure!(
            Instant::now() < deadline,
            "injected phase two did not complete"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let outputs = backend_store
        .list_stage1_outputs_for_global(/*n*/ 10)
        .await?;
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].thread_id, source);
    assert_eq!(outputs[0].raw_memory, "injected fact");
    assert_eq!(outputs[0].rollout_summary, "injected rollout");
    assert!(
        local
            .memories_for_version(codex_protocol::MemoryVersion::V1)
            .await?
            .list_stage1_outputs_for_global(/*n*/ 10)
            .await?
            .is_empty()
    );
    assert_eq!(
        local
            .memories_for_version(codex_protocol::MemoryVersion::V1)
            .await?
            .max_consolidated_thread_count()
            .await?,
        0
    );
    let summaries = read_rollout_summary_bodies(&memory_root).await?;
    assert_eq!(summaries.len(), 1);
    assert!(summaries[0].contains("injected rollout"));
    let requests = server.received_requests().await.expect("recorded requests");
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.url.path().ends_with("/responses"))
            .count(),
        2
    );

    shutdown_test_codex(&test).await?;
    Ok(())
}
