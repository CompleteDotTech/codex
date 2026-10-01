//! Cleanup authority for aliases left by no-clobber receipt publication.
//! Pre-publication files without a canonical receipt remain unowned and retained.

use std::fs::File;
use std::fs::OpenOptions;
use std::io;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;

use super::RolloutMoveIntent;
use super::rollout_file_identity;
use super::rollout_file_identity_from_handle;
use super::sync_parent_directory;
use crate::local::rollout_move_identity::RolloutFileIdentity;

const MAX_BYTES: u64 = 4096;

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct ReceiptPublication {
    #[serde(with = "crate::local::rollout_move_path_json")]
    path: PathBuf,
    identity: RolloutFileIdentity,
}

fn staging_path(stage: &Path) -> PathBuf {
    let mut name = stage.as_os_str().to_owned();
    name.push(".move-intent");
    PathBuf::from(name)
}

#[cfg(test)]
fn write(path: &Path, intent: &RolloutMoveIntent) -> io::Result<()> {
    write_with_sync(path, intent, sync_parent_directory)
}

pub(super) fn write_with_sync(
    path: &Path,
    intent: &RolloutMoveIntent,
    sync_parent: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<()> {
    write_with_publication_and_sync(
        path,
        intent,
        |temporary, canonical| {
            let temporary = tempfile::TempPath::try_from_path(temporary)?;
            if let Err(error) = temporary.persist_noclobber(canonical) {
                let kind = error.error.kind();
                let _ = error.path.keep();
                return Err(io::Error::from(kind));
            }
            Ok(())
        },
        sync_parent,
    )
}

#[cfg(test)]
fn write_with_publication(
    path: &Path,
    intent: &RolloutMoveIntent,
    publish: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    write_with_publication_and_sync(path, intent, publish, sync_parent_directory)
}

fn write_with_publication_and_sync(
    path: &Path,
    intent: &RolloutMoveIntent,
    publish: impl FnOnce(&Path, &Path) -> io::Result<()>,
    sync_parent: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<()> {
    let temporary = staging_path(&intent.stage_path);
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    let mut recorded = intent.clone();
    recorded.receipt_publication = Some(ReceiptPublication {
        path: temporary.clone(),
        identity: rollout_file_identity_from_handle(&file)?,
    });
    let mut bytes = serde_json::to_vec(&recorded).map_err(io::Error::other)?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_BYTES {
        // No receipt bytes were written. Keep the empty resource pending journal ownership.
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "rollout move intent is too large",
        ));
    }
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    publish(&temporary, path)?;
    sync_parent(path)
}

fn read_owned(path: &Path, identity: RolloutFileIdentity) -> io::Result<Vec<u8>> {
    if !std::fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(io::Error::other("move receipt is not a regular file"));
    }
    let mut file = File::open(path)?;
    let before = file.metadata()?;
    if rollout_file_identity_from_handle(&file)? != identity
        || rollout_file_identity(path)? != identity
        || before.len() > MAX_BYTES
    {
        return Err(io::Error::other("move receipt identity or size changed"));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    if bytes.len() as u64 > MAX_BYTES
        || bytes.len() as u64 != before.len()
        || before.len() != after.len()
        || before.modified()? != after.modified()?
        || rollout_file_identity(path)? != identity
    {
        return Err(io::Error::other("move receipt changed while reading"));
    }
    Ok(bytes)
}

pub(super) fn validate(path: &Path, intent: &RolloutMoveIntent) -> io::Result<()> {
    let Some(publication) = &intent.receipt_publication else {
        return Ok(());
    };
    if publication.path != staging_path(&intent.stage_path)
        || intent.stage_path.parent() != path.parent()
        || !intent
            .stage_path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with(".codex-rollout-stage-"))
        || super::rollout_move_intent_path(&intent.destination) != path
    {
        return Err(io::Error::other("move receipt publication path is invalid"));
    }
    let canonical = read_owned(path, publication.identity)?;
    let current: RolloutMoveIntent =
        serde_json::from_slice(&canonical).map_err(io::Error::other)?;
    if &current != intent {
        return Err(io::Error::other(
            "move receipt changed after it was decoded",
        ));
    }
    match std::fs::symlink_metadata(&publication.path) {
        Ok(_) => {
            if read_owned(&publication.path, publication.identity)? != canonical {
                return Err(io::Error::other(
                    "move receipt publication contents changed",
                ));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    Ok(())
}

pub(super) fn cleanup(path: &Path, intent: &RolloutMoveIntent) -> io::Result<()> {
    cleanup_with_sync(path, intent, sync_parent_directory)
}

fn cleanup_with_sync(
    path: &Path,
    intent: &RolloutMoveIntent,
    sync: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<()> {
    validate(path, intent)?;
    let Some(publication) = &intent.receipt_publication else {
        return Ok(());
    };
    match std::fs::remove_file(&publication.path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    // A retry must also sync an already absent alias before retiring canonical authority.
    sync(&publication.path)
}

#[cfg(test)]
#[path = "rollout_move_receipt_publication_tests.rs"]
mod tests;
