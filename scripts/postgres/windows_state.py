"""Pin Windows path components while inspecting or creating private state."""

import contextlib
import ctypes
from pathlib import Path

from state_io import ServiceError
from windows_acl import (
    P,
    W,
    bind,
    checked,
    close_handle,
    kernel32,
    private_attributes,
    validate_handle,
)

open_file = bind(
    kernel32,
    "CreateFileW",
    W.HANDLE,
    W.LPCWSTR,
    W.DWORD,
    W.DWORD,
    P,
    W.DWORD,
    W.DWORD,
    W.HANDLE,
)
mkdir = bind(kernel32, "CreateDirectoryW", W.BOOL, W.LPCWSTR, P)
file_info = bind(
    kernel32, "GetFileInformationByHandleEx", W.BOOL, W.HANDLE, W.DWORD, P, W.DWORD
)
seek = bind(
    kernel32, "SetFilePointerEx", W.BOOL, W.HANDLE, ctypes.c_longlong, P, W.DWORD
)
read_file = bind(kernel32, "ReadFile", W.BOOL, W.HANDLE, P, W.DWORD, P, P)
write_file = bind(kernel32, "WriteFile", W.BOOL, W.HANDLE, P, W.DWORD, P, P)
flush_file = bind(kernel32, "FlushFileBuffers", W.BOOL, W.HANDLE)
set_info = bind(
    kernel32, "SetFileInformationByHandle", W.BOOL, W.HANDLE, W.DWORD, P, W.DWORD
)
INVALID_HANDLE = P(-1).value


def absolute_path(value):
    path = Path(value)
    if (
        not path.is_absolute()
        or "\0" in str(path)
        or str(path).startswith(("\\\\?\\", "\\\\.\\"))
        or ".." in path.parts
        or any(":" in part for part in path.parts[1:])
    ):
        raise ServiceError("invalid_windows_state_path")
    return path


class PinnedPaths:
    """Keep all ancestor and target handles until the enclosing operation ends."""

    def __init__(self):
        self.handles = {}

    def _pin(self, path, directory):
        if path in self.handles:
            handle, was_directory = self.handles[path]
            if directory is not None and was_directory != directory:
                raise ServiceError("unexpected_windows_state_object")
            return handle
        access = (
            0x20080 if directory else 0x80020000
        )  # READ_CONTROL plus attributes/data.
        # Share read only: deny rename/deletion and writable reparse-point handles.
        handle = open_file(str(path), access, 1, None, 3, 0x02200000, None)
        if handle == INVALID_HANDLE:
            raise ctypes.WinError(ctypes.get_last_error())
        try:
            attributes = (W.DWORD * 2)()
            checked(file_info(handle, 9, attributes, ctypes.sizeof(attributes)))
            is_directory = bool(attributes[0] & 0x10)
            if attributes[0] & 0x400 or (
                directory is not None and is_directory != directory
            ):
                raise ServiceError("unexpected_or_reparse_windows_state_object")
            self.handles[path] = (handle, is_directory)
            return handle
        except BaseException:
            close_handle(handle)
            raise

    def parents(self, path):
        path = absolute_path(path)
        for parent in reversed(path.parents):
            self._pin(parent, True)
        return path

    def validate(self, path, *, directory=None):
        path = self.parents(path)
        validate_handle(self._pin(path, directory))

    def read(self, path, max_bytes):
        """Read bounded bytes from the same validated handle, never by pathname."""
        if type(max_bytes) is not int or not 0 <= max_bytes <= 65536:
            raise ValueError("invalid private read limit")
        path = absolute_path(path)
        self.validate(path, directory=False)
        handle = self.handles[path][0]
        checked(seek(handle, 0, None, 0))
        buffer = ctypes.create_string_buffer(max_bytes + 1)
        length = W.DWORD()
        checked(read_file(handle, buffer, len(buffer), ctypes.byref(length), None))
        return buffer.raw[: length.value]

    def close(self):
        for handle, _ in reversed(list(self.handles.values())):
            close_handle(handle)
        self.handles.clear()


@contextlib.contextmanager
def pinned_paths():
    scope = PinnedPaths()
    try:
        yield scope
    finally:
        scope.close()


def create_directory(value):
    path = absolute_path(value)
    with pinned_paths() as scope:
        scope.parents(path)
        with private_attributes() as attributes:
            checked(mkdir(str(path), ctypes.byref(attributes)))
        scope.validate(path, directory=True)


def write_new(value, data):
    """Create a protected file exclusively; remove only our partial file on failure."""
    path = absolute_path(value)
    with pinned_paths() as scope:
        scope.validate(path.parent, directory=True)
        with private_attributes() as attributes:
            handle = open_file(
                str(path),
                0x40030000,
                0,
                ctypes.byref(attributes),
                1,
                0x00200080,
                None,
            )  # GENERIC_WRITE | READ_CONTROL | DELETE, CREATE_NEW.
        if handle == INVALID_HANDLE:
            raise ctypes.WinError(ctypes.get_last_error())
        try:
            validate_handle(handle)
            offset = 0
            while offset < len(data):
                chunk = ctypes.create_string_buffer(data[offset : offset + 1048576])
                count = W.DWORD()
                checked(
                    write_file(handle, chunk, len(chunk) - 1, ctypes.byref(count), None)
                )
                if not count.value:
                    raise ServiceError("windows_private_write_failed")
                offset += count.value
            checked(flush_file(handle))
        except BaseException:
            # Deletion is bound to the created object, even if the name later changes.
            remove = W.BOOL(True)
            checked(set_info(handle, 4, ctypes.byref(remove), ctypes.sizeof(remove)))
            raise
        finally:
            close_handle(handle)
