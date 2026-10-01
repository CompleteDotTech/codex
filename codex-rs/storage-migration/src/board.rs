//! The agent message board: deletion tombstones, channels, posts and subscriptions.
//!
//! Board rows are immutable once written and the runtime role cannot rewrite them, so imports
//! leave a row that is already present untouched.

use crate::domain::Domain;
use crate::domain::DomainOps;
use crate::source::SourceDatabase;
use crate::source::SqliteSource;
use anyhow::Result;
use serde::Serialize;
use sqlx::PgConnection;
use sqlx::Row;

const SEPARATOR: char = '\u{1f}';

fn parts<const N: usize>(after: Option<&str>) -> Option<[&str; N]> {
    let cursor = after?;
    let mut pieces = [""; N];
    let mut split = cursor.splitn(N, SEPARATOR);
    for piece in &mut pieces {
        *piece = split.next()?;
    }
    Some(pieces)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct DeletedBoardRecord {
    board: String,
}

pub(crate) struct DeletedBoards;

impl DomainOps for DeletedBoards {
    type Record = DeletedBoardRecord;

    const DOMAIN: Domain = Domain::BoardDeleted;

    fn key(record: &DeletedBoardRecord) -> String {
        record.board.clone()
    }

    async fn export(
        source: &SqliteSource,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<DeletedBoardRecord>> {
        let Some(pool) = source.pool(SourceDatabase::Board).await? else {
            return Ok(Vec::new());
        };
        let rows = sqlx::query(
            "SELECT board FROM deleted_boards WHERE (?1 IS NULL OR board > ?1) \
             ORDER BY board LIMIT ?2",
        )
        .bind(after)
        .bind(i64::try_from(limit)?)
        .fetch_all(&pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(DeletedBoardRecord {
                    board: row.try_get("board")?,
                })
            })
            .collect()
    }

    async fn import(connection: &mut PgConnection, records: &[DeletedBoardRecord]) -> Result<()> {
        for record in records {
            sqlx::query(
                "INSERT INTO agent_board_deleted (board) VALUES ($1) \
                 ON CONFLICT (board) DO NOTHING",
            )
            .bind(&record.board)
            .execute(&mut *connection)
            .await?;
        }
        Ok(())
    }

    async fn read_back(
        connection: &mut PgConnection,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<DeletedBoardRecord>> {
        let rows = sqlx::query(
            "SELECT board FROM agent_board_deleted \
             WHERE ($1::text IS NULL OR board > $1) ORDER BY board LIMIT $2",
        )
        .bind(after)
        .bind(i64::try_from(limit)?)
        .fetch_all(connection)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(DeletedBoardRecord {
                    board: row.try_get("board")?,
                })
            })
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ChannelRecord {
    board: String,
    name: String,
    name_search: String,
    created_at: String,
    timestamp: i64,
    author: String,
}

pub(crate) struct Channels;

fn channel_from_row<R>(row: &R) -> Result<ChannelRecord>
where
    R: Row,
    for<'r> i64: sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
    for<'r> String: sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
    for<'r> &'r str: sqlx::ColumnIndex<R>,
{
    Ok(ChannelRecord {
        board: row.try_get("board")?,
        name: row.try_get("name")?,
        name_search: row.try_get("name_search")?,
        created_at: row.try_get("created_at")?,
        timestamp: row.try_get("timestamp")?,
        author: row.try_get("author")?,
    })
}

impl DomainOps for Channels {
    type Record = ChannelRecord;

    const DOMAIN: Domain = Domain::BoardChannels;

    fn key(record: &ChannelRecord) -> String {
        format!("{}{SEPARATOR}{}", record.board, record.name)
    }

    async fn export(
        source: &SqliteSource,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ChannelRecord>> {
        let Some(pool) = source.pool(SourceDatabase::Board).await? else {
            return Ok(Vec::new());
        };
        let [board, name] = parts::<2>(after).unwrap_or(["", ""]);
        let rows = sqlx::query(
            "SELECT board, name, name_search, created_at, timestamp, author FROM channels \
             WHERE (?1 = 0 OR (board, name) > (?2, ?3)) ORDER BY board, name LIMIT ?4",
        )
        .bind(i64::from(after.is_some()))
        .bind(board)
        .bind(name)
        .bind(i64::try_from(limit)?)
        .fetch_all(&pool)
        .await?;
        rows.iter().map(channel_from_row).collect()
    }

    async fn import(connection: &mut PgConnection, records: &[ChannelRecord]) -> Result<()> {
        for record in records {
            sqlx::query(
                "INSERT INTO agent_board_channels \
                 (board, name, name_search, created_at, timestamp, author) \
                 VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (board, name) DO NOTHING",
            )
            .bind(&record.board)
            .bind(&record.name)
            .bind(&record.name_search)
            .bind(&record.created_at)
            .bind(record.timestamp)
            .bind(&record.author)
            .execute(&mut *connection)
            .await?;
        }
        Ok(())
    }

    async fn read_back(
        connection: &mut PgConnection,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ChannelRecord>> {
        let [board, name] = parts::<2>(after).unwrap_or(["", ""]);
        let rows = sqlx::query(
            "SELECT board, name, name_search, created_at, timestamp, author \
             FROM agent_board_channels \
             WHERE ($1 = FALSE OR (board, name) > ($2, $3)) ORDER BY board, name LIMIT $4",
        )
        .bind(after.is_some())
        .bind(board)
        .bind(name)
        .bind(i64::try_from(limit)?)
        .fetch_all(connection)
        .await?;
        rows.iter().map(channel_from_row).collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct PostRecord {
    seq: i64,
    board: String,
    id: String,
    channel: String,
    root: String,
    author: String,
    timestamp: i64,
    body_search: String,
    payload: String,
    request_id: String,
    request: String,
}

pub(crate) struct Posts;

fn post_from_row<R>(row: &R) -> Result<PostRecord>
where
    R: Row,
    for<'r> i64: sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
    for<'r> String: sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
    for<'r> &'r str: sqlx::ColumnIndex<R>,
{
    Ok(PostRecord {
        seq: row.try_get("seq")?,
        board: row.try_get("board")?,
        id: row.try_get("id")?,
        channel: row.try_get("channel")?,
        root: row.try_get("root")?,
        author: row.try_get("author")?,
        timestamp: row.try_get("timestamp")?,
        body_search: row.try_get("body_search")?,
        payload: row.try_get("payload")?,
        request_id: row.try_get("request_id")?,
        request: row.try_get("request")?,
    })
}

impl DomainOps for Posts {
    type Record = PostRecord;

    const DOMAIN: Domain = Domain::BoardPosts;

    fn key(record: &PostRecord) -> String {
        record.seq.to_string()
    }

    async fn export(
        source: &SqliteSource,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<PostRecord>> {
        let Some(pool) = source.pool(SourceDatabase::Board).await? else {
            return Ok(Vec::new());
        };
        let rows = sqlx::query(
            "SELECT seq, board, id, channel, root, author, timestamp, body_search, payload, \
             request_id, request FROM posts WHERE seq > ?1 ORDER BY seq LIMIT ?2",
        )
        .bind(
            after
                .map(str::parse::<i64>)
                .transpose()?
                .unwrap_or(i64::MIN),
        )
        .bind(i64::try_from(limit)?)
        .fetch_all(&pool)
        .await?;
        rows.iter().map(post_from_row).collect()
    }

    async fn import(connection: &mut PgConnection, records: &[PostRecord]) -> Result<()> {
        for record in records {
            sqlx::query(
                "INSERT INTO agent_board_posts (seq, board, id, channel, root, \
                 author, timestamp, body_search, payload, request_id, request) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
                 ON CONFLICT (seq) DO NOTHING",
            )
            .bind(record.seq)
            .bind(&record.board)
            .bind(&record.id)
            .bind(&record.channel)
            .bind(&record.root)
            .bind(&record.author)
            .bind(record.timestamp)
            .bind(&record.body_search)
            .bind(&record.payload)
            .bind(&record.request_id)
            .bind(&record.request)
            .execute(&mut *connection)
            .await?;
        }
        // New posts must be numbered after every imported sequence number.
        if let Some(highest) = records.iter().map(|record| record.seq).max() {
            sqlx::query(
                "UPDATE agent_board_post_counter \
                 SET last_seq = GREATEST(last_seq, $1) WHERE singleton",
            )
            .bind(highest)
            .execute(&mut *connection)
            .await?;
        }
        Ok(())
    }

    async fn read_back(
        connection: &mut PgConnection,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<PostRecord>> {
        let rows = sqlx::query(
            "SELECT seq, board, id, channel, root, author, timestamp, body_search, payload, \
             request_id, request FROM agent_board_posts \
             WHERE seq > $1 ORDER BY seq LIMIT $2",
        )
        .bind(
            after
                .map(str::parse::<i64>)
                .transpose()?
                .unwrap_or(i64::MIN),
        )
        .bind(i64::try_from(limit)?)
        .fetch_all(connection)
        .await?;
        rows.iter().map(post_from_row).collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct MembershipRecord {
    board: String,
    target: String,
    agent: String,
}

fn membership_from_row<R>(row: &R) -> Result<MembershipRecord>
where
    R: Row,
    for<'r> String: sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
    for<'r> &'r str: sqlx::ColumnIndex<R>,
{
    Ok(MembershipRecord {
        board: row.try_get("board")?,
        target: row.try_get("target")?,
        agent: row.try_get("agent")?,
    })
}

fn membership_key(record: &MembershipRecord) -> String {
    format!(
        "{}{SEPARATOR}{}{SEPARATOR}{}",
        record.board, record.target, record.agent
    )
}

/// Subscriptions and opt-outs have the same shape and differ only in their tables.
macro_rules! membership_domain {
    ($name:ident, $domain:expr, $sqlite_table:literal, $pg_table:literal) => {
        pub(crate) struct $name;

        impl DomainOps for $name {
            type Record = MembershipRecord;

            const DOMAIN: Domain = $domain;

            fn key(record: &MembershipRecord) -> String {
                membership_key(record)
            }

            async fn export(
                source: &SqliteSource,
                after: Option<&str>,
                limit: usize,
            ) -> Result<Vec<MembershipRecord>> {
                let Some(pool) = source.pool(SourceDatabase::Board).await? else {
                    return Ok(Vec::new());
                };
                let [board, target, agent] = parts::<3>(after).unwrap_or(["", "", ""]);
                let rows = sqlx::query(concat!(
                    "SELECT board, target, agent FROM ",
                    $sqlite_table,
                    " WHERE (?1 = 0 OR (board, target, agent) > (?2, ?3, ?4)) \
                     ORDER BY board, target, agent LIMIT ?5"
                ))
                .bind(i64::from(after.is_some()))
                .bind(board)
                .bind(target)
                .bind(agent)
                .bind(i64::try_from(limit)?)
                .fetch_all(&pool)
                .await?;
                rows.iter().map(membership_from_row).collect()
            }

            async fn import(
                connection: &mut PgConnection,
                records: &[MembershipRecord],
            ) -> Result<()> {
                for record in records {
                    sqlx::query(concat!(
                        "INSERT INTO ",
                        $pg_table,
                        " (board, target, agent) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING"
                    ))
                    .bind(&record.board)
                    .bind(&record.target)
                    .bind(&record.agent)
                    .execute(&mut *connection)
                    .await?;
                }
                Ok(())
            }

            async fn read_back(
                connection: &mut PgConnection,
                after: Option<&str>,
                limit: usize,
            ) -> Result<Vec<MembershipRecord>> {
                let [board, target, agent] = parts::<3>(after).unwrap_or(["", "", ""]);
                let rows = sqlx::query(concat!(
                    "SELECT board, target, agent FROM ",
                    $pg_table,
                    " WHERE ($1 = FALSE OR (board, target, agent) > ($2, $3, $4)) \
                     ORDER BY board, target, agent LIMIT $5"
                ))
                .bind(after.is_some())
                .bind(board)
                .bind(target)
                .bind(agent)
                .bind(i64::try_from(limit)?)
                .fetch_all(connection)
                .await?;
                rows.iter().map(membership_from_row).collect()
            }
        }
    };
}

membership_domain!(
    Subscriptions,
    Domain::BoardSubscriptions,
    "subscriptions",
    "agent_board_subscriptions"
);
membership_domain!(
    OptOuts,
    Domain::BoardOptOuts,
    "subscription_opt_outs",
    "agent_board_opt_outs"
);
