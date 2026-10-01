//! Row mapping for the PostgreSQL thread catalog, shared by the stores that read thread rows.
//!
//! The catalog keeps host paths as recorded origins, so a mapped record carries the origin
//! rollout path and working directory verbatim.

use anyhow::Result;
use anyhow::anyhow;
use chrono::DateTime;
use chrono::Utc;
use codex_protocol::SanitizedGitUrl;
use codex_protocol::ThreadId;
use codex_protocol::openai_models::ReasoningEffort;
use codex_state::ThreadMetadata;
use codex_state::ThreadSection;
use codex_state::ThreadSectionAppearance;
use sqlx::Row;
use sqlx::postgres::PgRow;
use std::path::PathBuf;

/// Columns selected for a full thread record, in the shape [`thread_metadata_from_row`] reads.
#[macro_export]
macro_rules! thread_columns {
    () => {
        "threads.id::text AS id, threads.originator, threads.creator_user_id,          threads.creator_account_id, threads.origin_rollout_path, threads.created_at_ms,          threads.updated_at_ms, threads.recency_at_ms, threads.source, threads.history_mode,          threads.thread_source, threads.agent_nickname, threads.agent_role, threads.agent_path,          threads.model_provider, threads.model, threads.reasoning_effort, threads.origin_cwd,          threads.cli_version, threads.title, threads.name, threads.preview,          threads.sandbox_policy, threads.approval_mode, threads.tokens_used,          threads.first_user_message, threads.archived_at_s, threads.thread_section_id,          (SELECT thread_sections.name FROM codex_storage.thread_sections            WHERE thread_sections.id = threads.thread_section_id) AS section_name,          (SELECT thread_sections.appearance FROM codex_storage.thread_sections            WHERE thread_sections.id = threads.thread_section_id) AS section_appearance,          threads.section_position, threads.section_entered_at_ms, threads.project_id,          threads.daybreak_enabled, threads.git_sha, threads.git_branch, threads.git_origin_url"
    };
}

/// Matches the SQLite store: values older than 2020 read as milliseconds are legacy seconds.
pub fn epoch_millis_to_datetime(value: i64) -> Result<DateTime<Utc>> {
    const MIN_EPOCH_MILLIS: i64 = 1_577_836_800_000;
    let millis = if value < MIN_EPOCH_MILLIS {
        value.saturating_mul(1000)
    } else {
        value
    };
    DateTime::<Utc>::from_timestamp_millis(millis)
        .ok_or_else(|| anyhow!("invalid unix timestamp millis: {value}"))
}

pub fn epoch_seconds_to_datetime(value: i64) -> Result<DateTime<Utc>> {
    DateTime::<Utc>::from_timestamp(value, 0)
        .ok_or_else(|| anyhow!("invalid unix timestamp seconds: {value}"))
}

pub fn thread_metadata_from_row(row: &PgRow) -> Result<ThreadMetadata> {
    let section_id: Option<String> = row.try_get("thread_section_id")?;
    let section_name: Option<String> = row.try_get("section_name")?;
    let section_appearance: Option<String> = row.try_get("section_appearance")?;
    let section = match (section_id, section_name) {
        (Some(id), Some(name)) => Some(ThreadSection {
            id,
            name,
            appearance: section_appearance
                .map(|appearance| serde_json::from_str::<ThreadSectionAppearance>(&appearance))
                .transpose()?,
        }),
        (None, None) => None,
        (Some(id), None) => return Err(anyhow!("thread references an unknown section: {id}")),
        (None, Some(name)) => {
            return Err(anyhow!(
                "thread has a section name without a section id: {name}"
            ));
        }
    };
    let thread_source = row
        .try_get::<Option<String>, _>("thread_source")?
        .map(|value| value.parse())
        .transpose()
        .map_err(anyhow::Error::msg)?;
    let history_mode = row
        .try_get::<String, _>("history_mode")?
        .parse()
        .map_err(anyhow::Error::msg)?;
    let preview: Option<String> = row.try_get("preview")?;
    let first_user_message: Option<String> = row.try_get("first_user_message")?;
    let git_origin_url: Option<String> = row.try_get("git_origin_url")?;
    Ok(ThreadMetadata {
        id: ThreadId::try_from(row.try_get::<String, _>("id")?)?,
        originator: row.try_get("originator")?,
        creator_user_id: row.try_get("creator_user_id")?,
        creator_account_id: row.try_get("creator_account_id")?,
        rollout_path: PathBuf::from(row.try_get::<String, _>("origin_rollout_path")?),
        created_at: epoch_millis_to_datetime(row.try_get("created_at_ms")?)?,
        updated_at: epoch_millis_to_datetime(row.try_get("updated_at_ms")?)?,
        recency_at: epoch_millis_to_datetime(row.try_get("recency_at_ms")?)?,
        source: row.try_get("source")?,
        history_mode,
        thread_source,
        agent_nickname: row.try_get("agent_nickname")?,
        agent_role: row.try_get("agent_role")?,
        agent_path: row.try_get("agent_path")?,
        model_provider: row.try_get("model_provider")?,
        model: row.try_get("model")?,
        reasoning_effort: row
            .try_get::<Option<String>, _>("reasoning_effort")?
            .and_then(|value| value.parse::<ReasoningEffort>().ok()),
        cwd: PathBuf::from(row.try_get::<String, _>("origin_cwd")?),
        cli_version: row.try_get("cli_version")?,
        title: row.try_get("title")?,
        name: row.try_get("name")?,
        preview: preview.filter(|value| !value.is_empty()),
        sandbox_policy: row.try_get("sandbox_policy")?,
        approval_mode: row.try_get("approval_mode")?,
        tokens_used: row.try_get("tokens_used")?,
        first_user_message: first_user_message.filter(|value| !value.is_empty()),
        archived_at: row
            .try_get::<Option<i64>, _>("archived_at_s")?
            .map(epoch_seconds_to_datetime)
            .transpose()?,
        section,
        section_position: row.try_get("section_position")?,
        section_entered_at: row
            .try_get::<Option<i64>, _>("section_entered_at_ms")?
            .map(epoch_millis_to_datetime)
            .transpose()?,
        project_id: row.try_get("project_id")?,
        daybreak_enabled: row.try_get("daybreak_enabled")?,
        git_sha: row.try_get("git_sha")?,
        git_branch: row.try_get("git_branch")?,
        git_origin_url: git_origin_url.and_then(|url| SanitizedGitUrl::try_from(url).ok()),
    })
}
