//! The local half of a backend cutover.
//!
//! A cutover moves authority from one backend to the other in three durable steps: the host
//! records its intent here, the destination publishes the new generation, and the host then
//! rewrites its own authority records. Every step is idempotent and the intent file survives
//! until the last one finishes, so a restart can always tell which side got ahead and finish or
//! undo the work instead of leaving two writable histories.

use crate::ACTIVATION_FILE;
use crate::ActivationMarker;
use crate::ActiveBackend;
use crate::AuthorityError;
use crate::FORMAT_VERSION;
use crate::IDENTITY_FILE;
use crate::LocalAuthority;
use crate::LocalIdentity;
use crate::read_record;
use crate::sync_dir;
use crate::write_new;
use serde::Deserialize;
use serde::Serialize;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use uuid::Uuid;

pub(crate) const CUTOVER_FILE: &str = "storage-cutover.json";

/// A promise to move authority to `target` at `to_generation`, written before anything else
/// changes.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CutoverIntent {
    pub format_version: u32,
    pub run_id: Uuid,
    pub dataset_id: Uuid,
    pub from_generation: u64,
    pub to_generation: u64,
    pub target: ActiveBackend,
}

/// Read both authority records for any backend. Unlike [`crate::load_local_authority`], this
/// accepts a home whose authority is remote, so it is for callers that understand cutovers.
pub fn load_authority(home: &Path) -> Result<LocalAuthority, AuthorityError> {
    if read_cutover(home)?.is_some() {
        return Err(AuthorityError::Blocked("cutover in progress"));
    }
    let (identity, marker) = read_pair(home)?;
    check_pair(&identity, &marker)?;
    Ok(LocalAuthority { identity, marker })
}

fn read_pair(home: &Path) -> Result<(LocalIdentity, ActivationMarker), AuthorityError> {
    if std::fs::symlink_metadata(home)?.file_type().is_symlink() {
        return Err(AuthorityError::Blocked("home is a symlink"));
    }
    let identity: LocalIdentity = read_record(&home.join(IDENTITY_FILE))?;
    let marker: ActivationMarker = read_record(&home.join(ACTIVATION_FILE))?;
    if identity.format_version != FORMAT_VERSION || marker.format_version != FORMAT_VERSION {
        return Err(AuthorityError::Blocked(
            "unsupported authority record version",
        ));
    }
    Ok((identity, marker))
}

fn check_pair(identity: &LocalIdentity, marker: &ActivationMarker) -> Result<(), AuthorityError> {
    if identity.dataset_id != marker.dataset_id
        || identity.instance_id != marker.instance_id
        || identity.home_id != marker.home_id
        || identity.generation != marker.generation
    {
        return Err(AuthorityError::Blocked("authority records disagree"));
    }
    if !(1..=i64::MAX as u64).contains(&identity.generation) {
        return Err(AuthorityError::Blocked("invalid authority generation"));
    }
    Ok(())
}

/// What a home's authority records say, for deciding which backend a process may start.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthorityState {
    /// No authority records exist: a home that predates storage authority, which stays local.
    Unmanaged,
    /// Local files are authoritative.
    Local(LocalAuthority),
    /// PostgreSQL is authoritative.
    Remote(LocalAuthority),
    /// A cutover was interrupted; recovery must settle it before any backend starts.
    CutoverInProgress(CutoverIntent),
}

/// Classify a home without changing it. Partial or inconsistent records are an error, because
/// guessing a backend from them could expose a second writable history.
pub fn authority_state(home: &Path) -> Result<AuthorityState, AuthorityError> {
    if let Some(intent) = read_cutover(home)? {
        return Ok(AuthorityState::CutoverInProgress(intent));
    }
    let identity = home.join(IDENTITY_FILE).exists();
    let marker = home.join(ACTIVATION_FILE).exists();
    if !identity && !marker {
        return Ok(AuthorityState::Unmanaged);
    }
    let authority = load_authority(home)?;
    Ok(match authority.marker.active_backend {
        ActiveBackend::Local => AuthorityState::Local(authority),
        ActiveBackend::Remote => AuthorityState::Remote(authority),
    })
}

/// The intent a previous run left behind, if any.
pub fn read_cutover(home: &Path) -> Result<Option<CutoverIntent>, AuthorityError> {
    let path = home.join(CUTOVER_FILE);
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let intent: CutoverIntent = read_record(&path)?;
    if intent.format_version != FORMAT_VERSION {
        return Err(AuthorityError::Blocked(
            "unsupported cutover record version",
        ));
    }
    Ok(Some(intent))
}

/// Record the intent to make `target` authoritative for the next generation.
///
/// Fails if another cutover is already recorded or the authority records disagree. The caller
/// must hold the home exclusively; nothing here fences other processes.
pub fn begin_cutover(
    home: &Path,
    run_id: Uuid,
    target: ActiveBackend,
) -> Result<CutoverIntent, AuthorityError> {
    if read_cutover(home)?.is_some() {
        return Err(AuthorityError::Blocked("cutover already in progress"));
    }
    let (identity, marker) = read_pair(home)?;
    check_pair(&identity, &marker)?;
    if marker.active_backend == target {
        return Err(AuthorityError::Blocked("backend is already authoritative"));
    }
    let intent = CutoverIntent {
        format_version: FORMAT_VERSION,
        run_id,
        dataset_id: identity.dataset_id,
        from_generation: identity.generation,
        to_generation: identity
            .generation
            .checked_add(1)
            .ok_or(AuthorityError::Blocked("invalid authority generation"))?,
        target,
    };
    write_new(&home.join(CUTOVER_FILE), &intent)?;
    sync_dir(home)?;
    Ok(intent)
}

/// Rewrite the authority records for the intent's target, then forget the intent.
///
/// Safe to repeat after a crash at any point: each record is moved only if it is still at the
/// old generation, and an intent that is already satisfied is simply removed.
pub fn complete_cutover(
    home: &Path,
    intent: &CutoverIntent,
) -> Result<LocalAuthority, AuthorityError> {
    let (identity, marker) = read_pair(home)?;
    if identity.dataset_id != intent.dataset_id || marker.dataset_id != intent.dataset_id {
        return Err(AuthorityError::Blocked(
            "cutover belongs to another dataset",
        ));
    }
    let moved_identity = LocalIdentity {
        generation: intent.to_generation,
        ..identity
    };
    let moved_marker = ActivationMarker {
        generation: intent.to_generation,
        active_backend: intent.target,
        remote_ever_activated: marker.remote_ever_activated
            || intent.target == ActiveBackend::Remote,
        ..marker
    };
    match identity.generation {
        generation if generation == intent.to_generation => {}
        generation if generation == intent.from_generation => {
            replace_record(&home.join(IDENTITY_FILE), &moved_identity)?;
        }
        _ => {
            return Err(AuthorityError::Blocked(
                "authority generation is unexpected",
            ));
        }
    }
    match marker.generation {
        generation if generation == intent.to_generation => {}
        generation if generation == intent.from_generation => {
            replace_record(&home.join(ACTIVATION_FILE), &moved_marker)?;
        }
        _ => {
            return Err(AuthorityError::Blocked(
                "authority generation is unexpected",
            ));
        }
    }
    match std::fs::remove_file(home.join(CUTOVER_FILE)) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    sync_dir(home)?;
    let authority = load_authority(home)?;
    Ok(authority)
}

/// Forget an intent whose destination never published the new generation.
///
/// Refuses if the local records already moved, because that cutover can only roll forward.
pub fn abandon_cutover(home: &Path, intent: &CutoverIntent) -> Result<(), AuthorityError> {
    let (identity, marker) = read_pair(home)?;
    if identity.generation != intent.from_generation || marker.generation != intent.from_generation
    {
        return Err(AuthorityError::Blocked("cutover already moved the records"));
    }
    match std::fs::remove_file(home.join(CUTOVER_FILE)) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    sync_dir(home)?;
    Ok(())
}

/// Replace a record atomically: a reader sees the old file or the new one, never a mixture.
fn replace_record<T: Serialize>(path: &Path, value: &T) -> Result<(), AuthorityError> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = std::path::PathBuf::from(temporary);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // A leftover from a crashed attempt is ours and safe to discard; anything else is not.
    match options.open(&temporary) {
        Ok(mut file) => write_and_rename(&mut file, &temporary, path, value),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = std::fs::symlink_metadata(&temporary)?;
            if !metadata.is_file() {
                return Err(AuthorityError::Blocked(
                    "authority temporary is not regular",
                ));
            }
            std::fs::remove_file(&temporary)?;
            let mut file = options.open(&temporary)?;
            write_and_rename(&mut file, &temporary, path, value)
        }
        Err(error) => Err(error.into()),
    }
}

fn write_and_rename<T: Serialize>(
    file: &mut std::fs::File,
    temporary: &Path,
    path: &Path,
    value: &T,
) -> Result<(), AuthorityError> {
    serde_json::to_writer(&mut *file, value)
        .map_err(|_| AuthorityError::Blocked("authority record encoding failed"))?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    std::fs::rename(temporary, path)?;
    Ok(())
}

#[cfg(test)]
#[path = "cutover_tests.rs"]
mod tests;
