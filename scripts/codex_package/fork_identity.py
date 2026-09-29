"""Opt-in, source-bound identity and byte inventory for fork package archives.

This manifest detects accidental or local tampering. Authentication of a downloaded
manifest belongs to a future fork release channel, before installation is allowed.
"""

import hashlib
import json
import os
import re
import stat
import subprocess
from dataclasses import dataclass
from pathlib import Path

from .layout import validate_package_dir
from .targets import PACKAGE_VARIANTS
from .targets import REPO_ROOT
from .targets import TARGET_SPECS


MANIFEST_NAME = "codex-fork-package.json"
OWNER = "CompleteDotTech/codex"
COMMIT_RE = re.compile(r"[0-9a-f]{40}\Z")
DIGEST_RE = re.compile(r"sha256:[0-9a-f]{64}\Z")
MODE_RE = re.compile(r"[0-7]{4}\Z")


@dataclass(frozen=True)
class ForkPackageVerification:
    manifest: dict[str, object]
    unix_mode_status: str  # verified, unavailable, or notApplicable


def source_identity(base_commit: str, channel: str) -> dict[str, object]:
    """Describe committed source relative to a caller-declared local ancestor."""
    if not COMMIT_RE.fullmatch(base_commit):
        raise ValueError("base commit must be a full lowercase Git SHA-1")
    if channel not in {"preview", "stable"}:
        raise ValueError("fork channel must be preview or stable")
    if git("status", "--porcelain=v1", "--untracked-files=all"):
        raise ValueError("fork candidate source worktree is dirty")
    fork_commit = git("rev-parse", "HEAD").decode("ascii").strip()
    if not COMMIT_RE.fullmatch(fork_commit):
        raise ValueError("fork commit must be a full lowercase Git SHA-1")
    git("merge-base", "--is-ancestor", base_commit, fork_commit)
    patchset = git("diff", "--no-ext-diff", "--binary", base_commit, fork_commit)
    return {
        "owner": OWNER,
        "declaredBaseCommit": base_commit,
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
    files, directories = package_tree(package_dir)
    package_metadata = json.loads((package_dir / "codex-package.json").read_text())
    target = package_metadata["target"]
    if target not in TARGET_SPECS:
        raise ValueError("unsupported fork package target")
    if not TARGET_SPECS[target].is_windows and os.name == "nt":
        raise ValueError("Unix package modes cannot be sealed on Windows")
    check_package_shape(package_dir, files, directories, package_metadata)
    manifest = {
        "manifestVersion": 1,
        **identity,
        "packageVersion": package_metadata["version"],
        "target": target,
        "variant": package_metadata["variant"],
        "files": {
            name: {
                "sha256": "sha256:" + sha256(path),
                "unixMode": None
                if TARGET_SPECS[target].is_windows
                else unix_mode(path),
            }
            for name, path in files.items()
        },
        "directories": {
            name: None if TARGET_SPECS[target].is_windows else unix_mode(path)
            for name, path in directories.items()
        },
    }
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")


def verify_fork_package(
    package_dir: Path, *, pinned_root_fd: int | None = None
) -> ForkPackageVerification:
    """Reject incomplete, extended, or byte-modified packages without executing them."""
    if pinned_root_fd is not None:
        if os.name != "posix" or package_dir != Path(f"/proc/self/fd/{pinned_root_fd}"):
            raise ValueError("fork package pinned root descriptor is invalid")
        pinned = os.fstat(pinned_root_fd)
        root = os.stat(package_dir)
        if not stat.S_ISDIR(pinned.st_mode) or (
            root.st_dev,
            root.st_ino,
        ) != (pinned.st_dev, pinned.st_ino):
            raise ValueError("fork package pinned root descriptor is invalid")
    manifest_path = package_dir / MANIFEST_NAME
    if manifest_path.is_symlink() or not manifest_path.is_file():
        raise ValueError("missing regular fork package manifest")
    if manifest_path.stat().st_size > 4 * 1024 * 1024:
        raise ValueError("fork package manifest exceeds size limit")
    actual, directories = package_tree(
        package_dir, exclude_manifest=True, pinned_root_fd=pinned_root_fd
    )
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    if (
        not isinstance(manifest, dict)
        or set(manifest)
        != {
            "manifestVersion",
            "owner",
            "declaredBaseCommit",
            "forkCommit",
            "patchsetSha256",
            "channel",
            "storageCapabilities",
            "postgresSchemaVersions",
            "packageVersion",
            "target",
            "variant",
            "files",
            "directories",
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
    check_package_shape(package_dir, actual, directories, metadata)
    target = manifest["target"]
    spec = TARGET_SPECS[target]
    expected = manifest["files"]
    if not isinstance(expected, dict) or not expected:
        raise ValueError("fork package file inventory is missing")
    for name, claim in expected.items():
        if not isinstance(name, str) or not safe_name(name):
            raise ValueError("fork package file inventory contains an unsafe path")
        if not isinstance(claim, dict) or set(claim) != {"sha256", "unixMode"}:
            raise ValueError("fork package file inventory contains an invalid claim")
        digest = claim["sha256"]
        if not isinstance(digest, str) or not DIGEST_RE.fullmatch(digest):
            raise ValueError("fork package file inventory contains an invalid digest")
    if set(actual) != set(expected):
        raise ValueError("fork package file inventory differs from package contents")
    expected_directories = manifest["directories"]
    if not isinstance(expected_directories, dict) or set(directories) != set(
        expected_directories
    ):
        raise ValueError(
            "fork package directory inventory differs from package contents"
        )
    mode_status = (
        "notApplicable"
        if spec.is_windows
        else ("unavailable" if os.name == "nt" else "verified")
    )
    required_executables = {
        f"bin/{PACKAGE_VARIANTS[manifest['variant']].entrypoint_name(spec)}",
        f"bin/codex-code-mode-host{spec.exe_suffix}",
        f"codex-path/{spec.rg_name}",
    }
    if spec.is_linux:
        required_executables.add("codex-resources/bwrap")
    required_executables.update(
        name for name in actual if name == "codex-resources/zsh/bin/zsh"
    )
    for name, path in actual.items():
        claim = expected[name]
        if "sha256:" + sha256(path) != claim["sha256"]:
            raise ValueError(f"fork package file checksum differs: {name}")
        check_mode_claim(
            name,
            path,
            claim["unixMode"],
            spec.is_windows,
            mode_status,
            required_executable=name in required_executables,
        )
    for name, path in directories.items():
        check_mode_claim(
            name,
            path,
            expected_directories[name],
            spec.is_windows,
            mode_status,
            required_executable=True,
        )
    return ForkPackageVerification(manifest=manifest, unix_mode_status=mode_status)


def validate_identity(identity: dict[str, object]) -> None:
    if identity.get("owner") != OWNER:
        raise ValueError("fork package owner is not CompleteDotTech/codex")
    for key in ("declaredBaseCommit", "forkCommit"):
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


def check_package_shape(
    package_dir: Path,
    files: dict[str, Path],
    directories: dict[str, Path],
    metadata: dict[str, object],
) -> None:
    target = metadata.get("target")
    variant = metadata.get("variant")
    if (
        not isinstance(target, str)
        or not isinstance(variant, str)
        or target not in TARGET_SPECS
        or variant not in PACKAGE_VARIANTS
    ):
        raise ValueError("unsupported fork package target or variant")
    spec = TARGET_SPECS[target]
    try:
        validate_package_dir(
            package_dir,
            PACKAGE_VARIANTS[variant],
            spec,
            include_zsh=any(name.startswith("codex-resources/zsh/") for name in files),
            check_executable_permissions=False,
        )
    except RuntimeError as error:
        raise ValueError(str(error)) from error
    allowed_bin = {
        f"bin/{PACKAGE_VARIANTS[variant].entrypoint_name(spec)}",
        f"bin/codex-code-mode-host{spec.exe_suffix}",
    }
    allowed_resources = set()
    if spec.is_linux:
        allowed_resources.add("codex-resources/bwrap")
    if spec.is_windows:
        allowed_resources.update(
            {
                "codex-resources/codex-command-runner.exe",
                "codex-resources/codex-windows-sandbox-setup.exe",
            }
        )
    if "codex-resources/zsh/bin/zsh" in files:
        allowed_resources.add("codex-resources/zsh/bin/zsh")
    allowed_directories = {"bin", "codex-resources", "codex-path"}
    if "codex-resources/zsh/bin/zsh" in files:
        allowed_directories.update({"codex-resources/zsh", "codex-resources/zsh/bin"})
    if set(directories) != allowed_directories:
        raise ValueError("fork package contains unexpected or empty directories")
    for name in files:
        if "/" not in name and name != "codex-package.json":
            raise ValueError(f"unexpected fork package root file: {name}")
        if name.startswith("bin/") and name not in allowed_bin:
            raise ValueError(f"unexpected fork package executable: {name}")
        if name.startswith("codex-path/") and name != f"codex-path/{spec.rg_name}":
            raise ValueError(f"unexpected fork package path command: {name}")
        if name.startswith("codex-resources/") and name not in allowed_resources:
            raise ValueError(f"unexpected fork package resource: {name}")


def package_tree(
    package_dir: Path,
    *,
    exclude_manifest: bool = False,
    pinned_root_fd: int | None = None,
) -> tuple[dict[str, Path], dict[str, Path]]:
    if (
        package_dir.is_symlink() and pinned_root_fd is None
    ) or not package_dir.is_dir():
        raise ValueError("fork package root is not a regular directory")
    files = {}
    directories = {}
    for path in package_dir.rglob("*"):
        is_junction = getattr(path, "is_junction", lambda: False)
        if path.is_symlink() or is_junction() or not (path.is_file() or path.is_dir()):
            raise ValueError("fork package contains a link or special file")
        name = path.relative_to(package_dir).as_posix()
        if exclude_manifest and name == MANIFEST_NAME:
            continue
        if not safe_name(name):
            raise ValueError("fork package contains an unsafe path")
        if path.is_dir():
            directories[name] = path
        else:
            files[name] = path
    if "codex-package.json" not in files:
        raise ValueError("fork package lacks canonical package metadata")
    return files, directories


def check_mode_claim(
    name: str,
    path: Path,
    claim: object,
    is_windows_target: bool,
    status: str,
    *,
    required_executable: bool = False,
) -> None:
    if is_windows_target:
        if claim is not None:
            raise ValueError(f"Windows package has unexpected Unix mode: {name}")
        return
    if not isinstance(claim, str) or not MODE_RE.fullmatch(claim):
        raise ValueError(f"invalid Unix mode claim: {name}")
    if required_executable and not int(claim, 8) & 0o111:
        raise ValueError(f"package path is not executable: {name}")
    if status == "verified" and unix_mode(path) != claim:
        raise ValueError(f"Unix mode differs: {name}")


def unix_mode(path: Path) -> str:
    return format(stat.S_IMODE(path.stat().st_mode), "04o")


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
