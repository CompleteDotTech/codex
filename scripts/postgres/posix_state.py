"""POSIX owner and mode checks tied to the file descriptor used for reading."""

import os
import stat

from state_io import ServiceError
from posix_io import directory


def validate_directory(path):
    if not stat.S_ISDIR(path.lstat().st_mode):
        raise ServiceError("insecure_state_directory")
    with directory(path) as descriptor:
        metadata = os.fstat(descriptor)
    if (
        not stat.S_ISDIR(metadata.st_mode)
        or metadata.st_mode & 0o077
        or metadata.st_uid not in (os.geteuid(), 0)
    ):
        raise ServiceError("insecure_state_directory")
