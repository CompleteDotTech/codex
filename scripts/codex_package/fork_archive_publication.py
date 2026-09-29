"""Inactive Linux publication into a caller-owned pinned directory descriptor."""

import ctypes
import hashlib
import os
import secrets
import stat
import sys
from dataclasses import dataclass
from pathlib import Path

from .archive import archive_format_for_path
from .fork_archive import verify_fork_archive


RENAME_NOREPLACE = 1


@dataclass(frozen=True)
class PublicationReceipt:
    directory_device: int
    directory_inode: int
    file_device: int
    file_inode: int
    name: str
    sha256: str


def publish_verified_fork_archive_linux(
    package_dir: Path, source_archive: Path, destination_dir_fd: int, name: str
) -> PublicationReceipt:
    """Move verified bytes to a free name without claiming any destination path.

    The caller retains the directory descriptor. On failure, a staged file may
    remain for explicit reconciliation; no possibly replaced name is unlinked.
    """
    require_linux_publication()
    validate_name(name)
    if archive_format_for_path(source_archive) != archive_format_for_path(Path(name)):
        raise ValueError("fork archive source and destination formats differ")
    directory = os.fstat(destination_dir_fd)
    if not stat.S_ISDIR(directory.st_mode):
        raise ValueError("fork archive destination descriptor is not a directory")
    if directory.st_uid != os.getuid() or directory.st_mode & 0o022:
        raise ValueError("fork archive destination directory must be owner-private")

    package_fd = os.open(package_dir, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        package_identity = os.fstat(package_fd)
        if directory.st_dev == package_identity.st_dev:
            raise ValueError(
                "fork archive destination must be on a different filesystem "
                "from the sealed package"
            )
        if is_descendant(destination_dir_fd, package_identity):
            raise ValueError("fork archive destination is inside the sealed package")
        expected_digest = verify_fork_archive(package_dir, source_archive)
        source_fd = os.open(source_archive, os.O_RDONLY | os.O_NOFOLLOW)
        try:
            if not stat.S_ISREG(os.fstat(source_fd).st_mode):
                raise ValueError("fork archive source is not a regular file")
            stage_name = f".codex-fork-stage-{secrets.token_hex(16)}"
            stage_fd = os.open(
                stage_name,
                os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                0o600,
                dir_fd=destination_dir_fd,
            )
            try:
                digest = hashlib.sha256()
                with (
                    os.fdopen(os.dup(source_fd), "rb") as source,
                    os.fdopen(os.dup(stage_fd), "wb") as stage,
                ):
                    for chunk in iter(lambda: source.read(1024 * 1024), b""):
                        stage.write(chunk)
                        digest.update(chunk)
                    stage.flush()
                if digest.hexdigest() != expected_digest:
                    raise ValueError("fork archive changed after verification")
                os.fchmod(stage_fd, 0o444)
                os.fsync(stage_fd)
                staged = os.fstat(stage_fd)
                named_stage = os.stat(
                    stage_name, dir_fd=destination_dir_fd, follow_symlinks=False
                )
                if not same_inode(staged, named_stage):
                    raise ValueError("fork archive staging name changed")
                if is_descendant(destination_dir_fd, package_identity):
                    raise ValueError("fork archive destination moved inside package")
                rename_noreplace(destination_dir_fd, stage_name, name)
                os.fsync(destination_dir_fd)
                receipt = PublicationReceipt(
                    directory.st_dev,
                    directory.st_ino,
                    staged.st_dev,
                    staged.st_ino,
                    name,
                    expected_digest,
                )
                verify_publication_receipt(destination_dir_fd, receipt)
                return receipt
            finally:
                os.close(stage_fd)
        finally:
            os.close(source_fd)
    finally:
        os.close(package_fd)


def verify_publication_receipt(
    destination_dir_fd: int, receipt: PublicationReceipt
) -> None:
    """Recheck the pinned directory, final inode, and bytes before later use."""
    require_linux_publication()
    directory = os.fstat(destination_dir_fd)
    if (directory.st_dev, directory.st_ino) != (
        receipt.directory_device,
        receipt.directory_inode,
    ):
        raise ValueError("fork archive publication directory changed")
    with os.fdopen(
        os.open(receipt.name, os.O_RDONLY | os.O_NOFOLLOW, dir_fd=destination_dir_fd),
        "rb",
    ) as published:
        file_identity = os.fstat(published.fileno())
        if (file_identity.st_dev, file_identity.st_ino) != (
            receipt.file_device,
            receipt.file_inode,
        ):
            raise ValueError("fork archive published entry changed")
        digest = hashlib.sha256()
        for chunk in iter(lambda: published.read(1024 * 1024), b""):
            digest.update(chunk)
        if digest.hexdigest() != receipt.sha256:
            raise ValueError("fork archive published bytes changed")


def require_linux_publication() -> None:
    if sys.platform != "linux" or not all(
        function in os.supports_dir_fd for function in (os.open, os.stat)
    ):
        raise NotImplementedError("fork archive publication requires Linux dir_fd")


def validate_name(name: str) -> None:
    if not name or name in {".", ".."} or "/" in name or "\\" in name or "\0" in name:
        raise ValueError("fork archive destination must be a single filename")
    archive_format_for_path(Path(name))


def same_inode(left: os.stat_result, right: os.stat_result) -> bool:
    return (left.st_dev, left.st_ino) == (right.st_dev, right.st_ino)


def is_descendant(directory_fd: int, package: os.stat_result) -> bool:
    """Walk pinned ancestors, rejecting output within the sealed package."""
    current_fd = os.dup(directory_fd)
    try:
        for _ in range(256):
            current = os.fstat(current_fd)
            if same_inode(current, package):
                return True
            parent_fd = os.open("..", os.O_RDONLY | os.O_DIRECTORY, dir_fd=current_fd)
            parent = os.fstat(parent_fd)
            os.close(current_fd)
            current_fd = parent_fd
            if same_inode(current, parent):
                return False
        raise ValueError("fork archive destination ancestry exceeds limit")
    finally:
        os.close(current_fd)


def rename_noreplace(directory_fd: int, old_name: str, new_name: str) -> None:
    libc = ctypes.CDLL(None, use_errno=True)
    renameat2 = getattr(libc, "renameat2", None)
    if renameat2 is None:
        raise NotImplementedError(
            "Linux renameat2 is required for no-clobber publication"
        )
    renameat2.argtypes = [
        ctypes.c_int,
        ctypes.c_char_p,
        ctypes.c_int,
        ctypes.c_char_p,
        ctypes.c_uint,
    ]
    renameat2.restype = ctypes.c_int
    if (
        renameat2(
            directory_fd,
            os.fsencode(old_name),
            directory_fd,
            os.fsencode(new_name),
            RENAME_NOREPLACE,
        )
        != 0
    ):
        error = ctypes.get_errno()
        raise OSError(error, os.strerror(error), new_name)
