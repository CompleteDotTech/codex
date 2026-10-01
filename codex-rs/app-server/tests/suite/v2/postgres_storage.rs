//! A real app-server process running its threads against a real PostgreSQL dataset.
//!
//! The home is made remote through the storage service (migrate, verify, activate), then a child
//! app-server starts from it. It must connect before opening any local database, serve the
//! ordinary thread methods from PostgreSQL, and leave no new local history behind. The test runs
//! only when `CODEX_TEST_POSTGRES_APP_SERVER_STATE` names an isolated PostgreSQL fixture.

use anyhow::Result;
use anyhow::ensure;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use app_test_support::create_mock_responses_server_repeating_assistant;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::ThreadArchiveParams;
use codex_app_server_protocol::ThreadArchiveResponse;
use codex_app_server_protocol::ThreadForkParams;
use codex_app_server_protocol::ThreadForkResponse;
use codex_app_server_protocol::ThreadListParams;
use codex_app_server_protocol::ThreadListResponse;
use codex_app_server_protocol::ThreadReadParams;
use codex_app_server_protocol::ThreadReadResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::ThreadUnarchiveParams;
use codex_app_server_protocol::ThreadUnarchiveResponse;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::UserInput;
use codex_keyring_store::KeyringStore;
use codex_keyring_store::tests::MockKeyringStore;
use codex_postgres_runtime::ConnectionSettings;
use codex_postgres_runtime::PoolLimits;
use codex_postgres_runtime::PostgresPool;
use codex_state::SqliteConfig;
use codex_state::StateRuntime;
use codex_storage_authority::KEYRING_SERVICE;
use codex_storage_authority::RemotePostgresProfile;
use codex_storage_service::Confirmation;
use codex_storage_service::PlanAction;
use codex_storage_service::StorageService;
use codex_storage_service::StorageServiceInputs;
use codex_utils_absolute_path::test_support::PathExt;
use pretty_assertions::assert_eq;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::timeout;
use uuid::Uuid;

const READ_TIMEOUT: Duration = Duration::from_secs(30);
const PASSWORD_VARIABLE: &str = "CODEX_TEST_PG_RUNTIME_PASSWORD";

fn receipt(state: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(state.join("receipt.json")).expect("receipt"))
        .expect("receipt json")
}

fn password(state: &Path, role: &str) -> String {
    std::fs::read_to_string(state.join(format!("secrets/{role}.password")))
        .expect("password")
        .trim()
        .to_string()
}

async fn reset_dataset(state: &Path) {
    let pool = PostgresPool::connect_in_namespace(
        ConnectionSettings {
            host: "localhost".to_string(),
            port: receipt(state)["port"].as_u64().expect("port") as u16,
            database: "codex".to_string(),
            username: "codex_runtime".to_string(),
            password: password(state, "runtime").into(),
            ca_certificate: state.join("secrets/ca.crt"),
            limits: PoolLimits {
                connect_timeout: Duration::from_secs(5),
                acquire_timeout: Duration::from_secs(20),
                max_connections: 2,
            },
        },
        /*namespace*/ None,
    )
    .await
    .expect("reset pool");
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

fn candidate_toml(state: &Path) -> String {
    format!(
        "\n[storage_candidate]\nbackend = \"remote_postgres\"\nendpoint = \"localhost\"\n\
         port = {}\ndatabase = \"codex\"\nnamespace = \"codex_storage\"\n\
         connect_timeout_seconds = 5\npool_acquire_timeout_seconds = 20\nmax_connections = 4\n\n\
         [storage_candidate.credential]\nsource = \"environment\"\nvariable = \"{PASSWORD_VARIABLE}\"\n\n\
         [storage_candidate.tls]\nca_certificate = {:?}\n",
        receipt(state)["port"],
        state.join("secrets/ca.crt").display().to_string()
    )
}

/// Make the home remote exactly as an operator would: initialize, migrate, verify, activate.
async fn make_home_remote(state: &Path, home: &Path) -> Result<()> {
    let sqlite = SqliteConfig::new_for_testing(home.abs());
    let runtime = StateRuntime::init(sqlite.clone(), "mock_provider".to_string()).await?;
    runtime.close().await;
    let keyring = MockKeyringStore::default();
    keyring.save(KEYRING_SERVICE, "e2e-runtime", &password(state, "runtime"))?;
    keyring.save(KEYRING_SERVICE, "e2e-owner", &password(state, "migrator"))?;
    let profile: RemotePostgresProfile = serde_json::from_value(serde_json::json!({
        "endpoint": "localhost",
        "port": receipt(state)["port"],
        "database": "codex",
        "namespace": "codex_storage",
        "credential": {"source": "keyring", "id": "e2e-runtime"},
        "migrator_credential": {"source": "keyring", "id": "e2e-owner"},
        "tls": {"verification": "verify_full", "ca_certificate": state.join("secrets/ca.crt")},
        "connect_timeout_seconds": 5,
        "pool_acquire_timeout_seconds": 20,
        "max_connections": 4
    }))?;
    let service = StorageService::new(StorageServiceInputs {
        codex_home: home.to_path_buf(),
        sqlite,
        candidate: Some(profile),
        default_model_provider_id: "mock_provider".to_string(),
        host_label: "e2e-host".to_string(),
        keyring: Arc::new(keyring),
    });
    service.initialize_schema().await?;
    let plan = service.plan(PlanAction::Migrate).await?;
    ensure!(plan.is_startable(), "plan blocked: {:?}", plan.blockers);
    let confirmed = Confirmation {
        writers_stopped: true,
    };
    let operation = Uuid::new_v4();
    let ready = service
        .start_migration(operation, plan.plan_id, confirmed)
        .await?;
    ensure!(ready.state == codex_storage_service::OperationState::Ready);
    service.activate(operation).await?;
    Ok(())
}

fn text_input(text: &str) -> Vec<UserInput> {
    vec![UserInput::Text {
        text: text.to_string(),
        text_elements: Vec::new(),
    }]
}

async fn run_turn(app_server: &mut TestAppServer, thread_id: &str, text: &str) -> Result<()> {
    let _: TurnStartResponse = app_server
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread_id.to_string(),
                input: text_input(text),
                ..Default::default()
            },
        })
        .await?;
    timeout(
        READ_TIMEOUT,
        app_server.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    Ok(())
}

async fn list(app_server: &mut TestAppServer, archived: bool) -> Result<ThreadListResponse> {
    app_server
        .request(|request_id| ClientRequest::ThreadList {
            request_id,
            params: ThreadListParams {
                originators: None,
                cursor: None,
                limit: Some(50),
                sort_key: None,
                sort_direction: None,
                model_providers: Some(Vec::new()),
                source_kinds: None,
                archived: Some(archived),
                section_id: None,
                project_id: None,
                cwd: None,
                use_state_db_only: false,
                search_term: None,
                parent_thread_id: None,
                ancestor_thread_id: None,
            },
        })
        .await
}

#[tokio::test]
async fn app_server_serves_threads_from_postgres_without_local_history() -> Result<()> {
    let Ok(state) = std::env::var("CODEX_TEST_POSTGRES_APP_SERVER_STATE") else {
        return Ok(());
    };
    let state = Path::new(&state);
    reset_dataset(state).await;
    let model_server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&model_server.uri())
        .with_extra_config(&candidate_toml(state))
        .write(codex_home.path())?;
    make_home_remote(state, codex_home.path()).await?;
    let local_history_before: Vec<_> = std::fs::read_dir(codex_home.path())?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<_, _>>()?;
    assert!(!local_history_before.iter().any(|name| name == "sessions"));

    let runtime_password = password(state, "runtime");
    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&[(PASSWORD_VARIABLE, Some(runtime_password.as_str()))])
        .build_initialized()
        .await?;

    // Start, run a turn, and find the thread through list and read.
    let ThreadStartResponse { thread, .. } = app_server
        .start_thread(ThreadStartParams::default())
        .await?;
    run_turn(&mut app_server, &thread.id, "first message").await?;
    let listed = list(&mut app_server, /*archived*/ false).await?;
    assert!(
        listed
            .data
            .iter()
            .any(|candidate| candidate.id == thread.id)
    );
    let read: ThreadReadResponse = app_server
        .request(|request_id| ClientRequest::ThreadRead {
            request_id,
            params: ThreadReadParams {
                thread_id: thread.id.clone(),
                include_turns: true,
            },
        })
        .await?;
    assert_eq!(read.thread.turns.len(), 1);

    // A second turn extends the same history, and a fork carries it.
    run_turn(&mut app_server, &thread.id, "second message").await?;
    let read: ThreadReadResponse = app_server
        .request(|request_id| ClientRequest::ThreadRead {
            request_id,
            params: ThreadReadParams {
                thread_id: thread.id.clone(),
                include_turns: true,
            },
        })
        .await?;
    assert_eq!(read.thread.turns.len(), 2);
    let forked: ThreadForkResponse = app_server
        .request(|request_id| ClientRequest::ThreadFork {
            request_id,
            params: ThreadForkParams {
                thread_id: thread.id.clone(),
                ..Default::default()
            },
        })
        .await?;
    assert_ne!(forked.thread.id, thread.id);

    // Archiving moves it out of the active list and back.
    let _: ThreadArchiveResponse = app_server
        .request(|request_id| ClientRequest::ThreadArchive {
            request_id,
            params: ThreadArchiveParams {
                thread_id: thread.id.clone(),
            },
        })
        .await?;
    let archived = list(&mut app_server, /*archived*/ true).await?;
    assert!(
        archived
            .data
            .iter()
            .any(|candidate| candidate.id == thread.id)
    );
    let active = list(&mut app_server, /*archived*/ false).await?;
    assert!(
        !active
            .data
            .iter()
            .any(|candidate| candidate.id == thread.id)
    );
    let _: ThreadUnarchiveResponse = app_server
        .request(|request_id| ClientRequest::ThreadUnarchive {
            request_id,
            params: ThreadUnarchiveParams {
                thread_id: thread.id.clone(),
            },
        })
        .await?;
    app_server.shutdown_gracefully().await?;

    // Nothing was written locally: no rollout files and no new thread rows in the old database.
    assert!(!codex_home.path().join("sessions").exists());
    assert!(!codex_home.path().join("archived_sessions").exists());
    reset_dataset(state).await;
    Ok(())
}
