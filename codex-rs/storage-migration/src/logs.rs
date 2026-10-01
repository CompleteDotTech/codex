//! Runtime log rows, keeping their original ids so ordering and pagination survive the move.

use crate::domain::Domain;
use crate::domain::DomainOps;
use crate::source::SourceDatabase;
use crate::source::SqliteSource;
use anyhow::Result;
use serde::Serialize;
use sqlx::PgConnection;
use sqlx::Row;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct LogRecord {
    id: i64,
    ts: i64,
    ts_nanos: i64,
    level: String,
    target: String,
    feedback_log_body: Option<String>,
    module_path: Option<String>,
    file: Option<String>,
    line: Option<i64>,
    thread_id: Option<String>,
    process_uuid: Option<String>,
    estimated_bytes: i64,
}

pub(crate) struct Logs;

fn after_id(after: Option<&str>) -> Result<i64> {
    Ok(after
        .map(str::parse::<i64>)
        .transpose()?
        .unwrap_or(i64::MIN))
}

fn from_row<R>(row: &R) -> Result<LogRecord>
where
    R: Row,
    for<'r> i64: sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
    for<'r> String: sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
    for<'r> Option<i64>: sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
    for<'r> Option<String>: sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
    for<'r> &'r str: sqlx::ColumnIndex<R>,
{
    Ok(LogRecord {
        id: row.try_get("id")?,
        ts: row.try_get("ts")?,
        ts_nanos: row.try_get("ts_nanos")?,
        level: row.try_get("level")?,
        target: row.try_get("target")?,
        feedback_log_body: row.try_get("feedback_log_body")?,
        module_path: row.try_get("module_path")?,
        file: row.try_get("file")?,
        line: row.try_get("line")?,
        thread_id: row.try_get("thread_id")?,
        process_uuid: row.try_get("process_uuid")?,
        estimated_bytes: row.try_get("estimated_bytes")?,
    })
}

impl DomainOps for Logs {
    type Record = LogRecord;

    const DOMAIN: Domain = Domain::Logs;

    fn key(record: &LogRecord) -> String {
        record.id.to_string()
    }

    async fn export(
        source: &SqliteSource,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<LogRecord>> {
        let Some(pool) = source.pool(SourceDatabase::Logs).await? else {
            return Ok(Vec::new());
        };
        let rows = sqlx::query(
            "SELECT id, ts, ts_nanos, level, target, feedback_log_body, module_path, file, \
             line, thread_id, process_uuid, estimated_bytes FROM logs \
             WHERE id > ?1 ORDER BY id LIMIT ?2",
        )
        .bind(after_id(after)?)
        .bind(i64::try_from(limit)?)
        .fetch_all(&pool)
        .await?;
        rows.iter().map(from_row).collect()
    }

    async fn import(connection: &mut PgConnection, records: &[LogRecord]) -> Result<()> {
        for record in records {
            // The runtime role cannot rewrite log rows, and log rows never change, so a replay
            // leaves the row it already wrote.
            sqlx::query(
                "INSERT INTO logs (id, ts, ts_nanos, level, target, \
                 feedback_log_body, module_path, file, line, thread_id, process_uuid, \
                 estimated_bytes) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) \
                 ON CONFLICT (id) DO NOTHING",
            )
            .bind(record.id)
            .bind(record.ts)
            .bind(record.ts_nanos)
            .bind(&record.level)
            .bind(&record.target)
            .bind(&record.feedback_log_body)
            .bind(&record.module_path)
            .bind(&record.file)
            .bind(record.line)
            .bind(&record.thread_id)
            .bind(&record.process_uuid)
            .bind(record.estimated_bytes)
            .execute(&mut *connection)
            .await?;
        }
        // New rows must be numbered after every imported id.
        if let Some(highest) = records.iter().map(|record| record.id).max() {
            sqlx::query(
                "UPDATE log_id_counter \
                 SET last_id = GREATEST(last_id, $1) WHERE singleton",
            )
            .bind(highest)
            .execute(&mut *connection)
            .await?;
        }
        Ok(())
    }

    async fn read_back(
        connection: &mut PgConnection,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<LogRecord>> {
        let rows = sqlx::query(
            "SELECT id, ts, ts_nanos, level, target, feedback_log_body, module_path, file, \
             line, thread_id, process_uuid, estimated_bytes FROM logs \
             WHERE id > $1 ORDER BY id LIMIT $2",
        )
        .bind(after_id(after)?)
        .bind(i64::try_from(limit)?)
        .fetch_all(connection)
        .await?;
        rows.iter().map(from_row).collect()
    }
}
