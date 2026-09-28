//! Verifies cited memory usage follows the selected versioned store.

use anyhow::Result;
use chrono::DateTime;
use chrono::Utc;
use codex_features::Feature;
use codex_protocol::MemoryVersion;
use codex_protocol::ThreadId;
use codex_protocol::protocol::SessionSource;
use codex_state::Stage1JobClaimOutcome;
use codex_state::StateRuntime;
use codex_state::ThreadMetadataBuilder;
use codex_state::VersionedMemoryStores;
use codex_utils_absolute_path::test_support::PathExt;
use core_test_support::responses;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;
use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;

async fn seed_output(
    db: &StateRuntime,
    home: &Path,
    thread_id: ThreadId,
    updated_at: DateTime<Utc>,
) -> Result<()> {
    let mut builder = ThreadMetadataBuilder::new(
        thread_id,
        home.join(format!("rollout-{thread_id}.jsonl")),
        updated_at,
        SessionSource::Cli,
    );
    builder.cwd = home.to_path_buf();
    let metadata = builder.build("test-provider");
    db.upsert_thread(&metadata).await?;
    db.set_thread_memory_mode(thread_id, "enabled").await?;
    let store = db.memories_for_version(MemoryVersion::V1).await?;
    let Stage1JobClaimOutcome::Claimed { ownership_token } = store
        .try_claim_stage1_job(
            thread_id,
            thread_id,
            updated_at.timestamp(),
            /*lease_seconds*/ 60,
            /*max_running_jobs*/ 2,
        )
        .await?
    else {
        panic!("seed output claim must succeed");
    };
    assert!(
        store
            .mark_stage1_job_succeeded(
                thread_id,
                &ownership_token,
                updated_at.timestamp(),
                "raw memory",
                "rollout summary",
                /*rollout_slug*/ None,
            )
            .await?
    );
    Ok(())
}

async fn selected_thread(store: &codex_state::MemoryStore) -> Result<ThreadId> {
    Ok(store
        .get_phase2_input_selection(/*n*/ 1, /*max_unused_days*/ 30)
        .await?[0]
        .thread_id)
}

#[tokio::test]
async fn assistant_memory_citation_updates_only_injected_store_usage() -> Result<()> {
    let server = start_mock_server().await;
    let home = Arc::new(TempDir::new()?);
    let backend_home = TempDir::new()?;
    let backend = StateRuntime::init(
        codex_state::SqliteConfig::new_for_testing(backend_home.path().abs()),
        "test-provider".to_string(),
    )
    .await?;
    let local = StateRuntime::init_with_memory_stores(
        codex_state::SqliteConfig::new_for_testing(home.path().abs()),
        "test-provider".to_string(),
        VersionedMemoryStores {
            v1: Arc::new(backend.memories_for_version(MemoryVersion::V1).await?),
            v2: Arc::new(backend.memories_for_version(MemoryVersion::V2).await?),
        },
    )
    .await?;
    let now = Utc::now();
    let older = ThreadId::new();
    let newer = ThreadId::new();
    for db in [&backend, &local] {
        seed_output(db, home.path(), older, now - chrono::Duration::days(2)).await?;
        seed_output(db, home.path(), newer, now - chrono::Duration::days(1)).await?;
    }

    let mut builder = test_codex()
        .with_home(Arc::clone(&home))
        .with_state_db(Arc::clone(&local))
        .with_config(|config| {
            config
                .features
                .enable(Feature::Sqlite)
                .expect("test config should allow feature update");
            config.memories.generate_memories = false;
            config.memories.use_memories = false;
            config.memories.version = MemoryVersion::V1;
        });
    let test = builder.build_with_auto_env(&server).await?;
    let backend_store = backend.memories_for_version(MemoryVersion::V1).await?;
    let local_store = local.memories_for_version(MemoryVersion::V1).await?;
    assert_eq!(selected_thread(&backend_store).await?, newer);
    assert_eq!(selected_thread(&local_store).await?, newer);

    let cited_answer = format!(
        "answer<oai-mem-citation><citation_entries>\nMEMORY.md:1-2|note=[cited]\n</citation_entries>\n<rollout_ids>\n{older}\n</rollout_ids></oai-mem-citation>"
    );
    let mock_response = mount_sse_once(
        &server,
        responses::sse(vec![
            ev_response_created("resp-1"),
            ev_assistant_message("msg-1", &cited_answer),
            ev_completed("resp-1"),
        ]),
    )
    .await;
    test.submit_turn("Answer with the cited memory").await?;
    assert_eq!(mock_response.requests().len(), 1);
    assert_eq!(selected_thread(&backend_store).await?, older);
    assert_eq!(selected_thread(&local_store).await?, newer);
    Ok(())
}
