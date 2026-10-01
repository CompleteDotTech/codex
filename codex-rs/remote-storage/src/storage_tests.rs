use super::*;
use crate::RemoteStorageError;
use codex_keyring_store::KeyringStore;
use codex_keyring_store::tests::MockKeyringStore;
use codex_postgres_runtime::NamedNamespace;
use codex_postgres_runtime::PoolError;
use codex_postgres_runtime::bootstrap_codex_storage;
use codex_postgres_runtime::bootstrap_named_namespace;
use codex_protocol::ThreadId;
use codex_protocol::protocol::SessionSource;
use codex_state::LogEntry;
use codex_state::RuntimeLogStore;
use codex_state::ThreadGoal;
use codex_state::ThreadGoalStatus;
use codex_state::ThreadGoalStore;
use codex_state::ThreadMetadataBuilder;
use codex_storage_authority::CredentialResolutionError;
use codex_thread_store::QueueStore;
use pretty_assertions::assert_eq;
use std::path::Path;
use std::path::PathBuf;

const KEYRING_SERVICE: &str = "codex-postgres-storage";

fn port(state: &Path) -> u16 {
    let receipt: serde_json::Value = serde_json::from_slice(
        &std::fs::read(state.join("receipt.json")).expect("read isolated PostgreSQL receipt"),
    )
    .expect("parse PostgreSQL receipt");
    receipt["port"].as_u64().expect("PostgreSQL port") as u16
}

fn password(state: &Path, role: &str) -> String {
    std::fs::read_to_string(state.join(format!("secrets/{role}.password")))
        .expect("read role credential")
        .trim()
        .to_string()
}

fn profile(state: &Path, namespace: &str, ca: Option<PathBuf>) -> RemotePostgresProfile {
    serde_json::from_value(serde_json::json!({
        "endpoint": "localhost",
        "port": port(state),
        "database": "codex",
        "namespace": namespace,
        "credential": {"source": "keyring", "id": "remote-storage-test"},
        "tls": {"verification": "verify_full", "ca_certificate": ca},
        "connect_timeout_seconds": 5,
        "pool_acquire_timeout_seconds": 20,
        "max_connections": 4
    }))
    .expect("storage profile")
}

fn settings(state: &Path, username: &str, role: &str) -> ConnectionSettings {
    ConnectionSettings {
        host: "localhost".to_string(),
        port: port(state),
        database: "codex".to_string(),
        username: username.to_string(),
        password: password(state, role).into(),
        ca_certificate: state.join("secrets/ca.crt"),
        limits: PoolLimits {
            connect_timeout: Duration::from_secs(5),
            acquire_timeout: Duration::from_secs(20),
            max_connections: 4,
        },
    }
}

async fn scenario(state: &Path, namespace: &str, runtime_login: &str, runtime_role: &str) {
    let keyring = MockKeyringStore::default();
    keyring
        .save(
            KEYRING_SERVICE,
            "remote-storage-test",
            &password(state, runtime_role),
        )
        .expect("save credential");
    let resolver = HostCredentialResolver::new(&keyring);
    let ca = Some(state.join("secrets/ca.crt"));

    let storage = RemoteStorage::connect(&profile(state, namespace, ca.clone()), &resolver)
        .await
        .expect("connect");
    let before = storage.activation().await.expect("activation");
    assert!(!before.migrating);
    assert_eq!(storage.connected_generation(), before.generation);

    // The stores work through the namespace the profile selected.
    let thread_id = ThreadId::new();
    let mut builder = ThreadMetadataBuilder::new(
        thread_id,
        PathBuf::from("/remote/rollout.jsonl"),
        chrono::Utc::now(),
        SessionSource::Cli,
    );
    builder.model_provider = Some("remote-provider".to_string());
    storage
        .thread_catalog()
        .upsert_thread(&builder.build("remote-provider"))
        .await
        .expect("catalog write");
    let item = storage
        .queue_store()
        .enqueue(thread_id, "{\"text\":\"remote\"}".to_string())
        .await
        .expect("queue write");
    assert_eq!(item.thread_id, thread_id);
    storage
        .goal_store()
        .replace_thread_goal_snapshot(&ThreadGoal {
            thread_id,
            goal_id: "remote-goal".to_string(),
            objective: "stay remote".to_string(),
            status: ThreadGoalStatus::Active,
            token_budget: None,
            tokens_used: 0,
            time_used_seconds: 0,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        })
        .await
        .expect("goal write");
    storage
        .log_store()
        .insert_logs(&[LogEntry {
            ts: 1,
            ts_nanos: 0,
            level: "INFO".to_string(),
            target: "remote".to_string(),
            message: Some("hello".to_string()),
            feedback_log_body: None,
            thread_id: Some(thread_id.to_string()),
            process_uuid: None,
            module_path: None,
            file: None,
            line: None,
        }])
        .await
        .expect("log write");

    // A migration holding the dataset blocks writes and reconnect checks, and a new generation
    // is noticed by a connection opened before it.
    let admin = PostgresPool::connect_in_namespace(
        settings(state, runtime_login, runtime_role),
        NamedNamespace::new(namespace).ok().as_ref(),
    )
    .await
    .expect("second connection");
    let mut connection = admin.acquire().await.expect("connection");
    sqlx::query("UPDATE storage_activation SET state = 'migrating'")
        .execute(&mut *connection)
        .await
        .expect("hold dataset");
    assert!(storage.activation().await.expect("activation").migrating);
    let blocked = storage.require_current_generation().await;
    assert_eq!(blocked, Err(RemoteStorageError::Migrating));
    assert!(blocked.expect_err("blocked").is_retryable());
    assert!(
        storage
            .queue_store()
            .enqueue(thread_id, "{}".to_string())
            .await
            .is_err()
    );
    sqlx::query("UPDATE storage_activation SET state = 'open', generation = generation + 1")
        .execute(&mut *connection)
        .await
        .expect("reactivate dataset");
    assert_eq!(
        storage.require_current_generation().await,
        Err(RemoteStorageError::GenerationChanged)
    );
    let fresh = RemoteStorage::connect(&profile(state, namespace, ca.clone()), &resolver)
        .await
        .expect("reconnect");
    assert_eq!(fresh.require_current_generation().await, Ok(()));
    sqlx::query("DELETE FROM threads WHERE id = $1::uuid")
        .bind(thread_id.to_string())
        .execute(&mut *connection)
        .await
        .expect("remove test thread");
    drop(connection);
    admin.close().await.expect("close second connection");
    fresh.close().await;
    storage.close().await;

    // Rejections say whether trying again could help.
    let wrong = MockKeyringStore::default();
    wrong
        .save(KEYRING_SERVICE, "remote-storage-test", "not-the-password")
        .expect("save wrong credential");
    let rejected = RemoteStorage::connect(
        &profile(state, namespace, ca.clone()),
        &HostCredentialResolver::new(&wrong),
    )
    .await
    .expect_err("wrong password");
    assert_eq!(
        rejected,
        RemoteStorageError::Connection(PoolError::Authentication)
    );
    assert!(!rejected.is_retryable());
    let missing = RemoteStorage::connect(
        &profile(state, namespace, ca.clone()),
        &HostCredentialResolver::new(&MockKeyringStore::default()),
    )
    .await
    .expect_err("no credential");
    assert_eq!(
        missing,
        RemoteStorageError::Credential(CredentialResolutionError::Missing)
    );
    assert_eq!(
        RemoteStorage::connect(&profile(state, namespace, None), &resolver)
            .await
            .err(),
        Some(RemoteStorageError::CaCertificateRequired)
    );
}

#[tokio::test]
async fn real_postgres_remote_storage() {
    let Ok(state) = std::env::var("CODEX_TEST_POSTGRES_REMOTE_STORAGE_STATE") else {
        return;
    };
    let state = Path::new(&state);
    let migrator = PostgresPool::connect(settings(state, "codex_migrator", "migrator"))
        .await
        .expect("migrator pool");
    bootstrap_codex_storage(&migrator)
        .await
        .expect("bootstrap default namespace");
    let named = NamedNamespace::new("codex_storage_isolation").expect("named namespace");
    let named_migrator = PostgresPool::connect(settings(
        state,
        named.migrator_login(),
        "isolation_migrator",
    ))
    .await
    .expect("named migrator pool");
    bootstrap_named_namespace(&named_migrator, &named)
        .await
        .expect("bootstrap named namespace");

    scenario(state, "codex_storage", "codex_runtime", "runtime").await;
    scenario(
        state,
        "codex_storage_isolation",
        named.runtime_login(),
        "isolation_runtime",
    )
    .await;

    let unsupported = RemoteStorage::connect(
        &profile(
            state,
            "codex_other_namespace",
            Some(state.join("secrets/ca.crt")),
        ),
        &HostCredentialResolver::new(&MockKeyringStore::default()),
    )
    .await
    .err();
    assert_eq!(unsupported, Some(RemoteStorageError::UnsupportedNamespace));
}
