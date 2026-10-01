//! Read-only access to a local SQLite home.

use anyhow::Result;
use codex_state::SqliteConfig;
use sqlx::SqlitePool;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

/// The SQLite databases of one Codex home, opened only to read.
#[derive(Clone)]
pub struct SqliteSource {
    config: SqliteConfig,
    /// The home these files will be installed into, when this source is a staged copy whose
    /// recorded paths already name that final location.
    relocated_from: Option<PathBuf>,
}

/// Which database file a read targets.
#[derive(Clone, Copy, Debug)]
pub(crate) enum SourceDatabase {
    State,
    Goals,
    Queue,
    Logs,
    Memories,
    Board,
}

impl SqliteSource {
    pub fn new(config: SqliteConfig) -> Self {
        Self {
            config,
            relocated_from: None,
        }
    }

    /// Read a staged home whose recorded paths point into `final_home`, which does not exist
    /// yet: those paths are looked up in the staged directory instead.
    pub fn relocated_from(mut self, final_home: PathBuf) -> Self {
        self.relocated_from = Some(final_home);
        self
    }

    /// The file a recorded path refers to on this source.
    pub(crate) fn resolve(&self, recorded: &str) -> PathBuf {
        let path = PathBuf::from(recorded);
        if let Some(final_home) = &self.relocated_from
            && let Ok(relative) = path.strip_prefix(final_home)
        {
            return self.home().join(relative);
        }
        if path.is_absolute() {
            path
        } else {
            self.home().join(path)
        }
    }

    pub fn home(&self) -> &Path {
        self.config.home()
    }

    /// Identifies which home a run was started for, so a run is only ever resumed against the
    /// same source. Content drift is not part of the identity; verification catches it by
    /// comparing every domain's digest.
    pub async fn fingerprint(&self) -> Result<String> {
        use sha2::Digest;
        let home = tokio::fs::canonicalize(self.config.home())
            .await
            .unwrap_or_else(|_| self.config.home().to_path_buf());
        Ok(sha2::Sha256::digest(home.to_string_lossy().as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    }

    /// Open one database for reading, or `None` when the home never created it.
    pub(crate) async fn pool(&self, database: SourceDatabase) -> Result<Option<SqlitePool>> {
        let path = match database {
            SourceDatabase::State => self.config.state_db_path(),
            SourceDatabase::Goals => self.config.goals_db_path(),
            SourceDatabase::Queue => self.config.queue_db_path(),
            SourceDatabase::Logs => self.config.logs_db_path(),
            SourceDatabase::Memories => self.config.memories_db_path(),
            SourceDatabase::Board => self.config.home().join("agent_message_board_1.sqlite"),
        };
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(
            self.config
                .open_read_only_pool(&path, Some(Duration::from_secs(30)))
                .await?,
        ))
    }
}
