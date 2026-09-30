use super::*;
use crate::SqliteConfig;
use crate::Stage1JobClaimOutcome;
use crate::VersionedMemoryStores;
use crate::runtime::test_support::test_thread_metadata;
use codex_utils_absolute_path::test_support::PathExt;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn versioned_injected_stores_receive_writes_deletion_and_reset() -> anyhow::Result<()> {
    let local_home = crate::runtime::test_support::unique_temp_dir();
    let backend_home = crate::runtime::test_support::unique_temp_dir();
    let local_sqlite = SqliteConfig::new_for_testing(local_home.as_path().abs());
    let backend_sqlite = SqliteConfig::new_for_testing(backend_home.as_path().abs());
    let backend = StateRuntime::init(backend_sqlite, "test-provider".to_string()).await?;
    let stores = VersionedMemoryStores {
        v1: Arc::new(backend.memories_for_version(MemoryVersion::V1).await?),
        v2: Arc::new(backend.memories_for_version(MemoryVersion::V2).await?),
    };
    let local =
        StateRuntime::init_with_memory_stores(local_sqlite, "test-provider".to_string(), stores)
            .await?;

    let mut threads = Vec::new();
    for version in [MemoryVersion::V1, MemoryVersion::V2] {
        let thread_id = ThreadId::new();
        let metadata =
            test_thread_metadata(local_home.as_path(), thread_id, local_home.join("project"));
        backend.upsert_thread(&metadata).await?;
        local.upsert_thread(&metadata).await?;
        let store = local.memory_store_for_version(version).await?;
        let Stage1JobClaimOutcome::Claimed { ownership_token } = store
            .try_claim_stage1_job(
                thread_id,
                thread_id,
                metadata.updated_at.timestamp(),
                /*lease_seconds*/ 60,
                /*max_running_jobs*/ 1,
            )
            .await?
        else {
            panic!("injected store must claim the source")
        };
        assert!(
            store
                .mark_stage1_job_succeeded(
                    thread_id,
                    &ownership_token,
                    metadata.updated_at.timestamp(),
                    "raw",
                    version.directory_name(),
                    /*rollout_slug*/ None,
                )
                .await?
        );
        assert!(
            local
                .memories_for_version(version)
                .await?
                .list_stage1_outputs_for_global(/*n*/ 10)
                .await?
                .is_empty()
        );
        let outputs = backend
            .memories_for_version(version)
            .await?
            .list_stage1_outputs_for_global(/*n*/ 10)
            .await?;
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].thread_id, thread_id);
        threads.push(thread_id);
    }

    local.delete_thread(threads[0]).await?;
    assert!(
        backend
            .memories_for_version(MemoryVersion::V1)
            .await?
            .list_stage1_outputs_for_global(/*n*/ 10)
            .await?
            .is_empty()
    );
    assert_eq!(
        backend
            .memories_for_version(MemoryVersion::V2)
            .await?
            .list_stage1_outputs_for_global(/*n*/ 10)
            .await?
            .len(),
        1
    );
    local.clear_all_memory_data().await?;
    assert!(
        backend
            .memories_for_version(MemoryVersion::V2)
            .await?
            .list_stage1_outputs_for_global(/*n*/ 10)
            .await?
            .is_empty()
    );

    local.close().await;
    backend
        .memories_for_version(MemoryVersion::V1)
        .await?
        .list_stage1_outputs_for_global(/*n*/ 10)
        .await?;
    backend.close().await;
    tokio::fs::remove_dir_all(local_home).await?;
    tokio::fs::remove_dir_all(backend_home).await?;
    Ok(())
}
