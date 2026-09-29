"""Inactive side-by-side staging of a caller-authenticated fork package.

The caller must authenticate the release before calling this module. A stale slot or
receipt is never reclaimed implicitly, including after interrupted staging.
"""

import hashlib
import json
import os
import shutil
import stat
from pathlib import Path

from .fork_identity import MANIFEST_NAME
from .fork_identity import verify_fork_package


SLOTS = "fork-slots"
RECEIPTS = "fork-receipts"


def stage_fork_package(package_dir: Path, install_root: Path) -> Path:
    """Copy a verified package to a new, inactive slot and publish its receipt."""
    verification = verify_fork_package(package_dir)
    if verification.unix_mode_status == "unavailable":
        raise ValueError("Unix package modes must be verified on Unix before staging")
    require_private_root(install_root)
    manifest_bytes = (package_dir / MANIFEST_NAME).read_bytes()
    slot_id = hashlib.sha256(manifest_bytes).hexdigest()
    slots = owned_directory(install_root, SLOTS)
    receipts = owned_directory(install_root, RECEIPTS)
    slot = slots / slot_id
    receipt = receipts / f"{slot_id}.json"
    if slot.exists() or slot.is_symlink() or receipt.exists() or receipt.is_symlink():
        raise FileExistsError(
            "fork package slot or receipt already exists; reconcile explicitly"
        )
    slot.mkdir(mode=0o700)
    sync_directory(slots)
    # No cleanup on failure: a reserved slot is evidence of an incomplete attempt.
    for name, mode in verification.manifest["directories"].items():
        destination = slot / name
        destination.mkdir(mode=0o700)
    for name, claim in verification.manifest["files"].items():
        source = package_dir / name
        destination = slot / name
        with source.open("rb") as reader, destination.open("xb") as writer:
            shutil.copyfileobj(reader, writer, length=1024 * 1024)
            writer.flush()
            os.fsync(writer.fileno())
        if claim["unixMode"] is not None:
            destination.chmod(int(claim["unixMode"], 8))
    with (slot / MANIFEST_NAME).open("xb") as writer:
        writer.write(manifest_bytes)
        writer.flush()
        os.fsync(writer.fileno())
    for name, mode in verification.manifest["directories"].items():
        if mode is not None:
            (slot / name).chmod(int(mode, 8))
    verify_fork_package(slot)
    for directory in sorted(
        (path for path in slot.rglob("*") if path.is_dir()), reverse=True
    ):
        sync_directory(directory)
    sync_directory(slot)
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
    with receipt.open("x", encoding="utf-8") as writer:
        json.dump(payload, writer, sort_keys=True)
        writer.write("\n")
        writer.flush()
        os.fsync(writer.fileno())
    sync_directory(receipts)
    verify_staged_fork_package(install_root, slot_id)
    return slot


def verify_staged_fork_package(install_root: Path, slot_id: str) -> None:
    """Read back a staged slot and its external receipt; do not execute it."""
    require_private_root(install_root)
    if len(slot_id) != 64 or any(
        character not in "0123456789abcdef" for character in slot_id
    ):
        raise ValueError("invalid fork slot identifier")
    slots = owned_directory(install_root, SLOTS)
    receipts = owned_directory(install_root, RECEIPTS)
    slot = slots / slot_id
    receipt = receipts / f"{slot_id}.json"
    if slot.is_symlink() or receipt.is_symlink() or not receipt.is_file():
        raise ValueError("fork package receipt is missing or linked")
    payload = json.loads(receipt.read_text(encoding="utf-8"))
    verification = verify_fork_package(slot)
    manifest_digest = hashlib.sha256((slot / MANIFEST_NAME).read_bytes()).hexdigest()
    expected = {
        "receiptVersion": 1,
        "slot": slot_id,
        "manifestSha256": "sha256:" + manifest_digest,
        "owner": verification.manifest["owner"],
        "forkCommit": verification.manifest["forkCommit"],
        "target": verification.manifest["target"],
        "variant": verification.manifest["variant"],
        "active": False,
    }
    if payload != expected or manifest_digest != slot_id:
        raise ValueError("fork package receipt does not match its staged slot")


def require_private_root(root: Path) -> None:
    if (
        root.is_symlink()
        or not root.is_dir()
        or getattr(root, "is_junction", lambda: False)()
    ):
        raise ValueError("fork install root must be an existing regular directory")
    if os.name == "posix":
        metadata = root.stat()
        if metadata.st_uid != os.getuid() or stat.S_IMODE(metadata.st_mode) & 0o077:
            raise ValueError("fork install root must be owner-private")


def owned_directory(root: Path, name: str) -> Path:
    path = root / name
    if not path.exists() and not path.is_symlink():
        path.mkdir(mode=0o700)
        sync_directory(root)
    if (
        path.is_symlink()
        or getattr(path, "is_junction", lambda: False)()
        or not path.is_dir()
    ):
        raise ValueError("fork install component must be a regular directory")
    if os.name == "posix":
        metadata = path.stat()
        if metadata.st_uid != os.getuid() or stat.S_IMODE(metadata.st_mode) & 0o077:
            raise ValueError("fork install component must be owner-private")
    return path


def sync_directory(path: Path) -> None:
    if os.name == "posix":
        descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
