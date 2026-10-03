//! Unix descriptor-relative journal namespace. Caller must fence cooperative writers.
use std::ffi::CString;
use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Component;
use std::path::Path;

pub(super) struct Namespace {
    chain: Vec<File>,
    names: Vec<CString>,
}
fn name(value: &OsStr) -> io::Result<CString> {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes == b"." || bytes == b".." || bytes.contains(&b'/') {
        return Err(io::Error::other("journal component is not relative"));
    }
    CString::new(bytes).map_err(|_| io::Error::other("journal component contains NUL"))
}
fn from_fd(fd: libc::c_int) -> io::Result<File> {
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful native open returned a new, uniquely owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn directory(parent: &File, component: &CString) -> io::Result<File> {
    // SAFETY: live retained parent descriptor and bounded NUL-terminated component.
    from_fd(unsafe {
        libc::openat(
            parent.as_raw_fd(),
            component.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    })
}
fn same(left: &File, right: &File) -> io::Result<bool> {
    let a = left.metadata()?;
    let b = right.metadata()?;
    Ok(a.dev() == b.dev() && a.ino() == b.ino())
}
fn current_uid() -> libc::uid_t {
    // SAFETY: geteuid has no arguments and returns the current effective principal.
    unsafe { libc::geteuid() }
}
fn owned_directory(file: &File, sticky_ancestor: bool) -> io::Result<()> {
    let metadata = file.metadata()?;
    // SAFETY: geteuid has no arguments and returns the current effective principal.
    let uid = current_uid();
    if !metadata.is_dir()
        || (metadata.uid() != uid && metadata.uid() != 0)
        || (!sticky_ancestor && metadata.uid() != uid)
    {
        return Err(io::Error::other("journal ancestor owner/type refused"));
    }
    if metadata.mode() & 0o022 != 0 && !(sticky_ancestor && metadata.mode() & 0o1000 != 0) {
        return Err(io::Error::other(
            "journal ancestor permits foreign mutation",
        ));
    }
    Ok(())
}
impl Namespace {
    pub(super) fn acquire(path: &Path, create: bool) -> io::Result<Self> {
        if path.as_os_str().as_bytes().len() > 65536 {
            return Err(io::Error::other("journal namespace byte limit"));
        }
        let absolute = std::path::absolute(path)?;
        if absolute.as_os_str().as_bytes().len() > 65536 {
            return Err(io::Error::other("journal namespace byte limit"));
        }
        let names = absolute
            .components()
            .filter_map(|part| match part {
                Component::RootDir => None,
                Component::Normal(value) => Some(name(value)),
                _ => Some(Err(io::Error::other("journal path traversal refused"))),
            })
            .collect::<io::Result<Vec<_>>>()?;
        if names.is_empty() || names.len() > 128 || absolute.as_os_str().as_bytes().len() > 65536 {
            return Err(io::Error::other("journal namespace depth/byte limit"));
        }
        let root = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open("/")?;
        owned_directory(&root, true)?;
        let mut chain = vec![root];
        for (index, component) in names.iter().enumerate() {
            let parent = chain
                .last()
                .ok_or_else(|| io::Error::other("journal parent missing"))?;
            let child = match directory(parent, component) {
                Ok(child) => child,
                Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                    // SAFETY: retained directory and valid relative component; restrictive new mode.
                    if unsafe { libc::mkdirat(parent.as_raw_fd(), component.as_ptr(), 0o700) } != 0
                    {
                        let error = io::Error::last_os_error();
                        if error.kind() != io::ErrorKind::AlreadyExists {
                            return Err(error);
                        }
                    }
                    directory(parent, component)?
                }
                Err(error) => return Err(error),
            };
            owned_directory(&child, index + 1 < names.len())?;
            if parent.metadata()?.mode() & 0o022 != 0 && child.metadata()?.uid() != current_uid() {
                return Err(io::Error::other("sticky ancestor child owner refused"));
            }
            chain.push(child);
        }
        let lease = Self { chain, names };
        lease.revalidate()?;
        Ok(lease)
    }
    pub(super) fn revalidate(&self) -> io::Result<()> {
        for (index, component) in self.names.iter().enumerate() {
            let actual = directory(&self.chain[index], component)?;
            if !same(&actual, &self.chain[index + 1])? {
                return Err(io::Error::other("journal retained ancestor replaced"));
            }
            owned_directory(&actual, index + 1 < self.names.len())?;
        }
        Ok(())
    }
    pub(super) fn leaf(&self) -> io::Result<&File> {
        self.chain
            .last()
            .ok_or_else(|| io::Error::other("journal leaf missing"))
    }
    pub(super) fn ancestors(&self) -> impl Iterator<Item = &File> {
        self.chain.iter().rev().skip(1)
    }
    pub(super) fn open_regular(&self, basename: &OsStr, flags: libc::c_int) -> io::Result<File> {
        self.revalidate()?;
        let component = name(basename)?;
        // SAFETY: retained leaf and exact relative basename; successful fd captured immediately.
        let file = from_fd(unsafe {
            libc::openat(
                self.leaf()?.as_raw_fd(),
                component.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
                0o600,
            )
        })?;
        Self::validate_file(&file)?;
        self.verify_file(basename, &file)?;
        Ok(file)
    }
    pub(super) fn validate_file(file: &File) -> io::Result<()> {
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.uid() != current_uid()
            || metadata.mode() & 0o022 != 0
        {
            return Err(io::Error::other(
                "journal original file owner/type/alias refused",
            ));
        }
        Ok(())
    }
    pub(super) fn verify_file(&self, basename: &OsStr, original: &File) -> io::Result<()> {
        self.revalidate()?;
        let component = name(basename)?;
        // SAFETY: no-follow relative read of the retained named entry; no FIFO blocking.
        let actual = from_fd(unsafe {
            libc::openat(
                self.leaf()?.as_raw_fd(),
                component.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        })?;
        Self::validate_file(&actual)?;
        if !same(original, &actual)? {
            return Err(io::Error::other("journal original file replaced"));
        }
        Ok(())
    }
    pub(super) fn rename(&self, from: &OsStr, original: &File, to: &OsStr) -> io::Result<()> {
        self.verify_file(from, original)?;
        let from = name(from)?;
        let to = name(to)?;
        let parent = self.leaf()?.as_raw_fd();
        // SAFETY: both bounded names in the same retained directory, no lexical re-resolution.
        if unsafe { libc::renameat(parent, from.as_ptr(), parent, to.as_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        self.revalidate()
    }
}
