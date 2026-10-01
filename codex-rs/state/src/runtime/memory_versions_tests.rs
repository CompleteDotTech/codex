use super::*;
use crate::Phase2JobClaimOutcome;
use crate::SqliteConfig;
use crate::Stage1JobClaimOutcome;
use crate::runtime::test_support::test_thread_metadata;
use codex_utils_absolute_path::test_support::PathExt;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn versions_isolate_jobs_outputs_and_reset_without_losing_threads() -> anyhow::Result<()> {
    let home = crate::runtime::test_support::unique_temp_dir();
    let sqlite = SqliteConfig::new_for_testing(home.as_path().abs());
    let db = StateRuntime::init(sqlite.clone(), "test-provider".to_string()).await?;
    assert!(!sqlite.memories_v2_db_path().exists());
    let thread_id = ThreadId::new();
    let metadata = test_thread_metadata(home.as_path(), thread_id, home.as_path().join("project"));
    db.upsert_thread(&metadata).await?;

    for version in [MemoryVersion::V1, MemoryVersion::V2] {
        let store = db.memories_for_version(version).await?;
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
            panic!("each version must independently claim the same source")
        };
        assert!(
            store
                .mark_stage1_job_succeeded(
                    thread_id,
                    &ownership_token,
                    metadata.updated_at.timestamp(),
                    "raw",
                    version.directory_name(),
                    /*rollout_slug*/ None
                )
                .await?
        );
    }
    for version in [MemoryVersion::V1, MemoryVersion::V2] {
        let outputs = db
            .memories_for_version(version)
            .await?
            .list_stage1_outputs_for_global(/*n*/ 10)
            .await?;
        assert_eq!(
            outputs
                .iter()
                .map(|output| output.rollout_summary.as_str())
                .collect::<Vec<_>>(),
            vec![version.directory_name()]
        );
    }
    // Reopening under v1 must still clear existing v2 state.
    db.close().await;
    let db = StateRuntime::init(sqlite.clone(), "test-provider".to_string()).await?;
    db.delete_thread(thread_id).await?;
    for version in [MemoryVersion::V1, MemoryVersion::V2] {
        assert!(
            db.memories_for_version(version)
                .await?
                .list_stage1_outputs_for_global(/*n*/ 10)
                .await?
                .is_empty()
        );
    }
    db.upsert_thread(&metadata).await?;
    for version in [MemoryVersion::V1, MemoryVersion::V2] {
        let store = db.memories_for_version(version).await?;
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
            panic!("deletion must clear the job watermark")
        };
        store
            .mark_stage1_job_succeeded(
                thread_id,
                &ownership_token,
                metadata.updated_at.timestamp(),
                "raw",
                "summary",
                /*rollout_slug*/ None,
            )
            .await?;
    }
    db.clear_all_memory_data().await?;
    for version in [MemoryVersion::V1, MemoryVersion::V2] {
        assert!(
            db.memories_for_version(version)
                .await?
                .list_stage1_outputs_for_global(/*n*/ 10)
                .await?
                .is_empty()
        );
    }
    assert!(db.get_thread(thread_id).await?.is_some());
    db.close().await;
    assert!(StateRuntime::clear_memory_data_in_sqlite_home(&sqlite).await?);
    tokio::fs::remove_dir_all(home).await?;
    Ok(())
}

#[tokio::test]
async fn polluting_v2_thread_enqueues_v2_forgetting() -> anyhow::Result<()> {
    let home = crate::runtime::test_support::unique_temp_dir();
    let sqlite = SqliteConfig::new_for_testing(home.as_path().abs());
    let db = StateRuntime::init(sqlite.clone(), "test-provider".to_string()).await?;
    let thread_id = ThreadId::new();
    let metadata = test_thread_metadata(home.as_path(), thread_id, home.join("project"));
    db.upsert_thread(&metadata).await?;
    let v2 = db.memories_for_version(MemoryVersion::V2).await?;
    let Stage1JobClaimOutcome::Claimed { ownership_token } = v2
        .try_claim_stage1_job(
            thread_id,
            thread_id,
            metadata.updated_at.timestamp(),
            /*lease_seconds*/ 60,
            /*max_running_jobs*/ 1,
        )
        .await?
    else {
        panic!("expected v2 stage one claim");
    };
    assert!(
        v2.mark_stage1_job_succeeded(
            thread_id,
            &ownership_token,
            metadata.updated_at.timestamp(),
            "",
            "rollout summary",
            /*rollout_slug*/ None,
        )
        .await?
    );
    let outputs = v2.list_stage1_outputs_for_global(/*n*/ 10).await?;
    let Phase2JobClaimOutcome::Claimed {
        ownership_token,
        input_watermark,
    } = v2
        .try_claim_global_phase2_job(thread_id, /*lease_seconds*/ 60)
        .await?
    else {
        panic!("expected v2 phase two claim");
    };
    assert!(
        v2.mark_global_phase2_job_succeeded(&ownership_token, input_watermark, &outputs)
            .await?
    );
    let v2_pool = sqlite.open_memories_v2_db().await?;
    let status: String = sqlx::query_scalar(
        "SELECT status FROM jobs WHERE kind = 'memory_consolidate_global' AND job_key = 'global'",
    )
    .fetch_one(&v2_pool)
    .await?;
    assert_eq!(status, "done");

    assert!(
        db.mark_thread_memory_mode_polluted_for_version(MemoryVersion::V2, thread_id)
            .await?
    );
    assert_eq!(
        db.get_thread_memory_mode(thread_id).await?.as_deref(),
        Some("polluted")
    );
    let status: String = sqlx::query_scalar(
        "SELECT status FROM jobs WHERE kind = 'memory_consolidate_global' AND job_key = 'global'",
    )
    .fetch_one(&v2_pool)
    .await?;
    assert_eq!(status, "pending");
    db.close().await;
    v2_pool.close().await;
    tokio::fs::remove_dir_all(home).await?;
    Ok(())
}
