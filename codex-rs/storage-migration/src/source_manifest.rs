//! A record of the local files a migration leaves behind.
//!
//! Activating a migration makes PostgreSQL authoritative but never touches the local history. The
//! manifest written just before that point names every database and every history directory
//! with its digest or totals, so an operator can later prove what was there, and a restore can be
//! checked against it. Nothing is copied or moved.

use crate::install::directory_totals;
use crate::install::sha256_file;
use serde::Deserialize;
use serde::Serialize;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use uuid::Uuid;

const BACKUPS_DIR: &str = "storage-backups";
const MANIFEST_FILE: &str = "manifest.json";
const DIRECTORIES: [&str; 2] = ["sessions", "archived_sessions"];

/// One local file as it was when the migration was activated.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct SourceFile {
    /// Path relative to the home.
    pub name: String,
    pub bytes: u64,
    pub sha256: String,
}

/// One history directory as it was when the migration was activated.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct SourceDirectory {
    pub name: String,
    pub files: u64,
    pub bytes: u64,
}

/// Everything local that the migration read from and left in place.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct SourceManifest {
    pub run_id: Uuid,
    pub files: Vec<SourceFile>,
    pub directories: Vec<SourceDirectory>,
}

/// Where the manifest for a run is kept.
pub fn source_manifest_path(home: &Path, run_id: Uuid) -> PathBuf {
    home.join(BACKUPS_DIR)
        .join(format!("{run_id}-before-migrate"))
        .join(MANIFEST_FILE)
}

/// Record the local files for `run_id`. Repeating it keeps the first record, so a recovered
/// activation never rewrites what was written before the cutover began.
pub fn write_source_manifest(home: &Path, run_id: Uuid) -> io::Result<SourceManifest> {
    let path = source_manifest_path(home, run_id);
    if let Ok(existing) = std::fs::read(&path) {
        return serde_json::from_slice(&existing)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "source manifest"));
    }
    let mut names = std::fs::read_dir(home)?
        .collect::<io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| {
            name.ends_with(".sqlite")
                || name.ends_with(".sqlite-wal")
                || name.ends_with(".sqlite-shm")
        })
        .collect::<Vec<_>>();
    names.sort();
    let mut files = Vec::new();
    for name in names {
        let live = home.join(&name);
        files.push(SourceFile {
            bytes: std::fs::metadata(&live)?.len(),
            sha256: sha256_file(&live)?,
            name,
        });
    }
    let mut directories = Vec::new();
    for name in DIRECTORIES {
        let live = home.join(name);
        if live.is_dir() {
            let (entries, bytes) = directory_totals(&live)?;
            directories.push(SourceDirectory {
                name: name.to_string(),
                files: entries,
                bytes,
            });
        }
    }
    let manifest = SourceManifest {
        run_id,
        files,
        directories,
    };
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("manifest has no directory"))?;
    std::fs::create_dir_all(parent)?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(
        &temporary,
        serde_json::to_vec_pretty(&manifest).map_err(io::Error::other)?,
    )?;
    std::fs::rename(temporary, path)?;
    Ok(manifest)
}

#[cfg(test)]
#[path = "source_manifest_tests.rs"]
mod tests;
