use super::*;
use codex_postgres_runtime::ConnectionSettings;
use codex_postgres_runtime::PoolLimits;
use codex_postgres_runtime::bootstrap_codex_storage;
use codex_protocol::models::BaseInstructions;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::UserMessageEvent;
use codex_state::SqliteConfig;
use codex_state::StateRuntime;
use codex_thread_store::LocalThreadStore;
use codex_thread_store::LocalThreadStoreConfig;
use codex_thread_store::SortDirection;
use codex_thread_store::ThreadPersistenceMetadata;
use codex_utils_absolute_path::test_support::PathExt;
use pretty_assertions::assert_eq;
use serde_json::Value;

fn settings(state: &Path, role: &str) -> ConnectionSettings {
    let receipt: Value = serde_json::from_slice(
        &std::fs::read(state.join("receipt.json")).expect("read isolated PostgreSQL receipt"),
    )
    .expect("parse PostgreSQL receipt");
    ConnectionSettings {
        host: "localhost".to_string(),
        port: receipt["port"].as_u64().expect("PostgreSQL port") as u16,
        database: "codex".to_string(),
        username: format!("codex_{role}"),
        password: std::fs::read_to_string(state.join(format!("secrets/{role}.password")))
            .expect("read private role credential")
            .trim()
            .to_string()
            .into(),
        ca_certificate: state.join("secrets/ca.crt"),
        limits: PoolLimits {
            connect_timeout: std::time::Duration::from_secs(5),
            acquire_timeout: std::time::Duration::from_secs(20),
            max_connections: 12,
        },
    }
}

async fn connect(state: &Path, role: &str) -> Arc<PostgresPool> {
    Arc::new(
        PostgresPool::connect(settings(state, role))
            .await
            .unwrap_or_else(|error| panic!("{role} pool: {error:?}")),
    )
}

fn create_params(thread_id: ThreadId, provider: &str, cwd: &Path) -> CreateThreadParams {
    CreateThreadParams {
        creator_user_id: None,
        creator_account_id: None,
        session_id: thread_id.into(),
        thread_id,
        extra_config: None,
        forked_from_id: None,
        parent_thread_id: None,
        source: SessionSource::Exec,
        thread_source: None,
        originator: "test_originator".to_string(),
        base_instructions: BaseInstructions::default(),
        dynamic_tools: Vec::new(),
        selected_capability_roots: Vec::new(),
        multi_agent_version: None,
        history_mode: ThreadHistoryMode::Legacy,
        history_base: None,
        subagent_history_start_ordinal: None,
        initial_window_id: "window-1".to_string(),
        runtime_workspace_roots: None,
        metadata: ThreadPersistenceMetadata {
            cwd: Some(cwd.to_path_buf()),
            model_provider: provider.to_string(),
            memory_mode: ThreadMemoryMode::Enabled,
        },
    }
}

fn user_message(message: &str) -> RolloutItem {
    RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
        client_id: None,
        message: message.to_string(),
        images: None,
        local_images: Vec::new(),
        text_elements: Vec::new(),
        ..Default::default()
    }))
}

fn describe_item(item: &RolloutItem) -> String {
    match item {
        RolloutItem::SessionMeta(line) => format!(
            "session_meta {} {:?} {:?} {:?}",
            line.meta.id, line.meta.history_mode, line.meta.source, line.meta.memory_mode
        ),
        other => serde_json::to_string(other).expect("serialize item"),
    }
}

fn describe_thread(thread: &StoredThread) -> String {
    format!(
        "{} preview {:?} name {:?} provider {} model {:?} archived {} section {:?} project {:?} \
         cwd {} source {:?} mode {:?} first {:?} approval {:?} history {}",
        thread.thread_id,
        thread.preview,
        thread.name,
        thread.model_provider,
        thread.model,
        thread.archived_at.is_some(),
        thread.section.as_ref().map(|section| section.name.clone()),
        thread.project_id,
        thread.cwd.display(),
        thread.source,
        thread.history_mode,
        thread.first_user_message,
        thread.approval_mode,
        thread
            .history
            .as_ref()
            .map(|history| history
                .items
                .iter()
                .map(describe_item)
                .collect::<Vec<_>>()
                .join(" | "))
            .unwrap_or_default(),
    )
}

fn outcome<T>(result: ThreadStoreResult<T>, show: impl FnOnce(&T) -> String) -> String {
    match result {
        Ok(value) => format!("ok {}", show(&value)),
        Err(error) => format!("err {error}"),
    }
}

/// Runs a thread lifecycle through the shared trait and records each observable result.
async fn scenario(
    store: &dyn ThreadStore,
    provider: &str,
    cwd: &Path,
    ids: [ThreadId; 3],
) -> Vec<String> {
    let mut log = Vec::new();
    let [first, second, missing] = ids;

    log.push(outcome(
        store
            .create_thread(create_params(first, provider, cwd))
            .await,
        |_| String::new(),
    ));
    log.push(outcome(
        store
            .create_thread(create_params(first, provider, cwd))
            .await,
        |_| String::new(),
    ));
    // Nothing is durable until a barrier, so the thread cannot be read yet.
    log.push(outcome(
        store
            .read_thread(ReadThreadParams {
                thread_id: first,
                include_archived: false,
                include_history: false,
            })
            .await,
        describe_thread,
    ));
    log.push(outcome(
        store
            .append_items(AppendThreadItemsParams {
                thread_id: first,
                items: vec![user_message("hello world")],
            })
            .await,
        |_| String::new(),
    ));
    log.push(outcome(
        store.persist_thread(first, PersistContext::Standard).await,
        |_| String::new(),
    ));
    log.push(outcome(
        store
            .read_thread(ReadThreadParams {
                thread_id: first,
                include_archived: false,
                include_history: true,
            })
            .await,
        describe_thread,
    ));
    log.push(outcome(
        store
            .append_items(AppendThreadItemsParams {
                thread_id: first,
                items: vec![user_message("second turn"), user_message("third turn")],
            })
            .await,
        |_| String::new(),
    ));
    log.push(outcome(store.flush_thread(first).await, |_| String::new()));
    log.push(outcome(
        store
            .update_thread_metadata(UpdateThreadMetadataParams {
                thread_id: first,
                include_archived: false,
                patch: ThreadMetadataPatch {
                    preview: Some("hello world".to_string()),
                    title: Some("hello".to_string()),
                    first_user_message: Some("hello world".to_string()),
                    model: Some("test-model".to_string()),
                    source: Some(SessionSource::Exec),
                    cwd: Some(cwd.to_path_buf()),
                    ..Default::default()
                },
            })
            .await,
        |thread| thread.as_ref().map(describe_thread).unwrap_or_default(),
    ));
    log.push(outcome(
        store
            .update_thread_metadata(UpdateThreadMetadataParams {
                thread_id: missing,
                include_archived: false,
                patch: ThreadMetadataPatch {
                    preview: Some("nope".to_string()),
                    ..Default::default()
                },
            })
            .await,
        |_| String::new(),
    ));
    log.push(outcome(store.shutdown_thread(first).await, |_| {
        String::new()
    }));
    log.push(outcome(store.shutdown_thread(first).await, |_| {
        String::new()
    }));

    // Reopening returns everything that was durable, and a second writer is refused.
    let metadata = ThreadPersistenceMetadata {
        cwd: Some(cwd.to_path_buf()),
        model_provider: provider.to_string(),
        memory_mode: ThreadMemoryMode::Enabled,
    };
    let resume = |include_archived: bool| ResumeThreadParams {
        thread_id: first,
        rollout_path: None,
        history: None,
        history_revision: None,
        include_archived,
        metadata: metadata.clone(),
    };
    log.push(outcome(store.resume_thread(resume(false)).await, |items| {
        items
            .iter()
            .map(describe_item)
            .collect::<Vec<_>>()
            .join(" | ")
    }));
    log.push(outcome(store.resume_thread(resume(false)).await, |_| {
        String::new()
    }));
    log.push(outcome(
        store
            .append_items(AppendThreadItemsParams {
                thread_id: first,
                items: vec![user_message("after resume")],
            })
            .await,
        |_| String::new(),
    ));
    log.push(outcome(store.shutdown_thread(first).await, |_| {
        String::new()
    }));
    log.push(outcome(
        store
            .load_history(LoadThreadHistoryParams {
                thread_id: first,
                include_archived: false,
            })
            .await,
        |history| {
            history
                .items
                .iter()
                .map(describe_item)
                .collect::<Vec<_>>()
                .join(" | ")
        },
    ));
    log.push(outcome(
        store
            .load_history(LoadThreadHistoryParams {
                thread_id: missing,
                include_archived: false,
            })
            .await,
        |_| String::new(),
    ));
    log.push(outcome(
        store
            .load_latest_model_context(LoadThreadHistoryParams {
                thread_id: first,
                include_archived: false,
            })
            .await,
        |context| {
            context
                .items
                .iter()
                .map(describe_item)
                .collect::<Vec<_>>()
                .join(" | ")
        },
    ));

    // A second thread, discarded before it ever becomes durable.
    log.push(outcome(
        store
            .create_thread(create_params(second, provider, cwd))
            .await,
        |_| String::new(),
    ));
    log.push(outcome(store.flush_thread(second).await, |_| String::new()));
    log.push(outcome(store.discard_thread(second).await, |_| {
        String::new()
    }));
    log.push(outcome(store.discard_thread(second).await, |_| {
        String::new()
    }));
    log.push(outcome(
        store
            .read_thread(ReadThreadParams {
                thread_id: second,
                include_archived: false,
                include_history: false,
            })
            .await,
        describe_thread,
    ));

    // Listing, archiving, sections, projects, attachments and deletion.
    let list = |archived: bool| ListThreadsParams {
        page_size: 10,
        cursor: None,
        sort_key: ThreadSortKey::UpdatedAt,
        sort_direction: SortDirection::Desc,
        allowed_sources: Vec::new(),
        model_providers: Some(vec![provider.to_string()]),
        cwd_filters: None,
        section: None,
        project_id: None,
        archived,
        search_term: None,
        relation_filter: None,
        use_state_db_only: true,
    };
    let listing = |page: &ThreadPage| {
        page.items
            .iter()
            .map(describe_thread)
            .collect::<Vec<_>>()
            .join(" || ")
    };
    log.push(outcome(store.list_threads(list(false)).await, listing));
    log.push(outcome(store.list_threads(list(true)).await, listing));
    log.push(outcome(
        store
            .archive_thread(ArchiveThreadParams { thread_id: first })
            .await,
        |_| String::new(),
    ));
    log.push(outcome(
        store
            .archive_thread(ArchiveThreadParams { thread_id: first })
            .await,
        |_| String::new(),
    ));
    log.push(outcome(store.list_threads(list(false)).await, listing));
    log.push(outcome(store.list_threads(list(true)).await, listing));
    log.push(outcome(
        store
            .read_thread(ReadThreadParams {
                thread_id: first,
                include_archived: false,
                include_history: false,
            })
            .await,
        describe_thread,
    ));
    log.push(outcome(
        store
            .read_thread(ReadThreadParams {
                thread_id: first,
                include_archived: true,
                include_history: false,
            })
            .await,
        describe_thread,
    ));
    log.push(outcome(
        store
            .unarchive_thread(ArchiveThreadParams { thread_id: first })
            .await,
        describe_thread,
    ));
    log.push(outcome(
        store
            .unarchive_thread(ArchiveThreadParams { thread_id: first })
            .await,
        describe_thread,
    ));
    log.push(outcome(
        store
            .update_thread_metadata(UpdateThreadMetadataParams {
                thread_id: first,
                include_archived: false,
                patch: ThreadMetadataPatch {
                    name: Some(Some("Renamed thread".to_string())),
                    ..Default::default()
                },
            })
            .await,
        |thread| thread.as_ref().map(describe_thread).unwrap_or_default(),
    ));

    log.push(outcome(
        store
            .add_thread_attachment(AddThreadAttachmentParams {
                thread_id: first,
                attachment_type: "file".to_string(),
                identity_key: "a.txt".to_string(),
                payload: serde_json::json!({"size": 1}),
            })
            .await,
        |outcome| match outcome {
            AddThreadAttachmentOutcome::Created(a) => {
                format!("created {} {}", a.attachment_type, a.identity_key)
            }
            AddThreadAttachmentOutcome::Existing(a) => {
                format!("existing {} {}", a.attachment_type, a.identity_key)
            }
        },
    ));
    log.push(outcome(
        store
            .list_thread_attachments(ListThreadAttachmentsParams {
                thread_id: first,
                cursor: None,
                limit: 10,
            })
            .await,
        |page| format!("{} attachments", page.attachments.len()),
    ));

    log.push(outcome(
        store
            .delete_thread(DeleteThreadParams { thread_id: first })
            .await,
        |_| String::new(),
    ));
    log.push(outcome(
        store
            .delete_thread(DeleteThreadParams { thread_id: first })
            .await,
        |_| String::new(),
    ));
    log.push(outcome(store.list_threads(list(false)).await, listing));
    log
}

#[tokio::test]
async fn real_postgres_thread_store() {
    let Ok(state) = std::env::var("CODEX_TEST_POSTGRES_THREAD_STORE_STATE") else {
        return;
    };
    let state = Path::new(&state);
    bootstrap_codex_storage(&*connect(state, "migrator").await)
        .await
        .expect("bootstrap thread schema");
    let pool = connect(state, "runtime").await;

    let home = tempfile::tempdir().expect("local fixture home");
    let provider = format!(
        "store{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    );
    let cwd = home.path().join("work");
    std::fs::create_dir_all(&cwd).expect("create cwd");
    let sqlite = SqliteConfig::new_for_testing(home.path().abs());
    let state_db = StateRuntime::init(sqlite.clone(), provider.clone())
        .await
        .expect("sqlite runtime");
    let local = LocalThreadStore::new(
        LocalThreadStoreConfig {
            codex_home: home.path().to_path_buf(),
            sqlite,
            default_model_provider_id: provider.clone(),
        },
        Some(state_db),
    );
    let postgres = PostgresThreadStore::new(pool, provider.clone());

    let ids = [ThreadId::new(), ThreadId::new(), ThreadId::new()];
    let expected = scenario(&local, &provider, &cwd, ids).await;
    let actual = scenario(&postgres, &provider, &cwd, ids).await;
    assert_eq!(actual.len(), expected.len());
    // Only the backend name inside a message may differ.
    let normalize = |text: &String| text.replace("live local writer", "live writer");
    let mismatches: Vec<String> = actual
        .iter()
        .zip(&expected)
        .enumerate()
        .filter(|(_, (actual, expected))| normalize(actual) != normalize(expected))
        .map(|(index, (actual, expected))| {
            format!("[{index}]\n  postgres: {actual}\n  local:    {expected}")
        })
        .collect();
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}
