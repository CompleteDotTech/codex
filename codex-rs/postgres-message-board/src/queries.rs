//! Implements the board contract using bounded PostgreSQL queries with offset pagination.

use crate::board::MAX_READ_CHARS;
use crate::board::PostgresAgentMessageBoard;
use crate::board::StoredPost;
use crate::board::channel_summary;
use crate::board::invalid;
use crate::board::load_post;
use crate::board::serialization;
use crate::board::storage;
use crate::paging::Window;
use crate::paging::direction;
use crate::paging::preview;
use caseless::default_case_fold_str;
use codex_agent_message_board_extension::AgentMessageBoard;
use codex_agent_message_board_extension::ChannelQuery;
use codex_agent_message_board_extension::ChannelSummary;
use codex_agent_message_board_extension::CreateChannelRequest;
use codex_agent_message_board_extension::Page;
use codex_agent_message_board_extension::PostContent;
use codex_agent_message_board_extension::PostMetadata;
use codex_agent_message_board_extension::PostPreview;
use codex_agent_message_board_extension::PostQuery;
use codex_agent_message_board_extension::PostRequest;
use codex_agent_message_board_extension::ReadPostRequest;
use codex_agent_message_board_extension::ReadThreadRequest;
use codex_agent_message_board_extension::SubscriptionRequest;
use codex_agent_message_board_extension::SubscriptionState;
use codex_agent_message_board_extension::ThreadPage;
use codex_agent_message_board_extension::ThreadQuery;
use codex_agent_message_board_extension::ThreadSort;
use codex_agent_message_board_extension::ThreadSummary;
use codex_protocol::SessionId;
use codex_protocol::ThreadId;
use codex_protocol::error::Result;
use futures::future::BoxFuture;
use sqlx::Postgres;
use sqlx::QueryBuilder;

fn decode_posts(rows: Vec<String>) -> Result<Vec<StoredPost>> {
    rows.into_iter()
        .map(|row| serde_json::from_str(&row).map_err(serialization))
        .collect()
}

impl AgentMessageBoard for PostgresAgentMessageBoard {
    fn identity(&self) -> SessionId {
        self.identity
    }

    fn create_channel(
        &self,
        caller: ThreadId,
        request: CreateChannelRequest,
    ) -> BoxFuture<'_, Result<ChannelSummary>> {
        Box::pin(PostgresAgentMessageBoard::create_channel(
            self, caller, request,
        ))
    }

    fn post(&self, caller: ThreadId, request: PostRequest) -> BoxFuture<'_, Result<PostMetadata>> {
        Box::pin(PostgresAgentMessageBoard::post(self, caller, request))
    }

    fn set_subscription(
        &self,
        caller: ThreadId,
        request: SubscriptionRequest,
    ) -> BoxFuture<'_, Result<SubscriptionState>> {
        Box::pin(PostgresAgentMessageBoard::set_subscription(
            self, caller, request,
        ))
    }

    fn read_post(
        &self,
        caller: ThreadId,
        request: ReadPostRequest,
    ) -> BoxFuture<'_, Result<PostContent>> {
        Box::pin(PostgresAgentMessageBoard::read_post(self, caller, request))
    }

    fn list_channels(
        &self,
        caller: ThreadId,
        query: ChannelQuery,
    ) -> BoxFuture<'_, Result<Page<ChannelSummary>>> {
        Box::pin(async move {
            self.host.agent_path(caller).await?;
            let board = self.identity.to_string();
            let window = Window::new(&query.page)?;
            let order = direction(query.direction);
            self.read(move |connection| {
                Box::pin(async move {
                    let mut sql = QueryBuilder::<Postgres>::new(
                        "SELECT c.name FROM agent_board_channels c WHERE c.board = ",
                    );
                    sql.push_bind(board.clone())
                        .push(" AND strpos(c.name_search, ")
                        .push_bind(default_case_fold_str(&query.query.unwrap_or_default()))
                        .push(
                            ") > 0 ORDER BY COALESCE( \
                             (SELECT MAX(p.timestamp) FROM agent_board_posts p \
                              WHERE p.board = c.board AND p.channel = c.name), \
                             c.timestamp) ",
                        )
                        .push(order)
                        .push(", c.name ")
                        .push(order)
                        .push(" LIMIT ")
                        .push_bind((window.limit + 1) as i64)
                        .push(" OFFSET ")
                        .push_bind(window.offset());
                    let names = sql
                        .build_query_scalar::<String>()
                        .fetch_all(&mut *connection)
                        .await
                        .map_err(storage)?;
                    let mut channels = Vec::with_capacity(names.len());
                    for name in names {
                        channels.push(channel_summary(connection, &board, &name).await?);
                    }
                    window.finish(channels)
                })
            })
            .await
        })
    }

    fn list_threads(
        &self,
        caller: ThreadId,
        query: ThreadQuery,
    ) -> BoxFuture<'_, Result<Page<ThreadSummary>>> {
        Box::pin(async move {
            self.host.agent_path(caller).await?;
            let board = self.identity.to_string();
            let window = Window::new(&query.page)?;
            let order = direction(query.direction);
            self.read(move |connection| {
                Box::pin(async move {
                    let exists: bool = sqlx::query_scalar(
                        "SELECT EXISTS(SELECT 1 FROM agent_board_channels \
                         WHERE board = $1 AND name = $2)",
                    )
                    .bind(&board)
                    .bind(&query.channel_name)
                    .fetch_one(&mut *connection)
                    .await
                    .map_err(storage)?;
                    if !exists {
                        return Err(invalid("channel not found in this board"));
                    }
                    // Select the page before looking up reply summaries, especially when
                    // activity sorting examines more roots than the page will return.
                    let mut sql = QueryBuilder::<Postgres>::new(
                        "WITH page AS MATERIALIZED (SELECT p.board, p.id, p.payload, p.seq, ",
                    );
                    match query.sort {
                        ThreadSort::Created => {
                            sql.push("p.timestamp");
                        }
                        ThreadSort::Activity => {
                            sql.push(
                                "(SELECT MAX(r.timestamp) FROM agent_board_posts r \
                                 WHERE r.board = p.board AND r.root = p.id)",
                            );
                        }
                    }
                    sql.push(" AS sort_timestamp FROM agent_board_posts p WHERE p.board = ")
                        .push_bind(board)
                        .push(" AND p.channel = ")
                        .push_bind(query.channel_name)
                        .push(" AND p.id = p.root ORDER BY sort_timestamp ")
                        .push(order)
                        .push(", p.seq ")
                        .push(order)
                        .push(" LIMIT ")
                        .push_bind((window.limit + 1) as i64)
                        .push(" OFFSET ")
                        .push_bind(window.offset())
                        .push(
                            ") SELECT p.payload, \
                         (SELECT COUNT(*) FROM agent_board_posts r \
                          WHERE r.board = p.board AND r.root = p.id AND r.id <> r.root), \
                         (SELECT r.payload FROM agent_board_posts r \
                          WHERE r.board = p.board AND r.root = p.id AND r.id <> r.root \
                          ORDER BY r.timestamp DESC, r.seq DESC LIMIT 1) \
                         FROM page p ORDER BY p.sort_timestamp ",
                        )
                        .push(order)
                        .push(", p.seq ")
                        .push(order);
                    let rows = sql
                        .build_query_as::<(String, i64, Option<String>)>()
                        .fetch_all(&mut *connection)
                        .await
                        .map_err(storage)?;
                    let chars = (query.max_chars_per_post.get() as usize)
                        .min(MAX_READ_CHARS / (2 * rows.len().min(window.limit).max(1)));
                    let mut threads = Vec::with_capacity(rows.len());
                    for (root, count, last) in rows {
                        let root: StoredPost =
                            serde_json::from_str(&root).map_err(serialization)?;
                        let id = root.metadata.message_id;
                        let last: Option<StoredPost> = last
                            .map(|value| serde_json::from_str(&value))
                            .transpose()
                            .map_err(serialization)?;
                        let activity = last.as_ref().map_or(root.metadata.created_at, |last| {
                            last.metadata.created_at.max(root.metadata.created_at)
                        });
                        threads.push(ThreadSummary {
                            thread_id: id,
                            root_post: preview(root, chars),
                            reply_count: count as usize,
                            last_activity_at: activity,
                            latest_reply: last.map(|post| preview(post, chars)),
                        });
                    }
                    window.finish(threads)
                })
            })
            .await
        })
    }

    fn search_posts(
        &self,
        caller: ThreadId,
        query: PostQuery,
    ) -> BoxFuture<'_, Result<Page<PostPreview>>> {
        Box::pin(async move {
            self.host.agent_path(caller).await?;
            let board = self.identity.to_string();
            let window = Window::new(&query.page)?;
            self.read(move |connection| {
                Box::pin(async move {
                    let after = if let Some(id) = query.after_message_id {
                        Some(
                            sqlx::query_as::<_, (i64, i64)>(
                                "SELECT timestamp, seq FROM agent_board_posts \
                                 WHERE board = $1 AND id = $2",
                            )
                            .bind(&board)
                            .bind(id.to_string())
                            .fetch_optional(&mut *connection)
                            .await
                            .map_err(storage)?
                            .ok_or_else(|| invalid("post not found in this board"))?,
                        )
                    } else {
                        None
                    };
                    let mut sql = QueryBuilder::<Postgres>::new(
                        "SELECT payload FROM agent_board_posts WHERE board = ",
                    );
                    sql.push_bind(board);
                    if let Some(channel) = query.channel_name {
                        sql.push(" AND channel = ").push_bind(channel);
                    }
                    if let Some(author) = query.author {
                        sql.push(" AND author = ").push_bind(author.to_string());
                    }
                    if let Some(text) = query.query {
                        sql.push(" AND strpos(body_search, ")
                            .push_bind(default_case_fold_str(&text))
                            .push(") > 0");
                    }
                    if let Some((timestamp, seq)) = after {
                        sql.push(" AND (timestamp, seq) > (")
                            .push_bind(timestamp)
                            .push(", ")
                            .push_bind(seq)
                            .push(")");
                    }
                    sql.push(" ORDER BY timestamp DESC, seq DESC LIMIT ")
                        .push_bind((window.limit + 1) as i64)
                        .push(" OFFSET ")
                        .push_bind(window.offset());
                    let posts = decode_posts(
                        sql.build_query_scalar::<String>()
                            .fetch_all(&mut *connection)
                            .await
                            .map_err(storage)?,
                    )?;
                    let chars = (query.max_chars_per_post.get() as usize)
                        .min(MAX_READ_CHARS / posts.len().min(window.limit).max(1));
                    window.finish(posts.into_iter().map(|post| preview(post, chars)).collect())
                })
            })
            .await
        })
    }

    fn read_thread(
        &self,
        caller: ThreadId,
        request: ReadThreadRequest,
    ) -> BoxFuture<'_, Result<ThreadPage>> {
        Box::pin(async move {
            self.host.agent_path(caller).await?;
            let board = self.identity.to_string();
            let window = Window::new(&request.page)?;
            self.read(move |connection| {
                Box::pin(async move {
                    let root = load_post(connection, &board, request.thread_id).await?;
                    if root.metadata.thread_id != request.thread_id {
                        return Err(invalid("thread_id must identify a top-level post"));
                    }
                    let posts = decode_posts(
                        sqlx::query_scalar(
                            "SELECT payload FROM agent_board_posts \
                             WHERE board = $1 AND root = $2 AND id <> root \
                             ORDER BY timestamp DESC, seq DESC LIMIT $3 OFFSET $4",
                        )
                        .bind(&board)
                        .bind(request.thread_id.to_string())
                        .bind((window.limit + 1) as i64)
                        .bind(window.offset())
                        .fetch_all(&mut *connection)
                        .await
                        .map_err(storage)?,
                    )?;
                    let chars = (request.max_chars_per_post.get() as usize)
                        .min(MAX_READ_CHARS / (posts.len().min(window.limit) + 1));
                    Ok(ThreadPage {
                        root_post: preview(root, chars),
                        replies: window
                            .finish(posts.into_iter().map(|post| preview(post, chars)).collect())?,
                    })
                })
            })
            .await
        })
    }
}
