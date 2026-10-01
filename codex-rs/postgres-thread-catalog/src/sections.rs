//! Thread sections and the stable sparse ordering of threads inside them.
//!
//! Section writes lock the sections table in share row exclusive mode, which serializes them
//! the way SQLite immediate transactions do. Positions are sparse so a move usually rewrites
//! one row, and a section renumbers only when no gap remains.

use crate::catalog::PostgresThreadCatalog;
use anyhow::Result;
use anyhow::anyhow;
use chrono::DateTime;
use chrono::Utc;
use codex_protocol::ThreadId;
use codex_state::PINNED_THREAD_SECTION_ID;
use codex_state::ThreadSection;
use codex_state::ThreadSectionAppearance;
use codex_state::ThreadSectionsPage;
use sqlx::PgConnection;
use std::collections::HashMap;
use uuid::Uuid;

const SECTION_POSITION_GAP: i64 = 1_000_000;
const LOCK_SECTIONS: &str = "LOCK TABLE thread_sections IN SHARE ROW EXCLUSIVE MODE";

fn section_from_row(
    (id, name, appearance): (String, String, Option<String>),
) -> Result<ThreadSection> {
    Ok(ThreadSection {
        id,
        name,
        appearance: appearance
            .map(|appearance| serde_json::from_str::<ThreadSectionAppearance>(&appearance))
            .transpose()?,
    })
}

impl PostgresThreadCatalog {
    /// Create a custom thread section with a stable, server-assigned UUIDv7.
    pub async fn create_thread_section(
        &self,
        name: &str,
        appearance: Option<ThreadSectionAppearance>,
    ) -> Result<ThreadSection> {
        let section = ThreadSection {
            id: Uuid::now_v7().to_string(),
            name: name.to_string(),
            appearance,
        };
        let stored = section.clone();
        self.write(move |connection| {
            Box::pin(async move {
                sqlx::query(
                    "INSERT INTO thread_sections (id, name, appearance) \
                     VALUES ($1, $2, $3)",
                )
                .bind(&stored.id)
                .bind(&stored.name)
                .bind(
                    stored
                        .appearance
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()?,
                )
                .execute(&mut *connection)
                .await?;
                Ok(())
            })
        })
        .await?;
        Ok(section)
    }

    /// Rename a custom thread section without changing its stable identity.
    pub async fn rename_thread_section(
        &self,
        id: &str,
        name: &str,
        appearance: Option<Option<ThreadSectionAppearance>>,
    ) -> Result<Option<ThreadSection>> {
        if id == PINNED_THREAD_SECTION_ID {
            anyhow::bail!("built-in pinned thread section cannot be renamed");
        }
        let replace_appearance = appearance.is_some();
        let appearance = appearance
            .flatten()
            .map(|appearance| serde_json::to_string(&appearance))
            .transpose()?;
        let (id, name) = (id.to_string(), name.to_string());
        let section = self
            .write(move |connection| {
                Box::pin(async move {
                    Ok(sqlx::query_as::<_, (String, String, Option<String>)>(
                        "UPDATE thread_sections SET name = $1, \
                         appearance = CASE WHEN $2 THEN $3 ELSE appearance END \
                         WHERE id = $4 RETURNING id, name, appearance",
                    )
                    .bind(&name)
                    .bind(replace_appearance)
                    .bind(&appearance)
                    .bind(&id)
                    .fetch_optional(&mut *connection)
                    .await?)
                })
            })
            .await?;
        section.map(section_from_row).transpose()
    }

    /// Delete a custom section and return its threads to the unsectioned list.
    pub async fn delete_thread_section(&self, id: &str) -> Result<bool> {
        if id == PINNED_THREAD_SECTION_ID {
            anyhow::bail!("built-in pinned thread section cannot be deleted");
        }
        let id = id.to_string();
        self.write(move |connection| {
            Box::pin(async move {
                lock_sections(connection).await?;
                sqlx::query(
                    "UPDATE threads SET section_position = NULL, \
                     section_entered_at_ms = NULL WHERE thread_section_id = $1",
                )
                .bind(&id)
                .execute(&mut *connection)
                .await?;
                Ok(sqlx::query("DELETE FROM thread_sections WHERE id = $1")
                    .bind(&id)
                    .execute(&mut *connection)
                    .await?
                    .rows_affected()
                    > 0)
            })
        })
        .await
    }

    /// Read persisted section ordering for multiple threads in one query.
    pub async fn get_thread_section_ordering(
        &self,
        thread_ids: &[ThreadId],
    ) -> Result<HashMap<ThreadId, (Option<i64>, Option<DateTime<Utc>>)>> {
        if thread_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let ids: Vec<String> = thread_ids.iter().map(ToString::to_string).collect();
        let rows = self
            .read(move |connection| {
                Box::pin(async move {
                    Ok(sqlx::query_as::<_, (String, Option<i64>, Option<i64>)>(
                        "SELECT id::text, section_position, section_entered_at_ms \
                         FROM threads WHERE id = ANY($1::uuid[])",
                    )
                    .bind(&ids)
                    .fetch_all(&mut *connection)
                    .await?)
                })
            })
            .await?;
        rows.into_iter()
            .map(|(thread_id, section_position, section_entered_at_ms)| {
                let thread_id = ThreadId::try_from(thread_id)?;
                let section_entered_at = section_entered_at_ms
                    .map(|millis| {
                        DateTime::<Utc>::from_timestamp_millis(millis)
                            .ok_or_else(|| anyhow!("invalid unix timestamp millis: {millis}"))
                    })
                    .transpose()?;
                Ok((thread_id, (section_position, section_entered_at)))
            })
            .collect()
    }

    /// Read an independently persisted thread section by its opaque identifier.
    pub async fn get_thread_section(&self, id: &str) -> Result<Option<ThreadSection>> {
        let id = id.to_string();
        let row = self
            .read(move |connection| {
                Box::pin(async move {
                    Ok(sqlx::query_as::<_, (String, String, Option<String>)>(
                        "SELECT id, name, appearance FROM thread_sections \
                         WHERE id = $1",
                    )
                    .bind(&id)
                    .fetch_optional(&mut *connection)
                    .await?)
                })
            })
            .await?;
        row.map(section_from_row).transpose()
    }

    /// List sections in stable, cursor-paginated identifier order.
    pub async fn list_thread_sections(
        &self,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<ThreadSectionsPage> {
        let page_size = limit.max(1);
        let fetch_limit = i64::try_from(page_size.saturating_add(1))?;
        let cursor = cursor.map(str::to_string);
        let rows = self
            .read(move |connection| {
                Box::pin(async move {
                    Ok(sqlx::query_as::<_, (String, String, Option<String>)>(
                        "SELECT id, name, appearance FROM thread_sections \
                         WHERE ($1::text IS NULL OR id > $1) ORDER BY id LIMIT $2",
                    )
                    .bind(&cursor)
                    .bind(fetch_limit)
                    .fetch_all(&mut *connection)
                    .await?)
                })
            })
            .await?;
        let mut sections = rows
            .into_iter()
            .map(section_from_row)
            .collect::<Result<Vec<_>>>()?;
        let next_cursor = if sections.len() > page_size {
            sections.pop();
            sections.last().map(|section| section.id.clone())
        } else {
            None
        };
        Ok(ThreadSectionsPage {
            sections,
            next_cursor,
        })
    }

    /// Move a thread into or within a section, or clear its section.
    ///
    /// Omitting `before_thread_id` appends the thread to its destination section.
    pub async fn move_thread_to_section(
        &self,
        thread_id: ThreadId,
        section: Option<&str>,
        before_thread_id: Option<ThreadId>,
    ) -> Result<bool> {
        if section.is_none() && before_thread_id.is_some() {
            return Err(anyhow!(
                "before thread cannot be specified without a section"
            ));
        }
        let section = section.map(str::to_string);
        self.write(move |connection| {
            Box::pin(async move {
                lock_sections(connection).await?;
                let thread_id = thread_id.to_string();
                let current_section = sqlx::query_scalar::<_, Option<String>>(
                    "SELECT thread_section_id FROM threads \
                     WHERE id = $1::uuid FOR UPDATE",
                )
                .bind(&thread_id)
                .fetch_optional(&mut *connection)
                .await?;
                let Some(current_section) = current_section else {
                    return Ok(false);
                };
                let Some(section) = section else {
                    sqlx::query(
                        "UPDATE threads SET thread_section_id = NULL, \
                         section_position = NULL, section_entered_at_ms = NULL \
                         WHERE id = $1::uuid",
                    )
                    .bind(&thread_id)
                    .execute(&mut *connection)
                    .await?;
                    return Ok(true);
                };
                let exists: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM thread_sections WHERE id = $1)",
                )
                .bind(&section)
                .fetch_one(&mut *connection)
                .await?;
                if !exists {
                    return Err(anyhow!("section {section} does not exist"));
                }
                let before_thread_id = before_thread_id.map(|id| id.to_string());
                if before_thread_id.as_deref() == Some(thread_id.as_str()) {
                    return Err(anyhow!("thread {thread_id} cannot be moved before itself"));
                }
                if let Some(before_thread_id) = before_thread_id.as_deref() {
                    let before_section = sqlx::query_scalar::<_, Option<String>>(
                        "SELECT thread_section_id FROM threads \
                         WHERE id = $1::uuid",
                    )
                    .bind(before_thread_id)
                    .fetch_optional(&mut *connection)
                    .await?;
                    if before_section.flatten().as_deref() != Some(section.as_str()) {
                        return Err(anyhow!(
                            "before thread {before_thread_id} is not in section {section}"
                        ));
                    }
                }
                let position = section_move_position(
                    connection,
                    &section,
                    &thread_id,
                    before_thread_id.as_deref(),
                )
                .await?;
                if current_section.as_deref() == Some(section.as_str()) {
                    sqlx::query(
                        "UPDATE threads SET section_position = $1 \
                         WHERE id = $2::uuid",
                    )
                    .bind(position)
                    .bind(&thread_id)
                    .execute(&mut *connection)
                    .await?;
                } else {
                    sqlx::query(
                        "UPDATE threads SET thread_section_id = $1, \
                         section_position = $2, section_entered_at_ms = $3 WHERE id = $4::uuid",
                    )
                    .bind(&section)
                    .bind(position)
                    .bind(Utc::now().timestamp_millis())
                    .bind(&thread_id)
                    .execute(&mut *connection)
                    .await?;
                }
                Ok(true)
            })
        })
        .await
    }
}

async fn lock_sections(connection: &mut PgConnection) -> Result<()> {
    sqlx::query(LOCK_SECTIONS).execute(connection).await?;
    Ok(())
}

async fn section_move_position(
    connection: &mut PgConnection,
    section: &str,
    thread_id: &str,
    before_thread_id: Option<&str>,
) -> Result<i64> {
    let mut renumbered = false;
    loop {
        let position = if let Some(before_thread_id) = before_thread_id {
            let upper = sqlx::query_scalar::<_, Option<i64>>(
                "SELECT section_position FROM threads \
                 WHERE id = $1::uuid AND thread_section_id = $2",
            )
            .bind(before_thread_id)
            .bind(section)
            .fetch_optional(&mut *connection)
            .await?
            .flatten()
            .ok_or_else(|| {
                anyhow!("before thread {before_thread_id} is not in section {section}")
            })?;
            let lower = sqlx::query_scalar::<_, Option<i64>>(
                "SELECT MAX(section_position) FROM threads \
                 WHERE thread_section_id = $1 AND section_position < $2 AND id <> $3::uuid",
            )
            .bind(section)
            .bind(upper)
            .bind(thread_id)
            .fetch_one(&mut *connection)
            .await?;
            match lower {
                Some(lower) if i128::from(upper) - i128::from(lower) > 1 => Some(i64::try_from(
                    i128::from(lower) + (i128::from(upper) - i128::from(lower)) / 2,
                )?),
                Some(_) => None,
                None if upper > 1 => Some(upper / 2),
                None => None,
            }
        } else {
            let max_position = sqlx::query_scalar::<_, Option<i64>>(
                "SELECT MAX(section_position) FROM threads \
                 WHERE thread_section_id = $1 AND id <> $2::uuid",
            )
            .bind(section)
            .bind(thread_id)
            .fetch_one(&mut *connection)
            .await?;
            max_position
                .unwrap_or_default()
                .checked_add(SECTION_POSITION_GAP)
        };
        if let Some(position) = position {
            return Ok(position);
        }
        if renumbered {
            return Err(anyhow!(
                "section {section} has no remaining thread positions"
            ));
        }
        sqlx::query(
            "UPDATE threads SET section_position = ranked.position FROM ( \
               SELECT id, ROW_NUMBER() OVER ( \
                 ORDER BY section_position ASC NULLS FIRST, id ASC) * $1 AS position \
               FROM threads \
               WHERE thread_section_id = $2 AND id <> $3::uuid) AS ranked \
             WHERE threads.id = ranked.id",
        )
        .bind(SECTION_POSITION_GAP)
        .bind(section)
        .bind(thread_id)
        .execute(&mut *connection)
        .await?;
        renumbered = true;
    }
}
