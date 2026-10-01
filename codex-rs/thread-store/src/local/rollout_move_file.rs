//! Inactive no-clobber rollout publication primitive. Activated by the lifecycle stage.

use std::fs::FileTimes;
use std::fs::OpenOptions;
use std::io;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;

use serde::Deserialize;
use serde::Serialize;

use super::rollout_move_identity::RolloutFileIdentity;
use super::rollout_move_identity::rollout_file_digest;
use super::rollout_move_identity::rollout_file_identity;

const MAX_ROLLOUT_MOVE_INTENT_BYTES: usize = 4096;
pub(super) fn touch_modified_time(path: &Path) -> std::io::Result<()> {
    let times = FileTimes::new().set_modified(SystemTime::now());
    OpenOptions::new().append(true).open(path)?.set_times(times)
}

#[cfg(test)]
pub(super) fn move_rollout_noclobber(
    source: &Path,
    destination: &Path,
    codex_home: &Path,
) -> std::io::Result<()> {
    move_rollout_noclobber_retained(source, destination, codex_home)?;
    clear_rollout_move_intent(destination)
}

pub(super) fn move_rollout_noclobber_retained(
    source: &Path,
    destination: &Path,
    codex_home: &Path,
) -> std::io::Result<()> {
    move_rollout_noclobber_retained_with_intent_sync(
        source,
        destination,
        codex_home,
        sync_parent_directory,
    )
}

fn move_rollout_noclobber_retained_with_intent_sync(
    source: &Path,
    destination: &Path,
    codex_home: &Path,
    sync_intent_parent: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<()> {
    let canonical_sessions =
        std::fs::canonicalize(codex_home.join(codex_rollout::SESSIONS_SUBDIR))?;
    let canonical_archived =
        std::fs::canonicalize(codex_home.join(codex_rollout::ARCHIVED_SESSIONS_SUBDIR))?;
    let canonical_source = std::fs::canonicalize(source)?;
    let canonical_destination_parent = std::fs::canonicalize(
        destination
            .parent()
            .ok_or_else(|| std::io::Error::other("rollout destination has no parent"))?,
    )?;
    let destination_name = destination
        .file_name()
        .ok_or_else(|| std::io::Error::other("rollout destination has no filename"))?;
    let within_collections = (canonical_source.starts_with(&canonical_sessions)
        && canonical_destination_parent.starts_with(&canonical_archived))
        || (canonical_source.starts_with(&canonical_archived)
            && canonical_destination_parent.starts_with(&canonical_sessions));
    if !within_collections || !std::fs::symlink_metadata(source)?.file_type().is_file() {
        return Err(std::io::Error::other(
            "rollout move is outside its collection or is not a file",
        ));
    }

    let canonical_destination = canonical_destination_parent.join(destination_name);
    let intent_path = rollout_move_intent_path(&canonical_destination);
    let source_metadata = std::fs::metadata(source)?;
    let source_id = rollout_file_identity(source)?;
    let intent = match std::fs::symlink_metadata(&intent_path) {
        Ok(metadata) => {
            if !metadata.file_type().is_file() {
                return Err(io::Error::other("rollout move has a conflicting intent"));
            }
            let intent = read_rollout_move_intent(&intent_path)?;
            if intent.source != canonical_source
                || intent.destination != canonical_destination
                || intent.source_len != source_metadata.len()
                || intent.source_modified != source_metadata.modified()?
                || intent.source_id != source_id
            {
                return Err(io::Error::other("rollout move has a conflicting intent"));
            }
            intent
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            match std::fs::symlink_metadata(&canonical_destination) {
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Ok(_) => return Err(io::Error::from(io::ErrorKind::AlreadyExists)),
                Err(err) => return Err(err),
            }
            // Give the published file an identity distinct from the source before the
            // no-clobber publication. The durable intent can then distinguish our file
            // from even an unrelated hard link to the source after a crash.
            let mut stage = tempfile::Builder::new()
                .prefix(".codex-rollout-stage-")
                .tempfile_in(&canonical_destination_parent)?;
            let mut input = std::fs::File::open(source)?;
            io::copy(&mut input, &mut stage)?;
            stage
                .as_file()
                .set_times(FileTimes::new().set_modified(source_metadata.modified()?))?;
            stage.as_file().sync_all()?;
            let source_after_copy = std::fs::metadata(source)?;
            if source_after_copy.len() != source_metadata.len()
                || source_after_copy.modified()? != source_metadata.modified()?
                || rollout_file_identity(source)? != source_id
            {
                return Err(io::Error::other(
                    "rollout source changed while preparing move",
                ));
            }
            let stage_path = stage.path().to_path_buf();
            let stage_id = rollout_file_identity(&stage_path)?;
            let stage_digest = rollout_file_digest(&stage_path)?;
            let (stage_file, stage_path) = stage.keep().map_err(|err| err.error)?;
            drop(stage_file);
            sync_parent_directory(&stage_path)?;
            let intent = RolloutMoveIntent {
                source: canonical_source,
                destination: canonical_destination.clone(),
                source_len: source_metadata.len(),
                source_modified: source_metadata.modified()?,
                source_id,
                stage_path,
                stage_id,
                stage_digest,
            };
            // A failed directory sync can follow successful intent publication.
            // Preserve its stage even on error so a recorded move stays retryable.
            write_rollout_move_intent(&intent_path, &intent, sync_intent_parent)?;
            intent
        }
        Err(err) => return Err(err),
    };
    if intent.stage_path.parent() != Some(canonical_destination_parent.as_path())
        || !intent
            .stage_path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with(".codex-rollout-stage-"))
    {
        return Err(io::Error::other("rollout move has an invalid stage path"));
    }
    match std::fs::symlink_metadata(&canonical_destination) {
        Ok(_) => {
            if rollout_file_identity(&canonical_destination)? != intent.stage_id
                || rollout_file_digest(&canonical_destination)? != intent.stage_digest
            {
                return Err(io::Error::from(io::ErrorKind::AlreadyExists));
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            if rollout_file_identity(&intent.stage_path)? != intent.stage_id
                || rollout_file_digest(&intent.stage_path)? != intent.stage_digest
            {
                return Err(io::Error::other("rollout move stage identity changed"));
            }
            let stage = tempfile::TempPath::try_from_path(&intent.stage_path)?;
            if let Err(err) = stage.persist_noclobber(&canonical_destination) {
                let kind = err.error.kind();
                let _ = err.path.keep();
                return Err(io::Error::from(kind));
            }
            if rollout_file_identity(&canonical_destination)? != intent.stage_id
                || rollout_file_digest(&canonical_destination)? != intent.stage_digest
            {
                return Err(io::Error::other(
                    "rollout move destination identity changed",
                ));
            }
        }
        Err(err) => return Err(err),
    }
    sync_parent_directory(&canonical_destination)?;
    std::fs::remove_file(source)?;
    sync_parent_directory(source)?;
    Ok(())
}

fn sync_parent_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    std::fs::File::open(
        path.parent()
            .ok_or_else(|| io::Error::other("missing parent"))?,
    )?
    .sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
struct RolloutMoveIntent {
    source: PathBuf,
    destination: PathBuf,
    source_len: u64,
    source_modified: SystemTime,
    source_id: RolloutFileIdentity,
    stage_path: PathBuf,
    stage_id: RolloutFileIdentity,
    stage_digest: [u8; 32],
}

fn rollout_move_intent_path(destination: &Path) -> PathBuf {
    let mut name = destination.as_os_str().to_owned();
    name.push(".codex-move-intent");
    PathBuf::from(name)
}

fn write_rollout_move_intent(
    path: &Path,
    intent: &RolloutMoveIntent,
    sync_parent: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<()> {
    let mut contents = serde_json::to_vec(intent).map_err(io::Error::other)?;
    contents.push(b'\n');
    if contents.len() > MAX_ROLLOUT_MOVE_INTENT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "rollout move intent is too large",
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("missing intent parent"))?;
    let mut file = tempfile::Builder::new()
        .prefix(".codex-move-intent-")
        .tempfile_in(parent)?;
    file.write_all(&contents)?;
    file.as_file().sync_all()?;
    file.persist_noclobber(path).map_err(|err| err.error)?;
    sync_parent(path)
}

fn read_rollout_move_intent(path: &Path) -> io::Result<RolloutMoveIntent> {
    let mut file = std::fs::File::open(path)?;
    if file.metadata()?.len() > MAX_ROLLOUT_MOVE_INTENT_BYTES as u64 {
        return Err(io::Error::other("rollout move intent is too large"));
    }
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    if !contents.ends_with('\n') {
        return Err(io::Error::other("rollout move intent is incomplete"));
    }
    let mut lines = contents.lines();
    let intent = serde_json::from_str(
        lines
            .next()
            .ok_or_else(|| io::Error::other("empty intent"))?,
    )
    .map_err(io::Error::other)?;
    if lines.next().is_some() {
        return Err(io::Error::other("rollout move intent has extra records"));
    }
    Ok(intent)
}

pub(super) fn clear_rollout_move_intent(destination: &Path) -> io::Result<()> {
    let parent = std::fs::canonicalize(
        destination
            .parent()
            .ok_or_else(|| io::Error::other("rollout destination has no parent"))?,
    )?;
    let name = destination
        .file_name()
        .ok_or_else(|| io::Error::other("rollout destination has no filename"))?;
    let intent_path = rollout_move_intent_path(&parent.join(name));
    let intent = read_rollout_move_intent(&intent_path)?;
    if intent.stage_path.parent() != Some(parent.as_path())
        || !intent
            .stage_path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with(".codex-rollout-stage-"))
    {
        return Err(io::Error::other("rollout move has an invalid stage path"));
    }
    match std::fs::symlink_metadata(&intent.stage_path) {
        Ok(_) => {
            if rollout_file_identity(&intent.stage_path)? != intent.stage_id
                || rollout_file_digest(&intent.stage_path)? != intent.stage_digest
            {
                return Err(io::Error::other("rollout move stage identity changed"));
            }
            std::fs::remove_file(&intent.stage_path)?;
            sync_parent_directory(&intent.stage_path)?;
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    std::fs::remove_file(&intent_path)?;
    sync_parent_directory(&intent_path)
}

/// Verify a completed move whose source was unlinked before its caller committed metadata.
pub(super) fn verify_published_rollout_move(
    source: &Path,
    destination: &Path,
    codex_home: &Path,
) -> io::Result<()> {
    let canonical_sessions =
        std::fs::canonicalize(codex_home.join(codex_rollout::SESSIONS_SUBDIR))?;
    let canonical_archived =
        std::fs::canonicalize(codex_home.join(codex_rollout::ARCHIVED_SESSIONS_SUBDIR))?;
    let source_parent = std::fs::canonicalize(
        source
            .parent()
            .ok_or_else(|| io::Error::other("rollout source has no parent"))?,
    )?;
    let destination_parent = std::fs::canonicalize(
        destination
            .parent()
            .ok_or_else(|| io::Error::other("rollout destination has no parent"))?,
    )?;
    let source_name = source
        .file_name()
        .ok_or_else(|| io::Error::other("rollout source has no filename"))?;
    let destination_name = destination
        .file_name()
        .ok_or_else(|| io::Error::other("rollout destination has no filename"))?;
    let within_collections = (source_parent.starts_with(&canonical_sessions)
        && destination_parent.starts_with(&canonical_archived))
        || (source_parent.starts_with(&canonical_archived)
            && destination_parent.starts_with(&canonical_sessions));
    if !within_collections || source_name != destination_name {
        return Err(io::Error::other("rollout move is outside its collection"));
    }
    match std::fs::symlink_metadata(source) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Ok(_) => return Err(io::Error::other("rollout move source still exists")),
        Err(err) => return Err(err),
    }
    let canonical_destination = destination_parent.join(destination_name);
    let intent = read_rollout_move_intent(&rollout_move_intent_path(&canonical_destination))?;
    if intent.source != source_parent.join(source_name)
        || intent.destination != canonical_destination
        || intent.stage_id != rollout_file_identity(&canonical_destination)?
        || intent.stage_digest != rollout_file_digest(&canonical_destination)?
    {
        return Err(io::Error::other(
            "rollout move published identity does not match",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "rollout_move_file_tests.rs"]
mod tests;
