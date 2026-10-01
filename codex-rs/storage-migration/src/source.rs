//! Read-only access to a local SQLite home.

use anyhow::Result;
use codex_state::SqliteConfig;
use sqlx::SqlitePool;
use std::path::Path;
use std::time::Duration;

/// The SQLite databases of one Codex home, opened only to read.
#[derive(Clone)]
pub struct SqliteSource {
    config: SqliteConfig,
}

/// Which database file a read targets.
#[derive(Clone, Copy, Debug)]
pub(crate) enum SourceDatabase {
    State,
}

impl SqliteSource {
    pub fn new(config: SqliteConfig) -> Self {
        Self { config }
    }

    pub fn home(&self) -> &Path {
        self.config.home()
    }

    /// A digest of every database file and its write-ahead log, so a change made while a
    /// migration runs is noticed. Callers should quiesce Codex first; this catches mistakes.
    pub async fn fingerprint(&self) -> Result<String> {
        use sha2::Digest;
        let mut hasher = sha2::Sha256::new();
        let mut paths: Vec<_> = self
            .config
            .runtime_db_paths()
            .into_iter()
            .map(|database| database.path)
            .collect();
        paths.sort();
        for path in paths {
            for suffix in ["", "-wal"] {
                let mut file = path.clone().into_os_string();
                file.push(suffix);
                let file = std::path::PathBuf::from(file);
                let Ok(metadata) = tokio::fs::metadata(&file).await else {
                    continue;
                };
                let modified = metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |elapsed| elapsed.as_secs());
                hasher.update(
                    format!(
                        "{}:{}:{modified}
",
                        file.file_name()
                            .map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
                        metadata.len()
                    )
                    .as_bytes(),
                );
            }
        }
        Ok(hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    }

    /// Open one database for reading, or `None` when the home never created it.
    pub(crate) async fn pool(&self, database: SourceDatabase) -> Result<Option<SqlitePool>> {
        let path = match database {
            SourceDatabase::State => self.config.state_db_path(),
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
