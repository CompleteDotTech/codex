//! The thread catalog: one row per thread with every stored column.

use crate::domain::Domain;
use crate::domain::DomainOps;
use crate::source::SourceDatabase;
use crate::source::SqliteSource;
use anyhow::Result;
use serde::Serialize;
use sqlx::PgConnection;
use sqlx::Row;
use sqlx::postgres::PgRow;
use sqlx::sqlite::SqliteRow;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ThreadRecord {
    id: String,
    origin_rollout_path: String,
    created_at_ms: i64,
    updated_at_ms: i64,
    recency_at_ms: i64,
    source: String,
    originator: Option<String>,
    creator_user_id: Option<String>,
    creator_account_id: Option<String>,
    history_mode: String,
    thread_source: Option<String>,
    agent_nickname: Option<String>,
    agent_role: Option<String>,
    agent_path: Option<String>,
    model_provider: String,
    model: Option<String>,
    reasoning_effort: Option<String>,
    origin_cwd: String,
    cli_version: String,
    title: String,
    name: Option<String>,
    preview: String,
    sandbox_policy: String,
    approval_mode: String,
    tokens_used: i64,
    first_user_message: String,
    archived_at_s: Option<i64>,
    thread_section_id: Option<String>,
    section_position: Option<i64>,
    section_entered_at_ms: Option<i64>,
    project_id: Option<String>,
    daybreak_enabled: Option<bool>,
    git_sha: Option<String>,
    git_branch: Option<String>,
    git_origin_url: Option<String>,
    memory_mode: String,
}

pub(crate) struct Threads;

macro_rules! sqlite_columns {
    () => {
        "id, rollout_path, created_at_ms, updated_at_ms, recency_at_ms, \
    source, originator, creator_user_id, creator_account_id, history_mode, thread_source, \
    agent_nickname, agent_role, agent_path, model_provider, model, reasoning_effort, cwd, \
    cli_version, title, name, preview, sandbox_policy, approval_mode, tokens_used, \
    first_user_message, archived_at, thread_section_id, section_position, \
    section_entered_at_ms, project_id, daybreak_enabled, git_sha, git_branch, git_origin_url, \
    memory_mode"
    };
}

fn from_sqlite(row: &SqliteRow) -> Result<ThreadRecord> {
    Ok(ThreadRecord {
        id: row.try_get("id")?,
        origin_rollout_path: row.try_get("rollout_path")?,
        created_at_ms: row.try_get("created_at_ms")?,
        updated_at_ms: row.try_get("updated_at_ms")?,
        recency_at_ms: row.try_get("recency_at_ms")?,
        source: row.try_get("source")?,
        originator: row.try_get("originator")?,
        creator_user_id: row.try_get("creator_user_id")?,
        creator_account_id: row.try_get("creator_account_id")?,
        history_mode: row.try_get("history_mode")?,
        thread_source: row.try_get("thread_source")?,
        agent_nickname: row.try_get("agent_nickname")?,
        agent_role: row.try_get("agent_role")?,
        agent_path: row.try_get("agent_path")?,
        model_provider: row.try_get("model_provider")?,
        model: row.try_get("model")?,
        reasoning_effort: row.try_get("reasoning_effort")?,
        origin_cwd: row.try_get("cwd")?,
        cli_version: row.try_get("cli_version")?,
        title: row.try_get("title")?,
        name: row.try_get("name")?,
        preview: row
            .try_get::<Option<String>, _>("preview")?
            .unwrap_or_default(),
        sandbox_policy: row.try_get("sandbox_policy")?,
        approval_mode: row.try_get("approval_mode")?,
        tokens_used: row.try_get("tokens_used")?,
        first_user_message: row
            .try_get::<Option<String>, _>("first_user_message")?
            .unwrap_or_default(),
        archived_at_s: row.try_get("archived_at")?,
        thread_section_id: row.try_get("thread_section_id")?,
        section_position: row.try_get("section_position")?,
        section_entered_at_ms: row.try_get("section_entered_at_ms")?,
        project_id: row.try_get("project_id")?,
        daybreak_enabled: row.try_get("daybreak_enabled")?,
        git_sha: row.try_get("git_sha")?,
        git_branch: row.try_get("git_branch")?,
        git_origin_url: row.try_get("git_origin_url")?,
        memory_mode: row.try_get("memory_mode")?,
    })
}

fn from_postgres(row: &PgRow) -> Result<ThreadRecord> {
    Ok(ThreadRecord {
        id: row.try_get("id")?,
        origin_rollout_path: row.try_get("origin_rollout_path")?,
        created_at_ms: row.try_get("created_at_ms")?,
        updated_at_ms: row.try_get("updated_at_ms")?,
        recency_at_ms: row.try_get("recency_at_ms")?,
        source: row.try_get("source")?,
        originator: row.try_get("originator")?,
        creator_user_id: row.try_get("creator_user_id")?,
        creator_account_id: row.try_get("creator_account_id")?,
        history_mode: row.try_get("history_mode")?,
        thread_source: row.try_get("thread_source")?,
        agent_nickname: row.try_get("agent_nickname")?,
        agent_role: row.try_get("agent_role")?,
        agent_path: row.try_get("agent_path")?,
        model_provider: row.try_get("model_provider")?,
        model: row.try_get("model")?,
        reasoning_effort: row.try_get("reasoning_effort")?,
        origin_cwd: row.try_get("origin_cwd")?,
        cli_version: row.try_get("cli_version")?,
        title: row.try_get("title")?,
        name: row.try_get("name")?,
        preview: row.try_get("preview")?,
        sandbox_policy: row.try_get("sandbox_policy")?,
        approval_mode: row.try_get("approval_mode")?,
        tokens_used: row.try_get("tokens_used")?,
        first_user_message: row.try_get("first_user_message")?,
        archived_at_s: row.try_get("archived_at_s")?,
        thread_section_id: row.try_get("thread_section_id")?,
        section_position: row.try_get("section_position")?,
        section_entered_at_ms: row.try_get("section_entered_at_ms")?,
        project_id: row.try_get("project_id")?,
        daybreak_enabled: row.try_get("daybreak_enabled")?,
        git_sha: row.try_get("git_sha")?,
        git_branch: row.try_get("git_branch")?,
        git_origin_url: row.try_get("git_origin_url")?,
        memory_mode: row.try_get("memory_mode")?,
    })
}

impl DomainOps for Threads {
    type Record = ThreadRecord;

    const DOMAIN: Domain = Domain::Threads;

    fn key(record: &ThreadRecord) -> String {
        record.id.clone()
    }

    async fn export(
        source: &SqliteSource,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ThreadRecord>> {
        let Some(pool) = source.pool(SourceDatabase::State).await? else {
            return Ok(Vec::new());
        };
        let rows = sqlx::query(concat!(
            "SELECT ",
            sqlite_columns!(),
            " FROM threads WHERE (?1 IS NULL OR id > ?1) ORDER BY id LIMIT ?2"
        ))
        .bind(after)
        .bind(i64::try_from(limit)?)
        .fetch_all(&pool)
        .await?;
        // PostgreSQL stores every thread with the legacy history contract, and the rollout
        // import writes the complete logical history, so the mode is part of the conversion.
        rows.iter()
            .map(|row| {
                from_sqlite(row).map(|mut record| {
                    record.history_mode = "legacy".to_string();
                    record
                })
            })
            .collect()
    }

    async fn import(connection: &mut PgConnection, records: &[ThreadRecord]) -> Result<()> {
        for record in records {
            sqlx::query(
                "INSERT INTO codex_storage.threads (id, origin_rollout_path, created_at_ms, \
                 updated_at_ms, recency_at_ms, source, originator, creator_user_id, \
                 creator_account_id, history_mode, thread_source, agent_nickname, agent_role, \
                 agent_path, model_provider, model, reasoning_effort, origin_cwd, cli_version, \
                 title, name, preview, sandbox_policy, approval_mode, tokens_used, \
                 first_user_message, archived_at_s, thread_section_id, section_position, \
                 section_entered_at_ms, git_sha, git_branch, git_origin_url, memory_mode, \
                 project_id, daybreak_enabled) \
                 VALUES ($1::uuid, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, \
                 $15, $16, $17, $18, $19, $20, $21, $22, $23, $24, $25, $26, $27, $28, $29, \
                 $30, $31, $32, $33, $34, $35, $36) \
                 ON CONFLICT (id) DO UPDATE SET origin_rollout_path = excluded.origin_rollout_path, \
                 created_at_ms = excluded.created_at_ms, updated_at_ms = excluded.updated_at_ms, \
                 recency_at_ms = excluded.recency_at_ms, source = excluded.source, \
                 originator = excluded.originator, creator_user_id = excluded.creator_user_id, \
                 creator_account_id = excluded.creator_account_id, \
                 history_mode = excluded.history_mode, thread_source = excluded.thread_source, \
                 agent_nickname = excluded.agent_nickname, agent_role = excluded.agent_role, \
                 agent_path = excluded.agent_path, model_provider = excluded.model_provider, \
                 model = excluded.model, reasoning_effort = excluded.reasoning_effort, \
                 origin_cwd = excluded.origin_cwd, cli_version = excluded.cli_version, \
                 title = excluded.title, name = excluded.name, preview = excluded.preview, \
                 sandbox_policy = excluded.sandbox_policy, approval_mode = excluded.approval_mode, \
                 tokens_used = excluded.tokens_used, \
                 first_user_message = excluded.first_user_message, \
                 archived_at_s = excluded.archived_at_s, \
                 thread_section_id = excluded.thread_section_id, \
                 section_position = excluded.section_position, \
                 section_entered_at_ms = excluded.section_entered_at_ms, \
                 git_sha = excluded.git_sha, git_branch = excluded.git_branch, \
                 git_origin_url = excluded.git_origin_url, memory_mode = excluded.memory_mode, \
                 project_id = excluded.project_id, daybreak_enabled = excluded.daybreak_enabled",
            )
            .bind(&record.id)
            .bind(&record.origin_rollout_path)
            .bind(record.created_at_ms)
            .bind(record.updated_at_ms)
            .bind(record.recency_at_ms)
            .bind(&record.source)
            .bind(&record.originator)
            .bind(&record.creator_user_id)
            .bind(&record.creator_account_id)
            .bind(&record.history_mode)
            .bind(&record.thread_source)
            .bind(&record.agent_nickname)
            .bind(&record.agent_role)
            .bind(&record.agent_path)
            .bind(&record.model_provider)
            .bind(&record.model)
            .bind(&record.reasoning_effort)
            .bind(&record.origin_cwd)
            .bind(&record.cli_version)
            .bind(&record.title)
            .bind(&record.name)
            .bind(&record.preview)
            .bind(&record.sandbox_policy)
            .bind(&record.approval_mode)
            .bind(record.tokens_used)
            .bind(&record.first_user_message)
            .bind(record.archived_at_s)
            .bind(&record.thread_section_id)
            .bind(record.section_position)
            .bind(record.section_entered_at_ms)
            .bind(&record.git_sha)
            .bind(&record.git_branch)
            .bind(&record.git_origin_url)
            .bind(&record.memory_mode)
            .bind(&record.project_id)
            .bind(record.daybreak_enabled)
            .execute(&mut *connection)
            .await?;
        }
        // New timestamps must sort after everything that was imported, whichever host wrote it.
        let updated = records.iter().map(|record| record.updated_at_ms).max();
        let recency = records.iter().map(|record| record.recency_at_ms).max();
        if let (Some(updated), Some(recency)) = (updated, recency) {
            sqlx::query(
                "UPDATE codex_storage.thread_timestamp_marks SET \
                 updated_at_ms = GREATEST(updated_at_ms, $1), \
                 recency_at_ms = GREATEST(recency_at_ms, $2) WHERE singleton",
            )
            .bind(updated)
            .bind(recency)
            .execute(&mut *connection)
            .await?;
        }
        Ok(())
    }

    async fn read_back(
        connection: &mut PgConnection,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ThreadRecord>> {
        let rows = sqlx::query(
            "SELECT id::text AS id, origin_rollout_path, created_at_ms, updated_at_ms, \
             recency_at_ms, source, originator, creator_user_id, creator_account_id, \
             history_mode, thread_source, agent_nickname, agent_role, agent_path, \
             model_provider, model, reasoning_effort, origin_cwd, cli_version, title, name, \
             COALESCE(preview, '') AS preview, sandbox_policy, approval_mode, tokens_used, \
             COALESCE(first_user_message, '') AS first_user_message, archived_at_s, \
             thread_section_id, section_position, section_entered_at_ms, project_id, \
             daybreak_enabled, git_sha, git_branch, git_origin_url, memory_mode \
             FROM codex_storage.threads \
             WHERE ($1::uuid IS NULL OR id > $1::uuid) ORDER BY id LIMIT $2",
        )
        .bind(after)
        .bind(i64::try_from(limit)?)
        .fetch_all(connection)
        .await?;
        rows.iter().map(from_postgres).collect()
    }
}
