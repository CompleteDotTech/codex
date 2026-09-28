//! Exercises compression through local-store ownership, including detached recorder work.

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::SystemTime;

use chrono::Utc;
use codex_protocol::ThreadId;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::ThreadMemoryMode;
use codex_protocol::protocol::UserMessageEvent;
use codex_rollout::ARCHIVED_SESSIONS_SUBDIR;
use codex_rollout::RolloutItem;
use codex_rollout::RolloutRecorder;
use codex_rollout::WriterLockCoordinator;
use codex_rollout::append_rollout_item_to_path;
use codex_state::ThreadMetadataBuilder;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

use super::LocalThreadStore;
use super::test_support::test_config;
use super::test_support::write_archived_session_file;
use super::test_support::write_session_file;
use crate::ArchiveThreadParams;
use crate::ReadThreadParams;
use crate::ResumeThreadParams;
use crate::ThreadMetadataPatch;
use crate::ThreadPersistenceMetadata;
use crate::ThreadStore;
use crate::ThreadStoreError;
use crate::UpdateThreadMetadataParams;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn fixture() -> TestResult<(TempDir, ThreadId, PathBuf)> {
    let home = TempDir::new()?;
    let id = Uuid::new_v4();
    let path = write_session_file(home.path(), "2025-01-03T12-00-00", id)?;
    Ok((home, ThreadId::from_string(&id.to_string())?, path))
}

fn age(path: &Path) -> TestResult<()> {
    fs::OpenOptions::new().write(true).open(path)?.set_times(
        fs::FileTimes::new().set_modified(SystemTime::now() - Duration::from_secs(8 * 86400)),
    )?;
    Ok(())
}

fn resume(thread_id: ThreadId, path: &Path, home: &Path) -> ResumeThreadParams {
    ResumeThreadParams {
        thread_id,
        rollout_path: Some(path.to_path_buf()),
        history: None,
        include_archived: true,
        metadata: ThreadPersistenceMetadata {
            cwd: Some(home.to_path_buf()),
            model_provider: "test-provider".into(),
            memory_mode: ThreadMemoryMode::Enabled,
        },
    }
}

fn message() -> RolloutItem {
    RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
        message: "acknowledged write".into(),
        ..Default::default()
    }))
}

fn write_divergent_sibling(path: &Path) -> TestResult<(PathBuf, Vec<u8>, Vec<u8>)> {
    let plain_bytes = fs::read(path)?;
    let plain_text = String::from_utf8(plain_bytes.clone())?;
    assert!(plain_text.contains("Hello from user") || plain_text.contains("Archived user message"));
    let divergent = plain_text
        .replace("Hello from user", "different user message")
        .replace("Archived user message", "different archived message");
    let compressed_bytes = zstd::stream::encode_all(divergent.as_bytes(), /*level*/ 1)?;
    let compressed_path = path.with_extension("jsonl.zst");
    fs::write(&compressed_path, &compressed_bytes)?;
    Ok((compressed_path, plain_bytes, compressed_bytes))
}

#[tokio::test]
async fn divergent_siblings_refuse_append_and_resume_without_changing_files() -> TestResult<()> {
    let (home, thread_id, plain_path) = fixture()?;
    let (compressed_path, plain_bytes, compressed_bytes) = write_divergent_sibling(&plain_path)?;

    let append_error = append_rollout_item_to_path(&plain_path, &message())
        .await
        .expect_err("ambiguous rollout must block append");
    assert_eq!(append_error.kind(), std::io::ErrorKind::AlreadyExists);

    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
    let resume_error = store
        .resume_thread(resume(thread_id, &plain_path, home.path()))
        .await
        .expect_err("ambiguous rollout must block resume");
    assert!(matches!(resume_error, ThreadStoreError::Conflict { .. }));
    assert_eq!(fs::read(plain_path)?, plain_bytes);
    assert_eq!(fs::read(compressed_path)?, compressed_bytes);
    Ok(())
}

#[tokio::test]
async fn divergent_siblings_refuse_archive_without_changing_files_or_sqlite() -> TestResult<()> {
    let (home, thread_id, plain_path) = fixture()?;
    let (compressed_path, plain_bytes, compressed_bytes) = write_divergent_sibling(&plain_path)?;
    let config = test_config(home.path());
    let runtime = codex_state::StateRuntime::init(
        config.sqlite.clone(),
        config.default_model_provider_id.clone(),
    )
    .await?;
    let mut builder = ThreadMetadataBuilder::new(
        thread_id,
        plain_path.clone(),
        Utc::now(),
        SessionSource::Cli,
    );
    builder.cwd = home.path().to_path_buf();
    runtime
        .upsert_thread(&builder.build(config.default_model_provider_id.as_str()))
        .await?;
    let before = runtime.get_thread(thread_id).await?;
    let store = LocalThreadStore::new(config, Some(runtime.clone()));

    let error = store
        .archive_thread(ArchiveThreadParams { thread_id })
        .await
        .expect_err("ambiguous rollout must block archive");
    assert!(matches!(error, ThreadStoreError::Conflict { .. }));
    assert_eq!(fs::read(plain_path)?, plain_bytes);
    assert_eq!(fs::read(compressed_path)?, compressed_bytes);
    assert_eq!(runtime.get_thread(thread_id).await?, before);
    Ok(())
}

#[tokio::test]
async fn divergent_siblings_refuse_unarchive_without_changing_files_or_sqlite() -> TestResult<()> {
    let home = TempDir::new()?;
    let uuid = Uuid::new_v4();
    let thread_id = ThreadId::from_string(&uuid.to_string())?;
    let plain_path = write_archived_session_file(home.path(), "2025-01-03T12-00-00", uuid)?;
    let (compressed_path, plain_bytes, compressed_bytes) = write_divergent_sibling(&plain_path)?;
    let config = test_config(home.path());
    let runtime = codex_state::StateRuntime::init(
        config.sqlite.clone(),
        config.default_model_provider_id.clone(),
    )
    .await?;
    let mut builder = ThreadMetadataBuilder::new(
        thread_id,
        plain_path.clone(),
        Utc::now(),
        SessionSource::Cli,
    );
    builder.cwd = home.path().to_path_buf();
    let mut metadata = builder.build(config.default_model_provider_id.as_str());
    metadata.archived_at = Some(metadata.updated_at);
    runtime.upsert_thread(&metadata).await?;
    let before = runtime.get_thread(thread_id).await?;
    let store = LocalThreadStore::new(config, Some(runtime.clone()));

    let error = store
        .unarchive_thread(ArchiveThreadParams { thread_id })
        .await
        .expect_err("ambiguous rollout must block unarchive");
    assert!(matches!(error, ThreadStoreError::Conflict { .. }));
    assert_eq!(fs::read(plain_path)?, plain_bytes);
    assert_eq!(fs::read(compressed_path)?, compressed_bytes);
    assert_eq!(runtime.get_thread(thread_id).await?, before);
    Ok(())
}

#[tokio::test]
async fn compressed_rollout_survives_archive_and_unarchive() -> TestResult<()> {
    let (home, thread_id, plain_path) = fixture()?;
    let compressed_path = plain_path.with_extension("jsonl.zst");
    let original = fs::read(&plain_path)?;
    let compressed_bytes = zstd::stream::encode_all(original.as_slice(), /*level*/ 1)?;
    fs::write(&compressed_path, &compressed_bytes)?;
    fs::remove_file(&plain_path)?;

    let config = test_config(home.path());
    let runtime = codex_state::StateRuntime::init(
        config.sqlite.clone(),
        config.default_model_provider_id.clone(),
    )
    .await?;
    let mut builder = ThreadMetadataBuilder::new(
        thread_id,
        plain_path.clone(),
        Utc::now(),
        SessionSource::Cli,
    );
    builder.cwd = home.path().to_path_buf();
    runtime
        .upsert_thread(&builder.build(config.default_model_provider_id.as_str()))
        .await?;
    let store = LocalThreadStore::new(config, Some(runtime.clone()));
    let before = store
        .read_thread(ReadThreadParams {
            thread_id,
            include_archived: false,
            include_history: true,
        })
        .await?;
    let expected_history = serde_json::to_value(before.history)?;

    store
        .archive_thread(ArchiveThreadParams { thread_id })
        .await?;
    let archived_path = home
        .path()
        .join(ARCHIVED_SESSIONS_SUBDIR)
        .join(compressed_path.file_name().expect("compressed filename"));
    assert!(!compressed_path.exists());
    assert_eq!(fs::read(&archived_path)?, compressed_bytes);
    let archived_metadata = runtime
        .get_thread(thread_id)
        .await?
        .expect("archived metadata");
    assert_eq!(archived_metadata.rollout_path, archived_path);
    assert!(archived_metadata.archived_at.is_some());
    let archived = store
        .read_thread(ReadThreadParams {
            thread_id,
            include_archived: true,
            include_history: true,
        })
        .await?;
    assert_eq!(archived.thread_id, thread_id);
    assert_eq!(serde_json::to_value(archived.history)?, expected_history);

    store
        .unarchive_thread(ArchiveThreadParams { thread_id })
        .await?;
    assert!(!archived_path.exists());
    assert_eq!(fs::read(&compressed_path)?, compressed_bytes);
    assert!(!plain_path.exists());
    let restored_metadata = runtime
        .get_thread(thread_id)
        .await?
        .expect("restored metadata");
    assert_eq!(restored_metadata.rollout_path, compressed_path);
    assert_eq!(restored_metadata.archived_at, None);
    let restored = store
        .read_thread(ReadThreadParams {
            thread_id,
            include_archived: false,
            include_history: true,
        })
        .await?;
    assert_eq!(restored.thread_id, thread_id);
    assert_eq!(serde_json::to_value(restored.history)?, expected_history);
    Ok(())
}

async fn wait_until(ready: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("background file work completes");
}

async fn compress(home: &Path) -> TestResult<()> {
    let marker = home.join(".tmp/rollout-compression.lock");
    if marker.exists() {
        fs::remove_file(&marker)?;
    }
    codex_rollout::spawn_rollout_compression_worker(
        home.to_path_buf(),
        codex_rollout::RolloutCompressionTrigger::Startup,
    );
    // The marker proves startup; the maintenance lock proves every blocking job finished.
    wait_until(|| {
        marker.exists()
            && codex_rollout::try_acquire_rollout_maintenance_lock(home)
                .unwrap()
                .is_some()
    })
    .await;
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn idle_recorder_retains_stable_thread_ownership_after_store_drop() -> TestResult<()> {
    let (home, thread_id, old_path) = fixture()?;
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
    let path = old_path.with_file_name(format!(
        "rollout-2025-01-03T12-00-00-{thread_id}_{}.jsonl",
        Uuid::new_v4()
    ));
    fs::rename(old_path, &path)?;
    let cold = write_session_file(home.path(), "2025-01-03T12-00-01", Uuid::new_v4())?;
    age(&cold)?;
    store
        .resume_thread(resume(thread_id, &path, home.path()))
        .await?;
    let recorder = store
        .live_recorders
        .lock()
        .await
        .get(&thread_id)
        .unwrap()
        .recorder
        .clone();
    drop(store);
    age(&path)?;
    let (mut expected, _, _) = RolloutRecorder::load_rollout_items(&path).await?;
    compress(home.path()).await?;
    assert!(path.exists());
    assert!(!path.with_extension("jsonl.zst").exists());
    assert!(cold.with_extension("jsonl.zst").exists());

    recorder.record_canonical_items(&[message()]).await?;
    drop(recorder);
    let locks = Arc::new(WriterLockCoordinator::new(home.path()));
    // No yield since enqueueing: only the background task can own the pending write now.
    assert!(
        matches!(locks.acquire(thread_id), Err(err) if err.kind() == std::io::ErrorKind::WouldBlock)
    );
    wait_until(|| locks.acquire(thread_id).is_ok()).await;
    age(&path)?;
    compress(home.path()).await?;
    assert!(!path.exists());
    expected.push(message());
    let (actual, _, _) = RolloutRecorder::load_rollout_items(&path).await?;
    assert_eq!(json!(actual), json!(expected));
    Ok(())
}

#[tokio::test]
async fn metadata_updates_share_ownership_and_resume_compressed_rollouts() -> TestResult<()> {
    let (home, thread_id, path) = fixture()?;
    let config = test_config(home.path());
    let db = codex_state::StateRuntime::init(
        config.sqlite.clone(),
        config.default_model_provider_id.clone(),
    )
    .await?;
    let owner = LocalThreadStore::new(config.clone(), Some(db.clone()));
    owner
        .resume_thread(resume(thread_id, &path, home.path()))
        .await?;
    let competitor = LocalThreadStore::new(config, Some(db.clone()));
    let mut patch = UpdateThreadMetadataParams {
        thread_id,
        include_archived: true,
        patch: ThreadMetadataPatch {
            memory_mode: Some(ThreadMemoryMode::Enabled),
            ..Default::default()
        },
    };
    owner.update_thread_metadata(patch.clone()).await?;
    let original = fs::read(&path)?;
    let indexed_mode = db.get_thread_memory_mode(thread_id).await?;
    patch.patch.memory_mode = Some(ThreadMemoryMode::Disabled);
    let error = competitor
        .update_thread_metadata(patch.clone())
        .await
        .unwrap_err();
    assert!(matches!(error, crate::ThreadStoreError::Conflict { .. }));
    assert_eq!(fs::read(&path)?, original);
    assert_eq!(db.get_thread_memory_mode(thread_id).await?, indexed_mode);
    owner.update_thread_metadata(patch.clone()).await?;
    owner.shutdown_thread(thread_id).await?;
    let (items, _, _) = RolloutRecorder::load_rollout_items(&path).await?;
    let mut expected = codex_rollout::read_session_meta_line(&path).await?;
    expected.meta.memory_mode = Some("disabled".into());
    expected.git = None;
    assert_eq!(
        json!(items.last()),
        json!(Some(RolloutItem::SessionMeta(expected.clone())))
    );

    age(&path)?;
    compress(home.path()).await?;
    patch.patch.memory_mode = Some(ThreadMemoryMode::Enabled);
    competitor.update_thread_metadata(patch).await?;
    expected.meta.memory_mode = Some("enabled".into());
    let (items, _, _) = RolloutRecorder::load_rollout_items(&path).await?;
    assert_eq!(
        json!(items.last()),
        json!(Some(RolloutItem::SessionMeta(expected)))
    );
    Ok(())
}
