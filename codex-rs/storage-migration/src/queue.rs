//! Durable queued messages and the per-thread change revisions that watchers follow.

use crate::domain::Domain;
use crate::domain::DomainOps;
use crate::source::SourceDatabase;
use crate::source::SqliteSource;
use crate::sqlite_target::SqliteTarget;
use anyhow::Result;
use serde::Serialize;
use sqlx::PgConnection;
use sqlx::Row;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct QueuedItemRecord {
    id: String,
    thread_id: String,
    payload_json: String,
    queue_order: i64,
    created_at_ms: i64,
    updated_at_ms: i64,
}

pub(crate) struct QueuedItems;

impl DomainOps for QueuedItems {
    type Record = QueuedItemRecord;

    const DOMAIN: Domain = Domain::QueuedItems;

    fn key(record: &QueuedItemRecord) -> String {
        record.id.clone()
    }

    async fn export(
        source: &SqliteSource,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<QueuedItemRecord>> {
        let Some(pool) = source.pool(SourceDatabase::Queue).await? else {
            return Ok(Vec::new());
        };
        let rows = sqlx::query(
            "SELECT id, thread_id, payload_json, queue_order, created_at_ms, updated_at_ms \
             FROM queued_items WHERE (?1 IS NULL OR id > ?1) ORDER BY id LIMIT ?2",
        )
        .bind(after)
        .bind(i64::try_from(limit)?)
        .fetch_all(&pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(QueuedItemRecord {
                    id: row.try_get("id")?,
                    thread_id: row.try_get("thread_id")?,
                    payload_json: row.try_get("payload_json")?,
                    queue_order: row.try_get("queue_order")?,
                    created_at_ms: row.try_get("created_at_ms")?,
                    updated_at_ms: row.try_get("updated_at_ms")?,
                })
            })
            .collect()
    }

    async fn import(connection: &mut PgConnection, records: &[QueuedItemRecord]) -> Result<()> {
        for record in records {
            // Orders are unique per thread, so a replay that finds the item already present
            // must update it in place instead of inserting a second copy.
            sqlx::query(
                "INSERT INTO queued_items \
                 (id, thread_id, payload_json, queue_order, created_at_ms, updated_at_ms) \
                 VALUES ($1, $2::uuid, $3, $4, $5, $6) \
                 ON CONFLICT (id) DO UPDATE SET thread_id = excluded.thread_id, \
                 payload_json = excluded.payload_json, queue_order = excluded.queue_order, \
                 created_at_ms = excluded.created_at_ms, updated_at_ms = excluded.updated_at_ms",
            )
            .bind(&record.id)
            .bind(&record.thread_id)
            .bind(&record.payload_json)
            .bind(record.queue_order)
            .bind(record.created_at_ms)
            .bind(record.updated_at_ms)
            .execute(&mut *connection)
            .await?;
        }
        Ok(())
    }

    async fn read_back(
        connection: &mut PgConnection,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<QueuedItemRecord>> {
        let rows = sqlx::query(
            "SELECT id, thread_id::text AS thread_id, payload_json, queue_order, \
             created_at_ms, updated_at_ms FROM queued_items \
             WHERE ($1::text IS NULL OR id > $1) ORDER BY id LIMIT $2",
        )
        .bind(after)
        .bind(i64::try_from(limit)?)
        .fetch_all(connection)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(QueuedItemRecord {
                    id: row.try_get("id")?,
                    thread_id: row.try_get("thread_id")?,
                    payload_json: row.try_get("payload_json")?,
                    queue_order: row.try_get("queue_order")?,
                    created_at_ms: row.try_get("created_at_ms")?,
                    updated_at_ms: row.try_get("updated_at_ms")?,
                })
            })
            .collect()
    }

    async fn write_sqlite(target: &SqliteTarget, records: &[QueuedItemRecord]) -> Result<()> {
        for record in records {
            sqlx::query(
                "INSERT OR REPLACE INTO queued_items \
                 (id, thread_id, payload_json, queue_order, created_at_ms, updated_at_ms) \
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(&record.id)
            .bind(&record.thread_id)
            .bind(&record.payload_json)
            .bind(record.queue_order)
            .bind(record.created_at_ms)
            .bind(record.updated_at_ms)
            .execute(&target.queue)
            .await?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct RevisionRecord {
    thread_id: String,
    revision: i64,
}

pub(crate) struct QueueRevisions;

impl DomainOps for QueueRevisions {
    type Record = RevisionRecord;

    const DOMAIN: Domain = Domain::QueueRevisions;

    fn key(record: &RevisionRecord) -> String {
        record.thread_id.clone()
    }

    async fn export(
        source: &SqliteSource,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<RevisionRecord>> {
        let Some(pool) = source.pool(SourceDatabase::Queue).await? else {
            return Ok(Vec::new());
        };
        let rows = sqlx::query(
            "SELECT thread_id, revision FROM queued_thread_revisions \
             WHERE (?1 IS NULL OR thread_id > ?1) ORDER BY thread_id LIMIT ?2",
        )
        .bind(after)
        .bind(i64::try_from(limit)?)
        .fetch_all(&pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(RevisionRecord {
                    thread_id: row.try_get("thread_id")?,
                    revision: row.try_get("revision")?,
                })
            })
            .collect()
    }

    async fn import(connection: &mut PgConnection, records: &[RevisionRecord]) -> Result<()> {
        for record in records {
            sqlx::query(
                "INSERT INTO queued_thread_revisions (thread_id, revision) \
                 VALUES ($1::uuid, $2) ON CONFLICT (thread_id) DO UPDATE \
                 SET revision = excluded.revision",
            )
            .bind(&record.thread_id)
            .bind(record.revision)
            .execute(&mut *connection)
            .await?;
        }
        // New changes must be numbered after every imported revision.
        if let Some(highest) = records.iter().map(|record| record.revision).max() {
            sqlx::query(
                "UPDATE queue_change_counter \
                 SET version = GREATEST(version, $1) WHERE singleton",
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
    ) -> Result<Vec<RevisionRecord>> {
        let rows = sqlx::query(
            "SELECT thread_id::text AS thread_id, revision \
             FROM queued_thread_revisions \
             WHERE ($1::uuid IS NULL OR thread_id > $1::uuid) ORDER BY thread_id LIMIT $2",
        )
        .bind(after)
        .bind(i64::try_from(limit)?)
        .fetch_all(connection)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(RevisionRecord {
                    thread_id: row.try_get("thread_id")?,
                    revision: row.try_get("revision")?,
                })
            })
            .collect()
    }

    async fn write_sqlite(target: &SqliteTarget, records: &[RevisionRecord]) -> Result<()> {
        // Writing items fired the revision triggers, so the recorded revisions replace whatever
        // they produced.
        for record in records {
            sqlx::query("DELETE FROM queued_thread_revisions WHERE thread_id = ?")
                .bind(&record.thread_id)
                .execute(&target.queue)
                .await?;
            sqlx::query("INSERT INTO queued_thread_revisions (revision, thread_id) VALUES (?, ?)")
                .bind(record.revision)
                .bind(&record.thread_id)
                .execute(&target.queue)
                .await?;
        }
        Ok(())
    }
}
