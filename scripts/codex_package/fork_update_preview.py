"""Read-only local bundle comparison, not release authentication or activation."""

import hashlib
import os
import re
from pathlib import Path

from .fork_identity import MANIFEST_NAME
from .fork_identity import read_regular_file
from .fork_identity import verify_fork_package
from .fork_side_by_side import DIR_FLAGS
from .fork_side_by_side import fd_path
from .fork_side_by_side import read_staged_fork_manifest
from .fork_side_by_side import require_linux
from .fork_side_by_side import same_inode


def preview_fork_update(
    install_root: Path, slot_id: str, candidate: Path, expected_digest: str
) -> dict[str, object]:
    """Compare caller-pinned bundles without changing installation or user data.

    The digest must come from the caller's trust process. No signed release
    provenance or source ancestry is authenticated here; activation remains blocked.
    """
    require_linux()
    if re.fullmatch(r"[0-9a-f]{64}", expected_digest) is None:
        raise ValueError("invalid caller-pinned manifest digest")
    current = read_staged_fork_manifest(install_root, slot_id)
    root_fd = os.open(candidate, DIR_FLAGS)
    try:
        pinned = fd_path(root_fd)
        before = read_regular_file(pinned / MANIFEST_NAME, 4 * 1024 * 1024)
        if hashlib.sha256(before).hexdigest() != expected_digest:
            raise ValueError("candidate differs from caller-pinned digest")
        verified = verify_fork_package(pinned, pinned_root_fd=root_fd)
        if verified.unix_mode_status == "unavailable":
            raise ValueError("candidate Unix modes unavailable")
        if read_regular_file(
            pinned / MANIFEST_NAME, 4 * 1024 * 1024
        ) != before or not same_inode(
            os.stat(candidate, follow_symlinks=False), os.fstat(root_fd)
        ):
            raise ValueError("candidate changed during preview")
        proposed = verified.manifest
    finally:
        os.close(root_fd)
    for key in (
        "owner",
        "channel",
        "target",
        "variant",
        "storageCapabilities",
        "postgresSchemaVersions",
    ):
        if current[key] != proposed[key]:
            raise ValueError("candidate requires a separate compatibility plan")
    if current["forkCommit"] == proposed["forkCommit"] and any(
        current[key] != proposed[key]
        for key in ("declaredBaseCommit", "patchsetSha256")
    ):
        raise ValueError("same fork commit has conflicting source identity")
    fields = (
        "owner",
        "channel",
        "target",
        "variant",
        "packageVersion",
        "declaredBaseCommit",
        "forkCommit",
        "patchsetSha256",
    )
    for manifest in (current, proposed):
        version = manifest["packageVersion"]
        if not isinstance(version, str) or len(version.encode("utf-8")) > 128:
            raise ValueError("package version exceeds preview bounds")
    return {
        "status": "sameArtifact"
        if slot_id == expected_digest
        else "differentCandidate",
        "trust": "callerPinnedLocalBundle",
        "activationPermitted": False,
        "equalPackageVersion": current["packageVersion"] == proposed["packageVersion"],
        "current": {key: current[key] for key in fields},
        "candidate": {key: proposed[key] for key in fields},
    }
