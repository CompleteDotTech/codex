//! Stable, bounded reads of bare move receipts and their recorded aliases.
use super::RolloutFileIdentity;
use super::rollout_file_identity_from_handle;
use std::io;
use std::io::Read;
use std::path::Path;

enum ExpectedIdentity {
    Any,
    Owned(RolloutFileIdentity),
}

pub(super) fn read(path: &Path) -> io::Result<Vec<u8>> {
    read_at_boundaries(path, ExpectedIdentity::Any, || Ok(()), || Ok(()))
}

pub(super) fn read_owned(path: &Path, identity: RolloutFileIdentity) -> io::Result<Vec<u8>> {
    read_at_boundaries(
        path,
        ExpectedIdentity::Owned(identity),
        || Ok(()),
        || Ok(()),
    )
}

fn read_at_boundaries(
    path: &Path,
    expected_identity: ExpectedIdentity,
    before_open: impl FnOnce() -> io::Result<()>,
    after_snapshot: impl FnOnce() -> io::Result<()>,
) -> io::Result<Vec<u8>> {
    if !std::fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(io::Error::other("move receipt is not a regular file"));
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
    let mut file = options.open(path)?;
    let before = file.metadata()?;
    let identity = rollout_file_identity_from_handle(&file)?;
    if !before.is_file()
        || matches!(expected_identity, ExpectedIdentity::Owned(expected) if expected != identity)
    {
        return Err(io::Error::other("move receipt identity changed"));
    }
    if before.len() > 4096 {
        return Err(io::Error::other("rollout move intent is too large"));
    }
    after_snapshot()?;
    let mut bytes = Vec::with_capacity(/*capacity*/ 4097);
    Read::by_ref(&mut file).take(4097).read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    // Open the current path with the same non-following flags; do not use an
    // unguarded path helper whose check/open gap could block on a FIFO.
    let current = options.open(path)?;
    if !current.metadata()?.is_file()
        || rollout_file_identity_from_handle(&current)? != identity
        || !std::fs::symlink_metadata(path)?.file_type().is_file()
        || bytes.len() > 4096
        || bytes.len() as u64 != before.len()
        || before.len() != after.len()
        || before.modified()? != after.modified()?
    {
        return Err(io::Error::other("move receipt changed while reading"));
    }
    Ok(bytes)
}

#[cfg(test)]
#[path = "rollout_move_receipt_io_tests.rs"]
mod tests;
