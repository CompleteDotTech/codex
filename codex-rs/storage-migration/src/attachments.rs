//! Thread attachments and the directional spawn edges between threads.

use crate::domain::Domain;
use crate::domain::DomainOps;
use crate::source::SourceDatabase;
use crate::source::SqliteSource;
use anyhow::Result;
use serde::Serialize;
use sqlx::PgConnection;
use sqlx::Row;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct AttachmentRecord {
    id: String,
    thread_id: String,
    attachment_type: String,
    identity_key: String,
    payload: String,
    created_at: i64,
}

pub(crate) struct Attachments;

impl DomainOps for Attachments {
    type Record = AttachmentRecord;

    const DOMAIN: Domain = Domain::Attachments;

    fn key(record: &AttachmentRecord) -> String {
        record.id.clone()
    }

    async fn export(
        source: &SqliteSource,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<AttachmentRecord>> {
        let Some(pool) = source.pool(SourceDatabase::State).await? else {
            return Ok(Vec::new());
        };
        let rows = sqlx::query(
            "SELECT id, thread_id, attachment_type, identity_key, payload, created_at \
             FROM thread_attachments WHERE (?1 IS NULL OR id > ?1) ORDER BY id LIMIT ?2",
        )
        .bind(after)
        .bind(i64::try_from(limit)?)
        .fetch_all(&pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(AttachmentRecord {
                    id: row.try_get("id")?,
                    thread_id: row.try_get("thread_id")?,
                    attachment_type: row.try_get("attachment_type")?,
                    identity_key: row.try_get("identity_key")?,
                    payload: row.try_get("payload")?,
                    created_at: row.try_get("created_at")?,
                })
            })
            .collect()
    }

    async fn import(connection: &mut PgConnection, records: &[AttachmentRecord]) -> Result<()> {
        for record in records {
            sqlx::query(
                "INSERT INTO thread_attachments \
                 (id, thread_id, attachment_type, identity_key, payload, created_at) \
                 VALUES ($1, $2::uuid, $3, $4, $5, $6) ON CONFLICT (id) DO NOTHING",
            )
            .bind(&record.id)
            .bind(&record.thread_id)
            .bind(&record.attachment_type)
            .bind(&record.identity_key)
            .bind(&record.payload)
            .bind(record.created_at)
            .execute(&mut *connection)
            .await?;
        }
        Ok(())
    }

    async fn read_back(
        connection: &mut PgConnection,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<AttachmentRecord>> {
        let rows = sqlx::query(
            "SELECT id, thread_id::text AS thread_id, attachment_type, identity_key, payload, \
             created_at FROM thread_attachments \
             WHERE ($1::text IS NULL OR id > $1) ORDER BY id LIMIT $2",
        )
        .bind(after)
        .bind(i64::try_from(limit)?)
        .fetch_all(connection)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(AttachmentRecord {
                    id: row.try_get("id")?,
                    thread_id: row.try_get("thread_id")?,
                    attachment_type: row.try_get("attachment_type")?,
                    identity_key: row.try_get("identity_key")?,
                    payload: row.try_get("payload")?,
                    created_at: row.try_get("created_at")?,
                })
            })
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct EdgeRecord {
    child_thread_id: String,
    parent_thread_id: String,
    status: String,
}

pub(crate) struct SpawnEdges;

impl DomainOps for SpawnEdges {
    type Record = EdgeRecord;

    const DOMAIN: Domain = Domain::SpawnEdges;

    fn key(record: &EdgeRecord) -> String {
        record.child_thread_id.clone()
    }

    async fn export(
        source: &SqliteSource,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<EdgeRecord>> {
        let Some(pool) = source.pool(SourceDatabase::State).await? else {
            return Ok(Vec::new());
        };
        let rows = sqlx::query(
            "SELECT child_thread_id, parent_thread_id, status FROM thread_spawn_edges \
             WHERE (?1 IS NULL OR child_thread_id > ?1) ORDER BY child_thread_id LIMIT ?2",
        )
        .bind(after)
        .bind(i64::try_from(limit)?)
        .fetch_all(&pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(EdgeRecord {
                    child_thread_id: row.try_get("child_thread_id")?,
                    parent_thread_id: row.try_get("parent_thread_id")?,
                    status: row.try_get("status")?,
                })
            })
            .collect()
    }

    async fn import(connection: &mut PgConnection, records: &[EdgeRecord]) -> Result<()> {
        for record in records {
            sqlx::query(
                "INSERT INTO thread_spawn_edges \
                 (parent_thread_id, child_thread_id, status) VALUES ($1::uuid, $2::uuid, $3) \
                 ON CONFLICT (child_thread_id) DO UPDATE SET \
                 parent_thread_id = excluded.parent_thread_id, status = excluded.status",
            )
            .bind(&record.parent_thread_id)
            .bind(&record.child_thread_id)
            .bind(&record.status)
            .execute(&mut *connection)
            .await?;
        }
        Ok(())
    }

    async fn read_back(
        connection: &mut PgConnection,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<EdgeRecord>> {
        let rows = sqlx::query(
            "SELECT child_thread_id::text AS child_thread_id, \
             parent_thread_id::text AS parent_thread_id, status \
             FROM thread_spawn_edges \
             WHERE ($1::uuid IS NULL OR child_thread_id > $1::uuid) \
             ORDER BY child_thread_id LIMIT $2",
        )
        .bind(after)
        .bind(i64::try_from(limit)?)
        .fetch_all(connection)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(EdgeRecord {
                    child_thread_id: row.try_get("child_thread_id")?,
                    parent_thread_id: row.try_get("parent_thread_id")?,
                    status: row.try_get("status")?,
                })
            })
            .collect()
    }
}
