"""Bounded, regular-file-only input handles shared by offline audit commands."""

import os
import stat
from contextlib import contextmanager

from .records import require


@contextmanager
def open_regular_input(path: str, maximum: int):
    """Open without blocking on a FIFO and close on every success/error path."""
    require(type(path) is str and path and "\0" not in path, "invalid_input_path")
    # Windows refuses directory opens before a descriptor can be inspected.
    require(stat.S_ISREG(os.stat(path).st_mode), "input_not_regular_file")
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_NONBLOCK", 0)
    descriptor = os.open(path, flags)
    stream = None
    try:
        metadata = os.fstat(descriptor)
        # Recheck the opened object in case the path changed after os.stat.
        require(stat.S_ISREG(metadata.st_mode), "input_not_regular_file")
        require(metadata.st_size <= maximum, "input_too_large")
        stream = os.fdopen(descriptor, "rb")
        yield stream
    finally:
        if stream is None:
            os.close(descriptor)
        else:
            stream.close()
