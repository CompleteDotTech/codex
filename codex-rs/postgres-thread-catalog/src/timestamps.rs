//! Unique, monotonic thread timestamps for cursor ordering.
//!
//! SQLite keeps a process-local high-water mark. Remote clients share one mark per namespace and
//! advance it in the same transaction as the write that uses it, so two hosts can never persist
//! the same timestamp for ordered listings.

use anyhow::Result;
use chrono::DateTime;
use chrono::Utc;
use sqlx::PgConnection;
use sqlx::Row;

/// One value allocated for a write, with the mark that must be saved if it moved.
pub(crate) struct Marks {
    updated_at_ms: i64,
    recency_at_ms: i64,
    changed: bool,
}

/// Lock the shared marks. Callers allocate from the returned value, then call [`Marks::save`].
pub(crate) async fn lock_marks(connection: &mut PgConnection) -> Result<Marks> {
    let row = sqlx::query(
        "SELECT updated_at_ms, recency_at_ms FROM codex_storage.thread_timestamp_marks \
         WHERE singleton FOR UPDATE",
    )
    .fetch_one(connection)
    .await?;
    Ok(Marks {
        updated_at_ms: row.try_get("updated_at_ms")?,
        recency_at_ms: row.try_get("recency_at_ms")?,
        changed: false,
    })
}

impl Marks {
    pub(crate) fn allocate_updated_at(&mut self, timestamp: DateTime<Utc>) -> i64 {
        self.changed = true;
        allocate(&mut self.updated_at_ms, timestamp.timestamp_millis())
    }

    pub(crate) fn allocate_recency_at(&mut self, timestamp: DateTime<Utc>) -> i64 {
        self.changed = true;
        allocate(&mut self.recency_at_ms, timestamp.timestamp_millis())
    }

    pub(crate) async fn save(self, connection: &mut PgConnection) -> Result<()> {
        if self.changed {
            sqlx::query(
                "UPDATE codex_storage.thread_timestamp_marks \
                 SET updated_at_ms = $1, recency_at_ms = $2 WHERE singleton",
            )
            .bind(self.updated_at_ms)
            .bind(self.recency_at_ms)
            .execute(connection)
            .await?;
        }
        Ok(())
    }
}

/// Mirrors the SQLite allocation rules: newer time advances the mark, much older time (backfill
/// and repair) passes through unchanged, and the same hot second gets the next millisecond.
fn allocate(high_water_mark: &mut i64, candidate: i64) -> i64 {
    let current = *high_water_mark;
    if candidate > current {
        *high_water_mark = candidate;
        return candidate;
    }
    if candidate.saturating_add(1000) <= current {
        return candidate;
    }
    let bumped = current.saturating_add(1);
    *high_water_mark = bumped;
    bumped
}
