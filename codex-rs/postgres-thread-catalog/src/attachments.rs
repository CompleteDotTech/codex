//! Thread attachments with bounded identity, payload and per-thread count.
//!
//! Writes lock the owning thread row, so the per-thread limit and the identity uniqueness hold
//! under concurrent writers without serializing unrelated threads.

use crate::catalog::PostgresThreadCatalog;
use anyhow::Context;
use anyhow::Result;
use chrono::Utc;
use codex_protocol::ThreadId;
use codex_state::AddThreadAttachmentOutcome;
use codex_state::MAX_THREAD_ATTACHMENT_IDENTITY_KEY_BYTES;
use codex_state::MAX_THREAD_ATTACHMENT_LIST_PAGE_SIZE;
use codex_state::MAX_THREAD_ATTACHMENT_PAYLOAD_BYTES;
use codex_state::MAX_THREAD_ATTACHMENT_TYPE_BYTES;
use codex_state::MAX_THREAD_ATTACHMENTS_PER_THREAD;
use codex_state::RemoveThreadAttachmentOutcome;
use codex_state::ThreadAttachment;
use codex_state::ThreadAttachmentPage;
use serde_json::Value;
use sqlx::PgConnection;
use sqlx::Postgres;
use sqlx::QueryBuilder;
use sqlx::Row;
use sqlx::postgres::PgRow;
use uuid::Uuid;

macro_rules! attachment_columns {
    () => {
        "id, thread_id::text AS thread_id, attachment_type, identity_key, payload, created_at"
    };
}

impl PostgresThreadCatalog {
    /// Atomically copies current membership into a new, empty fork, independent of history cutoffs.
    pub async fn copy_thread_attachments(
        &self,
        source_thread_id: ThreadId,
        destination_thread_id: ThreadId,
    ) -> Result<()> {
        self.write(move |connection| {
            Box::pin(async move {
                lock_thread(connection, destination_thread_id).await?;
                let rows = sqlx::query(
                    "SELECT attachment_type, identity_key, payload \
                     FROM thread_attachments \
                     WHERE thread_id = $1::uuid ORDER BY created_at, id",
                )
                .bind(source_thread_id.to_string())
                .fetch_all(&mut *connection)
                .await?;
                let created_at = Utc::now().timestamp();
                for row in rows {
                    sqlx::query(
                        "INSERT INTO thread_attachments (id, thread_id, \
                         attachment_type, identity_key, payload, created_at) \
                         VALUES ($1, $2::uuid, $3, $4, $5, $6)",
                    )
                    .bind(Uuid::now_v7().to_string())
                    .bind(destination_thread_id.to_string())
                    .bind(row.try_get::<String, _>("attachment_type")?)
                    .bind(row.try_get::<String, _>("identity_key")?)
                    .bind(row.try_get::<String, _>("payload")?)
                    .bind(created_at)
                    .execute(&mut *connection)
                    .await?;
                }
                Ok(())
            })
        })
        .await
    }

    /// Attach an attachment once, returning an existing attachment for repeated requests.
    pub async fn add_thread_attachment(
        &self,
        thread_id: ThreadId,
        attachment_type: &str,
        identity_key: &str,
        payload: &Value,
    ) -> Result<AddThreadAttachmentOutcome> {
        validate_attachment_identity(attachment_type, identity_key)?;
        let serialized_payload = serde_json::to_string(payload).context(
            "invalid thread attachment request: attachment payload cannot be serialized",
        )?;
        if serialized_payload.len() > MAX_THREAD_ATTACHMENT_PAYLOAD_BYTES {
            anyhow::bail!(
                "invalid thread attachment request: attachment payload exceeds {MAX_THREAD_ATTACHMENT_PAYLOAD_BYTES} bytes"
            );
        }
        let (attachment_type, identity_key) =
            (attachment_type.to_string(), identity_key.to_string());
        let payload = payload.clone();
        self.write(move |connection| {
            Box::pin(async move {
                lock_thread(connection, thread_id).await?;
                let existing = sqlx::query(concat!(
                    "SELECT ",
                    attachment_columns!(),
                    " FROM thread_attachments ",
                    "WHERE thread_id = $1::uuid AND attachment_type = $2 AND identity_key = $3"
                ))
                .bind(thread_id.to_string())
                .bind(&attachment_type)
                .bind(&identity_key)
                .fetch_optional(&mut *connection)
                .await?;
                if let Some(existing) = existing {
                    return Ok(AddThreadAttachmentOutcome::Existing(attachment_from_row(
                        &existing,
                    )?));
                }
                let identity_count: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM thread_attachments \
                     WHERE thread_id = $1::uuid",
                )
                .bind(thread_id.to_string())
                .fetch_one(&mut *connection)
                .await?;
                if usize::try_from(identity_count)? >= MAX_THREAD_ATTACHMENTS_PER_THREAD {
                    anyhow::bail!(
                        "invalid thread attachment request: thread attachment identity count exceeds {MAX_THREAD_ATTACHMENTS_PER_THREAD}"
                    );
                }
                let attachment = ThreadAttachment {
                    id: Uuid::now_v7().to_string(),
                    thread_id,
                    attachment_type,
                    identity_key,
                    payload,
                    created_at: Utc::now().timestamp(),
                };
                sqlx::query(
                    "INSERT INTO thread_attachments (id, thread_id, \
                     attachment_type, identity_key, payload, created_at) \
                     VALUES ($1, $2::uuid, $3, $4, $5, $6)",
                )
                .bind(&attachment.id)
                .bind(thread_id.to_string())
                .bind(&attachment.attachment_type)
                .bind(&attachment.identity_key)
                .bind(&serialized_payload)
                .bind(attachment.created_at)
                .execute(&mut *connection)
                .await?;
                Ok(AddThreadAttachmentOutcome::Created(attachment))
            })
        })
        .await
    }

    /// Remove an attached attachment, immediately freeing its slot.
    pub async fn remove_thread_attachment(
        &self,
        thread_id: ThreadId,
        attachment_type: &str,
        identity_key: &str,
    ) -> Result<RemoveThreadAttachmentOutcome> {
        validate_attachment_identity(attachment_type, identity_key)?;
        let (attachment_type, identity_key) =
            (attachment_type.to_string(), identity_key.to_string());
        self.write(move |connection| {
            Box::pin(async move {
                lock_thread(connection, thread_id).await?;
                let removed = sqlx::query(concat!(
                    "DELETE FROM thread_attachments ",
                    "WHERE thread_id = $1::uuid AND attachment_type = $2 AND identity_key = $3 ",
                    "RETURNING ",
                    attachment_columns!()
                ))
                .bind(thread_id.to_string())
                .bind(&attachment_type)
                .bind(&identity_key)
                .fetch_optional(&mut *connection)
                .await?;
                Ok(match removed {
                    Some(row) => RemoveThreadAttachmentOutcome::Removed(attachment_from_row(&row)?),
                    None => RemoveThreadAttachmentOutcome::NotFound,
                })
            })
        })
        .await
    }

    /// List one bounded page of attachments for one thread in stable keyset order.
    pub async fn list_thread_attachments(
        &self,
        thread_id: ThreadId,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<ThreadAttachmentPage> {
        if !(1..=MAX_THREAD_ATTACHMENT_LIST_PAGE_SIZE).contains(&limit) {
            anyhow::bail!(
                "invalid thread attachment request: page limit must be between 1 and {MAX_THREAD_ATTACHMENT_LIST_PAGE_SIZE}"
            );
        }
        let thread_id_string = thread_id.to_string();
        let anchor = cursor.map(parse_attachment_cursor).transpose()?;
        if let Some((cursor_thread_id, _, _)) = anchor.as_ref()
            && cursor_thread_id != &thread_id_string
        {
            anyhow::bail!("invalid thread attachment request: invalid pagination cursor");
        }
        let mut query = QueryBuilder::<Postgres>::new(concat!(
            "SELECT ",
            attachment_columns!(),
            " FROM thread_attachments WHERE thread_id = "
        ));
        query.push_bind(thread_id_string).push("::uuid");
        if let Some((_, created_at, attachment_id)) = anchor {
            query
                .push(" AND (created_at, id) > (")
                .push_bind(created_at)
                .push(", ")
                .push_bind(attachment_id)
                .push(")");
        }
        query
            .push(" ORDER BY created_at ASC, id ASC LIMIT ")
            .push_bind(i64::try_from(limit + 1)?);
        let rows = self.fetch_rows(query).await?;
        let mut attachments = rows
            .iter()
            .map(attachment_from_row)
            .collect::<Result<Vec<_>>>()?;
        let next_cursor = if attachments.len() > limit {
            attachments.pop();
            attachments.last().map(|attachment| {
                format!(
                    "{}|{}|{}",
                    attachment.thread_id, attachment.created_at, attachment.id
                )
            })
        } else {
            None
        };
        Ok(ThreadAttachmentPage {
            attachments,
            next_cursor,
        })
    }
}

/// Lock the owning thread row, which also proves the thread exists.
async fn lock_thread(connection: &mut PgConnection, thread_id: ThreadId) -> Result<()> {
    let exists =
        sqlx::query_scalar::<_, i32>("SELECT 1 FROM threads WHERE id = $1::uuid FOR UPDATE")
            .bind(thread_id.to_string())
            .fetch_optional(connection)
            .await?
            .is_some();
    if !exists {
        anyhow::bail!("thread not found: {thread_id}");
    }
    Ok(())
}

fn validate_attachment_identity(attachment_type: &str, identity_key: &str) -> Result<()> {
    if attachment_type.trim().is_empty() {
        anyhow::bail!("invalid thread attachment request: attachment type must not be empty");
    }
    if attachment_type.len() > MAX_THREAD_ATTACHMENT_TYPE_BYTES {
        anyhow::bail!(
            "invalid thread attachment request: attachment type exceeds {MAX_THREAD_ATTACHMENT_TYPE_BYTES} bytes"
        );
    }
    if identity_key.trim().is_empty() {
        anyhow::bail!(
            "invalid thread attachment request: attachment identity key must not be empty"
        );
    }
    if identity_key.len() > MAX_THREAD_ATTACHMENT_IDENTITY_KEY_BYTES {
        anyhow::bail!(
            "invalid thread attachment request: attachment identity key exceeds {MAX_THREAD_ATTACHMENT_IDENTITY_KEY_BYTES} bytes"
        );
    }
    Ok(())
}

fn parse_attachment_cursor(cursor: &str) -> Result<(String, i64, String)> {
    let mut segments = cursor.split('|');
    let (Some(thread_id), Some(created_at), Some(attachment_id), None) = (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    ) else {
        anyhow::bail!("invalid thread attachment request: invalid pagination cursor");
    };
    if ThreadId::from_string(thread_id).is_err() || Uuid::parse_str(attachment_id).is_err() {
        anyhow::bail!("invalid thread attachment request: invalid pagination cursor");
    }
    let created_at = created_at
        .parse::<i64>()
        .context("invalid thread attachment request: invalid pagination cursor")?;
    Ok((thread_id.to_string(), created_at, attachment_id.to_string()))
}

fn attachment_from_row(row: &PgRow) -> Result<ThreadAttachment> {
    let thread_id: String = row.try_get("thread_id")?;
    let payload: String = row.try_get("payload")?;
    Ok(ThreadAttachment {
        id: row.try_get("id")?,
        thread_id: ThreadId::from_string(&thread_id)
            .context("invalid persisted thread attachment owner")?,
        attachment_type: row.try_get("attachment_type")?,
        identity_key: row.try_get("identity_key")?,
        payload: serde_json::from_str(&payload)
            .context("invalid persisted thread attachment payload")?,
        created_at: row.try_get("created_at")?,
    })
}
