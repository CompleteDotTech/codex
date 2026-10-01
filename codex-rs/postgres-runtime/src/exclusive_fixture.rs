//! Private destructive-fixture supervisor; quarantine is cleared only by external verified recovery.
use crate::PostgresPool;
use std::future::Future;
#[cfg(unix)]
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

#[derive(Clone, Copy)]
pub(crate) enum FixtureScope {
    Default,
    Named,
    DisposalProbe,
}
pub(crate) enum FixtureDeadline {
    Upgrade,
    ObservedQuery(tokio::sync::oneshot::Receiver<()>),
}

#[derive(Clone)]
pub(crate) struct ExclusiveFixture {
    pools: Arc<Mutex<Vec<Arc<PostgresPool>>>>,
    quarantine: PathBuf,
}
impl ExclusiveFixture {
    pub(crate) fn arm(state: &Path, scope: FixtureScope) -> std::io::Result<Self> {
        #[cfg(not(unix))]
        {
            let _ = (state, scope);
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "durable fixture marker directory sync is not qualified on this platform",
            ));
        }
        #[cfg(unix)]
        {
            let quarantine = state.join(match scope {
                FixtureScope::Default => "postgres-default-qualification.quarantine.json",
                FixtureScope::Named => "postgres-named-qualification.quarantine.json",
                FixtureScope::DisposalProbe => "postgres-disposal-qualification.quarantine.json",
            });
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&quarantine)?;
            file.write_all(b"{\"state\":\"armed\",\"scope\":\"exclusive-postgres-fixture\",\"recovery_requires_full_identity_catalog_and_zero_clients\":true}\n")?;
            file.sync_all()?;
            drop(file);
            std::fs::File::open(state)?.sync_all()?;
            Ok(Self {
                pools: Arc::new(Mutex::new(Vec::new())),
                quarantine,
            })
        }
    }
    pub(crate) fn retain(&self, pool: PostgresPool) -> Result<Arc<PostgresPool>, &'static str> {
        let pool = Arc::new(pool);
        let mut pools = self.pools.lock().map_err(|_| "fixture_registry_poisoned")?;
        pools.push(pool.clone());
        Ok(pool)
    }
    /// The owning launcher must retain and fully join this supervisor through terminal disposal.
    /// Dropping/cancelling this future can detach the exercise; it is NOT a generic cancellation-safe API.
    /// An interrupted launcher waits for its native controller and leaves quarantine armed.
    pub(crate) async fn supervise<T, F>(
        &self,
        deadline: FixtureDeadline,
        exercise: F,
    ) -> Result<T, FixtureFailure>
    where
        T: Send + 'static,
        F: Future<Output = Result<T, &'static str>> + Send + 'static,
    {
        let task = tokio::spawn(async move {
            match deadline {
                FixtureDeadline::Upgrade => {
                    tokio::time::timeout(Duration::from_secs(/*secs*/ 35), exercise).await
                }
                FixtureDeadline::ObservedQuery(readiness) => {
                    tokio::pin!(exercise);
                    let observed = tokio::select! {
                        result=&mut exercise=>return Ok(result),
                        proof=tokio::time::timeout(Duration::from_secs(/*secs*/ 8),readiness)=>proof,
                    };
                    if !matches!(observed, Ok(Ok(()))) {
                        return Ok(Err("disposal_query_not_observed"));
                    }
                    tokio::time::timeout(Duration::from_secs(/*secs*/ 1), exercise).await
                }
            }
        });
        let outcome = task.await;
        let primary = match &outcome {
            Err(_) => Some("fixture_exercise_panicked"),
            Ok(Err(_)) => Some("fixture_exercise_timeout"),
            Ok(Ok(Err(code))) => Some(*code),
            Ok(Ok(Ok(_))) => None,
        };
        let pools = self.pools.lock().map(|pools| pools.clone());
        let mut closures = Vec::new();
        if let Ok(pools) = pools {
            let tasks: Vec<_> = pools
                .into_iter()
                .map(|pool| {
                    tokio::spawn(async move {
                        tokio::time::timeout(Duration::from_secs(/*secs*/ 5), pool.close())
                            .await
                            .is_ok_and(|result| result.is_ok())
                    })
                })
                .collect();
            for task in tasks {
                closures.push(task.await.is_ok_and(|closed| closed));
            }
        } else {
            closures.push(false);
        }
        // No marker deletion: even nominal success needs independent server quiescence/full-state proof.
        let successful_closure = closures.iter().all(|closed| *closed);
        match outcome {
            Ok(Ok(Ok(value))) if successful_closure => Ok(value),
            _ => Err(FixtureFailure {
                quarantine: self.quarantine.clone(),
                primary,
                pool_closures: closures,
            }),
        }
    }
}
#[derive(Debug)]
pub(crate) struct FixtureFailure {
    pub(crate) quarantine: PathBuf,
    pub(crate) primary: Option<&'static str>,
    pub(crate) pool_closures: Vec<bool>,
}

#[cfg(test)]
#[path = "exclusive_fixture_tests.rs"]
mod tests;
