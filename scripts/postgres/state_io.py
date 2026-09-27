"""Durable files and owned operation locks for external PostgreSQL service state."""

import contextlib
import json
import os
from pathlib import Path
import secrets
import socket
import stat
import subprocess

MAX_RECEIPT_BYTES = 65536


class ServiceError(Exception):
    """A fixed public diagnostic code; never include secret subprocess output."""


def run(argv, *, env=None, timeout=120, discard_output=False):
    try:
        if os.name == "nt":
            from programs import resolve_program

            argv = [resolve_program(argv[0], environment=env), *argv[1:]]
        result = subprocess.run(
            argv, env=env, capture_output=True, timeout=timeout, check=False
        )
    except (OSError, subprocess.TimeoutExpired):
        raise ServiceError("command_unavailable_or_timed_out") from None
    if result.returncode:
        raise ServiceError("command_failed")
    # Native Windows status output may use a legacy code page. Decode only
    # machine-readable output that callers actually consume.
    return "" if discard_output else result.stdout.decode("utf-8")


def write_new(path, data):
    if os.name == "nt":
        from windows_state import write_new as windows_write_new

        return windows_write_new(path, data)
    from posix_io import write_new as posix_write_new

    return posix_write_new(path, data)


def publish_json(path, data):
    encoded = (json.dumps(data, sort_keys=True, indent=2) + "\n").encode()
    if len(encoded) > MAX_RECEIPT_BYTES:
        raise ServiceError("receipt_too_large")
    pending = path.with_name(path.name + ".pending-" + secrets.token_hex(8))
    if os.name != "nt":
        from posix_io import publish

        return publish(pending, path, encoded)
    write_new(pending, encoded)
    try:
        replace_file(pending, path)
        sync_directory(path.parent)
    finally:
        pending.unlink(missing_ok=True)


def replace_file(source, destination):
    """Publish a flushed file, requesting write-through rename on Windows."""
    if os.name == "nt":
        import ctypes
        from ctypes import wintypes

        move = ctypes.WinDLL("kernel32", use_last_error=True).MoveFileExW
        move.argtypes = [wintypes.LPCWSTR, wintypes.LPCWSTR, wintypes.DWORD]
        move.restype = wintypes.BOOL
        # MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH; same-directory move.
        if not move(str(source), str(destination), 0x1 | 0x8):
            raise ctypes.WinError(ctypes.get_last_error())
    else:
        from posix_io import directory

        if source.parent != destination.parent:
            raise ServiceError("cross_directory_publication_refused")
        with directory(source.parent) as parent:
            os.replace(
                source.name, destination.name, src_dir_fd=parent, dst_dir_fd=parent
            )


def sync_directory(path):
    if os.name != "nt":
        from posix_io import directory

        with directory(path) as descriptor:
            os.fsync(descriptor)


def state_path(value):
    path = Path(value).expanduser()
    if (
        not path.is_absolute()
        or ".." in path.parts
        or any(c in str(path) for c in "\r\n'$\0")
    ):
        raise ServiceError("invalid_state_path")
    # lstat also identifies NTFS junctions, unlike is_symlink on Windows.
    for candidate in (path, *path.parents):
        try:
            metadata = candidate.lstat()
        except FileNotFoundError:
            continue
        if (
            stat.S_ISLNK(metadata.st_mode)
            or getattr(metadata, "st_file_attributes", 0) & 0x400
        ):
            raise ServiceError("symlink_state_path")
    if os.name != "nt":
        from posix_io import directory

        try:
            with directory(path, missing=True):
                pass
        except OSError:
            raise ServiceError("symlink_state_path") from None
    source_root = Path(__file__).resolve().parents[2]
    # Compare existing directory identities, including case-insensitive volumes.
    if os.name == "nt" and any(
        candidate.exists() and candidate.samefile(source_root)
        for candidate in (path, *path.parents)
    ):
        raise ServiceError("state_must_be_outside_source_tree")
    return path


@contextlib.contextmanager
def _operation_guard(path):
    if os.name == "nt":
        # Windows keeps this open file undeletable until the lock is released.
        descriptor = os.open(path / ".operation.guard", os.O_RDWR | os.O_CREAT, 0o600)
    else:
        from posix_io import open_guard

        descriptor = open_guard(path)
    try:
        mode = os.fstat(descriptor).st_mode
        if not stat.S_ISREG(mode):
            raise ServiceError("invalid_operation_guard")
        try:
            if os.name == "nt":
                import msvcrt

                msvcrt.locking(descriptor, msvcrt.LK_NBLCK, 1)
            else:
                import fcntl

                fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError:
            raise ServiceError(
                "operation_locked_inspect_owner_before_manual_recovery"
            ) from None
        yield
    finally:
        os.close(descriptor)


@contextlib.contextmanager
def operation_lock(path):
    with contextlib.ExitStack() as stack:
        if os.name == "nt":
            from windows_state import pinned_paths

            scope = stack.enter_context(pinned_paths())
            scope.validate(path, directory=True)
        stack.enter_context(_operation_guard(path))
        lock = path / ".operation.lock"
        payload = json.dumps(
            {
                "pid": os.getpid(),
                "host": socket.gethostname(),
                "token": secrets.token_hex(16),
            }
        ).encode()
        try:
            write_new(lock, payload)
        except FileExistsError:
            raise ServiceError(
                "operation_locked_inspect_owner_before_manual_recovery"
            ) from None
        try:
            yield
        finally:
            try:
                if os.name != "nt":
                    from posix_io import release_marker

                    release_marker(lock, payload)
                else:
                    mode = lock.lstat().st_mode
                    if (
                        not stat.S_ISREG(mode)
                        or lock.stat().st_size != len(payload)
                        or lock.read_bytes() != payload
                    ):
                        raise ServiceError("lock_ownership_changed_preserved")
                    lock.unlink()
            except OSError:
                raise ServiceError("lock_ownership_changed_preserved") from None
