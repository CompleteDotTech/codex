//! Thread goals and their pending continuation deferrals.

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
pub(crate) struct GoalRecord {
    thread_id: String,
    goal_id: String,
    objective: String,
    status: String,
    token_budget: Option<i64>,
    tokens_used: i64,
    time_used_seconds: i64,
    created_at_ms: i64,
    updated_at_ms: i64,
    deferred: bool,
}

pub(crate) struct Goals;

impl DomainOps for Goals {
    type Record = GoalRecord;

    const DOMAIN: Domain = Domain::Goals;

    fn key(record: &GoalRecord) -> String {
        record.thread_id.clone()
    }

    async fn export(
        source: &SqliteSource,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<GoalRecord>> {
        let Some(pool) = source.pool(SourceDatabase::Goals).await? else {
            return Ok(Vec::new());
        };
        let rows = sqlx::query(
            "SELECT g.thread_id, g.goal_id, g.objective, g.status, g.token_budget, \
             g.tokens_used, g.time_used_seconds, g.created_at_ms, g.updated_at_ms, \
             (d.thread_id IS NOT NULL) AS deferred \
             FROM thread_goals g \
             LEFT JOIN thread_goal_continuation_deferrals d ON d.thread_id = g.thread_id \
             WHERE (?1 IS NULL OR g.thread_id > ?1) ORDER BY g.thread_id LIMIT ?2",
        )
        .bind(after)
        .bind(i64::try_from(limit)?)
        .fetch_all(&pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(GoalRecord {
                    thread_id: row.try_get("thread_id")?,
                    goal_id: row.try_get("goal_id")?,
                    objective: row.try_get("objective")?,
                    status: row.try_get("status")?,
                    token_budget: row.try_get("token_budget")?,
                    tokens_used: row.try_get("tokens_used")?,
                    time_used_seconds: row.try_get("time_used_seconds")?,
                    created_at_ms: row.try_get("created_at_ms")?,
                    updated_at_ms: row.try_get("updated_at_ms")?,
                    deferred: row.try_get("deferred")?,
                })
            })
            .collect()
    }

    async fn import(connection: &mut PgConnection, records: &[GoalRecord]) -> Result<()> {
        for record in records {
            sqlx::query(
                "INSERT INTO thread_goals (thread_id, goal_id, objective, status, \
                 token_budget, tokens_used, time_used_seconds, created_at_ms, updated_at_ms) \
                 VALUES ($1::uuid, $2, $3, $4, $5, $6, $7, $8, $9) \
                 ON CONFLICT (thread_id) DO UPDATE SET goal_id = excluded.goal_id, \
                 objective = excluded.objective, status = excluded.status, \
                 token_budget = excluded.token_budget, tokens_used = excluded.tokens_used, \
                 time_used_seconds = excluded.time_used_seconds, \
                 created_at_ms = excluded.created_at_ms, updated_at_ms = excluded.updated_at_ms",
            )
            .bind(&record.thread_id)
            .bind(&record.goal_id)
            .bind(&record.objective)
            .bind(&record.status)
            .bind(record.token_budget)
            .bind(record.tokens_used)
            .bind(record.time_used_seconds)
            .bind(record.created_at_ms)
            .bind(record.updated_at_ms)
            .execute(&mut *connection)
            .await?;
            if record.deferred {
                sqlx::query(
                    "INSERT INTO thread_goal_continuation_deferrals (thread_id) \
                     VALUES ($1::uuid) ON CONFLICT (thread_id) DO NOTHING",
                )
                .bind(&record.thread_id)
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
    ) -> Result<Vec<GoalRecord>> {
        let rows = sqlx::query(
            "SELECT g.thread_id::text AS thread_id, g.goal_id, g.objective, g.status, \
             g.token_budget, g.tokens_used, g.time_used_seconds, g.created_at_ms, \
             g.updated_at_ms, (d.thread_id IS NOT NULL) AS deferred \
             FROM thread_goals g \
             LEFT JOIN thread_goal_continuation_deferrals d \
             ON d.thread_id = g.thread_id \
             WHERE ($1::uuid IS NULL OR g.thread_id > $1::uuid) \
             ORDER BY g.thread_id LIMIT $2",
        )
        .bind(after)
        .bind(i64::try_from(limit)?)
        .fetch_all(connection)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(GoalRecord {
                    thread_id: row.try_get("thread_id")?,
                    goal_id: row.try_get("goal_id")?,
                    objective: row.try_get("objective")?,
                    status: row.try_get("status")?,
                    token_budget: row.try_get("token_budget")?,
                    tokens_used: row.try_get("tokens_used")?,
                    time_used_seconds: row.try_get("time_used_seconds")?,
                    created_at_ms: row.try_get("created_at_ms")?,
                    updated_at_ms: row.try_get("updated_at_ms")?,
                    deferred: row.try_get("deferred")?,
                })
            })
            .collect()
    }

    async fn write_sqlite(target: &SqliteTarget, records: &[GoalRecord]) -> Result<()> {
        for record in records {
            sqlx::query(
                "INSERT OR REPLACE INTO thread_goals (thread_id, goal_id, objective, status, \
                 token_budget, tokens_used, time_used_seconds, created_at_ms, updated_at_ms) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&record.thread_id)
            .bind(&record.goal_id)
            .bind(&record.objective)
            .bind(&record.status)
            .bind(record.token_budget)
            .bind(record.tokens_used)
            .bind(record.time_used_seconds)
            .bind(record.created_at_ms)
            .bind(record.updated_at_ms)
            .execute(&target.goals)
            .await?;
            if record.deferred {
                sqlx::query(
                    "INSERT OR IGNORE INTO thread_goal_continuation_deferrals (thread_id) VALUES (?)",
                )
                .bind(&record.thread_id)
                .execute(&target.goals)
                .await?;
            }
        }
        Ok(())
    }
}
