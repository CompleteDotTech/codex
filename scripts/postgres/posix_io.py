"""Descriptor-relative POSIX access and a guard outside replaceable service state."""

import contextlib
import hashlib
import os
from pathlib import Path
import stat
import sys
import unicodedata

from state_io import ServiceError

DIRECTORY_FLAGS = (
    os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0)
)


def lexical_path(value):
    path = Path(value)
    if path.anchor != "/" or ".." in path.parts:
        raise ServiceError("invalid_state_path")
    return path


@contextlib.contextmanager
def directory(value, *, create=False, missing=False):
    """Traverse every component from root using pinned parent descriptors."""
    path = lexical_path(value)
    source = Path(__file__).resolve().parents[2].stat()
    handles = []
    try:
        current = os.open(path.anchor, DIRECTORY_FLAGS)
        handles.append(current)
        for part in (None, *path.parts[1:]):
            if part is not None:
                try:
                    current = os.open(part, DIRECTORY_FLAGS, dir_fd=current)
                except FileNotFoundError:
                    if missing:
                        break
                    if not create:
                        raise
                    try:
                        os.mkdir(part, 0o700, dir_fd=current)
                    except FileExistsError:
                        pass
                    current = os.open(part, DIRECTORY_FLAGS, dir_fd=current)
                handles.append(current)
            metadata = os.fstat(current)
            if (metadata.st_dev, metadata.st_ino) == (source.st_dev, source.st_ino):
                raise ServiceError("state_must_be_outside_source_tree")
            if metadata.st_uid not in (os.geteuid(), 0) or (
                metadata.st_mode & 0o022 and not metadata.st_mode & stat.S_ISVTX
            ):
                raise ServiceError("unsafe_state_ancestor")
        yield current
    finally:
        for descriptor in reversed(handles):
            os.close(descriptor)


def create_directory(path):
    path = lexical_path(path)
    with directory(path.parent) as parent:
        os.mkdir(path.name, 0o700, dir_fd=parent)
        os.fsync(parent)


def _write_new(parent, name, data):
    descriptor = os.open(
        name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=parent
    )
    try:
        with os.fdopen(descriptor, "wb") as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
    except BaseException:
        os.unlink(name, dir_fd=parent)
        raise


def write_new(path, data):
    path = lexical_path(path)
    with directory(path.parent) as parent:
        _write_new(parent, path.name, data)


def publish(pending, path, encoded):
    with directory(path.parent) as parent:
        _write_new(parent, pending.name, encoded)
        try:
            os.replace(pending.name, path.name, src_dir_fd=parent, dst_dir_fd=parent)
            os.fsync(parent)
        finally:
            try:
                os.unlink(pending.name, dir_fd=parent)
            except FileNotFoundError:
                pass


def guard_path(path):
    import pwd

    account_home = Path(pwd.getpwuid(os.geteuid()).pw_dir).resolve()
    root = account_home / ".local/state/codex-postgres/locks"
    identity = str(lexical_path(path))
    if sys.platform == "darwin":
        # Conservatively serialize case/canonical-Unicode aliases on macOS.
        identity = unicodedata.normalize("NFD", identity).casefold()
    return root / hashlib.sha256(os.fsencode(identity)).hexdigest()


def open_guard(path):
    guard = guard_path(path)
    with directory(guard.parent, create=True) as parent:
        root = os.fstat(parent)
        if root.st_uid != os.geteuid() or root.st_mode & 0o077:
            raise ServiceError("invalid_operation_guard_directory")
        descriptor = os.open(
            guard.name, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600, dir_fd=parent
        )
    metadata = os.fstat(descriptor)
    if (
        not stat.S_ISREG(metadata.st_mode)
        or metadata.st_mode & 0o077
        or metadata.st_uid != os.geteuid()
    ):
        os.close(descriptor)
        raise ServiceError("invalid_operation_guard")
    return descriptor


def release_marker(path, payload):
    with directory(path.parent) as parent:
        descriptor = os.open(
            path.name, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW, dir_fd=parent
        )
        with os.fdopen(descriptor, "rb") as stream:
            if (
                not stat.S_ISREG(os.fstat(stream.fileno()).st_mode)
                or stream.read(len(payload) + 1) != payload
            ):
                raise ServiceError("lock_ownership_changed_preserved")
        os.unlink(path.name, dir_fd=parent)


def read_private(path, limit, *, invalid_type, insecure_permissions):
    """Validate the owner/mode of the opened object before consuming bounded bytes."""
    if type(limit) is not int or not 0 <= limit <= 65536:
        raise ValueError("invalid private read limit")
    path = lexical_path(path)
    with directory(path.parent) as parent:
        if not stat.S_ISREG(
            os.stat(path.name, dir_fd=parent, follow_symlinks=False).st_mode
        ):
            raise ServiceError(invalid_type)
        descriptor = os.open(
            path.name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=parent
        )
        with os.fdopen(descriptor, "rb") as stream:
            metadata = os.fstat(stream.fileno())
            if not stat.S_ISREG(metadata.st_mode):
                raise ServiceError(invalid_type)
            if metadata.st_mode & 0o077 or metadata.st_uid not in (os.geteuid(), 0):
                raise ServiceError(insecure_permissions)
            return stream.read(limit + 1)
