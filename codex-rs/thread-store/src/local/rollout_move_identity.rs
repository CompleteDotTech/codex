use std::io;
use std::path::Path;

use serde::Deserialize;
use serde::Serialize;

/// Stable filesystem identity used to verify a move after its source was unlinked.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) enum RolloutFileIdentity {
    #[cfg(unix)]
    Unix { device: u64, inode: u64 },
    #[cfg(windows)]
    Windows { volume: u64, file_id: [u8; 16] },
}

pub(super) fn rollout_file_identity(path: &Path) -> io::Result<RolloutFileIdentity> {
    identity_before_open(path, || Ok(()))
}

fn identity_before_open(
    path: &Path,
    before_open: impl FnOnce() -> io::Result<()>,
) -> io::Result<RolloutFileIdentity> {
    rollout_file_identity_from_handle(&open_rollout_read_before_open(path, before_open)?)
}

pub(super) fn rollout_file_identity_from_handle(
    file: &std::fs::File,
) -> io::Result<RolloutFileIdentity> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let metadata = file.metadata()?;
        Ok(RolloutFileIdentity::Unix {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;

        use windows_sys::Win32::Storage::FileSystem::FILE_ID_INFO;
        use windows_sys::Win32::Storage::FileSystem::FileIdInfo;
        use windows_sys::Win32::Storage::FileSystem::GetFileInformationByHandleEx;

        let mut info: FILE_ID_INFO = unsafe { std::mem::zeroed() };
        let result = unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle() as _,
                FileIdInfo,
                (&mut info as *mut FILE_ID_INFO).cast(),
                std::mem::size_of::<FILE_ID_INFO>() as u32,
            )
        };
        if result == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(RolloutFileIdentity::Windows {
            volume: info.VolumeSerialNumber,
            file_id: info.FileId.Identifier,
        })
    }
}

pub(super) fn rollout_file_digest(path: &Path) -> io::Result<[u8; 32]> {
    digest_before_open(path, || Ok(()))
}

fn digest_before_open(
    path: &Path,
    before_open: impl FnOnce() -> io::Result<()>,
) -> io::Result<[u8; 32]> {
    digest_at_read_boundaries(path, before_open, || Ok(()), |_| Ok(()))
}

fn digest_at_read_boundaries(
    path: &Path,
    before_open: impl FnOnce() -> io::Result<()>,
    after_snapshot: impl FnOnce() -> io::Result<()>,
    after_read: impl FnOnce(u64) -> io::Result<()>,
) -> io::Result<[u8; 32]> {
    let mut file = open_rollout_read_before_open(path, before_open)?;
    let before = file.metadata()?;
    let identity = rollout_file_identity_from_handle(&file)?;
    let limit = before
        .len()
        .checked_add(1)
        .ok_or_else(|| io::Error::other("rollout length overflows read bound"))?;
    after_snapshot()?;
    let mut hasher = blake3::Hasher::new();
    let consumed = {
        let mut bounded = std::io::Read::take(&mut file, limit);
        hasher.update_reader(&mut bounded)?;
        limit - bounded.limit()
    };
    after_read(consumed)?;
    let after = file.metadata()?;
    if consumed != before.len()
        || before.len() != after.len()
        || before.modified()? != after.modified()?
        || rollout_file_identity_from_handle(&file)? != identity
        || rollout_file_identity(path)? != identity
    {
        return Err(io::Error::other("rollout changed while hashing"));
    }
    Ok(*hasher.finalize().as_bytes())
}

fn open_rollout_read_before_open(
    path: &Path,
    before_open: impl FnOnce() -> io::Result<()>,
) -> io::Result<std::fs::File> {
    if !std::fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(io::Error::other("rollout move path is not a regular file"));
    }
    before_open()?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    if !file.metadata()?.file_type().is_file()
        || !std::fs::symlink_metadata(path)?.file_type().is_file()
    {
        return Err(io::Error::other(
            "rollout source changed to a nonregular file",
        ));
    }
    let current = options.open(path)?;
    if !current.metadata()?.file_type().is_file()
        || rollout_file_identity_from_handle(&current)? != rollout_file_identity_from_handle(&file)?
    {
        return Err(io::Error::other("rollout source changed while opening"));
    }
    Ok(file)
}

#[cfg(test)]
#[path = "rollout_move_identity_tests.rs"]
mod tests;
