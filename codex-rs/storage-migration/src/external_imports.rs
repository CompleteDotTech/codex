//! The history of external agent configuration imports.

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
pub(crate) struct ExternalImportRecord {
    import_id: String,
    provider_id: Option<String>,
    completed_at_ms: i64,
    successes: String,
    failures: String,
}

pub(crate) struct ExternalImports;

impl DomainOps for ExternalImports {
    type Record = ExternalImportRecord;

    const DOMAIN: Domain = Domain::ExternalImports;

    fn key(record: &ExternalImportRecord) -> String {
        record.import_id.clone()
    }

    async fn export(
        source: &SqliteSource,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ExternalImportRecord>> {
        let Some(pool) = source.pool(SourceDatabase::State).await? else {
            return Ok(Vec::new());
        };
        let rows = sqlx::query(
            "SELECT import_id, provider_id, completed_at_ms, successes, failures \
             FROM external_agent_config_imports WHERE (?1 IS NULL OR import_id > ?1) \
             ORDER BY import_id LIMIT ?2",
        )
        .bind(after)
        .bind(i64::try_from(limit)?)
        .fetch_all(&pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(ExternalImportRecord {
                    import_id: row.try_get("import_id")?,
                    provider_id: row.try_get("provider_id")?,
                    completed_at_ms: row.try_get("completed_at_ms")?,
                    successes: row.try_get("successes")?,
                    failures: row.try_get("failures")?,
                })
            })
            .collect()
    }

    async fn import(connection: &mut PgConnection, records: &[ExternalImportRecord]) -> Result<()> {
        for record in records {
            sqlx::query(
                "INSERT INTO external_agent_config_imports \
                 (import_id, provider_id, completed_at_ms, successes, failures) \
                 VALUES ($1, $2, $3, $4, $5) ON CONFLICT (import_id) DO UPDATE SET \
                 provider_id = excluded.provider_id, completed_at_ms = excluded.completed_at_ms, \
                 successes = excluded.successes, failures = excluded.failures",
            )
            .bind(&record.import_id)
            .bind(&record.provider_id)
            .bind(record.completed_at_ms)
            .bind(&record.successes)
            .bind(&record.failures)
            .execute(&mut *connection)
            .await?;
        }
        Ok(())
    }

    async fn read_back(
        connection: &mut PgConnection,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ExternalImportRecord>> {
        let rows = sqlx::query(
            "SELECT import_id, provider_id, completed_at_ms, successes, failures \
             FROM external_agent_config_imports \
             WHERE ($1::text IS NULL OR import_id COLLATE \"C\" > $1 COLLATE \"C\") \
             ORDER BY import_id COLLATE \"C\" LIMIT $2",
        )
        .bind(after)
        .bind(i64::try_from(limit)?)
        .fetch_all(connection)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(ExternalImportRecord {
                    import_id: row.try_get("import_id")?,
                    provider_id: row.try_get("provider_id")?,
                    completed_at_ms: row.try_get("completed_at_ms")?,
                    successes: row.try_get("successes")?,
                    failures: row.try_get("failures")?,
                })
            })
            .collect()
    }

    async fn write_sqlite(target: &SqliteTarget, records: &[ExternalImportRecord]) -> Result<()> {
        for record in records {
            sqlx::query(
                "INSERT OR REPLACE INTO external_agent_config_imports \
                 (import_id, provider_id, completed_at_ms, successes, failures) \
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(&record.import_id)
            .bind(&record.provider_id)
            .bind(record.completed_at_ms)
            .bind(&record.successes)
            .bind(&record.failures)
            .execute(&target.state)
            .await?;
        }
        Ok(())
    }
}
