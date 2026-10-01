use super::*;
use codex_core::config::Config;
use codex_core::config::ConfigBuilder;
use codex_keyring_store::tests::MockKeyringStore;
use codex_postgres_runtime::ConnectionSettings;
use codex_postgres_runtime::PoolLimits;
use codex_postgres_runtime::PostgresPool;
use codex_postgres_runtime::bootstrap_codex_storage;
use codex_postgres_thread_store::PostgresThreadStore;
use codex_state::SqliteConfig;
use codex_storage_authority::ActiveBackend;
use codex_storage_authority::adopt_quiesced_home;
use codex_storage_authority::begin_cutover;
use codex_storage_migration::Cutover;
use codex_storage_migration::Migrator;
use codex_storage_migration::SqliteSource;
use codex_utils_absolute_path::test_support::PathExt;
use pretty_assertions::assert_eq;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const KEYRING_SERVICE: &str = "codex-postgres-storage";

fn receipt(state: &Path) -> serde_json::Value {
    serde_json::from_slice(
        &std::fs::read(state.join("receipt.json")).expect("read isolated PostgreSQL receipt"),
    )
    .expect("parse PostgreSQL receipt")
}

fn settings(state: &Path, username: &str, role: &str) -> ConnectionSettings {
    ConnectionSettings {
        host: "localhost".to_string(),
        port: receipt(state)["port"].as_u64().expect("PostgreSQL port") as u16,
        database: "codex".to_string(),
        username: username.to_string(),
        password: std::fs::read_to_string(state.join(format!("secrets/{role}.password")))
            .expect("read role credential")
            .trim()
            .to_string()
            .into(),
        ca_certificate: state.join("secrets/ca.crt"),
        limits: PoolLimits {
            connect_timeout: Duration::from_secs(5),
            acquire_timeout: Duration::from_secs(20),
            max_connections: 4,
        },
    }
}

fn write_profile(home: &Path, state: &Path) {
    let ca = state.join("secrets/ca.crt");
    let profile = format!(
        "[storage_candidate]\nbackend = \"remote_postgres\"\nendpoint = \"localhost\"\n\
         port = {}\ndatabase = \"codex\"\nnamespace = \"codex_storage\"\n\
         connect_timeout_seconds = 5\npool_acquire_timeout_seconds = 20\nmax_connections = 4\n\n\
         [storage_candidate.credential]\nsource = \"keyring\"\nid = \"remote-backend-test\"\n\n\
         [storage_candidate.tls]\nca_certificate = {:?}\n",
        receipt(state)["port"],
        ca.display().to_string()
    );
    std::fs::write(home.join("config.toml"), profile).expect("write profile");
}

async fn config_for(home: &Path) -> Config {
    ConfigBuilder::default()
        .codex_home(home.to_path_buf())
        .build()
        .await
        .expect("config")
}

async fn reset_target(pool: &PostgresPool) {
    let mut connection = pool.acquire().await.expect("connection");
    for statement in [
        "UPDATE storage_activation SET state = 'open', run_id = NULL, generation = 0, dataset_id = NULL",
        "DELETE FROM storage_migration_runs",
        "DELETE FROM threads",
        "DELETE FROM projects",
        "DELETE FROM logs",
        "DELETE FROM agent_board_posts",
        "DELETE FROM memory_stage1_outputs",
    ] {
        sqlx::query(sqlx::AssertSqlSafe(statement))
            .execute(&mut *connection)
            .await
            .unwrap_or_else(|error| panic!("{statement}: {error}"));
    }
}

#[tokio::test]
async fn real_postgres_remote_backend_selection() {
    let Ok(state) = std::env::var("CODEX_TEST_POSTGRES_REMOTE_BACKEND_STATE") else {
        return;
    };
    let state = Path::new(&state);
    bootstrap_codex_storage(
        &PostgresPool::connect(settings(state, "codex_migrator", "migrator"))
            .await
            .expect("migrator pool"),
    )
    .await
    .expect("bootstrap");
    let pool = Arc::new(
        PostgresPool::connect_in_namespace(
            settings(state, "codex_runtime", "runtime"),
            /*namespace*/ None,
        )
        .await
        .expect("runtime pool"),
    );
    reset_target(&pool).await;
    let keyring = MockKeyringStore::default();
    keyring
        .save(
            KEYRING_SERVICE,
            "remote-backend-test",
            std::fs::read_to_string(state.join("secrets/runtime.password"))
                .expect("password")
                .trim(),
        )
        .expect("save credential");

    let temp = tempfile::tempdir().expect("home");
    let home = temp.path();
    write_profile(home, state);
    let config = config_for(home).await;

    // Homes that predate storage authority, and local homes, stay local.
    assert_eq!(prepare_storage_with(&config, &keyring).await, Ok(false));
    adopt_quiesced_home(home).expect("adopt");
    assert_eq!(prepare_storage_with(&config, &keyring).await, Ok(false));
    assert!(codex_core::remote_backend().is_none());

    // An interrupted cutover refuses to start anything.
    let intent = begin_cutover(home, uuid::Uuid::new_v4(), ActiveBackend::Remote).expect("intent");
    assert_eq!(
        prepare_storage_with(&config, &keyring).await,
        Err(RemoteBackendError::CutoverInProgress)
    );
    codex_storage_authority::abandon_cutover(home, &intent).expect("abandon");

    // A real cutover of an empty home makes PostgreSQL authoritative.
    let sqlite = SqliteConfig::new_for_testing(home.abs());
    // A real home has its databases, with the rows every fresh home starts with.
    let runtime = codex_state::StateRuntime::init(sqlite.clone(), "test-provider".to_string())
        .await
        .expect("create the source databases");
    runtime.close().await;
    drop(runtime);
    let source = SqliteSource::new(sqlite);
    let migrator = Migrator::new(source.clone(), pool.clone());
    let summary = migrator.import().await.expect("import");
    migrator.verify(summary.run_id).await.expect("verify");
    let cutover = Cutover::new(home.to_path_buf(), Migrator::new(source, pool.clone()));
    let moved = cutover.execute(summary.run_id).await.expect("cutover");
    assert_eq!(moved.marker.active_backend, ActiveBackend::Remote);

    // Without a credential the process refuses instead of falling back to local files.
    let missing = prepare_storage_with(&config, &MockKeyringStore::default()).await;
    assert!(
        matches!(missing, Err(RemoteBackendError::Storage(_))),
        "{missing:?}"
    );
    assert!(codex_core::remote_backend().is_none());

    // A store that is not the dataset this home published is refused.
    let published_dataset = moved.identity.dataset_id;
    {
        let mut connection = pool.acquire().await.expect("connection");
        sqlx::query("UPDATE storage_activation SET dataset_id = $1")
            .bind(uuid::Uuid::new_v4().to_string())
            .execute(&mut *connection)
            .await
            .expect("swap dataset");
        assert_eq!(
            prepare_storage_with(&config, &keyring).await,
            Err(RemoteBackendError::DatasetMismatch)
        );
        sqlx::query("UPDATE storage_activation SET dataset_id = $1")
            .bind(published_dataset.to_string())
            .execute(&mut *connection)
            .await
            .expect("restore dataset");
    }

    // The matching dataset installs the remote stores, and core hands them to every consumer.
    assert_eq!(prepare_storage_with(&config, &keyring).await, Ok(true));
    let store = codex_core::thread_store_from_config(&config, /*state_db*/ None);
    assert!(store.as_any().is::<PostgresThreadStore>());
    assert!(codex_core::remote_backend().is_some());
    assert_eq!(
        prepare_storage_with(&config, &keyring).await,
        Err(RemoteBackendError::AlreadyInstalled)
    );
    // The local state database is never opened for a remote home, even though the migrated
    // source files are still there.
    assert!(codex_core::init_state_db(&config).await.is_none());
    reset_target(&pool).await;
}
