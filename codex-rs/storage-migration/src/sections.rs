//! Thread sections, including the built-in pinned section.

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
pub(crate) struct SectionRecord {
    id: String,
    name: String,
    appearance: Option<String>,
}

pub(crate) struct Sections;

impl DomainOps for Sections {
    type Record = SectionRecord;

    const DOMAIN: Domain = Domain::Sections;

    fn key(record: &SectionRecord) -> String {
        record.id.clone()
    }

    async fn export(
        source: &SqliteSource,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SectionRecord>> {
        let Some(pool) = source.pool(SourceDatabase::State).await? else {
            return Ok(Vec::new());
        };
        let rows = sqlx::query(
            "SELECT id, name, appearance FROM thread_sections \
             WHERE (?1 IS NULL OR id > ?1) ORDER BY id LIMIT ?2",
        )
        .bind(after)
        .bind(i64::try_from(limit)?)
        .fetch_all(&pool)
        .await?;
        rows.iter().map(section_from_row).collect()
    }

    async fn import(connection: &mut PgConnection, records: &[SectionRecord]) -> Result<()> {
        for record in records {
            sqlx::query(
                "INSERT INTO thread_sections (id, name, appearance) \
                 VALUES ($1, $2, $3) \
                 ON CONFLICT (id) DO UPDATE SET name = excluded.name, \
                 appearance = excluded.appearance",
            )
            .bind(&record.id)
            .bind(&record.name)
            .bind(&record.appearance)
            .execute(&mut *connection)
            .await?;
        }
        Ok(())
    }

    async fn read_back(
        connection: &mut PgConnection,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SectionRecord>> {
        let rows = sqlx::query(
            "SELECT id, name, appearance FROM thread_sections \
             WHERE ($1::text IS NULL OR id > $1) ORDER BY id LIMIT $2",
        )
        .bind(after)
        .bind(i64::try_from(limit)?)
        .fetch_all(connection)
        .await?;
        rows.iter().map(section_from_row).collect()
    }

    async fn write_sqlite(target: &SqliteTarget, records: &[SectionRecord]) -> Result<()> {
        for record in records {
            sqlx::query(
                "INSERT INTO thread_sections (id, name, appearance) VALUES (?, ?, ?) \
                 ON CONFLICT(id) DO UPDATE SET name = excluded.name, \
                 appearance = excluded.appearance",
            )
            .bind(&record.id)
            .bind(&record.name)
            .bind(&record.appearance)
            .execute(&target.state)
            .await?;
        }
        Ok(())
    }
}

fn section_from_row<R: Row>(row: &R) -> Result<SectionRecord>
where
    for<'r> String: sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
    for<'r> Option<String>: sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
    for<'r> &'r str: sqlx::ColumnIndex<R>,
{
    Ok(SectionRecord {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        appearance: row.try_get("appearance")?,
    })
}
