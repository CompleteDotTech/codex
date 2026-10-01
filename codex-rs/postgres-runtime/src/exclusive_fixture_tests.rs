use super::*;
use pretty_assertions::assert_eq;

#[cfg(unix)]
#[test]
fn preexisting_quarantine_is_preserved_and_refuses_arming() {
    let directory = tempfile::tempdir().expect("owned fixture directory");
    let first = ExclusiveFixture::arm(directory.path(), FixtureScope::Default).expect("first arm");
    let before = std::fs::read(&first.quarantine).expect("published marker");
    let second = ExclusiveFixture::arm(directory.path(), FixtureScope::Default);
    assert!(matches!(second,Err(error) if error.kind()==std::io::ErrorKind::AlreadyExists));
    assert_eq!(
        std::fs::read(&first.quarantine).expect("retained marker"),
        before
    );
}

#[cfg(unix)]
#[tokio::test]
async fn captured_exercise_error_preserves_cause_and_quarantine() {
    let directory = tempfile::tempdir().expect("owned fixture directory");
    let guard = ExclusiveFixture::arm(directory.path(), FixtureScope::Named).expect("arm");
    let retained = [
        guard.retain(lazy_pool()).expect("first retained pool"),
        guard.retain(lazy_pool()).expect("second retained pool"),
    ];
    let before = std::fs::read(&guard.quarantine).expect("marker");
    let result = guard
        .supervise(FixtureDeadline::Upgrade, async {
            Err::<(), _>("causal_fixture_failure")
        })
        .await;
    let failure = result.expect_err("exercise must fail");
    assert_eq!(failure.primary, Some("causal_fixture_failure"));
    assert_eq!(failure.pool_closures, vec![true, true]);
    for pool in retained {
        assert_eq!(pool.acquire().await.err(), Some(crate::PoolError::Closed));
    }
    assert_eq!(
        std::fs::read(&failure.quarantine).expect("retained marker"),
        before
    );
}

#[cfg(unix)]
#[tokio::test]
async fn panic_is_joined_before_supervisor_returns_and_quarantine_remains() {
    let directory = tempfile::tempdir().expect("owned fixture directory");
    let guard = ExclusiveFixture::arm(directory.path(), FixtureScope::Default).expect("arm");
    let retained = [
        guard.retain(lazy_pool()).expect("first retained pool"),
        guard.retain(lazy_pool()).expect("second retained pool"),
    ];
    let before = std::fs::read(&guard.quarantine).expect("published marker");
    let result: Result<(), _> = guard
        .supervise(FixtureDeadline::Upgrade, async {
            panic!("owned fixture causal panic")
        })
        .await;
    let failure = result.expect_err("panic must be captured");
    assert_eq!(failure.primary, Some("fixture_exercise_panicked"));
    assert_eq!(failure.pool_closures, vec![true, true]);
    for pool in retained {
        assert_eq!(pool.acquire().await.err(), Some(crate::PoolError::Closed));
    }
    assert_eq!(
        std::fs::read(&failure.quarantine).expect("retained marker"),
        before
    );
    assert!(failure.quarantine.is_file());
}

#[cfg(unix)]
#[tokio::test]
async fn successful_exercise_returns_result_and_retains_quarantine_for_external_verification() {
    let directory = tempfile::tempdir().expect("owned fixture directory");
    let guard = ExclusiveFixture::arm(directory.path(), FixtureScope::Default).expect("arm");
    let retained = [
        guard.retain(lazy_pool()).expect("first retained pool"),
        guard.retain(lazy_pool()).expect("second retained pool"),
    ];
    let quarantine = guard.quarantine.clone();
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(&quarantine).expect("marker metadata");
    let before = (
        metadata.dev(),
        metadata.ino(),
        std::fs::read(&quarantine).expect("published marker"),
    );
    let artifact = directory.path().join("exercise-result");
    let result = guard
        .supervise(FixtureDeadline::Upgrade, async move {
            std::fs::write(&artifact, b"completed exercise").map_err(|_| "exercise_write")?;
            std::fs::read(&artifact).map_err(|_| "exercise_read")
        })
        .await
        .expect("successful exercise");
    assert_eq!(result, b"completed exercise");
    for pool in retained {
        assert_eq!(pool.acquire().await.err(), Some(crate::PoolError::Closed));
    }
    let second = ExclusiveFixture::arm(directory.path(), FixtureScope::Default);
    assert!(matches!(second, Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists));
    let metadata = std::fs::metadata(&quarantine).expect("retained metadata");
    assert_eq!(
        (
            metadata.dev(),
            metadata.ino(),
            std::fs::read(&quarantine).expect("retained marker")
        ),
        before
    );
}

#[tokio::test]
async fn missing_query_observation_refuses_and_closes_retained_pool() {
    let directory = tempfile::tempdir().expect("owned fixture directory");
    let guard = ExclusiveFixture::arm(directory.path(), FixtureScope::DisposalProbe).expect("arm");
    let retained = guard.retain(lazy_pool()).expect("retained pool");
    let before = std::fs::read(&guard.quarantine).expect("published marker");
    let (sender, readiness) = tokio::sync::oneshot::channel();
    drop(sender);
    let failure = guard
        .supervise(FixtureDeadline::ObservedQuery(readiness), async {
            std::future::pending::<Result<(), &'static str>>().await
        })
        .await
        .expect_err("unobserved query must refuse");
    assert_eq!(failure.primary, Some("disposal_query_not_observed"));
    assert_eq!(failure.pool_closures, vec![true]);
    assert_eq!(
        retained.acquire().await.err(),
        Some(crate::PoolError::Closed)
    );
    assert_eq!(
        std::fs::read(&failure.quarantine).expect("retained marker"),
        before
    );
}

fn lazy_pool() -> PostgresPool {
    PostgresPool {
        pool: sqlx_postgres::PgPoolOptions::new().connect_lazy_with(
            sqlx_postgres::PgConnectOptions::new()
                .host("127.0.0.1")
                .port(1),
        ),
        acquire_timeout: Duration::from_secs(1),
    }
}
