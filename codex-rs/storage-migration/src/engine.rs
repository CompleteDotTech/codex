//! The resumable migration engine.
//!
//! A run claims the store through the activation row, writes each domain in key order with a
//! checkpoint committed in the same transaction as every batch, and then verifies by comparing
//! the source and target digest of every domain. Replaying a batch is harmless because every
//! write is an upsert, so a crash or a lost commit result only costs a repeat.

use crate::attachments::Attachments;
use crate::attachments::SpawnEdges;
use crate::digest::DigestBuilder;
use crate::digest::DomainDigest;
use crate::domain::Domain;
use crate::domain::DomainOps;
use crate::goals::Goals;
use crate::projects::ProjectKeys;
use crate::projects::Projects;
use crate::queue::QueueRevisions;
use crate::queue::QueuedItems;
use crate::sections::Sections;
use crate::source::SqliteSource;
use crate::threads::Threads;
use codex_postgres_runtime::PostgresPool;
use sqlx::Acquire;
use sqlx::PgConnection;
use sqlx::Row;
use std::sync::Arc;
use thiserror::Error;
use uuid::Uuid;

const DEFAULT_BATCH: usize = 500;

/// Run one expression for the domain's operations without repeating the list of domains.
macro_rules! with_domain {
    ($domain:expr, $ops:ident => $body:expr) => {
        match $domain {
            Domain::Sections => {
                type $ops = Sections;
                $body
            }
            Domain::Projects => {
                type $ops = Projects;
                $body
            }
            Domain::ProjectKeys => {
                type $ops = ProjectKeys;
                $body
            }
            Domain::Threads => {
                type $ops = Threads;
                $body
            }
            Domain::Attachments => {
                type $ops = Attachments;
                $body
            }
            Domain::SpawnEdges => {
                type $ops = SpawnEdges;
                $body
            }
            Domain::Goals => {
                type $ops = Goals;
                $body
            }
            Domain::QueuedItems => {
                type $ops = QueuedItems;
                $body
            }
            Domain::QueueRevisions => {
                type $ops = QueueRevisions;
                $body
            }
        }
    };
}

/// Failures never carry connection details or row contents.
#[derive(Debug, Error)]
pub enum MigrationError {
    #[error("the source could not be read: {0}")]
    Source(String),
    #[error("the target rejected the migration: {0}")]
    Target(String),
    #[error("another migration holds the target store")]
    TargetBusy,
    #[error("{domain} differs between the source and the target")]
    Mismatch { domain: &'static str },
    #[error("the run stopped after its batch limit and can be resumed")]
    Interrupted,
}

fn target(error: impl std::fmt::Display) -> MigrationError {
    MigrationError::Target(error.to_string())
}

fn source(error: impl std::fmt::Display) -> MigrationError {
    MigrationError::Source(error.to_string())
}

/// What a run moved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunSummary {
    pub run_id: Uuid,
    pub resumed: bool,
    pub domains: Vec<(Domain, u64)>,
}

/// Source and target digests per domain, equal when verification passes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerificationReport {
    pub run_id: Uuid,
    pub domains: Vec<(Domain, DomainDigest)>,
}

/// Moves a SQLite home into a PostgreSQL store.
pub struct Migrator {
    source: SqliteSource,
    target: Arc<PostgresPool>,
    batch_size: usize,
    batch_limit: Option<usize>,
}

impl Migrator {
    pub fn new(source: SqliteSource, target: Arc<PostgresPool>) -> Self {
        Self {
            source,
            target,
            batch_size: DEFAULT_BATCH,
            batch_limit: None,
        }
    }

    /// Smaller batches checkpoint more often; tests use them to exercise resumption.
    pub fn with_batch_size(mut self, batch_size: usize) -> Self {
        self.batch_size = batch_size.max(1);
        self
    }

    /// Stop after this many committed batches. The run keeps its checkpoints, so importing
    /// again continues where this one stopped.
    pub fn with_batch_limit(mut self, batch_limit: usize) -> Self {
        self.batch_limit = Some(batch_limit);
        self
    }

    /// Claim the store and write every domain, resuming a run that was interrupted.
    pub async fn import(&self) -> Result<RunSummary, MigrationError> {
        let fingerprint = self.source.fingerprint().await.map_err(source)?;
        let (run_id, resumed) = self.begin(&fingerprint).await?;
        let mut domains = Vec::new();
        let mut batches = 0;
        for domain in Domain::ALL {
            let moved = with_domain!(
                *domain,
                Ops => self.import_domain::<Ops>(run_id, &mut batches).await?
            );
            domains.push((*domain, moved));
        }
        Ok(RunSummary {
            run_id,
            resumed,
            domains,
        })
    }

    /// Prove the target holds exactly what the source holds, then mark the run verified.
    pub async fn verify(&self, run_id: Uuid) -> Result<VerificationReport, MigrationError> {
        let mut domains = Vec::new();
        for domain in Domain::ALL {
            let (source_digest, target_digest) =
                with_domain!(*domain, Ops => self.digests::<Ops>().await?);
            if source_digest != target_digest {
                return Err(MigrationError::Mismatch {
                    domain: domain.name(),
                });
            }
            self.record_digest(run_id, *domain, &target_digest).await?;
            domains.push((*domain, target_digest));
        }
        self.set_run_state(run_id, "verified").await?;
        Ok(VerificationReport { run_id, domains })
    }

    async fn connection(
        &self,
    ) -> Result<sqlx::pool::PoolConnection<sqlx::Postgres>, MigrationError> {
        self.target
            .acquire()
            .await
            .map_err(|error| MigrationError::Target(format!("{error:?}")))
    }

    /// Take the store: only one run may hold it, and an interrupted run of the same source
    /// resumes instead of starting over.
    async fn begin(&self, fingerprint: &str) -> Result<(Uuid, bool), MigrationError> {
        let mut connection = self.connection().await?;
        let mut tx = connection.begin().await.map_err(target)?;
        let row = sqlx::query(
            "SELECT state, run_id FROM codex_storage.storage_activation WHERE singleton FOR UPDATE",
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(target)?;
        let state: String = row.try_get("state").map_err(target)?;
        let held: Option<Uuid> = row.try_get("run_id").map_err(target)?;
        let now = chrono::Utc::now().timestamp_millis();
        let outcome = if state == "migrating" {
            let run_id = held.ok_or(MigrationError::TargetBusy)?;
            let existing = sqlx::query(
                "SELECT source_fingerprint, state FROM codex_storage.storage_migration_runs \
                 WHERE run_id = $1",
            )
            .bind(run_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(target)?;
            let resumable = existing.is_some_and(|run| {
                run.try_get::<String, _>("source_fingerprint")
                    .ok()
                    .as_deref()
                    == Some(fingerprint)
                    && run.try_get::<String, _>("state").ok().as_deref() == Some("running")
            });
            if !resumable {
                return Err(MigrationError::TargetBusy);
            }
            (run_id, true)
        } else {
            let run_id = Uuid::now_v7();
            sqlx::query(
                "INSERT INTO codex_storage.storage_migration_runs \
                 (run_id, direction, source_fingerprint, state, started_at_ms, updated_at_ms) \
                 VALUES ($1, 'import', $2, 'running', $3, $3)",
            )
            .bind(run_id)
            .bind(fingerprint)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(target)?;
            sqlx::query(
                "UPDATE codex_storage.storage_activation \
                 SET state = 'migrating', run_id = $1, updated_at_ms = $2 WHERE singleton",
            )
            .bind(run_id)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(target)?;
            (run_id, false)
        };
        tx.commit().await.map_err(target)?;
        Ok(outcome)
    }

    async fn set_run_state(&self, run_id: Uuid, state: &str) -> Result<(), MigrationError> {
        let mut connection = self.connection().await?;
        sqlx::query(
            "UPDATE codex_storage.storage_migration_runs SET state = $2, updated_at_ms = $3 \
             WHERE run_id = $1",
        )
        .bind(run_id)
        .bind(state)
        .bind(chrono::Utc::now().timestamp_millis())
        .execute(&mut *connection)
        .await
        .map_err(target)?;
        Ok(())
    }

    async fn record_digest(
        &self,
        run_id: Uuid,
        domain: Domain,
        digest: &DomainDigest,
    ) -> Result<(), MigrationError> {
        let mut connection = self.connection().await?;
        sqlx::query(
            "INSERT INTO codex_storage.storage_migration_domains \
             (run_id, domain, done, row_count, digest) VALUES ($1, $2, TRUE, $3, $4) \
             ON CONFLICT (run_id, domain) DO UPDATE SET done = TRUE, row_count = $3, digest = $4",
        )
        .bind(run_id)
        .bind(domain.name())
        .bind(i64::try_from(digest.count).unwrap_or(i64::MAX))
        .bind(&digest.digest)
        .execute(&mut *connection)
        .await
        .map_err(target)?;
        Ok(())
    }

    /// Write one domain in key order, committing a checkpoint with every batch.
    async fn import_domain<D: DomainOps>(
        &self,
        run_id: Uuid,
        batches: &mut usize,
    ) -> Result<u64, MigrationError> {
        let (mut cursor, done, mut moved) = {
            let mut connection = self.connection().await?;
            let row = sqlx::query(
                "SELECT resume_cursor, done, row_count \
                 FROM codex_storage.storage_migration_domains WHERE run_id = $1 AND domain = $2",
            )
            .bind(run_id)
            .bind(D::DOMAIN.name())
            .fetch_optional(&mut *connection)
            .await
            .map_err(target)?;
            match row {
                Some(row) => (
                    row.try_get::<Option<String>, _>("resume_cursor")
                        .map_err(target)?,
                    row.try_get::<bool, _>("done").map_err(target)?,
                    row.try_get::<i64, _>("row_count").map_err(target)? as u64,
                ),
                None => (None, false, 0),
            }
        };
        if done {
            return Ok(moved);
        }
        loop {
            let records = D::export(&self.source, cursor.as_deref(), self.batch_size)
                .await
                .map_err(source)?;
            let last = records.last().map(D::key);
            let mut connection = self.connection().await?;
            let mut tx = connection.begin().await.map_err(target)?;
            D::import(&mut tx, &records).await.map_err(target)?;
            moved += records.len() as u64;
            let finished = records.len() < self.batch_size;
            checkpoint(
                &mut tx,
                run_id,
                D::DOMAIN,
                last.as_deref().or(cursor.as_deref()),
                finished,
                moved,
            )
            .await?;
            tx.commit().await.map_err(target)?;
            *batches += 1;
            if finished {
                return Ok(moved);
            }
            if self.batch_limit.is_some_and(|limit| *batches >= limit) {
                return Err(MigrationError::Interrupted);
            }
            cursor = last;
        }
    }

    /// The source digest and the digest of what the target now holds, for one domain.
    async fn digests<D: DomainOps>(&self) -> Result<(DomainDigest, DomainDigest), MigrationError> {
        let mut from_source = DigestBuilder::new();
        let mut cursor: Option<String> = None;
        loop {
            let records = D::export(&self.source, cursor.as_deref(), self.batch_size)
                .await
                .map_err(source)?;
            for record in &records {
                from_source.add(record).map_err(source)?;
            }
            let Some(last) = records.last() else { break };
            cursor = Some(D::key(last));
            if records.len() < self.batch_size {
                break;
            }
        }
        let mut from_target = DigestBuilder::new();
        let mut connection = self.connection().await?;
        let mut cursor: Option<String> = None;
        loop {
            let records = D::read_back(&mut connection, cursor.as_deref(), self.batch_size)
                .await
                .map_err(target)?;
            for record in &records {
                from_target.add(record).map_err(target)?;
            }
            let Some(last) = records.last() else { break };
            cursor = Some(D::key(last));
            if records.len() < self.batch_size {
                break;
            }
        }
        Ok((from_source.finish(), from_target.finish()))
    }
}

async fn checkpoint(
    connection: &mut PgConnection,
    run_id: Uuid,
    domain: Domain,
    cursor: Option<&str>,
    done: bool,
    moved: u64,
) -> Result<(), MigrationError> {
    sqlx::query(
        "INSERT INTO codex_storage.storage_migration_domains \
         (run_id, domain, resume_cursor, done, row_count) VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (run_id, domain) DO UPDATE \
         SET resume_cursor = $3, done = $4, row_count = $5",
    )
    .bind(run_id)
    .bind(domain.name())
    .bind(cursor)
    .bind(done)
    .bind(i64::try_from(moved).unwrap_or(i64::MAX))
    .execute(connection)
    .await
    .map_err(target)?;
    Ok(())
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
