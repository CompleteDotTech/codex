//! A run holds the store: writers are refused until the verified import is activated.

use super::tests::metadata;
use super::tests::reset_target;
use super::*;
use chrono::Utc;
use codex_postgres_log_store::PostgresLogStore;
use codex_postgres_queue_store::PostgresQueueStore;
use codex_postgres_rollout_store::PostgresRolloutStore;
use codex_postgres_thread_catalog::PostgresThreadCatalog;
use codex_protocol::ThreadId;
use codex_protocol::protocol::SessionSource;
use codex_state::LogEntry;
use codex_state::RuntimeLogStore;
use codex_thread_store::QueueStore;

/// Every store that can be reached without extra setup refuses a write with the migration
/// reason, while reads keep working.
async fn assert_writes_refused(pool: &Arc<PostgresPool>, existing: ThreadId) {
    let catalog = PostgresThreadCatalog::new(pool.clone());
    let error = catalog
        .upsert_thread(&metadata(500, Utc::now(), SessionSource::Cli))
        .await
        .expect_err("catalog write");
    assert!(error.to_string().contains("being migrated"), "{error}");
    let error = PostgresRolloutStore::new(pool.clone())
        .append(existing, 0, vec![(None, "{}".to_string())])
        .await
        .expect_err("rollout write");
    assert!(error.to_string().contains("being migrated"), "{error}");
    let error = PostgresQueueStore::new(pool.clone())
        .enqueue(existing, "{}".to_string())
        .await
        .expect_err("queue write");
    assert!(error.to_string().contains("being migrated"), "{error}");
    let error = PostgresLogStore::new(pool.clone())
        .insert_logs(&[log_entry()])
        .await
        .expect_err("log write");
    assert!(error.to_string().contains("being migrated"), "{error}");
    assert!(catalog.get_thread(existing).await.expect("read").is_some());
}

fn log_entry() -> LogEntry {
    LogEntry {
        ts: 1,
        ts_nanos: 0,
        level: "INFO".to_string(),
        target: "gate".to_string(),
        message: Some("blocked".to_string()),
        feedback_log_body: None,
        thread_id: None,
        process_uuid: None,
        module_path: None,
        file: None,
        line: None,
    }
}

pub(super) async fn gate_phase(
    pool: &Arc<PostgresPool>,
    source: &SqliteSource,
    existing: ThreadId,
) {
    reset_target(pool).await;
    let interrupted = Migrator::new(source.clone(), pool.clone())
        .with_batch_size(2)
        .with_batch_limit(3)
        .import()
        .await;
    assert!(
        matches!(interrupted, Err(MigrationError::Interrupted)),
        "{interrupted:?}"
    );
    let summary = Migrator::new(source.clone(), pool.clone())
        .import()
        .await
        .expect("resume");
    assert_writes_refused(pool, existing).await;

    // An unverified run cannot be activated, and neither can a run that does not hold the store.
    let migrator = Migrator::new(source.clone(), pool.clone());
    let unverified = migrator.activate(summary.run_id).await;
    assert!(
        matches!(unverified, Err(MigrationError::NotVerified)),
        "{unverified:?}"
    );
    let stranger = migrator.activate(uuid::Uuid::now_v7()).await;
    assert!(
        matches!(stranger, Err(MigrationError::TargetBusy)),
        "{stranger:?}"
    );
    assert_writes_refused(pool, existing).await;

    migrator.verify(summary.run_id).await.expect("verification");
    assert_writes_refused(pool, existing).await;
    let generation = migrator.activate(summary.run_id).await.expect("activation");
    assert!(generation >= 1);

    // The store is writable again, its data is the verified import, and a replay changes nothing.
    PostgresQueueStore::new(pool.clone())
        .enqueue(existing, "{}".to_string())
        .await
        .expect("queue write after activation");
    PostgresLogStore::new(pool.clone())
        .insert_logs(&[log_entry()])
        .await
        .expect("log write after activation");
    let replay = migrator.activate(summary.run_id).await;
    assert!(
        matches!(replay, Err(MigrationError::TargetBusy)),
        "{replay:?}"
    );

    // Activated data is never merged into by a later run.
    let occupied = Migrator::new(source.clone(), pool.clone()).import().await;
    assert!(
        matches!(occupied, Err(MigrationError::TargetNotEmpty)),
        "{occupied:?}"
    );
    reset_target(pool).await;
}
