//! Projects with their ordered roots, and the idempotency keys that created them.

use crate::domain::Domain;
use crate::domain::DomainOps;
use crate::source::SourceDatabase;
use crate::source::SqliteSource;
use anyhow::Result;
use serde::Serialize;
use sqlx::PgConnection;
use sqlx::Row;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ProjectRecord {
    id: String,
    name: String,
    metadata: String,
    position: i64,
    created_at_ms: i64,
    updated_at_ms: i64,
    roots: Vec<String>,
}

pub(crate) struct Projects;

impl DomainOps for Projects {
    type Record = ProjectRecord;

    const DOMAIN: Domain = Domain::Projects;

    fn key(record: &ProjectRecord) -> String {
        record.id.clone()
    }

    async fn export(
        source: &SqliteSource,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ProjectRecord>> {
        let Some(pool) = source.pool(SourceDatabase::State).await? else {
            return Ok(Vec::new());
        };
        let rows = sqlx::query(
            "SELECT id, name, metadata, position, created_at_ms, updated_at_ms FROM projects \
             WHERE (?1 IS NULL OR id > ?1) ORDER BY id LIMIT ?2",
        )
        .bind(after)
        .bind(i64::try_from(limit)?)
        .fetch_all(&pool)
        .await?;
        let mut records = Vec::with_capacity(rows.len());
        for row in rows {
            let id: String = row.try_get("id")?;
            let roots = sqlx::query_scalar::<_, String>(
                "SELECT path FROM project_roots WHERE project_id = ? ORDER BY position",
            )
            .bind(&id)
            .fetch_all(&pool)
            .await?;
            records.push(ProjectRecord {
                id,
                name: row.try_get("name")?,
                metadata: row.try_get("metadata")?,
                position: row.try_get("position")?,
                created_at_ms: row.try_get("created_at_ms")?,
                updated_at_ms: row.try_get("updated_at_ms")?,
                roots,
            });
        }
        Ok(records)
    }

    async fn import(connection: &mut PgConnection, records: &[ProjectRecord]) -> Result<()> {
        for record in records {
            sqlx::query(
                "INSERT INTO projects \
                 (id, name, metadata, position, created_at_ms, updated_at_ms) \
                 VALUES ($1, $2, $3, $4, $5, $6) \
                 ON CONFLICT (id) DO UPDATE SET name = excluded.name, \
                 metadata = excluded.metadata, position = excluded.position, \
                 created_at_ms = excluded.created_at_ms, updated_at_ms = excluded.updated_at_ms",
            )
            .bind(&record.id)
            .bind(&record.name)
            .bind(&record.metadata)
            .bind(record.position)
            .bind(record.created_at_ms)
            .bind(record.updated_at_ms)
            .execute(&mut *connection)
            .await?;
            sqlx::query("DELETE FROM project_roots WHERE project_id = $1")
                .bind(&record.id)
                .execute(&mut *connection)
                .await?;
            for (position, path) in record.roots.iter().enumerate() {
                sqlx::query(
                    "INSERT INTO project_roots (project_id, position, path) \
                     VALUES ($1, $2, $3)",
                )
                .bind(&record.id)
                .bind(position as i64)
                .bind(path)
                .execute(&mut *connection)
                .await?;
            }
        }
        Ok(())
    }

    async fn read_back(
        connection: &mut PgConnection,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ProjectRecord>> {
        let rows = sqlx::query(
            "SELECT id, name, metadata, position, created_at_ms, updated_at_ms \
             FROM projects \
             WHERE ($1::text IS NULL OR id COLLATE \"C\" > $1 COLLATE \"C\") \
             ORDER BY id COLLATE \"C\" LIMIT $2",
        )
        .bind(after)
        .bind(i64::try_from(limit)?)
        .fetch_all(&mut *connection)
        .await?;
        let mut records = Vec::with_capacity(rows.len());
        for row in rows {
            let id: String = row.try_get("id")?;
            let roots = sqlx::query_scalar::<_, String>(
                "SELECT path FROM project_roots \
                 WHERE project_id = $1 ORDER BY position",
            )
            .bind(&id)
            .fetch_all(&mut *connection)
            .await?;
            records.push(ProjectRecord {
                id,
                name: row.try_get("name")?,
                metadata: row.try_get("metadata")?,
                position: row.try_get("position")?,
                created_at_ms: row.try_get("created_at_ms")?,
                updated_at_ms: row.try_get("updated_at_ms")?,
                roots,
            });
        }
        Ok(records)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ProjectKeyRecord {
    key: String,
    project_id: String,
    created_at_ms: i64,
}

pub(crate) struct ProjectKeys;

impl DomainOps for ProjectKeys {
    type Record = ProjectKeyRecord;

    const DOMAIN: Domain = Domain::ProjectKeys;

    fn key(record: &ProjectKeyRecord) -> String {
        record.key.clone()
    }

    async fn export(
        source: &SqliteSource,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ProjectKeyRecord>> {
        let Some(pool) = source.pool(SourceDatabase::State).await? else {
            return Ok(Vec::new());
        };
        let rows = sqlx::query(
            "SELECT key, project_id, created_at_ms FROM project_idempotency_keys \
             WHERE (?1 IS NULL OR key > ?1) ORDER BY key LIMIT ?2",
        )
        .bind(after)
        .bind(i64::try_from(limit)?)
        .fetch_all(&pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(ProjectKeyRecord {
                    key: row.try_get("key")?,
                    project_id: row.try_get("project_id")?,
                    created_at_ms: row.try_get("created_at_ms")?,
                })
            })
            .collect()
    }

    async fn import(connection: &mut PgConnection, records: &[ProjectKeyRecord]) -> Result<()> {
        for record in records {
            sqlx::query(
                "INSERT INTO project_idempotency_keys \
                 (key, project_id, created_at_ms) VALUES ($1, $2, $3) \
                 ON CONFLICT (key) DO NOTHING",
            )
            .bind(&record.key)
            .bind(&record.project_id)
            .bind(record.created_at_ms)
            .execute(&mut *connection)
            .await?;
        }
        Ok(())
    }

    async fn read_back(
        connection: &mut PgConnection,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ProjectKeyRecord>> {
        let rows = sqlx::query(
            "SELECT key, project_id, created_at_ms FROM project_idempotency_keys \
             WHERE ($1::text IS NULL OR key COLLATE \"C\" > $1 COLLATE \"C\") \
             ORDER BY key COLLATE \"C\" LIMIT $2",
        )
        .bind(after)
        .bind(i64::try_from(limit)?)
        .fetch_all(connection)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(ProjectKeyRecord {
                    key: row.try_get("key")?,
                    project_id: row.try_get("project_id")?,
                    created_at_ms: row.try_get("created_at_ms")?,
                })
            })
            .collect()
    }
}
