#![expect(
    clippy::expect_used,
    reason = "isolated PostgreSQL fixture failures should identify their source"
)]

use codex_postgres_runtime::ConnectionSettings;
use codex_postgres_runtime::PoolLimits;
use codex_postgres_runtime::PostgresPool;
use codex_postgres_runtime::TransactionError;
use serde_json::Value;
use sqlx::Acquire;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

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
            connect_timeout: Duration::from_secs(5),
            acquire_timeout: Duration::from_secs(5),
            max_connections: 4,
        },
    }
}

#[tokio::test]
async fn real_serializable_conflict_deadlock_and_cancelled_write() {
    let Ok(state) = std::env::var("CODEX_TEST_POSTGRES_TRANSACTION_STATE") else {
        return;
    };
    let state = Path::new(&state);
    // Join the exercise so panics are observed before owned fixture cleanup.
    let exercise_state = state.to_path_buf();
    let created = Arc::new(AtomicBool::new(false));
    let exercise_created = Arc::clone(&created);
    let outcome = tokio::spawn(async move {
        exercise_transaction_outcomes(&exercise_state, &exercise_created).await;
    })
    .await;

    let cleanup = if created.load(Ordering::Acquire) {
        let cleanup_state = state.to_path_buf();
        tokio::spawn(async move { cleanup_transaction_probe(&cleanup_state).await })
            .await
            .map_err(|error| format!("cleanup task failed: {error}"))
            .and_then(|result| result)
    } else {
        Ok(())
    };

    let result = match (outcome, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(primary), Ok(())) => Err(format!("transaction exercise failed: {primary}")),
        (Ok(()), Err(cleanup)) => Err(format!("owned probe cleanup failed: {cleanup}")),
        (Err(primary), Err(cleanup)) => Err(format!(
            "transaction exercise failed: {primary}; owned probe cleanup also failed: {cleanup}"
        )),
    };
    result.expect("transaction exercise and owned fixture cleanup should complete");
}

async fn cleanup_transaction_probe(state: &Path) -> Result<(), String> {
    let migrator = PostgresPool::connect(settings(state, "migrator"))
        .await
        .map_err(|error| format!("connect cleanup migrator: {error}"))?;
    let mut connection = migrator
        .acquire()
        .await
        .map_err(|error| format!("acquire cleanup migrator: {error}"))?;
    let mut owner = connection
        .begin()
        .await
        .map_err(|error| format!("begin probe cleanup: {error}"))?;
    sqlx::query("SET LOCAL ROLE codex_owner")
        .execute(&mut *owner)
        .await
        .map_err(|error| format!("assume owner for probe cleanup: {error}"))?;
    sqlx::query("DROP TABLE IF EXISTS codex_storage.transaction_probe")
        .execute(&mut *owner)
        .await
        .map_err(|error| format!("drop owned transaction probe: {error}"))?;
    owner
        .commit()
        .await
        .map_err(|error| format!("commit probe cleanup: {error}"))?;
    Ok(())
}

async fn exercise_transaction_outcomes(state: &Path, created: &AtomicBool) {
    let migrator = PostgresPool::connect(settings(state, "migrator"))
        .await
        .expect("migrator pool");
    let mut connection = migrator.acquire().await.expect("acquire migrator");
    let mut owner = connection.begin().await.expect("begin owner transaction");
    sqlx::query("SET LOCAL ROLE codex_owner")
        .execute(&mut *owner)
        .await
        .expect("assume owner");

    // A same-name pre-existing object is an obstruction, not this run's cleanup target.
    sqlx::query("CREATE TABLE codex_storage.transaction_probe (id INTEGER PRIMARY KEY, value INTEGER NOT NULL)")
        .execute(&mut *owner)
        .await
        .expect("create transaction probe");
    created.store(true, Ordering::Release);
    sqlx::query("INSERT INTO codex_storage.transaction_probe VALUES (1, 0), (2, 0)")
        .execute(&mut *owner)
        .await
        .expect("seed transaction probe");
    owner.commit().await.expect("commit transaction probe");
    drop(connection);
    let pool = Arc::new(
        PostgresPool::connect(settings(state, "runtime"))
            .await
            .expect("runtime pool"),
    );

    let mut first = pool.begin_serializable().await.expect("first transaction");
    let mut second = pool.begin_serializable().await.expect("second transaction");
    for transaction in [&mut first, &mut second] {
        let value: i32 =
            sqlx::query_scalar("SELECT value FROM codex_storage.transaction_probe WHERE id = 1")
                .fetch_one(transaction.connection())
                .await
                .expect("read same snapshot");
        assert_eq!(value, 0);
    }
    sqlx::query("UPDATE codex_storage.transaction_probe SET value = 1 WHERE id = 1")
        .execute(first.connection())
        .await
        .expect("first writer updates");
    assert_eq!(first.commit().await, Ok(()));
    let conflict = sqlx::query("UPDATE codex_storage.transaction_probe SET value = 2 WHERE id = 1")
        .execute(second.connection())
        .await
        .expect_err("stale serializable writer must abort");
    assert_eq!(
        TransactionError::classify_statement(&conflict),
        TransactionError::SerializationConflict
    );
    second.rollback().await.expect("finish aborted transaction");

    let mut left = pool.begin_serializable().await.expect("left transaction");
    let mut right = pool.begin_serializable().await.expect("right transaction");
    sqlx::query("UPDATE codex_storage.transaction_probe SET value = value + 1 WHERE id = 1")
        .execute(left.connection())
        .await
        .expect("left locks first row");
    sqlx::query("UPDATE codex_storage.transaction_probe SET value = value + 1 WHERE id = 2")
        .execute(right.connection())
        .await
        .expect("right locks second row");
    let (left_result, right_result) = tokio::join!(
        sqlx::query("UPDATE codex_storage.transaction_probe SET value = value + 1 WHERE id = 2")
            .execute(left.connection()),
        sqlx::query("UPDATE codex_storage.transaction_probe SET value = value + 1 WHERE id = 1")
            .execute(right.connection())
    );
    let outcomes = [left_result, right_result]
        .into_iter()
        .filter_map(Result::err)
        .map(|error| TransactionError::classify_statement(&error))
        .collect::<Vec<_>>();
    assert!(outcomes.contains(&TransactionError::Deadlock));
    left.rollback().await.expect("finish left transaction");
    right.rollback().await.expect("finish right transaction");

    let (ready, started) = tokio::sync::oneshot::channel();
    let cancelled_pool = Arc::clone(&pool);
    let pending = tokio::spawn(async move {
        let mut transaction = cancelled_pool
            .begin_serializable()
            .await
            .expect("begin cancelled transaction");
        sqlx::query("UPDATE codex_storage.transaction_probe SET value = 99 WHERE id = 1")
            .execute(transaction.connection())
            .await
            .expect("write uncommitted value");
        ready.send(()).expect("signal uncommitted write");
        let _ = sqlx::query("SELECT pg_sleep(10)")
            .execute(transaction.connection())
            .await;
        transaction.commit().await
    });
    started.await.expect("wait for uncommitted write");
    pending.abort();
    assert!(
        pending
            .await
            .expect_err("cancel pending transaction")
            .is_cancelled()
    );
    pool.health()
        .await
        .expect("pool remains healthy after cancellation");
    let mut reader = pool.acquire().await.expect("inspect rollback");
    let values: Vec<i32> =
        sqlx::query_scalar("SELECT value FROM codex_storage.transaction_probe ORDER BY id")
            .fetch_all(&mut *reader)
            .await
            .expect("read committed probe values");
    assert_eq!(values, vec![1, 0]);
    drop(reader);
}
