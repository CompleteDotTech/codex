use super::*;
use chrono::DateTime;
use chrono::Utc;
use codex_postgres_runtime::ConnectionSettings;
use codex_postgres_runtime::PoolLimits;
use codex_postgres_runtime::bootstrap_codex_storage;
use codex_postgres_thread_catalog::PostgresThreadCatalog;
use codex_protocol::ThreadId;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_state::ProjectRoot;
use codex_state::SqliteConfig;
use codex_state::StateRuntime;
use codex_state::ThreadMetadata;
use codex_state::ThreadMetadataBuilder;
use codex_state::ThreadSectionAppearance;
use codex_utils_absolute_path::test_support::PathExt;
use pretty_assertions::assert_eq;
use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;

fn settings(state: &Path, role: &str) -> ConnectionSettings {
    let receipt: serde_json::Value = serde_json::from_slice(
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

/// Leave the shared namespace with no migrated data and no run, as a fresh target would be.
async fn reset_target(owner: &PostgresPool) {
    let mut connection = owner.acquire().await.expect("owner connection");
    for statement in [
        "TRUNCATE codex_storage.storage_migration_runs CASCADE",
        "UPDATE codex_storage.storage_activation SET state = 'open', run_id = NULL",
        "TRUNCATE codex_storage.threads CASCADE",
        "TRUNCATE codex_storage.projects CASCADE",
        "TRUNCATE codex_storage.project_idempotency_keys",
        "DELETE FROM codex_storage.thread_sections WHERE id <> '01984de2-8f74-7c91-a3b2-5c5e937cf318'",
    ] {
        sqlx::query(statement)
            .execute(&mut *connection)
            .await
            .unwrap_or_else(|error| panic!("{statement}: {error}"));
    }
}

fn metadata(index: i64, base: DateTime<Utc>, source: SessionSource) -> ThreadMetadata {
    let created = base + chrono::Duration::seconds(index * 11);
    let mut builder = ThreadMetadataBuilder::new(
        ThreadId::new(),
        PathBuf::from(format!("/source-host/rollouts/thread-{index}.jsonl")),
        created,
        source,
    );
    builder.updated_at = Some(created + chrono::Duration::seconds(7));
    builder.cwd = PathBuf::from(format!("/source-host/work/{index}"));
    builder.git_branch = Some(format!("branch-{index}"));
    builder.model_provider = Some("migration-provider".to_string());
    let mut metadata = builder.build("migration-provider");
    metadata.title = format!("thread {index} — café 🦀");
    metadata.preview = Some(format!("preview {index}"));
    metadata.first_user_message = Some(format!("hello {index}"));
    metadata.tokens_used = index * 100;
    metadata
}

/// A SQLite home with every catalog feature in use, built through the real runtime.
async fn populate(home: &Path) -> Vec<ThreadMetadata> {
    let runtime = StateRuntime::init(
        SqliteConfig::new_for_testing(home.abs()),
        "migration-provider".to_string(),
    )
    .await
    .expect("sqlite runtime");
    let base = Utc::now() - chrono::Duration::days(3);
    let mut threads: Vec<ThreadMetadata> = Vec::new();
    for index in 0..9 {
        let mut thread = metadata(index, base, SessionSource::Cli);
        if index >= 6 {
            thread.source = serde_json::to_string(&SessionSource::SubAgent(
                SubAgentSource::ThreadSpawn {
                    parent_thread_id: threads[0].id,
                    depth: 1,
                    agent_path: None,
                    agent_nickname: None,
                    agent_role: None,
                },
            ))
            .expect("source");
        }
        runtime.upsert_thread(&thread).await.expect("upsert");
        threads.push(thread);
    }
    let work = runtime
        .create_thread_section(
            "Work",
            Some(ThreadSectionAppearance {
                icon: Some("star".to_string()),
                color: None,
            }),
        )
        .await
        .expect("section");
    let later = runtime
        .create_thread_section("Later", None)
        .await
        .expect("section");
    for (index, thread) in threads.iter().take(4).enumerate() {
        runtime
            .move_thread_to_section(thread.id, Some(&work.id), None)
            .await
            .expect("move");
        if index == 3 {
            runtime
                .move_thread_to_section(thread.id, Some(&later.id), None)
                .await
                .expect("move again");
        }
    }
    let created = runtime
        .create_project(
            "Migrated project".to_string(),
            vec![ProjectRoot {
                path: "/source-host/project".to_string(),
            }],
            BTreeMap::from([("kind".to_string(), "test".to_string())]),
            &[threads[1].id.to_string(), threads[2].id.to_string()],
            "migration-key",
        )
        .await
        .expect("project");
    assert!(created.created);
    for thread in threads.iter().take(3) {
        for index in 0..2 {
            runtime
                .add_thread_attachment(
                    thread.id,
                    "file",
                    &format!("file-{index}.txt"),
                    &serde_json::json!({"index": index}),
                )
                .await
                .expect("attachment");
        }
    }
    runtime
        .set_thread_memory_mode(threads[4].id, "disabled")
        .await
        .expect("memory mode");
    runtime
        .mark_archived(
            threads[5].id,
            Path::new("/source-host/archived/thread-5.jsonl"),
            base + chrono::Duration::days(1),
        )
        .await
        .expect("archive");
    // The source owns the checkpointed state, so the runtime must be done writing.
    drop(runtime);
    threads
}

#[tokio::test]
async fn real_postgres_catalog_migration() {
    let Ok(state) = std::env::var("CODEX_TEST_POSTGRES_MIGRATION_STATE") else {
        return;
    };
    let state = Path::new(&state);
    bootstrap_codex_storage(&*connect(state, "migrator").await)
        .await
        .expect("bootstrap migration schema");
    let owner = connect(state, "migrator").await;
    let pool = connect(state, "runtime").await;
    let home = tempfile::tempdir().expect("source home");
    let threads = populate(home.path()).await;
    let config = SqliteConfig::new_for_testing(home.path().abs());
    let source = SqliteSource::new(config.clone());

    // An interrupted run resumes from its checkpoints, and nobody else can take the store.
    reset_target(&owner).await;
    let interrupted = Migrator::new(source.clone(), pool.clone())
        .with_batch_size(2)
        .with_batch_limit(3)
        .import()
        .await;
    assert!(
        matches!(interrupted, Err(MigrationError::Interrupted)),
        "{interrupted:?}"
    );
    let competing = Migrator::new(
        SqliteSource::new(SqliteConfig::new_for_testing(tempfile::tempdir().expect("other").path().abs())),
        pool.clone(),
    )
    .import()
    .await;
    assert!(
        matches!(competing, Err(MigrationError::TargetBusy)),
        "{competing:?}"
    );
    let summary = Migrator::new(source.clone(), pool.clone())
        .with_batch_size(2)
        .import()
        .await
        .expect("resumed import");
    assert!(summary.resumed);
    let report = Migrator::new(source.clone(), pool.clone())
        .with_batch_size(2)
        .verify(summary.run_id)
        .await
        .expect("verification");
    assert_eq!(
        report
            .domains
            .iter()
            .map(|(domain, digest)| (domain.name(), digest.count))
            .collect::<Vec<_>>(),
        vec![
            ("sections", 3),
            ("projects", 1),
            ("project_keys", 1),
            ("threads", 9),
            ("attachments", 6),
            ("spawn_edges", 3),
        ]
    );

    // The migrated catalog answers exactly as the source does.
    let runtime = StateRuntime::init(config.clone(), "migration-provider".to_string())
        .await
        .expect("source runtime");
    let catalog = PostgresThreadCatalog::new(pool.clone());
    for thread in &threads {
        assert_eq!(
            catalog.get_thread(thread.id).await.expect("target thread"),
            runtime.get_thread(thread.id).await.expect("source thread"),
            "thread {}",
            thread.id
        );
    }
    drop(runtime);

    // A second run on a populated target is refused, a changed source fails verification, and a
    // tampered row is caught.
    reset_target(&owner).await;
    let first = Migrator::new(source.clone(), pool.clone())
        .import()
        .await
        .expect("import again");
    sqlx::query("UPDATE codex_storage.threads SET title = 'tampered' WHERE id = $1::uuid")
        .bind(threads[3].id.to_string())
        .execute(&mut *pool.acquire().await.expect("connection"))
        .await
        .expect("tamper");
    let mismatch = Migrator::new(source.clone(), pool.clone())
        .verify(first.run_id)
        .await;
    assert!(
        matches!(mismatch, Err(MigrationError::Mismatch { domain: "threads" })),
        "{mismatch:?}"
    );
    let stale_home = home.path().join("added-later");
    std::fs::write(&stale_home, b"change").expect("write marker");
    let state_db = config.state_db_path();
    let runtime = StateRuntime::init(config.clone(), "migration-provider".to_string())
        .await
        .expect("runtime");
    runtime
        .upsert_thread(&metadata(99, Utc::now(), SessionSource::Cli))
        .await
        .expect("late thread");
    drop(runtime);
    let _ = state_db;
    let changed = Migrator::new(source.clone(), pool.clone())
        .verify(first.run_id)
        .await;
    assert!(
        matches!(changed, Err(MigrationError::SourceChanged)),
        "{changed:?}"
    );
    reset_target(&owner).await;
}
