"""POSIX owner and mode checks tied to the file descriptor used for reading."""

import os
import stat

from state_io import ServiceError


def validate_directory(path):
    metadata = path.lstat()
    if (
        not stat.S_ISDIR(metadata.st_mode)
        or metadata.st_mode & 0o077
        or metadata.st_uid not in (os.geteuid(), 0)
    ):
        raise ServiceError("insecure_state_directory")


def read_private(path, limit, *, invalid_type, insecure_permissions):
    if not stat.S_ISREG(path.lstat().st_mode):
        raise ServiceError(invalid_type)
    # Nonblocking also rejects a FIFO swapped in after the preliminary type check.
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as stream:
        metadata = os.fstat(stream.fileno())
        if not stat.S_ISREG(metadata.st_mode):
            raise ServiceError(invalid_type)
        if metadata.st_mode & 0o077 or metadata.st_uid not in (os.geteuid(), 0):
            raise ServiceError(insecure_permissions)
        return stream.read(limit + 1)
