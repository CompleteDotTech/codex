"""Opt-in, source-bound identity and byte inventory for fork package archives.

This manifest detects accidental or local tampering. Authentication of a downloaded
manifest belongs to a future fork release channel, before installation is allowed.
"""

import hashlib
import json
import re
import subprocess
from pathlib import Path

from .layout import validate_package_dir
from .targets import PACKAGE_VARIANTS
from .targets import REPO_ROOT
from .targets import TARGET_SPECS


MANIFEST_NAME = "codex-fork-package.json"
OWNER = "CompleteDotTech/codex"
COMMIT_RE = re.compile(r"[0-9a-f]{40}\Z")
DIGEST_RE = re.compile(r"sha256:[0-9a-f]{64}\Z")


def source_identity(upstream_commit: str, channel: str) -> dict[str, object]:
    """Bind a proposed fork package to committed repository source."""
    if not COMMIT_RE.fullmatch(upstream_commit):
        raise ValueError("upstream commit must be a full lowercase Git SHA-1")
    if channel not in {"preview", "stable"}:
        raise ValueError("fork channel must be preview or stable")
    fork_commit = git("rev-parse", "HEAD").decode("ascii").strip()
    if not COMMIT_RE.fullmatch(fork_commit):
        raise ValueError("fork commit must be a full lowercase Git SHA-1")
    git("merge-base", "--is-ancestor", upstream_commit, fork_commit)
    patchset = git("diff", "--no-ext-diff", "--binary", upstream_commit, fork_commit)
    return {
        "owner": OWNER,
        "upstreamCommit": upstream_commit,
        "forkCommit": fork_commit,
        "patchsetSha256": "sha256:" + hashlib.sha256(patchset).hexdigest(),
        "channel": channel,
        "storageCapabilities": ["sqlite"],
        "postgresSchemaVersions": [],
    }


def git(*args: str) -> bytes:
    return subprocess.check_output(["git", "-C", str(REPO_ROOT), *args])


def seal_fork_package(package_dir: Path, identity: dict[str, object]) -> None:
    """Write a byte inventory only after the ordinary package has been assembled."""
    validate_identity(identity)
    manifest_path = package_dir / MANIFEST_NAME
    if manifest_path.exists() or manifest_path.is_symlink():
        raise ValueError("fork package manifest already exists")
    files = package_files(package_dir)
    package_metadata = json.loads((package_dir / "codex-package.json").read_text())
    manifest = {
        "manifestVersion": 1,
        **identity,
        "packageVersion": package_metadata["version"],
        "target": package_metadata["target"],
        "variant": package_metadata["variant"],
        "files": {name: "sha256:" + sha256(path) for name, path in files.items()},
    }
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")


def write_archive_checksum(archive_path: Path, *, force: bool = False) -> Path:
    """Write a separate archive checksum for a future authenticated release index."""
    checksum_path = archive_path.with_name(archive_path.name + ".sha256")
    if checksum_path.is_symlink() or (not force and checksum_path.exists()):
        raise ValueError(f"fork archive checksum already exists: {checksum_path}")
    checksum_path.write_text(
        f"{sha256(archive_path)}  {archive_path.name}\n", encoding="ascii"
    )
    return checksum_path


def verify_fork_package(package_dir: Path) -> dict[str, object]:
    """Reject incomplete, extended, or byte-modified packages without executing them."""
    manifest_path = package_dir / MANIFEST_NAME
    if manifest_path.is_symlink() or not manifest_path.is_file():
        raise ValueError("missing regular fork package manifest")
    if manifest_path.stat().st_size > 4 * 1024 * 1024:
        raise ValueError("fork package manifest exceeds size limit")
    actual = package_files(package_dir, exclude_manifest=True)
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    if (
        not isinstance(manifest, dict)
        or set(manifest)
        != {
            "manifestVersion",
            "owner",
            "upstreamCommit",
            "forkCommit",
            "patchsetSha256",
            "channel",
            "storageCapabilities",
            "postgresSchemaVersions",
            "packageVersion",
            "target",
            "variant",
            "files",
        }
        or manifest["manifestVersion"] != 1
    ):
        raise ValueError("unsupported fork package manifest")
    validate_identity(manifest)
    metadata = json.loads(actual["codex-package.json"].read_text())
    if not isinstance(metadata, dict):
        raise ValueError("invalid canonical package metadata")
    for key, metadata_key in (
        ("packageVersion", "version"),
        ("target", "target"),
        ("variant", "variant"),
    ):
        if manifest[key] != metadata.get(metadata_key):
            raise ValueError(f"fork package {key} disagrees with package metadata")
    if not isinstance(manifest["packageVersion"], str):
        raise ValueError("invalid fork package version")
    target = manifest["target"]
    variant = manifest["variant"]
    if target not in TARGET_SPECS or variant not in PACKAGE_VARIANTS:
        raise ValueError("unsupported fork package target or variant")
    try:
        validate_package_dir(
            package_dir,
            PACKAGE_VARIANTS[variant],
            TARGET_SPECS[target],
            include_zsh=any(name.startswith("codex-resources/zsh/") for name in actual),
            check_executable_permissions=False,
        )
    except RuntimeError as error:
        raise ValueError(str(error)) from error
    spec = TARGET_SPECS[target]
    allowed_bin = {
        f"bin/{PACKAGE_VARIANTS[variant].entrypoint_name(spec)}",
        f"bin/codex-code-mode-host{spec.exe_suffix}",
    }
    for name in actual:
        if "/" not in name and name != "codex-package.json":
            raise ValueError(f"unexpected fork package root file: {name}")
        if name.startswith("bin/") and name not in allowed_bin:
            raise ValueError(f"unexpected fork package executable: {name}")
        if name.startswith("codex-path/") and name != f"codex-path/{spec.rg_name}":
            raise ValueError(f"unexpected fork package path command: {name}")
    expected = manifest["files"]
    if not isinstance(expected, dict) or not expected:
        raise ValueError("fork package file inventory is missing")
    for name, digest in expected.items():
        if not isinstance(name, str) or not safe_name(name):
            raise ValueError("fork package file inventory contains an unsafe path")
        if not isinstance(digest, str) or not DIGEST_RE.fullmatch(digest):
            raise ValueError("fork package file inventory contains an invalid digest")
    if set(actual) != set(expected):
        raise ValueError("fork package file inventory differs from package contents")
    for name, path in actual.items():
        if "sha256:" + sha256(path) != expected[name]:
            raise ValueError(f"fork package file checksum differs: {name}")
    return manifest


def validate_identity(identity: dict[str, object]) -> None:
    if identity.get("owner") != OWNER:
        raise ValueError("fork package owner is not CompleteDotTech/codex")
    for key in ("upstreamCommit", "forkCommit"):
        value = identity.get(key)
        if not isinstance(value, str) or not COMMIT_RE.fullmatch(value):
            raise ValueError(f"invalid fork package {key}")
    patchset = identity.get("patchsetSha256")
    if not isinstance(patchset, str) or not DIGEST_RE.fullmatch(patchset):
        raise ValueError("invalid fork package patchset digest")
    if identity.get("channel") not in {"preview", "stable"}:
        raise ValueError("invalid fork package channel")
    if identity.get("storageCapabilities") != ["sqlite"]:
        raise ValueError("unqualified storage capability in fork package")
    if identity.get("postgresSchemaVersions") != []:
        raise ValueError("unqualified PostgreSQL schema in fork package")


def package_files(
    package_dir: Path, *, exclude_manifest: bool = False
) -> dict[str, Path]:
    if package_dir.is_symlink() or not package_dir.is_dir():
        raise ValueError("fork package root is not a regular directory")
    files = {}
    for path in package_dir.rglob("*"):
        if path.is_symlink() or not (path.is_file() or path.is_dir()):
            raise ValueError("fork package contains a link or special file")
        if path.is_dir():
            continue
        name = path.relative_to(package_dir).as_posix()
        if exclude_manifest and name == MANIFEST_NAME:
            continue
        if not safe_name(name):
            raise ValueError("fork package contains an unsafe path")
        files[name] = path
    if "codex-package.json" not in files:
        raise ValueError("fork package lacks canonical package metadata")
    return files


def safe_name(name: str) -> bool:
    return (
        name != MANIFEST_NAME
        and "\\" not in name
        and all(component not in {"", ".", ".."} for component in name.split("/"))
        and not name.startswith("/")
    )


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()
