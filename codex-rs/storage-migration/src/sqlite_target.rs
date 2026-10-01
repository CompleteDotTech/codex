//! A fresh SQLite home that a PostgreSQL dataset is written into.
//!
//! The target is a staging directory, never the live home. Its databases get their schema from
//! the same migrations the runtime applies, so a staged home opens like any other. The paths it
//! records inside rows are the paths the files will have once the stage is installed in the
//! final home.

use anyhow::Result;
use codex_state::SqliteConfig;
use codex_state::StateRuntime;
use sqlx::SqlitePool;
use std::path::Path;
use std::path::PathBuf;

/// The statements that create the message board's database, matching the board's own schema.
const BOARD_SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS deleted_boards (board TEXT PRIMARY KEY NOT NULL)",
    "CREATE TABLE IF NOT EXISTS channels (board TEXT NOT NULL, name TEXT NOT NULL, \
     name_search TEXT NOT NULL, created_at TEXT NOT NULL, timestamp INTEGER NOT NULL, \
     author TEXT NOT NULL, PRIMARY KEY(board,name))",
    "CREATE TABLE IF NOT EXISTS posts (seq INTEGER PRIMARY KEY AUTOINCREMENT, \
     board TEXT NOT NULL, id TEXT NOT NULL, channel TEXT NOT NULL, root TEXT NOT NULL, \
     author TEXT NOT NULL, timestamp INTEGER NOT NULL, body_search TEXT NOT NULL, \
     payload TEXT NOT NULL, request_id TEXT NOT NULL, request TEXT NOT NULL, \
     UNIQUE(board,id), UNIQUE(board,request_id))",
    "CREATE INDEX IF NOT EXISTS posts_board_channel ON posts(board,channel,seq)",
    "CREATE INDEX IF NOT EXISTS posts_board_channel_timestamp ON posts(board,channel,timestamp,seq)",
    "CREATE INDEX IF NOT EXISTS posts_roots_created ON posts(board,channel,timestamp,seq) WHERE id=root",
    "CREATE INDEX IF NOT EXISTS posts_board_root ON posts(board,root,seq)",
    "CREATE INDEX IF NOT EXISTS posts_board_root_timestamp ON posts(board,root,timestamp,seq)",
    "CREATE INDEX IF NOT EXISTS posts_board_timestamp ON posts(board,timestamp,seq)",
    "CREATE TABLE IF NOT EXISTS subscriptions (board TEXT NOT NULL, target TEXT NOT NULL, \
     agent TEXT NOT NULL, PRIMARY KEY(board,target,agent))",
    "CREATE TABLE IF NOT EXISTS subscription_opt_outs (board TEXT NOT NULL, target TEXT NOT NULL, \
     agent TEXT NOT NULL, PRIMARY KEY(board,target,agent))",
];

/// Read-write handles to every database of a staged home.
pub struct SqliteTarget {
    final_home: PathBuf,
    staging: SqliteConfig,
    pub(crate) state: SqlitePool,
    pub(crate) goals: SqlitePool,
    pub(crate) queue: SqlitePool,
    pub(crate) logs: SqlitePool,
    pub(crate) memories: SqlitePool,
    pub(crate) board: SqlitePool,
}

impl SqliteTarget {
    /// Create (or reopen) the staged databases under `staging`, for a home that will live at
    /// `final_home`.
    pub async fn create(
        staging: SqliteConfig,
        final_home: PathBuf,
        default_provider: &str,
    ) -> Result<Self> {
        // Initializing the runtime applies every migration; the pools below then reopen the files.
        let runtime = StateRuntime::init(staging.clone(), default_provider.to_string()).await?;
        runtime.close().await;
        drop(runtime);
        let state = staging
            .open_read_write_pool(&staging.state_db_path())
            .await?;
        let goals = staging
            .open_read_write_pool(&staging.goals_db_path())
            .await?;
        let queue = staging
            .open_read_write_pool(&staging.queue_db_path())
            .await?;
        let logs = staging
            .open_read_write_pool(&staging.logs_db_path())
            .await?;
        let memories = staging
            .open_read_write_pool(&staging.memories_db_path())
            .await?;
        let board = staging
            .open_read_write_pool(&staging.home().join("agent_message_board_1.sqlite"))
            .await?;
        for statement in BOARD_SCHEMA {
            sqlx::query(sqlx::AssertSqlSafe(*statement))
                .execute(&board)
                .await?;
        }
        Ok(Self {
            final_home,
            staging,
            state,
            goals,
            queue,
            logs,
            memories,
            board,
        })
    }

    /// Where the files will live once installed.
    pub fn final_home(&self) -> &Path {
        &self.final_home
    }

    /// The staging configuration, for reading the staged home back.
    pub fn staging(&self) -> &SqliteConfig {
        &self.staging
    }

    /// The path a thread's rollout file has in the final home, using the layout the recorder
    /// writes: dated session directories for active threads and one flat directory for archived
    /// ones.
    pub(crate) fn rollout_path(
        &self,
        thread_id: &str,
        created_at_ms: i64,
        archived: bool,
    ) -> PathBuf {
        let created = chrono::DateTime::from_timestamp_millis(created_at_ms)
            .unwrap_or(chrono::DateTime::UNIX_EPOCH);
        let name = format!(
            "rollout-{}-{thread_id}.jsonl",
            created.format("%Y-%m-%dT%H-%M-%S")
        );
        if archived {
            self.final_home.join("archived_sessions").join(name)
        } else {
            self.final_home
                .join("sessions")
                .join(created.format("%Y").to_string())
                .join(created.format("%m").to_string())
                .join(created.format("%d").to_string())
                .join(name)
        }
    }

    /// Where a path in the final home sits inside the staging directory.
    pub(crate) fn staged_path(&self, final_path: &Path) -> PathBuf {
        match final_path.strip_prefix(&self.final_home) {
            Ok(relative) => self.staging.home().join(relative),
            Err(_) => self
                .staging
                .home()
                .join(final_path.file_name().unwrap_or_default()),
        }
    }

    /// Close every pool so the files can be moved.
    pub async fn close(&self) {
        for pool in [
            &self.state,
            &self.goals,
            &self.queue,
            &self.logs,
            &self.memories,
            &self.board,
        ] {
            pool.close().await;
        }
    }
}
