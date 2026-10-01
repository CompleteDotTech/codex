use super::*;
use chrono::TimeZone;
use codex_postgres_runtime::ConnectionSettings;
use codex_postgres_runtime::bootstrap_codex_storage;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_state::SqliteConfig;
use codex_state::StateRuntime;
use codex_state::ThreadMetadataBuilder;
use codex_utils_absolute_path::test_support::PathExt;
use pretty_assertions::assert_eq;
use serde_json::Value;
use sqlx::Row;

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
        limits: codex_postgres_runtime::PoolLimits {
            connect_timeout: Duration::from_secs(5),
            acquire_timeout: Duration::from_secs(20),
            max_connections: 8,
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

/// The same operation on either backend, so one scenario can compare their results.
enum Backend {
    Sqlite(Arc<StateRuntime>),
    Postgres(PostgresThreadCatalog),
}

macro_rules! both {
    ($backend:expr, $method:ident($($arg:expr),*)) => {
        match $backend {
            Backend::Sqlite(runtime) => runtime.$method($($arg),*).await,
            Backend::Postgres(catalog) => catalog.$method($($arg),*).await,
        }
    };
}

fn metadata(
    id: ThreadId,
    index: i64,
    base: DateTime<Utc>,
    source: SessionSource,
) -> ThreadMetadata {
    let created = base + chrono::Duration::seconds(index * 10);
    let mut builder = ThreadMetadataBuilder::new(
        id,
        PathBuf::from(format!("/rollouts/thread-{index}.jsonl")),
        created,
        source,
    );
    builder.updated_at = Some(created + chrono::Duration::seconds(5));
    builder.cwd = PathBuf::from(format!("/work/project-{index}"));
    builder.git_branch = Some("main".to_string());
    let mut metadata = builder.build("provider");
    metadata.title = format!("title {index}");
    metadata.first_user_message = Some(format!("hello {index}"));
    metadata
}

/// Runs the thread operations a caller can observe and records each result.
async fn scenario(backend: &Backend, base: DateTime<Utc>, ids: [ThreadId; 4]) -> Vec<String> {
    let mut log = Vec::new();
    let [parent, child, plain, missing] = ids;
    let parent_metadata = metadata(parent, 0, base, SessionSource::Cli);
    let child_source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id: parent,
        depth: 1,
        agent_path: None,
        agent_nickname: None,
        agent_role: None,
    });
    let child_metadata = metadata(child, 1, base, child_source);
    let plain_metadata = metadata(plain, 2, base, SessionSource::Exec);

    log.push(format!(
        "missing: {:?}",
        both!(backend, get_thread(missing)).expect("get")
    ));
    both!(backend, upsert_thread(&parent_metadata)).expect("upsert parent");
    both!(backend, upsert_thread(&child_metadata)).expect("upsert child");
    log.push(format!(
        "parent: {:?}",
        both!(backend, get_thread(parent)).expect("get")
    ));
    log.push(format!(
        "child: {:?}",
        both!(backend, get_thread(child)).expect("get")
    ));
    log.push(format!(
        "insert if absent: {} then {}",
        both!(backend, insert_thread_if_absent(&plain_metadata)).expect("insert"),
        both!(backend, insert_thread_if_absent(&plain_metadata)).expect("insert again"),
    ));
    log.push(format!(
        "plain: {:?}",
        both!(backend, get_thread(plain)).expect("get")
    ));

    // A later upsert keeps first-seen git fields and origin identity, and ignores empty previews.
    let mut changed = plain_metadata.clone();
    changed.title = "renamed".to_string();
    changed.git_branch = Some("other".to_string());
    changed.git_sha = Some("abc".to_string());
    changed.originator = Some("codex".to_string());
    changed.preview = Some(String::new());
    changed.first_user_message = None;
    changed.tokens_used = 42;
    changed.updated_at += chrono::Duration::seconds(30);
    both!(backend, upsert_thread(&changed)).expect("upsert changed");
    log.push(format!(
        "changed: {:?}",
        both!(backend, get_thread(plain)).expect("get")
    ));

    log.push(format!(
        "daybreak: {}",
        both!(backend, set_thread_daybreak_enabled(plain, true)).expect("daybreak")
    ));
    log.push(format!(
        "memory mode: {:?} {} {:?} {}",
        both!(backend, get_thread_memory_mode(plain)).expect("mode"),
        both!(backend, set_thread_memory_mode(plain, "disabled")).expect("set mode"),
        both!(backend, get_thread_memory_mode(plain)).expect("mode again"),
        both!(backend, set_thread_memory_mode(missing, "disabled")).expect("missing mode"),
    ));
    log.push(format!(
        "title: {} {}",
        both!(backend, update_thread_title(plain, "retitled")).expect("title"),
        both!(backend, update_thread_title(missing, "x")).expect("missing title"),
    ));
    log.push(format!(
        "name: {} {}",
        both!(backend, update_thread_name(plain, Some("named"))).expect("name"),
        both!(backend, update_thread_name(plain, None)).expect("clear name"),
    ));
    log.push(format!(
        "preview: {} {} {} {}",
        both!(backend, set_thread_preview_if_empty(plain, "   ")).expect("blank preview"),
        both!(backend, set_thread_preview_if_empty(plain, " first words ")).expect("preview"),
        both!(backend, set_thread_preview_if_empty(plain, "second")).expect("preview kept"),
        both!(backend, set_thread_preview_if_empty(missing, "x")).expect("missing preview"),
    ));
    log.push(format!(
        "paginated: {} {:?}",
        both!(backend, mark_thread_paginated(plain, Some("legacy name"))).expect("paginate"),
        both!(backend, get_thread(plain))
            .expect("get")
            .map(|thread| (thread.name, thread.history_mode)),
    ));
    let sanitized = SanitizedGitUrl::try_from("https://example.com/repo.git".to_string()).ok();
    log.push(format!(
        "git: {} {} {} {:?}",
        both!(
            backend,
            update_thread_git_info(plain, Some(Some("def")), None, None)
        )
        .expect("sha"),
        both!(
            backend,
            update_thread_git_info(plain, None, Some(None), sanitized.as_ref().map(Some))
        )
        .expect("branch"),
        both!(
            backend,
            update_thread_git_info(missing, Some(None), None, None)
        )
        .expect("missing git"),
        both!(backend, get_thread(plain))
            .expect("get")
            .map(|thread| (thread.git_sha, thread.git_branch, thread.git_origin_url)),
    ));

    // Timestamps stay unique and ordered inside a hot second, and old ones pass through.
    let hot = base + chrono::Duration::seconds(500);
    for offset_ms in [0, 0, 400, 2000, -5000] {
        let at = hot + chrono::Duration::milliseconds(offset_ms);
        both!(backend, touch_thread_updated_at(plain, at)).expect("touch updated");
        both!(backend, touch_thread_recency_at(plain, at)).expect("touch recency");
        let thread = both!(backend, get_thread(plain))
            .expect("get")
            .expect("thread");
        log.push(format!(
            "touch {offset_ms}: {} {}",
            thread.updated_at.timestamp_millis() - hot.timestamp_millis(),
            thread.recency_at.timestamp_millis() - hot.timestamp_millis()
        ));
    }

    log.push(format!(
        "paths: {:?} {:?} {:?} {:?}",
        both!(backend, find_rollout_path_by_id(plain, None)).expect("path"),
        both!(backend, find_rollout_path_by_id(plain, Some(true))).expect("archived path"),
        both!(backend, find_rollout_path_by_id(plain, Some(false))).expect("active path"),
        both!(backend, find_rollout_path_by_id(missing, None)).expect("missing path"),
    ));
    let moved = PathBuf::from("/rollouts/moved.jsonl");
    log.push(format!(
        "replace: {} {}",
        both!(
            backend,
            replace_rollout_path_if_current(plain, Path::new("/wrong"), &moved)
        )
        .expect("wrong"),
        both!(
            backend,
            replace_rollout_path_if_current(plain, Path::new("/rollouts/thread-2.jsonl"), &moved)
        )
        .expect("replace"),
    ));
    let archived_path = PathBuf::from("/rollouts/archived/thread-2.jsonl");
    both!(
        backend,
        mark_archived(plain, &archived_path, base + chrono::Duration::seconds(900))
    )
    .expect("archive");
    both!(backend, mark_archived(missing, &archived_path, base)).expect("archive missing");
    log.push(format!(
        "archived: {:?} {:?} {:?}",
        both!(backend, get_thread(plain))
            .expect("get")
            .map(|thread| (thread.archived_at, thread.rollout_path)),
        both!(backend, find_rollout_path_by_id(plain, Some(true))).expect("archived path"),
        both!(backend, find_rollout_path_by_id(plain, Some(false))).expect("active path"),
    ));
    both!(backend, mark_unarchived(plain, &moved)).expect("unarchive");
    log.push(format!(
        "unarchived: {:?}",
        both!(backend, get_thread(plain))
            .expect("get")
            .map(|thread| (thread.archived_at, thread.rollout_path)),
    ));

    log.push(format!(
        "delete: {} {} {}",
        both!(backend, delete_thread(plain)).expect("delete"),
        both!(backend, delete_thread(plain)).expect("delete again"),
        both!(backend, delete_threads_strict(&[])).expect("delete none"),
    ));
    log.push(format!(
        "after delete: {:?}",
        both!(backend, get_thread(plain)).expect("get")
    ));
    log.push(format!(
        "delete subtree: {}",
        both!(backend, delete_threads_strict(&[parent, child])).expect("delete subtree")
    ));
    log
}

async fn setup(state: &Path) -> Arc<PostgresPool> {
    bootstrap_codex_storage(&*connect(state, "migrator").await)
        .await
        .expect("bootstrap thread schema");
    connect(state, "runtime").await
}

/// Later runs and the SQLite process-local mark start from the same point, so allocation
/// behaves identically: the base is always ahead of anything an earlier run allocated.
fn run_base() -> DateTime<Utc> {
    Utc.timestamp_opt(Utc::now().timestamp() + 10 * 86_400, 0)
        .single()
        .expect("base time")
}

async fn real_postgres_threads_match_sqlite() {
    let Ok(state) = std::env::var("CODEX_TEST_POSTGRES_THREAD_CATALOG_STATE") else {
        return;
    };
    let pool = setup(Path::new(&state)).await;
    let home = tempfile::tempdir().expect("sqlite fixture home");
    let sqlite = StateRuntime::init(
        SqliteConfig::new_for_testing(home.path().abs()),
        "provider".to_string(),
    )
    .await
    .expect("sqlite runtime");
    let base = run_base();
    let ids = [
        ThreadId::new(),
        ThreadId::new(),
        ThreadId::new(),
        ThreadId::new(),
    ];
    let expected = scenario(&Backend::Sqlite(sqlite), base, ids).await;
    let actual = scenario(
        &Backend::Postgres(PostgresThreadCatalog::new(pool)),
        base,
        ids,
    )
    .await;
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(&expected) {
        assert_eq!(actual, expected);
    }
}

async fn real_postgres_delete_removes_thread_state_and_keeps_queue_changes_visible() {
    let Ok(state) = std::env::var("CODEX_TEST_POSTGRES_THREAD_CATALOG_STATE") else {
        return;
    };
    let pool = setup(Path::new(&state)).await;
    let catalog = PostgresThreadCatalog::new(pool.clone());
    let thread_id = ThreadId::new();
    let id = thread_id.to_string();
    let base = run_base() + chrono::Duration::seconds(5_000);
    catalog
        .upsert_thread(&metadata(thread_id, 0, base, SessionSource::Cli))
        .await
        .expect("upsert thread");

    let mut connection = pool.acquire().await.expect("runtime connection");
    let revision_before: i64 =
        sqlx::query_scalar("SELECT version FROM codex_storage.queue_change_counter")
            .fetch_one(&mut *connection)
            .await
            .expect("queue version");
    sqlx::query(
        "INSERT INTO codex_storage.queued_items (id, thread_id, payload_json, queue_order, \
         created_at_ms, updated_at_ms) VALUES ($1, $2::uuid, '{}', 0, 1, 1)",
    )
    .bind(format!("item-{id}"))
    .bind(&id)
    .execute(&mut *connection)
    .await
    .expect("queue item");
    sqlx::query(
        "INSERT INTO codex_storage.logs (id, ts, ts_nanos, level, target, thread_id, estimated_bytes) \
         SELECT last_id + 1, 1, 0, 'INFO', 'test', $1, 1 FROM codex_storage.log_id_counter",
    )
    .bind(&id)
    .execute(&mut *connection)
    .await
    .expect("log row");
    sqlx::query("UPDATE codex_storage.log_id_counter SET last_id = last_id + 1")
        .execute(&mut *connection)
        .await
        .expect("advance log counter");
    sqlx::query(
        "INSERT INTO codex_storage.thread_goals (thread_id, goal_id, objective, status, \
         tokens_used, time_used_seconds, created_at_ms, updated_at_ms) \
         VALUES ($1::uuid, 'goal', 'finish', 'active', 0, 0, 1, 1)",
    )
    .bind(&id)
    .execute(&mut *connection)
    .await
    .expect("goal row");
    sqlx::query(
        "INSERT INTO codex_storage.memory_stage1_outputs (thread_id, source_updated_at, \
         raw_memory, rollout_summary, generated_at, selected_for_phase2) \
         VALUES ($1::uuid, 1, 'raw', 'summary', 1, 1)",
    )
    .bind(&id)
    .execute(&mut *connection)
    .await
    .expect("memory output");
    drop(connection);

    assert_eq!(catalog.delete_thread(thread_id).await.expect("delete"), 1);

    let mut connection = pool.acquire().await.expect("runtime connection");
    for (label, sql) in [
        (
            "logs",
            "SELECT COUNT(*) FROM codex_storage.logs WHERE thread_id = $1",
        ),
        (
            "queue",
            "SELECT COUNT(*) FROM codex_storage.queued_items WHERE thread_id = $1::uuid",
        ),
        (
            "goals",
            "SELECT COUNT(*) FROM codex_storage.thread_goals WHERE thread_id = $1::uuid",
        ),
        (
            "memory",
            "SELECT COUNT(*) FROM codex_storage.memory_stage1_outputs WHERE thread_id = $1::uuid",
        ),
        (
            "thread",
            "SELECT COUNT(*) FROM codex_storage.threads WHERE id = $1::uuid",
        ),
    ] {
        let count: i64 = sqlx::query_scalar(sql)
            .bind(&id)
            .fetch_one(&mut *connection)
            .await
            .expect("count");
        assert_eq!((label, count), (label, 0));
    }
    // The queue change survives the thread row, so a watcher that already saw an older
    // revision still learns the queue was removed.
    let revision = sqlx::query(
        "SELECT revision FROM codex_storage.queued_thread_revisions WHERE thread_id = $1::uuid",
    )
    .bind(&id)
    .fetch_one(&mut *connection)
    .await
    .expect("revision row")
    .try_get::<i64, _>("revision")
    .expect("revision");
    assert!(revision > revision_before);
    // Removing generated memory that fed the last consolidation queues another one.
    let pending: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM codex_storage.memory_jobs \
         WHERE kind = 'memory_consolidate_global' AND status IN ('pending', 'running')",
    )
    .fetch_one(&mut *connection)
    .await
    .expect("consolidation job");
    assert_eq!(pending, 1);
}

/// The checks share the namespace-wide timestamp marks, so they run one after another.
#[tokio::test]
async fn real_postgres_thread_catalog() {
    real_postgres_threads_match_sqlite().await;
    real_postgres_delete_removes_thread_state_and_keeps_queue_changes_visible().await;
}
