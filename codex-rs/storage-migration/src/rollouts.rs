//! Canonical rollout lines, one record per thread.
//!
//! A thread's history is the JSONL file its metadata points at. A thread forked from another
//! rollout stores only its own records and refers to a prefix of the parent, so the import
//! writes the whole logical history: the parent prefix followed by the thread's own records.
//! PostgreSQL stores every thread with the legacy history contract, so the lines are written
//! without ordinals.

use crate::domain::Domain;
use crate::domain::DomainOps;
use crate::source::SourceDatabase;
use crate::source::SqliteSource;
use crate::sqlite_target::SqliteTarget;
use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use sqlx::PgConnection;
use sqlx::Row;
use std::path::Path;
use std::path::PathBuf;

const MAX_FORK_DEPTH: usize = 64;
const INSERT_CHUNK: usize = 500;

/// What a thread's history contains. The lines themselves are written but not part of the
/// digest; their count and hash are.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct RolloutRecord {
    thread_id: String,
    line_count: u64,
    sha256: String,
    #[serde(skip)]
    lines: Vec<String>,
    /// What a staged home needs to place the file; not part of what verification compares.
    #[serde(skip)]
    created_at_ms: i64,
    #[serde(skip)]
    archived: bool,
}

fn summarize(
    thread_id: String,
    lines: Vec<String>,
    created_at_ms: i64,
    archived: bool,
) -> RolloutRecord {
    let mut hasher = Sha256::new();
    for line in &lines {
        hasher.update((line.len() as u64).to_be_bytes());
        hasher.update(line.as_bytes());
    }
    RolloutRecord {
        thread_id,
        line_count: lines.len() as u64,
        sha256: hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
        lines,
        created_at_ms,
        archived,
    }
}

fn split_lines(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_string)
        .collect()
}

pub(crate) struct Rollouts;

/// Find the file for a rollout id: the thread row first, then the session directories.
async fn locate(source: &SqliteSource, rollout_id: &str) -> Result<Option<PathBuf>> {
    if let Some(pool) = source.pool(SourceDatabase::State).await?
        && let Some(path) =
            sqlx::query_scalar::<_, String>("SELECT rollout_path FROM threads WHERE id = ?")
                .bind(rollout_id)
                .fetch_optional(&pool)
                .await?
    {
        let path = resolve(source.home(), &path);
        if tokio::fs::try_exists(&path).await? {
            return Ok(Some(path));
        }
    }
    let suffix = format!("{rollout_id}.jsonl");
    let roots = [
        source.home().join("sessions"),
        source.home().join("archived_sessions"),
    ];
    tokio::task::spawn_blocking(move || {
        let mut pending = Vec::from(roots);
        while let Some(directory) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else if path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().ends_with(&suffix))
                {
                    return Some(path);
                }
            }
        }
        None
    })
    .await
    .context("search session directories")
}

fn resolve(home: &Path, recorded: &str) -> PathBuf {
    let path = PathBuf::from(recorded);
    if path.is_absolute() {
        path
    } else {
        home.join(path)
    }
}

/// The logical history of one rollout file, including inherited fork prefixes.
async fn materialize(source: &SqliteSource, path: PathBuf, depth: usize) -> Result<Vec<String>> {
    if depth > MAX_FORK_DEPTH {
        bail!("rollout fork chain is deeper than {MAX_FORK_DEPTH}");
    }
    let text = match tokio::fs::read_to_string(&path).await {
        Ok(text) => text,
        // A thread that never wrote a record has no file yet.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && depth == 0 => {
            return Ok(Vec::new());
        }
        Err(error) => return Err(error).with_context(|| "read rollout file"),
    };
    let own = split_lines(&text);
    let Some(first) = own.first() else {
        return Ok(own);
    };
    let meta: serde_json::Value = serde_json::from_str(first).context("parse rollout header")?;
    let base = &meta["payload"]["history_base"];
    if base.is_null() {
        return Ok(own);
    }
    let rollout_id = base["thread_id"]
        .as_str()
        .context("history base has no rollout id")?;
    let end = usize::try_from(
        base["end_ordinal_exclusive"]
            .as_u64()
            .context("history base has no end ordinal")?,
    )?;
    let Some(prefix_path) = locate(source, rollout_id).await? else {
        bail!("the history prefix {rollout_id} is missing from the source");
    };
    let mut prefix = Box::pin(materialize(source, prefix_path, depth + 1)).await?;
    if prefix.len() < end {
        bail!("the history prefix {rollout_id} is shorter than the fork point");
    }
    prefix.truncate(end);
    prefix.extend(own);
    Ok(prefix)
}

impl DomainOps for Rollouts {
    type Record = RolloutRecord;

    const DOMAIN: Domain = Domain::Rollouts;

    fn key(record: &RolloutRecord) -> String {
        record.thread_id.clone()
    }

    async fn export(
        source: &SqliteSource,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<RolloutRecord>> {
        let Some(pool) = source.pool(SourceDatabase::State).await? else {
            return Ok(Vec::new());
        };
        let rows = sqlx::query(
            "SELECT id, rollout_path, created_at_ms, archived_at FROM threads \
             WHERE (?1 IS NULL OR id > ?1) ORDER BY id LIMIT ?2",
        )
        .bind(after)
        .bind(i64::try_from(limit)?)
        .fetch_all(&pool)
        .await?;
        let mut records = Vec::with_capacity(rows.len());
        for row in rows {
            let id: String = row.try_get("id")?;
            let recorded: String = row.try_get("rollout_path")?;
            let lines = materialize(source, resolve(source.home(), &recorded), 0)
                .await
                .with_context(|| format!("thread {id}"))?;
            records.push(summarize(
                id,
                lines,
                row.try_get::<Option<i64>, _>("created_at_ms")?.unwrap_or(0),
                row.try_get::<Option<i64>, _>("archived_at")?.is_some(),
            ));
        }
        Ok(records)
    }

    async fn import(connection: &mut PgConnection, records: &[RolloutRecord]) -> Result<()> {
        for record in records {
            sqlx::query("DELETE FROM thread_rollout_lines WHERE thread_id = $1::uuid")
                .bind(&record.thread_id)
                .execute(&mut *connection)
                .await?;
            for (chunk_index, chunk) in record.lines.chunks(INSERT_CHUNK).enumerate() {
                let offset = i64::try_from(chunk_index * INSERT_CHUNK)?;
                sqlx::query(
                    "INSERT INTO thread_rollout_lines \
                     (thread_id, position, ordinal, line) \
                     SELECT $1::uuid, $2 + item.index - 1, NULL, item.line \
                     FROM UNNEST($3::text[]) WITH ORDINALITY AS item(line, index)",
                )
                .bind(&record.thread_id)
                .bind(offset)
                .bind(chunk)
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
    ) -> Result<Vec<RolloutRecord>> {
        let ids: Vec<(String, i64, bool)> = sqlx::query_as(
            "SELECT id::text, created_at_ms, archived_at_s IS NOT NULL FROM threads \
             WHERE ($1::uuid IS NULL OR id > $1::uuid) ORDER BY id LIMIT $2",
        )
        .bind(after)
        .bind(i64::try_from(limit)?)
        .fetch_all(&mut *connection)
        .await?;
        let mut records = Vec::with_capacity(ids.len());
        for (id, created_at_ms, archived) in ids {
            let lines: Vec<String> = sqlx::query_scalar(
                "SELECT line FROM thread_rollout_lines \
                 WHERE thread_id = $1::uuid ORDER BY position",
            )
            .bind(&id)
            .fetch_all(&mut *connection)
            .await?;
            records.push(summarize(id, lines, created_at_ms, archived));
        }
        Ok(records)
    }

    async fn write_sqlite(target: &SqliteTarget, records: &[RolloutRecord]) -> Result<()> {
        for record in records {
            // A thread that never wrote a record has no file, like a local thread before its
            // first message.
            if record.lines.is_empty() {
                continue;
            }
            let path = target.staged_path(&target.rollout_path(
                &record.thread_id,
                record.created_at_ms,
                record.archived,
            ));
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            let mut text = record.lines.join("\n");
            text.push('\n');
            tokio::fs::write(&path, text).await?;
        }
        Ok(())
    }
}
