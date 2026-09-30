"""Inactive Linux staging of a caller-authenticated fork package.

The caller must authenticate and pin the expected manifest digest. Existing slots and
receipts are never reclaimed implicitly, including after interrupted staging.
"""

import hashlib
import json
import os
import shutil
import stat
import sys
from pathlib import Path

from .fork_identity import MANIFEST_NAME
from .fork_identity import open_regular_file
from .fork_identity import read_regular_file
from .fork_identity import verify_fork_package
from .fork_archive_publication import is_descendant

SLOTS = "fork-slots"
RECEIPTS = "fork-receipts"
DIR_FLAGS = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0)


def stage_fork_package(
    package_dir: Path, install_root: Path, expected_manifest_sha256: str
) -> Path:
    """Stage an inactive package; return its path as an untrusted locator."""
    require_linux()
    if len(expected_manifest_sha256) != 64 or any(
        char not in "0123456789abcdef" for char in expected_manifest_sha256
    ):
        raise ValueError("expected manifest SHA-256 must be lowercase hexadecimal")
    package_fd = os.open(package_dir, DIR_FLAGS)
    root_fd = None
    try:
        root_fd = os.open(install_root, DIR_FLAGS)
        require_private_directory(root_fd)
        if is_descendant(root_fd, os.fstat(package_fd)) or is_descendant(
            package_fd, os.fstat(root_fd)
        ):
            raise ValueError("fork package and install root must not overlap")
        package_path = fd_path(package_fd)
        with open_regular_file(fd_path(package_fd) / MANIFEST_NAME) as manifest_file:
            manifest_bytes = manifest_file.read(4 * 1024 * 1024 + 1)
        if hashlib.sha256(manifest_bytes).hexdigest() != expected_manifest_sha256:
            raise ValueError("fork package manifest differs from authenticated digest")
        verification = verify_fork_package(package_path, pinned_root_fd=package_fd)
        if verification.unix_mode_status == "unavailable":
            raise ValueError("Unix package modes could not be verified")
        if (
            read_regular_file(fd_path(package_fd) / MANIFEST_NAME, 4 * 1024 * 1024)
            != manifest_bytes
        ):
            raise ValueError("fork package manifest changed during verification")
        slots_fd = owned_child_directory(root_fd, SLOTS)
        receipts_fd = None
        try:
            receipts_fd = owned_child_directory(root_fd, RECEIPTS)
            slot_id = expected_manifest_sha256
            if exists_at(slots_fd, slot_id) or exists_at(
                receipts_fd, slot_id + ".json"
            ):
                raise FileExistsError(
                    "fork package slot or receipt already exists; reconcile explicitly"
                )
            os.mkdir(slot_id, 0o700, dir_fd=slots_fd)
            os.chmod(slot_id, 0o700, dir_fd=slots_fd, follow_symlinks=False)
            os.fsync(slots_fd)
            slot_fd = os.open(slot_id, DIR_FLAGS, dir_fd=slots_fd)
            try:
                # Failure leaves a reserved slot for explicit reconciliation.
                slot_path = fd_path(slot_fd)
                for name in sorted(
                    verification.manifest["directories"],
                    key=lambda path: (path.count("/"), path),
                ):
                    directory = slot_path / name
                    directory.mkdir(mode=0o700)
                    directory.chmod(0o700)
                for name, claim in verification.manifest["files"].items():
                    source = package_path / name
                    destination = slot_path / name
                    with (
                        open_regular_file(source) as reader,
                        destination.open("xb") as writer,
                    ):
                        shutil.copyfileobj(reader, writer, length=1024 * 1024)
                        writer.flush()
                        if claim["unixMode"] is not None:
                            os.fchmod(writer.fileno(), int(claim["unixMode"], 8))
                        else:
                            os.fchmod(writer.fileno(), 0o600)
                        os.fsync(writer.fileno())
                manifest_target_fd = os.open(
                    MANIFEST_NAME,
                    os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                    0o600,
                    dir_fd=slot_fd,
                )
                with os.fdopen(manifest_target_fd, "wb") as writer:
                    writer.write(manifest_bytes)
                    writer.flush()
                    os.fchmod(writer.fileno(), 0o600)
                    os.fsync(writer.fileno())
                for name, mode in sorted(
                    verification.manifest["directories"].items(),
                    key=lambda item: item[0].count("/"),
                    reverse=True,
                ):
                    directory = slot_path / name
                    if mode is not None:
                        directory.chmod(int(mode, 8))
                    sync_directory(directory)
                os.fsync(slot_fd)
                verify_fork_package(slot_path, pinned_root_fd=slot_fd)
                if not same_inode(
                    os.stat(slot_id, dir_fd=slots_fd, follow_symlinks=False),
                    os.fstat(slot_fd),
                ):
                    raise ValueError("fork package slot name changed")
                payload = {
                    "receiptVersion": 1,
                    "slot": slot_id,
                    "manifestSha256": "sha256:" + slot_id,
                    "owner": verification.manifest["owner"],
                    "forkCommit": verification.manifest["forkCommit"],
                    "target": verification.manifest["target"],
                    "variant": verification.manifest["variant"],
                    "active": False,
                }
                receipt_fd = os.open(
                    slot_id + ".json",
                    os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                    0o600,
                    dir_fd=receipts_fd,
                )
                with os.fdopen(receipt_fd, "w", encoding="utf-8") as writer:
                    json.dump(payload, writer, sort_keys=True)
                    writer.write("\n")
                    writer.flush()
                    os.fchmod(writer.fileno(), 0o600)
                    os.fsync(writer.fileno())
                os.fsync(receipts_fd)
                verify_staged_fork_package(install_root, slot_id)
                return install_root / SLOTS / slot_id
            finally:
                os.close(slot_fd)
        finally:
            if receipts_fd is not None:
                os.close(receipts_fd)
            os.close(slots_fd)
    finally:
        if root_fd is not None:
            os.close(root_fd)
        os.close(package_fd)


def verify_staged_fork_package(install_root: Path, slot_id: str) -> None:
    """Read back a slot and receipt; this is a point-in-time check."""
    require_linux()
    if len(slot_id) != 64 or any(char not in "0123456789abcdef" for char in slot_id):
        raise ValueError("invalid fork slot identifier")
    root_fd = os.open(install_root, DIR_FLAGS)
    try:
        require_private_directory(root_fd)
        slots_fd = owned_child_directory(root_fd, SLOTS, create=False)
        receipts_fd = None
        try:
            receipts_fd = owned_child_directory(root_fd, RECEIPTS, create=False)
            slot_fd = os.open(slot_id, DIR_FLAGS, dir_fd=slots_fd)
            try:
                with open_regular_file(
                    fd_path(receipts_fd) / (slot_id + ".json")
                ) as reader:
                    receipt_bytes = reader.read(1024 * 1024 + 1)
                if len(receipt_bytes) > 1024 * 1024:
                    raise ValueError("fork package receipt exceeds size limit")
                payload = json.loads(receipt_bytes)
                verification = verify_fork_package(
                    fd_path(slot_fd), pinned_root_fd=slot_fd
                )
                manifest = read_regular_file(
                    fd_path(slot_fd) / MANIFEST_NAME, 4 * 1024 * 1024
                )
                expected = {
                    "receiptVersion": 1,
                    "slot": slot_id,
                    "manifestSha256": "sha256:" + hashlib.sha256(manifest).hexdigest(),
                    "owner": verification.manifest["owner"],
                    "forkCommit": verification.manifest["forkCommit"],
                    "target": verification.manifest["target"],
                    "variant": verification.manifest["variant"],
                    "active": False,
                }
                if (
                    payload != expected
                    or hashlib.sha256(manifest).hexdigest() != slot_id
                ):
                    raise ValueError(
                        "fork package receipt does not match its staged slot"
                    )
                if not same_inode(
                    os.stat(slot_id, dir_fd=slots_fd, follow_symlinks=False),
                    os.fstat(slot_fd),
                ):
                    raise ValueError("fork package slot name changed")
            finally:
                os.close(slot_fd)
        finally:
            if receipts_fd is not None:
                os.close(receipts_fd)
            os.close(slots_fd)
    finally:
        os.close(root_fd)


def require_linux() -> None:
    if sys.platform != "linux" or not Path("/proc/self/fd").is_dir():
        raise NotImplementedError(
            "fork package staging requires Linux descriptor pinning"
        )


def fd_path(descriptor: int) -> Path:
    return Path(f"/proc/self/fd/{descriptor}")


def require_private_directory(descriptor: int) -> None:
    metadata = os.fstat(descriptor)
    if (
        not stat.S_ISDIR(metadata.st_mode)
        or metadata.st_uid != os.getuid()
        or metadata.st_mode & 0o077
    ):
        raise ValueError("fork install directory must be owner-private")


def owned_child_directory(parent_fd: int, name: str, *, create: bool = True) -> int:
    if create:
        try:
            os.mkdir(name, 0o700, dir_fd=parent_fd)
            os.chmod(name, 0o700, dir_fd=parent_fd, follow_symlinks=False)
            os.fsync(parent_fd)
        except FileExistsError:
            pass
    child_fd = os.open(name, DIR_FLAGS, dir_fd=parent_fd)
    try:
        require_private_directory(child_fd)
    except BaseException:
        os.close(child_fd)
        raise
    return child_fd


def exists_at(directory_fd: int, name: str) -> bool:
    try:
        os.stat(name, dir_fd=directory_fd, follow_symlinks=False)
        return True
    except FileNotFoundError:
        return False


def same_inode(left: os.stat_result, right: os.stat_result) -> bool:
    return (left.st_dev, left.st_ino) == (right.st_dev, right.st_ino)


def sync_directory(path: Path) -> None:
    descriptor = os.open(path, DIR_FLAGS)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
