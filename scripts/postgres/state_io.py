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
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
    fd = os.open(path, flags, 0o600)
    with os.fdopen(fd, "wb") as stream:
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())


def publish_json(path, data):
    encoded = (json.dumps(data, sort_keys=True, indent=2) + "\n").encode()
    if len(encoded) > MAX_RECEIPT_BYTES:
        raise ServiceError("receipt_too_large")
    pending = path.with_name(path.name + ".pending-" + secrets.token_hex(8))
    write_new(pending, encoded)
    os.replace(pending, path)
    sync_directory(path.parent)


def sync_directory(path):
    if os.name != "nt":
        fd = os.open(path, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
        try:
            os.fsync(fd)
        finally:
            os.close(fd)


def state_path(value):
    path = Path(value).expanduser()
    if not path.is_absolute() or any(c in str(path) for c in "\r\n'$\0"):
        raise ServiceError("invalid_state_path")
    # Reject symlink traversal instead of silently writing a different destination.
    if any(p.is_symlink() for p in (path, *path.parents)):
        raise ServiceError("symlink_state_path")
    path = path.resolve()
    source_root = Path(__file__).resolve().parents[2]
    if path == source_root or source_root in path.parents:
        raise ServiceError("state_must_be_outside_source_tree")
    return path


@contextlib.contextmanager
def operation_lock(path):
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
