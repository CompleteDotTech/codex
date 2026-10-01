use super::*;
use chrono::TimeZone;
use codex_postgres_runtime::ConnectionSettings;
use codex_postgres_runtime::bootstrap_codex_storage;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_state::AddThreadAttachmentOutcome;
use codex_state::Anchor;
use codex_state::MAX_THREAD_ATTACHMENTS_PER_THREAD;
use codex_state::PINNED_THREAD_SECTION_ID;
use codex_state::Project;
use codex_state::ProjectRoot;
use codex_state::ProjectSortKey;
use codex_state::SortDirection;
use codex_state::SortKey;
use codex_state::SqliteConfig;
use codex_state::StateRuntime;
use codex_state::ThreadAttachment;
use codex_state::ThreadFilterOptions;
use codex_state::ThreadMetadataBuilder;
use codex_state::ThreadRelationFilter;
use codex_state::ThreadSectionAppearance;
use codex_utils_absolute_path::test_support::PathExt;
use pretty_assertions::assert_eq;
use serde_json::Value;
use sqlx::Row;
use std::collections::BTreeMap;
use std::collections::HashMap;
use uuid::Uuid;

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

/// Times for one run, placed well above every mark an earlier run stored. A fresh SQLite
/// process starts its marks at zero, so PostgreSQL must see the same "newer than anything
/// allocated" situation for both backends to allocate identically.
async fn run_base(pool: &PostgresPool) -> DateTime<Utc> {
    let mut connection = pool.acquire().await.expect("runtime connection");
    let stored_ms: i64 = sqlx::query_scalar(
        "SELECT GREATEST(updated_at_ms, recency_at_ms) FROM codex_storage.thread_timestamp_marks",
    )
    .fetch_one(&mut *connection)
    .await
    .expect("stored marks");
    let floor = Utc::now().timestamp() + 10 * 86_400;
    Utc.timestamp_opt(floor.max(stored_ms / 1000 + 200_000), 0)
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
    let base = run_base(&pool).await;
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
    let base = run_base(&pool).await + chrono::Duration::seconds(5_000);
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

/// Threads for the listing scenario. Titles are unique, so results compare by title.
fn listing_fixture(token: &str, base: DateTime<Utc>, ids: &[ThreadId]) -> Vec<ThreadMetadata> {
    let sources = [
        SessionSource::Cli,
        SessionSource::Exec,
        SessionSource::VSCode,
        SessionSource::Custom("custom".to_string()),
    ];
    let mut threads = Vec::new();
    for (index, id) in ids.iter().enumerate() {
        let number = index as i64;
        let created = base + chrono::Duration::seconds(number * 7);
        let mut builder = ThreadMetadataBuilder::new(
            *id,
            PathBuf::from(format!("/rollouts/list-{number}.jsonl")),
            created,
            sources[index % sources.len()].clone(),
        );
        builder.updated_at = Some(created + chrono::Duration::seconds(100 - number * 3));
        // Several threads share one old recency time, so ties fall back to the thread id.
        builder.recency_at = Some(if index % 3 == 0 {
            base + chrono::Duration::seconds(1)
        } else {
            created + chrono::Duration::seconds(2)
        });
        builder.cwd = PathBuf::from(format!("/work/area-{}", index % 3));
        builder.model_provider = Some(format!(
            "{token}-{}",
            if index % 2 == 0 { "alpha" } else { "beta" }
        ));
        let mut metadata = builder.build("provider");
        metadata.title = format!("title {number}");
        metadata.preview = (index % 5 != 4).then(|| format!("preview words {number}"));
        metadata.first_user_message = None;
        // Positions on some threads exercise NULL placement when sorting by position.
        metadata.section_position = (index % 4 != 0).then_some(1_000 + (number % 5) * 10);
        threads.push(metadata);
    }
    threads
}

fn titles(threads: &[ThreadMetadata]) -> Vec<String> {
    threads.iter().map(|thread| thread.title.clone()).collect()
}

/// Lists every page of a query and records each page, so ordering and cursors both compare.
async fn page_through(
    backend: &Backend,
    label: &str,
    page_size: usize,
    filters: ThreadFilterOptions<'_>,
    log: &mut Vec<String>,
) {
    let mut anchor: Option<Anchor> = None;
    for page_number in 0..12 {
        let page = both!(
            backend,
            list_threads(
                page_size,
                ThreadFilterOptions {
                    anchor: anchor.as_ref(),
                    ..filters
                }
            )
        )
        .expect("list threads");
        log.push(format!(
            "{label} page {page_number}: {:?} scanned {} next {:?}",
            titles(&page.items),
            page.num_scanned_rows,
            page.next_anchor
                .as_ref()
                .map(|next| (next.ts, next.id.is_some())),
        ));
        anchor = page.next_anchor;
        if anchor.is_none() {
            break;
        }
    }
}

async fn listing_scenario(
    backend: &Backend,
    token: &str,
    base: DateTime<Utc>,
    ids: &[ThreadId],
) -> Vec<String> {
    let mut log = Vec::new();
    let fixture = listing_fixture(token, base, ids);
    // Every query is scoped to this run's providers, so rows from earlier runs cannot leak in.
    let alpha = format!("{token}-alpha");
    let all_providers = vec![alpha.clone(), format!("{token}-beta")];
    // A later thread raises the shared recency mark first, so the older tied times pass through.
    let mut newest = metadata(
        ThreadId::new(),
        99,
        base + chrono::Duration::seconds(5_000),
        SessionSource::Cli,
    );
    newest.title = "newest".to_string();
    newest.preview = Some("newest preview".to_string());
    newest.model_provider = alpha.clone();
    let newest_id = newest.id;
    both!(backend, upsert_thread(&newest)).expect("upsert newest");
    for thread in &fixture {
        both!(backend, upsert_thread(thread)).expect("upsert listed thread");
    }
    // The first threads are archived.
    for thread in fixture.iter().take(2) {
        both!(
            backend,
            mark_archived(
                thread.id,
                &thread.rollout_path,
                base + chrono::Duration::seconds(9_000)
            )
        )
        .expect("archive");
    }
    let sources: Vec<String> = Vec::new();
    let exec_only = vec![fixture[1].source.clone()];
    let providers = vec![alpha.clone()];
    let cwds = vec![PathBuf::from("/work/area-1")];
    let two_cwds = vec![PathBuf::from("/work/area-0"), PathBuf::from("/work/area-2")];
    let no_cwds: Vec<PathBuf> = Vec::new();
    let defaults = ThreadFilterOptions {
        archived_only: false,
        allowed_sources: &sources,
        model_providers: Some(&all_providers),
        cwd_filters: None,
        section: None,
        project_id: None,
        anchor: None,
        sort_key: SortKey::UpdatedAt,
        sort_direction: SortDirection::Desc,
        search_term: None,
    };
    for sort_key in [
        SortKey::CreatedAt,
        SortKey::UpdatedAt,
        SortKey::RecencyAt,
        SortKey::SectionPosition,
    ] {
        for sort_direction in [SortDirection::Desc, SortDirection::Asc] {
            page_through(
                backend,
                &format!("{sort_key:?} {sort_direction:?}"),
                4,
                ThreadFilterOptions {
                    sort_key,
                    sort_direction,
                    ..defaults
                },
                &mut log,
            )
            .await;
        }
    }
    page_through(
        backend,
        "archived",
        3,
        ThreadFilterOptions {
            archived_only: true,
            ..defaults
        },
        &mut log,
    )
    .await;
    page_through(
        backend,
        "exec",
        3,
        ThreadFilterOptions {
            allowed_sources: &exec_only,
            ..defaults
        },
        &mut log,
    )
    .await;
    page_through(
        backend,
        "provider",
        3,
        ThreadFilterOptions {
            model_providers: Some(&providers),
            ..defaults
        },
        &mut log,
    )
    .await;
    page_through(
        backend,
        "cwd",
        3,
        ThreadFilterOptions {
            cwd_filters: Some(&cwds),
            ..defaults
        },
        &mut log,
    )
    .await;
    page_through(
        backend,
        "two cwds",
        5,
        ThreadFilterOptions {
            cwd_filters: Some(&two_cwds),
            ..defaults
        },
        &mut log,
    )
    .await;
    page_through(
        backend,
        "no cwds",
        5,
        ThreadFilterOptions {
            cwd_filters: Some(&no_cwds),
            ..defaults
        },
        &mut log,
    )
    .await;
    page_through(
        backend,
        "search title",
        5,
        ThreadFilterOptions {
            search_term: Some("title 1"),
            ..defaults
        },
        &mut log,
    )
    .await;
    page_through(
        backend,
        "search preview",
        5,
        ThreadFilterOptions {
            search_term: Some("words 3"),
            ..defaults
        },
        &mut log,
    )
    .await;
    page_through(
        backend,
        "search none",
        5,
        ThreadFilterOptions {
            search_term: Some("nothing matches"),
            ..defaults
        },
        &mut log,
    )
    .await;
    page_through(
        backend,
        "no section",
        5,
        ThreadFilterOptions {
            section: Some(None),
            ..defaults
        },
        &mut log,
    )
    .await;
    page_through(
        backend,
        "no project",
        5,
        ThreadFilterOptions {
            project_id: Some(None),
            ..defaults
        },
        &mut log,
    )
    .await;
    page_through(
        backend,
        "unknown project",
        5,
        ThreadFilterOptions {
            project_id: Some(Some("missing")),
            ..defaults
        },
        &mut log,
    )
    .await;

    for sort_key in [
        SortKey::CreatedAt,
        SortKey::RecencyAt,
        SortKey::SectionPosition,
    ] {
        let listed = both!(
            backend,
            list_thread_ids(6, None, sort_key, &sources, Some(&all_providers), false)
        )
        .expect("list ids");
        let mut names = Vec::new();
        for id in listed {
            names.push(
                both!(backend, get_thread(id))
                    .expect("get")
                    .map(|thread| thread.title),
            );
        }
        log.push(format!("ids {sort_key:?}: {names:?}"));
    }
    for (title, archived_only, cwd) in [
        ("title 3", false, None),
        ("title 0", false, None),
        ("title 0", true, None),
        ("title 3", false, Some(PathBuf::from("/work/area-0"))),
        ("title 3", false, Some(PathBuf::from("/work/area-2"))),
        ("absent", false, None),
    ] {
        log.push(format!(
            "exact {title} {archived_only} {cwd:?}: {:?}",
            both!(
                backend,
                find_thread_by_exact_title(
                    title,
                    &sources,
                    Some(&all_providers),
                    archived_only,
                    cwd.as_deref()
                )
            )
            .expect("find")
            .map(|thread| thread.id == newest_id)
        ));
    }

    // Relations: children recorded from a spawn source, and everything below a root.
    let parent_id = fixture[2].id;
    let mut child = metadata(
        ThreadId::new(),
        50,
        base + chrono::Duration::seconds(6_000),
        SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id: parent_id,
            depth: 1,
            agent_path: None,
            agent_nickname: None,
            agent_role: None,
        }),
    );
    child.title = "child".to_string();
    child.model_provider = alpha.clone();
    let mut grandchild = metadata(
        ThreadId::new(),
        51,
        base + chrono::Duration::seconds(6_100),
        SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id: child.id,
            depth: 2,
            agent_path: None,
            agent_nickname: None,
            agent_role: None,
        }),
    );
    grandchild.title = "grandchild".to_string();
    grandchild.model_provider = alpha.clone();
    both!(backend, upsert_thread(&child)).expect("upsert child");
    both!(backend, upsert_thread(&grandchild)).expect("upsert grandchild");
    for (label, page) in [
        (
            "children",
            both!(backend, list_threads_by_parent(5, parent_id, defaults)).expect("children"),
        ),
        (
            "descendants",
            both!(
                backend,
                list_threads_by_relation(
                    5,
                    ThreadRelationFilter::DescendantsOf(parent_id),
                    defaults
                )
            )
            .expect("descendants"),
        ),
        (
            "descendants of child",
            both!(
                backend,
                list_threads_by_relation(
                    1,
                    ThreadRelationFilter::DescendantsOf(child.id),
                    defaults
                )
            )
            .expect("descendants of child"),
        ),
    ] {
        let mut parents: Vec<(String, String)> = page
            .parent_thread_ids
            .iter()
            .map(|(thread, parent)| (thread.to_string(), parent.to_string()))
            .collect();
        parents.sort();
        log.push(format!(
            "{label}: {:?} parents {} next {:?}",
            titles(&page.items),
            parents.len(),
            page.next_anchor
                .as_ref()
                .map(|next| (next.ts, next.id.is_some()))
        ));
    }
    log
}

async fn real_postgres_listing_matches_sqlite() {
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
    let token = format!(
        "list{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    );
    let base = run_base(&pool).await + chrono::Duration::seconds(20_000);
    let ids: Vec<ThreadId> = (0..14).map(|_| ThreadId::new()).collect();
    let expected = listing_scenario(&Backend::Sqlite(sqlite), &token, base, &ids).await;
    let actual = listing_scenario(
        &Backend::Postgres(PostgresThreadCatalog::new(pool)),
        &token,
        base,
        &ids,
    )
    .await;
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(&expected) {
        assert_eq!(actual, expected);
    }
}

/// Replaces generated project ids with labels, so backends that mint their own ids compare.
struct ProjectLabels(Vec<String>);

impl ProjectLabels {
    fn text(&self, text: String) -> String {
        let mut text = text;
        for (index, id) in self.0.iter().enumerate() {
            text = text.replace(id, &format!("project-{index}"));
        }
        text
    }

    fn show(&self, project: &Project) -> String {
        self.text(format!(
            "{} {:?} {:?} {:?} position {} recency {:?}",
            project.id,
            project.name,
            project
                .roots
                .iter()
                .map(|root| root.path.as_str())
                .collect::<Vec<_>>(),
            project.metadata,
            project.position,
            project.recency_at_ms,
        ))
    }
}

fn roots(paths: &[&str]) -> Vec<ProjectRoot> {
    paths
        .iter()
        .map(|path| ProjectRoot {
            path: (*path).to_string(),
        })
        .collect()
}

fn attributes(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

async fn project_scenario(
    backend: &Backend,
    token: &str,
    base: DateTime<Utc>,
    ids: &[ThreadId],
) -> Vec<String> {
    let mut log = Vec::new();
    let mut labels = ProjectLabels(Vec::new());
    // Idempotency keys are never deleted, so each run mints its own.
    let key = |name: &str| format!("{token}-{name}");
    let ghost = ids[6];

    // Start from an empty project table: leftovers from earlier runs would share the positions.
    loop {
        let page = both!(
            backend,
            list_projects(None, 50, ProjectSortKey::Position, SortDirection::Asc)
        )
        .expect("list leftovers");
        if page.projects.is_empty() {
            break;
        }
        for project in page.projects {
            both!(backend, delete_project(&project.id)).expect("delete leftover");
        }
    }

    for (index, id) in ids.iter().take(6).enumerate() {
        let mut thread = metadata(*id, index as i64, base, SessionSource::Cli);
        thread.model_provider = format!("{token}-projects");
        thread.title = format!("member {index}");
        both!(backend, upsert_thread(&thread)).expect("upsert member");
    }
    // The last member is archived, so it never counts toward project recency.
    both!(
        backend,
        mark_archived(ids[5], Path::new("/rollouts/archived-member.jsonl"), base)
    )
    .expect("archive member");
    let strings: Vec<String> = ids.iter().map(ToString::to_string).collect();

    let created = both!(
        backend,
        create_project(
            "alpha".to_string(),
            roots(&["/a", "/b"]),
            attributes(&[("kind", "one")]),
            &strings[0..2],
            &key("alpha")
        )
    )
    .expect("create alpha");
    labels.0.push(created.project.id.clone());
    log.push(format!(
        "created alpha: {} {}",
        created.created,
        labels.show(&created.project)
    ));
    let repeated = both!(
        backend,
        create_project(
            "ignored".to_string(),
            Vec::new(),
            BTreeMap::new(),
            &[],
            &key("alpha")
        )
    )
    .expect("repeat alpha");
    log.push(format!(
        "repeated alpha: {} {}",
        repeated.created,
        labels.show(&repeated.project)
    ));
    log.push(format!(
        "unknown member: {}",
        both!(
            backend,
            create_project(
                "ghost".to_string(),
                Vec::new(),
                BTreeMap::new(),
                &[ghost.to_string()],
                &key("ghost")
            )
        )
        .map(|created| created.created.to_string())
        .unwrap_or_else(|error| error.to_string())
    ));
    log.push(format!(
        "invalid member: {}",
        both!(
            backend,
            create_project(
                "ghost".to_string(),
                Vec::new(),
                BTreeMap::new(),
                &["not-a-thread".to_string()],
                &key("invalid")
            )
        )
        .map(|created| created.created.to_string())
        .unwrap_or_else(|error| error.to_string())
    ));
    for (name, paths, member_range, name_key) in [
        ("beta", &["/c"][..], 2..3, "beta"),
        ("gamma", &[][..], 3..6, "gamma"),
        ("delta", &["/d", "/e", "/f"][..], 0..0, "delta"),
    ] {
        let created = both!(
            backend,
            create_project(
                name.to_string(),
                roots(paths),
                BTreeMap::new(),
                &strings[member_range],
                &key(name_key)
            )
        )
        .expect("create project");
        labels.0.push(created.project.id.clone());
        log.push(format!("created {name}: {}", labels.show(&created.project)));
    }
    let alpha = labels.0[0].clone();
    let beta = labels.0[1].clone();
    let gamma = labels.0[2].clone();

    log.push(format!(
        "get: {:?} {:?}",
        both!(backend, get_project(&alpha))
            .expect("get")
            .map(|p| labels.show(&p)),
        both!(backend, get_project("missing"))
            .expect("get missing")
            .map(|p| labels.show(&p)),
    ));
    log.push(format!(
        "by key: {:?} {:?}",
        both!(backend, get_project_by_idempotency_key(&key("beta")))
            .expect("by key")
            .map(|p| labels.show(&p)),
        both!(backend, get_project_by_idempotency_key(&key("none")))
            .expect("by missing key")
            .map(|p| labels.show(&p)),
    ));

    for (label, name, new_roots, new_metadata) in [
        ("rename", Some("alpha renamed".to_string()), None, None),
        ("same", Some("alpha renamed".to_string()), None, None),
        ("roots", None, Some(roots(&["/z"])), None),
        (
            "metadata",
            None,
            None,
            Some(attributes(&[("kind", "two"), ("extra", "yes")])),
        ),
        ("clear roots", None, Some(Vec::new()), None),
    ] {
        let updated = both!(
            backend,
            update_project(&alpha, name, new_roots, new_metadata)
        )
        .expect("update");
        log.push(format!(
            "update {label}: {:?}",
            updated.map(|(project, changed)| (labels.show(&project), changed))
        ));
    }
    log.push(format!(
        "update missing: {:?}",
        both!(
            backend,
            update_project("missing", Some("x".to_string()), None, None)
        )
        .expect("update missing")
        .map(|(project, changed)| (project.id, changed))
    ));

    for (label, moved, before) in [
        ("to front", &gamma, Some(&alpha)),
        ("to end", &gamma, None),
        ("no-op", &gamma, None),
        ("before beta", &alpha, Some(&beta)),
        ("missing", &"missing".to_string(), None),
        ("before itself", &alpha, Some(&alpha)),
        ("before unknown", &alpha, Some(&"unknown".to_string())),
    ] {
        log.push(format!(
            "move {label}: {}",
            labels.text(format!(
                "{:?}",
                both!(backend, move_project(moved, before.map(String::as_str)))
                    .map_err(|error| error.to_string())
            ))
        ));
    }

    for (sort_key, sort_direction) in [
        (ProjectSortKey::Position, SortDirection::Asc),
        (ProjectSortKey::Position, SortDirection::Desc),
        (ProjectSortKey::RecencyAt, SortDirection::Asc),
        (ProjectSortKey::RecencyAt, SortDirection::Desc),
    ] {
        let mut cursor: Option<String> = None;
        for page_number in 0..6 {
            let page = both!(
                backend,
                list_projects(cursor.as_deref(), 2, sort_key, sort_direction)
            )
            .expect("list projects");
            log.push(format!(
                "list {sort_key:?} {sort_direction:?} page {page_number}: {:?} next {:?}",
                page.projects
                    .iter()
                    .map(|p| labels.show(p))
                    .collect::<Vec<_>>(),
                page.next_cursor.clone().map(|cursor| labels.text(cursor)),
            ));
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
    }
    for cursor in [
        "garbage",
        "v1|position|asc|1|x",
        "5|not-a-uuid",
        "v1|recencyAt|desc|7",
    ] {
        log.push(format!(
            "cursor {cursor}: {}",
            both!(
                backend,
                list_projects(
                    Some(cursor),
                    2,
                    ProjectSortKey::Position,
                    SortDirection::Asc
                )
            )
            .map(|_| "accepted".to_string())
            .unwrap_or_else(|error| error.to_string())
        ));
    }

    // Assignments report the previous project and reject unknown projects.
    log.push(
        format!(
            "assign: {:?} {:?} {:?} {:?} {:?}",
            both!(backend, set_thread_project(&strings[5], Some(&beta))).expect("assign"),
            both!(backend, set_thread_project(&strings[5], Some(&gamma))).expect("reassign"),
            both!(backend, set_thread_project(&strings[5], None)).expect("clear"),
            both!(backend, set_thread_project("not-a-thread", None)).expect("invalid thread"),
            both!(backend, set_thread_project(&strings[5], Some("missing")))
                .map_err(|error| error.to_string()),
        )
        .replace(&beta, "beta")
        .replace(&gamma, "gamma"),
    );

    // Deleting a project unassigns its threads and reports which were active or archived.
    log.push(format!(
        "delete: {:?} {:?}",
        both!(backend, delete_project(&gamma)).expect("delete"),
        both!(backend, delete_project(&gamma)).expect("delete again"),
    ));
    log.push(format!(
        "after delete: {:?} {:?}",
        both!(backend, get_thread(ids[3]))
            .expect("get member")
            .map(|thread| thread.project_id),
        both!(backend, get_project_by_idempotency_key(&key("gamma")))
            .map(|project| project.map(|p| labels.show(&p)))
            .map_err(|error| error.to_string()),
    ));
    log
}

async fn real_postgres_projects_match_sqlite() {
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
    let token = format!(
        "proj{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    );
    let base = run_base(&pool).await + chrono::Duration::seconds(40_000);
    let ids: Vec<ThreadId> = (0..7).map(|_| ThreadId::new()).collect();
    let expected = project_scenario(&Backend::Sqlite(sqlite), &token, base, &ids).await;
    let actual = project_scenario(
        &Backend::Postgres(PostgresThreadCatalog::new(pool)),
        &token,
        base,
        &ids,
    )
    .await;
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(&expected) {
        assert_eq!(actual, expected);
    }
}

/// Replaces generated section and attachment ids with labels, so each backend can mint its own.
struct IdLabels(Vec<String>);

impl IdLabels {
    fn text(&mut self, text: String) -> String {
        let mut text = text;
        for (index, id) in self.0.iter().enumerate() {
            text = text.replace(id, &format!("id-{index}"));
        }
        text
    }

    fn add(&mut self, id: &str) {
        if !self.0.iter().any(|known| known == id) {
            self.0.push(id.to_string());
        }
    }
}

async fn section_scenario(
    backend: &Backend,
    token: &str,
    base: DateTime<Utc>,
    ids: &[ThreadId],
) -> Vec<String> {
    let mut log = Vec::new();
    let mut labels = IdLabels(Vec::new());

    // Start from the built-in sections only: custom sections from earlier runs would share the
    // listing.
    loop {
        let page = both!(backend, list_thread_sections(None, 100)).expect("list leftovers");
        let custom: Vec<_> = page
            .sections
            .iter()
            .filter(|section| section.id != PINNED_THREAD_SECTION_ID)
            .collect();
        if custom.is_empty() {
            break;
        }
        for section in custom {
            both!(backend, delete_thread_section(&section.id)).expect("delete leftover");
        }
    }
    for (index, id) in ids.iter().enumerate() {
        let mut thread = metadata(*id, index as i64, base, SessionSource::Cli);
        thread.model_provider = format!("{token}-sections");
        thread.title = format!("section member {index}");
        both!(backend, upsert_thread(&thread)).expect("upsert member");
    }

    let first = both!(
        backend,
        create_thread_section(
            "First",
            Some(ThreadSectionAppearance {
                icon: Some("star".to_string()),
                color: None,
            })
        )
    )
    .expect("create first");
    let second = both!(backend, create_thread_section("Second", None)).expect("create second");
    labels.add(&first.id);
    labels.add(&second.id);
    log.push(labels.text(format!("created: {first:?} {second:?}")));

    for (label, id, name, appearance) in [
        ("rename", first.id.clone(), "First renamed", None),
        (
            "set appearance",
            first.id.clone(),
            "First renamed",
            Some(Some(ThreadSectionAppearance {
                icon: None,
                color: Some("blue".to_string()),
            })),
        ),
        (
            "clear appearance",
            first.id.clone(),
            "First again",
            Some(None),
        ),
        ("unknown", "missing".to_string(), "x", None),
        ("pinned", PINNED_THREAD_SECTION_ID.to_string(), "x", None),
    ] {
        let renamed = both!(backend, rename_thread_section(&id, name, appearance))
            .map(|section| format!("{section:?}"))
            .unwrap_or_else(|error| error.to_string());
        log.push(labels.text(format!("{label}: {renamed}")));
    }
    log.push(labels.text(format!(
        "get: {:?} {:?}",
        both!(backend, get_thread_section(&first.id)).expect("get"),
        both!(backend, get_thread_section("missing")).expect("get missing"),
    )));
    let mut cursor: Option<String> = None;
    for page_number in 0..5 {
        let page = both!(backend, list_thread_sections(cursor.as_deref(), 2)).expect("list");
        log.push(labels.text(format!("sections page {page_number}: {page:?}")));
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }

    let [a, b, c, d] = [ids[0], ids[1], ids[2], ids[3]];
    let show_order =
        |label: &str,
         log: &mut Vec<String>,
         order: HashMap<ThreadId, (Option<i64>, Option<DateTime<Utc>>)>| {
            let mut entries: Vec<_> = [a, b, c, d]
                .into_iter()
                .map(|id| {
                    (
                        id.to_string(),
                        order
                            .get(&id)
                            .map(|(position, entered)| (*position, entered.is_some())),
                    )
                })
                .collect();
            entries.sort();
            log.push(format!("{label}: {entries:?}"));
        };
    for (label, thread, section, before) in [
        ("append a", a, Some(first.id.as_str()), None),
        ("append b", b, Some(first.id.as_str()), None),
        ("append c", c, Some(first.id.as_str()), None),
        ("c before a", c, Some(first.id.as_str()), Some(a)),
        ("b before c", b, Some(first.id.as_str()), Some(c)),
        ("a to second", a, Some(second.id.as_str()), None),
        ("d to second before a", d, Some(second.id.as_str()), Some(a)),
        (
            "before in other section",
            b,
            Some(first.id.as_str()),
            Some(d),
        ),
        ("before itself", b, Some(first.id.as_str()), Some(b)),
        ("unknown section", b, Some("missing"), None),
        ("before without section", b, None, Some(a)),
        (
            "unknown thread",
            ThreadId::new(),
            Some(first.id.as_str()),
            None,
        ),
    ] {
        let moved = both!(backend, move_thread_to_section(thread, section, before))
            .map(|moved| moved.to_string())
            .unwrap_or_else(|error| error.to_string());
        let order = both!(backend, get_thread_section_ordering(&[a, b, c, d])).expect("order");
        log.push(labels.text(format!("move {label}: {moved}")));
        show_order(label, &mut log, order);
    }
    log.push(labels.text(format!(
        "empty ordering: {:?}",
        both!(backend, get_thread_section_ordering(&[])).expect("empty")
    )));

    // Repeatedly moving the last thread to the front halves the gap until a renumber is needed.
    for round in 0..24 {
        let order = both!(backend, get_thread_section_ordering(&[a, b, c, d])).expect("order");
        let mut members: Vec<ThreadId> = Vec::new();
        for id in [a, b, c, d] {
            let in_first = both!(backend, get_thread(id))
                .expect("get")
                .and_then(|thread| thread.section)
                .is_some_and(|section| section.id == first.id);
            if in_first && order[&id].0.is_some() {
                members.push(id);
            }
        }
        let mut sorted = members.clone();
        sorted.sort_by_key(|id| (order[id].0, id.to_string()));
        let (Some(front), Some(last)) = (sorted.first().copied(), sorted.last().copied()) else {
            break;
        };
        if front == last {
            break;
        }
        let section = both!(backend, get_thread(last))
            .expect("get")
            .and_then(|thread| thread.section)
            .map(|section| section.id)
            .expect("member section");
        both!(
            backend,
            move_thread_to_section(last, Some(&section), Some(front))
        )
        .expect("move to front");
        let order = both!(backend, get_thread_section_ordering(&[a, b, c, d])).expect("order");
        show_order(&format!("round {round}"), &mut log, order);
    }

    log.push(labels.text(format!(
            "thread view: {:?}",
            both!(backend, get_thread(c))
                .expect("get")
                .map(|thread| (thread.section, thread.section_position.is_some()))
        )));
    log.push(format!(
        "clear: {}",
        both!(backend, move_thread_to_section(c, None, None)).expect("clear")
    ));
    log.push(labels.text(format!(
            "after clear: {:?}",
            both!(backend, get_thread(c))
                .expect("get")
                .map(|thread| (thread.section, thread.section_position))
        )));
    log.push(format!(
        "delete: {} {} {:?}",
        both!(backend, delete_thread_section(&first.id)).expect("delete"),
        both!(backend, delete_thread_section(&first.id)).expect("delete again"),
        both!(backend, delete_thread_section(PINNED_THREAD_SECTION_ID))
            .map_err(|error| error.to_string()),
    ));
    let order = both!(backend, get_thread_section_ordering(&[a, b, c, d])).expect("order");
    show_order("after delete", &mut log, order);
    log
}

async fn attachment_scenario(
    backend: &Backend,
    token: &str,
    base: DateTime<Utc>,
    ids: &[ThreadId],
) -> Vec<String> {
    let mut log = Vec::new();
    let mut labels = IdLabels(Vec::new());
    let [source, fork, missing] = [ids[0], ids[1], ids[2]];
    for (index, id) in [source, fork].iter().enumerate() {
        let mut thread = metadata(*id, index as i64, base, SessionSource::Cli);
        thread.model_provider = format!("{token}-attachments");
        both!(backend, upsert_thread(&thread)).expect("upsert attachment owner");
    }
    let render = |labels: &mut IdLabels, outcome: String| labels.text(outcome);
    let describe = |attachment: &ThreadAttachment| {
        format!(
            "{} {} {} {} {}",
            attachment.id,
            attachment.thread_id,
            attachment.attachment_type,
            attachment.identity_key,
            attachment.payload
        )
    };

    for (label, thread, attachment_type, key, payload) in [
        (
            "first",
            source,
            "file",
            "a.txt",
            serde_json::json!({"size": 1}),
        ),
        (
            "repeat",
            source,
            "file",
            "a.txt",
            serde_json::json!({"size": 99}),
        ),
        (
            "second",
            source,
            "file",
            "b.txt",
            serde_json::json!({"size": 2}),
        ),
        (
            "other type",
            source,
            "link",
            "a.txt",
            serde_json::json!(null),
        ),
        (
            "unknown thread",
            missing,
            "file",
            "a.txt",
            serde_json::json!({}),
        ),
        ("empty type", source, "  ", "a.txt", serde_json::json!({})),
        ("empty key", source, "file", "", serde_json::json!({})),
        (
            "long type",
            source,
            &"t".repeat(257),
            "a.txt",
            serde_json::json!({}),
        ),
        (
            "long key",
            source,
            "file",
            &"k".repeat(257),
            serde_json::json!({}),
        ),
        (
            "large payload",
            source,
            "file",
            "big",
            serde_json::json!({"text": "x".repeat(70_000)}),
        ),
    ] {
        let outcome = both!(
            backend,
            add_thread_attachment(thread, attachment_type, key, &payload)
        )
        .map(|outcome| match outcome {
            AddThreadAttachmentOutcome::Created(attachment) => {
                labels.add(&attachment.id);
                format!("created {}", describe(&attachment))
            }
            AddThreadAttachmentOutcome::Existing(attachment) => {
                labels.add(&attachment.id);
                format!("existing {}", describe(&attachment))
            }
        })
        .unwrap_or_else(|error| error.to_string());
        log.push(render(&mut labels, format!("add {label}: {outcome}")));
    }

    // The per-thread limit holds, and removing an attachment frees its slot.
    let mut added = 3;
    for index in 0..MAX_THREAD_ATTACHMENTS_PER_THREAD {
        let result = both!(
            backend,
            add_thread_attachment(
                source,
                "bulk",
                &format!("item-{index}"),
                &serde_json::json!(index)
            )
        );
        match result {
            Ok(AddThreadAttachmentOutcome::Created(attachment)) => {
                labels.add(&attachment.id);
                added += 1;
            }
            Ok(AddThreadAttachmentOutcome::Existing(_)) => {
                log.push("unexpected existing".to_string())
            }
            Err(error) => {
                log.push(render(
                    &mut labels,
                    format!("bulk {index} (after {added}): {error}"),
                ));
                break;
            }
        }
    }
    log.push(render(
        &mut labels,
        format!(
            "remove: {:?} {:?} {:?}",
            both!(backend, remove_thread_attachment(source, "bulk", "item-0"))
                .map(|outcome| format!("{outcome:?}"))
                .map_err(|error| error.to_string()),
            both!(backend, remove_thread_attachment(source, "bulk", "item-0"))
                .map(|outcome| format!("{outcome:?}"))
                .map_err(|error| error.to_string()),
            both!(backend, remove_thread_attachment(missing, "bulk", "item-0"))
                .map(|outcome| format!("{outcome:?}"))
                .map_err(|error| error.to_string()),
        ),
    ));
    log.push(render(
        &mut labels,
        format!(
            "after remove: {:?}",
            both!(
                backend,
                add_thread_attachment(source, "bulk", "after-remove", &serde_json::json!(true))
            )
            .map(|outcome| matches!(outcome, AddThreadAttachmentOutcome::Created(_)))
            .map_err(|error| error.to_string())
        ),
    ));

    let mut cursor: Option<String> = None;
    for page_number in 0..8 {
        let page = both!(
            backend,
            list_thread_attachments(source, cursor.as_deref(), 40)
        )
        .expect("list");
        for attachment in &page.attachments {
            labels.add(&attachment.id);
        }
        // Creation seconds can differ between backends, so only keys and cursors are compared.
        let keys: Vec<_> = page
            .attachments
            .iter()
            .map(|a| (a.attachment_type.as_str(), a.identity_key.as_str()))
            .take(3)
            .collect();
        log.push(render(
            &mut labels,
            format!(
                "page {page_number}: {} first {keys:?} next {:?}",
                page.attachments.len(),
                page.next_cursor.as_ref().map(|cursor| cursor
                    .split('|')
                    .map(str::to_string)
                    .enumerate()
                    .filter(|(index, _)| *index != 1)
                    .map(|(_, part)| part)
                    .collect::<Vec<_>>())
            ),
        ));
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    for (label, cursor, limit) in [
        ("zero limit", None, 0),
        ("huge limit", None, 101),
        ("garbage cursor", Some("garbage".to_string()), 5),
        (
            "other thread cursor",
            Some(format!("{fork}|1|{}", Uuid::now_v7())),
            5,
        ),
    ] {
        let outcome = both!(
            backend,
            list_thread_attachments(source, cursor.as_deref(), limit)
        )
        .map(|page| page.attachments.len().to_string())
        .unwrap_or_else(|error| error.to_string());
        log.push(render(&mut labels, format!("list {label}: {outcome}")));
    }

    both!(backend, copy_thread_attachments(source, fork)).expect("copy");
    let copied = both!(backend, list_thread_attachments(fork, None, 100)).expect("list copy");
    log.push(format!(
        "copied: {} {:?}",
        copied.attachments.len(),
        copied
            .attachments
            .iter()
            .take(3)
            .map(|a| (
                a.attachment_type.as_str(),
                a.identity_key.as_str(),
                a.payload.to_string()
            ))
            .collect::<Vec<_>>()
    ));
    log.push(render(
        &mut labels,
        format!(
            "copy to unknown: {:?}",
            both!(backend, copy_thread_attachments(source, missing))
                .map_err(|error| error.to_string())
        ),
    ));
    log
}

async fn real_postgres_sections_and_attachments_match_sqlite() {
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
    let token = format!(
        "sect{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    );
    let base = run_base(&pool).await + chrono::Duration::seconds(60_000);
    let sqlite = Backend::Sqlite(sqlite);
    let postgres = Backend::Postgres(PostgresThreadCatalog::new(pool));

    let ids: Vec<ThreadId> = (0..4).map(|_| ThreadId::new()).collect();
    let expected = section_scenario(&sqlite, &token, base, &ids).await;
    let actual = section_scenario(&postgres, &token, base, &ids).await;
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(&expected) {
        assert_eq!(actual, expected);
    }

    let ids: Vec<ThreadId> = (0..3).map(|_| ThreadId::new()).collect();
    let base = base + chrono::Duration::seconds(10_000);
    let expected = attachment_scenario(&sqlite, &token, base, &ids).await;
    let actual = attachment_scenario(&postgres, &token, base, &ids).await;
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(&expected) {
        assert_eq!(actual, expected);
    }
}

/// The checks share the namespace-wide timestamp marks, so they run one after another.
#[tokio::test]
async fn real_postgres_thread_catalog() {
    real_postgres_threads_match_sqlite().await;
    real_postgres_listing_matches_sqlite().await;
    real_postgres_projects_match_sqlite().await;
    real_postgres_sections_and_attachments_match_sqlite().await;
    real_postgres_delete_removes_thread_state_and_keeps_queue_changes_visible().await;
}
