//! Generated memories: stage-one outputs, consolidation jobs and the consolidation progress row.

use crate::domain::Domain;
use crate::domain::DomainOps;
use crate::source::SourceDatabase;
use crate::source::SqliteSource;
use crate::sqlite_target::SqliteTarget;
use anyhow::Result;
use serde::Serialize;
use sqlx::PgConnection;
use sqlx::Row;

const SEPARATOR: char = '\u{1f}';

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Stage1Record {
    thread_id: String,
    source_updated_at: i64,
    raw_memory: String,
    rollout_summary: String,
    rollout_slug: Option<String>,
    generated_at: i64,
    usage_count: Option<i64>,
    last_usage: Option<i64>,
    selected_for_phase2: i64,
    selected_for_phase2_source_updated_at: Option<i64>,
}

pub(crate) struct Stage1Outputs;

impl DomainOps for Stage1Outputs {
    type Record = Stage1Record;

    const DOMAIN: Domain = Domain::MemoryOutputs;

    fn key(record: &Stage1Record) -> String {
        record.thread_id.clone()
    }

    async fn export(
        source: &SqliteSource,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Stage1Record>> {
        let Some(pool) = source.pool(SourceDatabase::Memories).await? else {
            return Ok(Vec::new());
        };
        let rows = sqlx::query(
            "SELECT thread_id, source_updated_at, raw_memory, rollout_summary, rollout_slug, \
             generated_at, usage_count, last_usage, selected_for_phase2, \
             selected_for_phase2_source_updated_at FROM stage1_outputs \
             WHERE (?1 IS NULL OR thread_id > ?1) ORDER BY thread_id LIMIT ?2",
        )
        .bind(after)
        .bind(i64::try_from(limit)?)
        .fetch_all(&pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(Stage1Record {
                    thread_id: row.try_get("thread_id")?,
                    source_updated_at: row.try_get("source_updated_at")?,
                    raw_memory: row.try_get("raw_memory")?,
                    rollout_summary: row.try_get("rollout_summary")?,
                    rollout_slug: row.try_get("rollout_slug")?,
                    generated_at: row.try_get("generated_at")?,
                    usage_count: row.try_get("usage_count")?,
                    last_usage: row.try_get("last_usage")?,
                    selected_for_phase2: row.try_get("selected_for_phase2")?,
                    selected_for_phase2_source_updated_at: row
                        .try_get("selected_for_phase2_source_updated_at")?,
                })
            })
            .collect()
    }

    async fn import(connection: &mut PgConnection, records: &[Stage1Record]) -> Result<()> {
        for record in records {
            sqlx::query(
                "INSERT INTO memory_stage1_outputs (thread_id, source_updated_at, \
                 raw_memory, rollout_summary, rollout_slug, generated_at, usage_count, \
                 last_usage, selected_for_phase2, selected_for_phase2_source_updated_at) \
                 VALUES ($1::uuid, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
                 ON CONFLICT (thread_id) DO UPDATE SET \
                 source_updated_at = excluded.source_updated_at, \
                 raw_memory = excluded.raw_memory, rollout_summary = excluded.rollout_summary, \
                 rollout_slug = excluded.rollout_slug, generated_at = excluded.generated_at, \
                 usage_count = excluded.usage_count, last_usage = excluded.last_usage, \
                 selected_for_phase2 = excluded.selected_for_phase2, \
                 selected_for_phase2_source_updated_at = \
                 excluded.selected_for_phase2_source_updated_at",
            )
            .bind(&record.thread_id)
            .bind(record.source_updated_at)
            .bind(&record.raw_memory)
            .bind(&record.rollout_summary)
            .bind(&record.rollout_slug)
            .bind(record.generated_at)
            .bind(record.usage_count)
            .bind(record.last_usage)
            .bind(record.selected_for_phase2)
            .bind(record.selected_for_phase2_source_updated_at)
            .execute(&mut *connection)
            .await?;
        }
        Ok(())
    }

    async fn read_back(
        connection: &mut PgConnection,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Stage1Record>> {
        let rows = sqlx::query(
            "SELECT thread_id::text AS thread_id, source_updated_at, raw_memory, \
             rollout_summary, rollout_slug, generated_at, usage_count, last_usage, \
             selected_for_phase2, selected_for_phase2_source_updated_at \
             FROM memory_stage1_outputs \
             WHERE ($1::uuid IS NULL OR thread_id > $1::uuid) ORDER BY thread_id LIMIT $2",
        )
        .bind(after)
        .bind(i64::try_from(limit)?)
        .fetch_all(connection)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(Stage1Record {
                    thread_id: row.try_get("thread_id")?,
                    source_updated_at: row.try_get("source_updated_at")?,
                    raw_memory: row.try_get("raw_memory")?,
                    rollout_summary: row.try_get("rollout_summary")?,
                    rollout_slug: row.try_get("rollout_slug")?,
                    generated_at: row.try_get("generated_at")?,
                    usage_count: row.try_get("usage_count")?,
                    last_usage: row.try_get("last_usage")?,
                    selected_for_phase2: row.try_get("selected_for_phase2")?,
                    selected_for_phase2_source_updated_at: row
                        .try_get("selected_for_phase2_source_updated_at")?,
                })
            })
            .collect()
    }

    async fn write_sqlite(target: &SqliteTarget, records: &[Stage1Record]) -> Result<()> {
        for record in records {
            sqlx::query(
                "INSERT OR REPLACE INTO stage1_outputs (thread_id, source_updated_at, raw_memory, \
                 rollout_summary, rollout_slug, generated_at, usage_count, last_usage, \
                 selected_for_phase2, selected_for_phase2_source_updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&record.thread_id)
            .bind(record.source_updated_at)
            .bind(&record.raw_memory)
            .bind(&record.rollout_summary)
            .bind(&record.rollout_slug)
            .bind(record.generated_at)
            .bind(record.usage_count)
            .bind(record.last_usage)
            .bind(record.selected_for_phase2)
            .bind(record.selected_for_phase2_source_updated_at)
            .execute(&target.memories)
            .await?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct JobRecord {
    kind: String,
    job_key: String,
    status: String,
    worker_id: Option<String>,
    ownership_token: Option<String>,
    started_at: Option<i64>,
    finished_at: Option<i64>,
    lease_until: Option<i64>,
    retry_at: Option<i64>,
    retry_remaining: i64,
    last_error: Option<String>,
    input_watermark: Option<i64>,
    last_success_watermark: Option<i64>,
}

pub(crate) struct MemoryJobs;

fn split_job_cursor(after: Option<&str>) -> (Option<&str>, &str) {
    match after.and_then(|cursor| cursor.split_once(SEPARATOR)) {
        Some((kind, job_key)) => (Some(kind), job_key),
        None => (None, ""),
    }
}

fn job_from_row<R>(row: &R) -> Result<JobRecord>
where
    R: Row,
    for<'r> i64: sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
    for<'r> String: sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
    for<'r> Option<i64>: sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
    for<'r> Option<String>: sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
    for<'r> &'r str: sqlx::ColumnIndex<R>,
{
    Ok(JobRecord {
        kind: row.try_get("kind")?,
        job_key: row.try_get("job_key")?,
        status: row.try_get("status")?,
        worker_id: row.try_get("worker_id")?,
        ownership_token: row.try_get("ownership_token")?,
        started_at: row.try_get("started_at")?,
        finished_at: row.try_get("finished_at")?,
        lease_until: row.try_get("lease_until")?,
        retry_at: row.try_get("retry_at")?,
        retry_remaining: row.try_get("retry_remaining")?,
        last_error: row.try_get("last_error")?,
        input_watermark: row.try_get("input_watermark")?,
        last_success_watermark: row.try_get("last_success_watermark")?,
    })
}

impl DomainOps for MemoryJobs {
    type Record = JobRecord;

    const DOMAIN: Domain = Domain::MemoryJobs;

    fn key(record: &JobRecord) -> String {
        format!("{}{SEPARATOR}{}", record.kind, record.job_key)
    }

    async fn export(
        source: &SqliteSource,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<JobRecord>> {
        let Some(pool) = source.pool(SourceDatabase::Memories).await? else {
            return Ok(Vec::new());
        };
        let (kind, job_key) = split_job_cursor(after);
        let rows = sqlx::query(
            "SELECT kind, job_key, status, worker_id, ownership_token, started_at, finished_at, \
             lease_until, retry_at, retry_remaining, last_error, input_watermark, \
             last_success_watermark FROM jobs \
             WHERE (?1 IS NULL OR (kind, job_key) > (?1, ?2)) \
             ORDER BY kind, job_key LIMIT ?3",
        )
        .bind(kind)
        .bind(job_key)
        .bind(i64::try_from(limit)?)
        .fetch_all(&pool)
        .await?;
        rows.iter().map(job_from_row).collect()
    }

    async fn import(connection: &mut PgConnection, records: &[JobRecord]) -> Result<()> {
        for record in records {
            sqlx::query(
                "INSERT INTO memory_jobs (kind, job_key, status, worker_id, \
                 ownership_token, started_at, finished_at, lease_until, retry_at, \
                 retry_remaining, last_error, input_watermark, last_success_watermark) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) \
                 ON CONFLICT (kind, job_key) DO UPDATE SET status = excluded.status, \
                 worker_id = excluded.worker_id, ownership_token = excluded.ownership_token, \
                 started_at = excluded.started_at, finished_at = excluded.finished_at, \
                 lease_until = excluded.lease_until, retry_at = excluded.retry_at, \
                 retry_remaining = excluded.retry_remaining, last_error = excluded.last_error, \
                 input_watermark = excluded.input_watermark, \
                 last_success_watermark = excluded.last_success_watermark",
            )
            .bind(&record.kind)
            .bind(&record.job_key)
            .bind(&record.status)
            .bind(&record.worker_id)
            .bind(&record.ownership_token)
            .bind(record.started_at)
            .bind(record.finished_at)
            .bind(record.lease_until)
            .bind(record.retry_at)
            .bind(record.retry_remaining)
            .bind(&record.last_error)
            .bind(record.input_watermark)
            .bind(record.last_success_watermark)
            .execute(&mut *connection)
            .await?;
        }
        Ok(())
    }

    async fn read_back(
        connection: &mut PgConnection,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<JobRecord>> {
        let (kind, job_key) = split_job_cursor(after);
        let rows = sqlx::query(
            "SELECT kind, job_key, status, worker_id, ownership_token, started_at, finished_at, \
             lease_until, retry_at, retry_remaining, last_error, input_watermark, \
             last_success_watermark FROM memory_jobs \
             WHERE ($1::text IS NULL OR (kind COLLATE \"C\", job_key COLLATE \"C\") > \
             ($1 COLLATE \"C\", $2 COLLATE \"C\")) \
             ORDER BY kind COLLATE \"C\", job_key COLLATE \"C\" LIMIT $3",
        )
        .bind(kind)
        .bind(job_key)
        .bind(i64::try_from(limit)?)
        .fetch_all(connection)
        .await?;
        rows.iter().map(job_from_row).collect()
    }

    async fn write_sqlite(target: &SqliteTarget, records: &[JobRecord]) -> Result<()> {
        for record in records {
            sqlx::query(
                "INSERT OR REPLACE INTO jobs (kind, job_key, status, worker_id, ownership_token, \
                 started_at, finished_at, lease_until, retry_at, retry_remaining, last_error, \
                 input_watermark, last_success_watermark) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&record.kind)
            .bind(&record.job_key)
            .bind(&record.status)
            .bind(&record.worker_id)
            .bind(&record.ownership_token)
            .bind(record.started_at)
            .bind(record.finished_at)
            .bind(record.lease_until)
            .bind(record.retry_at)
            .bind(record.retry_remaining)
            .bind(&record.last_error)
            .bind(record.input_watermark)
            .bind(record.last_success_watermark)
            .execute(&target.memories)
            .await?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ProgressRecord {
    max_thread_count: i64,
}

pub(crate) struct MemoryProgress;

impl DomainOps for MemoryProgress {
    type Record = ProgressRecord;

    const DOMAIN: Domain = Domain::MemoryProgress;

    fn key(_record: &ProgressRecord) -> String {
        "1".to_string()
    }

    async fn export(
        source: &SqliteSource,
        after: Option<&str>,
        _limit: usize,
    ) -> Result<Vec<ProgressRecord>> {
        if after.is_some() {
            return Ok(Vec::new());
        }
        // A fresh target always holds the seeded row, so an absent source reads as that row.
        let Some(pool) = source.pool(SourceDatabase::Memories).await? else {
            return Ok(vec![ProgressRecord {
                max_thread_count: 0,
            }]);
        };
        let rows = sqlx::query("SELECT max_thread_count FROM consolidation_progress")
            .fetch_all(&pool)
            .await?;
        rows.iter()
            .map(|row| {
                Ok(ProgressRecord {
                    max_thread_count: row.try_get("max_thread_count")?,
                })
            })
            .collect()
    }

    async fn import(connection: &mut PgConnection, records: &[ProgressRecord]) -> Result<()> {
        for record in records {
            sqlx::query(
                "UPDATE memory_consolidation_progress \
                 SET max_thread_count = $1 WHERE singleton",
            )
            .bind(record.max_thread_count)
            .execute(&mut *connection)
            .await?;
        }
        Ok(())
    }

    async fn read_back(
        connection: &mut PgConnection,
        after: Option<&str>,
        _limit: usize,
    ) -> Result<Vec<ProgressRecord>> {
        if after.is_some() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query("SELECT max_thread_count FROM memory_consolidation_progress")
            .fetch_all(connection)
            .await?;
        rows.iter()
            .map(|row| {
                Ok(ProgressRecord {
                    max_thread_count: row.try_get("max_thread_count")?,
                })
            })
            .collect()
    }

    async fn write_sqlite(target: &SqliteTarget, records: &[ProgressRecord]) -> Result<()> {
        for record in records {
            sqlx::query("UPDATE consolidation_progress SET max_thread_count = ?")
                .bind(record.max_thread_count)
                .execute(&target.memories)
                .await?;
        }
        Ok(())
    }
}
