"""Verify serialized fork package candidates before publishing their checksums."""

import hashlib
import json
import os
import stat
import subprocess
import tarfile
import tempfile
import zipfile
from contextlib import ExitStack
from pathlib import Path

from .archive import archive_format_for_path
from .archive import is_relative_to
from .archive import resolve_zstd_command
from .archive import write_archive
from .fork_identity import MANIFEST_NAME
from .fork_identity import safe_name
from .fork_identity import verify_fork_package
from .targets import TARGET_SPECS


def publish_verified_fork_archives(
    package_dir: Path, outputs: list[Path], *, force: bool
) -> list[Path]:
    """Stage and verify every archive before publishing any archive or checksum."""
    verification = verify_fork_package(package_dir)
    manifest_bytes = (package_dir / MANIFEST_NAME).read_bytes()
    if json.loads(manifest_bytes) != verification.manifest:
        raise ValueError("fork manifest changed after directory verification")
    package_root = package_dir.resolve()
    destinations = [output.absolute() for output in outputs]
    sidecars = [dest.with_name(dest.name + ".sha256") for dest in destinations]
    if len(set(destinations + sidecars)) != len(destinations) * 2:
        raise ValueError("fork archive outputs or checksum paths collide")
    for dest, sidecar in zip(destinations, sidecars, strict=True):
        archive_format_for_path(dest)
        if is_relative_to(dest.resolve(), package_root) or is_relative_to(
            sidecar.resolve(), package_root
        ):
            raise ValueError(
                "fork archive output must be outside the package directory"
            )
        if dest.is_symlink() or sidecar.is_symlink():
            raise ValueError("fork archive output or checksum is a link")
        if not force and (dest.exists() or sidecar.exists()):
            raise ValueError("fork archive output or checksum already exists")

    staged = []
    with ExitStack() as stack:
        for dest, sidecar in zip(destinations, sidecars, strict=True):
            dest.parent.mkdir(parents=True, exist_ok=True)
            staging_dir = Path(
                stack.enter_context(
                    tempfile.TemporaryDirectory(prefix=".codex-fork-", dir=dest.parent)
                )
            )
            archive_path = staging_dir / dest.name
            write_archive(package_dir, archive_path, force=False)
            digest = verify_fork_archive(archive_path, manifest_bytes)
            staged_sidecar = staging_dir / sidecar.name
            staged_sidecar.write_text(f"{digest}  {dest.name}\n", encoding="ascii")
            staged.append((archive_path, staged_sidecar, dest, sidecar, digest))
        for archive_path, staged_sidecar, dest, sidecar, digest in staged:
            if file_digest(archive_path) != digest:
                raise ValueError("fork archive changed before publication")
            os.replace(archive_path, dest)
            os.replace(staged_sidecar, sidecar)
    return destinations


def verify_fork_archive(archive_path: Path, manifest_bytes: bytes) -> str:
    """Validate archive entries and return the SHA-256 of those exact archive bytes."""
    manifest = json.loads(manifest_bytes)
    expected_files = manifest["files"]
    expected_directories = manifest["directories"]
    target = TARGET_SPECS[manifest["target"]]
    before = file_digest(archive_path)
    archive_format = archive_format_for_path(archive_path)
    if archive_format == "zip":
        with zipfile.ZipFile(archive_path) as archive:
            entries = (
                (
                    info.filename,
                    info.is_dir(),
                    stat.S_IFMT(info.external_attr >> 16),
                    stat.S_IMODE(info.external_attr >> 16),
                    lambda info=info: archive.open(info),
                )
                for info in archive.infolist()
            )
            check_entries(
                entries,
                expected_files,
                expected_directories,
                manifest_bytes,
                target.is_windows,
            )
    else:
        tar_path = archive_path
        with ExitStack() as stack:
            if archive_format == "tar.zst":
                temporary = stack.enter_context(
                    tempfile.TemporaryDirectory(prefix="fork-tar-")
                )
                tar_path = Path(temporary) / "archive.tar"
                with tar_path.open("wb") as out:
                    subprocess.run(
                        [*resolve_zstd_command(), "-d", "-q", "-c", str(archive_path)],
                        stdout=out,
                        check=True,
                    )
            archive = stack.enter_context(tarfile.open(tar_path, "r:*"))
            entries = (
                (
                    member.name,
                    member.isdir(),
                    stat.S_IFDIR
                    if member.isdir()
                    else stat.S_IFREG
                    if member.isfile()
                    else -1,
                    member.mode,
                    lambda member=member: archive.extractfile(member),
                )
                for member in archive.getmembers()
            )
            check_entries(
                entries,
                expected_files,
                expected_directories,
                manifest_bytes,
                target.is_windows,
            )
    after = file_digest(archive_path)
    if after != before:
        raise ValueError("fork archive changed during verification")
    return after


def check_entries(
    entries, expected_files, expected_directories, manifest_bytes, windows_target
):
    found_files = set()
    found_directories = set()
    for raw_name, is_directory, kind, mode, open_entry in entries:
        name = raw_name[:-1] if is_directory and raw_name.endswith("/") else raw_name
        if name != MANIFEST_NAME and not safe_name(name):
            raise ValueError(f"unsafe fork archive entry: {raw_name}")
        if is_directory:
            if (
                kind != stat.S_IFDIR and not (windows_target and kind == 0)
            ) or name in found_directories:
                raise ValueError(
                    f"non-directory or duplicate fork archive entry: {raw_name}"
                )
            found_directories.add(name)
            claim = expected_directories.get(name)
            if claim is None and not windows_target:
                raise ValueError(f"unexpected fork archive directory: {name}")
            check_archive_mode(name, mode, claim, windows_target)
            continue
        if (
            (kind != stat.S_IFREG and not (windows_target and kind == 0))
            or name in found_files
            or name in found_directories
        ):
            raise ValueError(
                f"link, special, or duplicate fork archive entry: {raw_name}"
            )
        found_files.add(name)
        if name == MANIFEST_NAME:
            expected_digest = hashlib.sha256(manifest_bytes).hexdigest()
            expected_mode = None
        else:
            claim = expected_files.get(name)
            if claim is None:
                raise ValueError(f"unexpected fork archive file: {name}")
            expected_digest = claim["sha256"].removeprefix("sha256:")
            expected_mode = claim["unixMode"]
        if not windows_target and name != MANIFEST_NAME:
            check_archive_mode(name, mode, expected_mode, windows_target)
        stream = open_entry()
        if stream is None:
            raise ValueError(f"unreadable fork archive file: {name}")
        with stream:
            digest = stream_digest(stream)
        if digest != expected_digest:
            raise ValueError(f"fork archive byte mismatch: {name}")
    if found_files != set(expected_files) | {MANIFEST_NAME}:
        raise ValueError("fork archive file set differs from manifest")
    if found_directories != set(expected_directories):
        raise ValueError("fork archive directory set differs from manifest")


def check_archive_mode(
    name: str, mode: int, claim: str | None, windows_target: bool
) -> None:
    if windows_target:
        return
    if format(stat.S_IMODE(mode), "04o") != claim:
        raise ValueError(f"fork archive Unix mode differs: {name}")


def file_digest(path: Path) -> str:
    with path.open("rb") as stream:
        return stream_digest(stream)


def stream_digest(stream) -> str:
    digest = hashlib.sha256()
    for chunk in iter(lambda: stream.read(1024 * 1024), b""):
        digest.update(chunk)
    return digest.hexdigest()
