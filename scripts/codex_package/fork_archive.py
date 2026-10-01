"""Read-only verification of serialized fork package candidates."""

import hashlib
import json
import stat
import subprocess
import tarfile
import tempfile
import zipfile
from contextlib import ExitStack
from pathlib import Path

from .archive import archive_format_for_path
from .archive import resolve_zstd_command
from .fork_identity import MANIFEST_NAME
from .fork_identity import open_regular_file
from .fork_identity import read_regular_file
from .fork_identity import safe_name
from .fork_identity import verify_fork_package
from .targets import TARGET_SPECS


MAX_DECOMPRESSED_TAR_BYTES = 4 * 1024 * 1024 * 1024


def verify_fork_archive(
    package_dir: Path, archive_path: Path, *, pinned_root_fd: int | None = None
) -> str:
    """Check a sealed directory and archive, returning the archive's SHA-256."""
    verification = verify_fork_package(package_dir, pinned_root_fd=pinned_root_fd)
    manifest_bytes = read_regular_file(package_dir / MANIFEST_NAME, 4 * 1024 * 1024)
    if json.loads(manifest_bytes) != verification.manifest:
        raise ValueError("fork manifest changed after directory verification")
    manifest = verification.manifest
    expected_files = manifest["files"]
    expected_directories = manifest["directories"]
    target = TARGET_SPECS[manifest["target"]]
    archive_format = archive_format_for_path(archive_path)
    with open_regular_file(archive_path) as archive_stream:
        before = stream_digest(archive_stream)
        archive_stream.seek(0)
        check_archive_stream(
            archive_stream,
            archive_format,
            expected_files,
            expected_directories,
            manifest_bytes,
            target.is_windows,
        )
        archive_stream.seek(0)
        after = stream_digest(archive_stream)
        if after != before:
            raise ValueError("fork archive changed during verification")
        return after


def check_archive_stream(
    archive_stream,
    archive_format,
    expected_files,
    expected_directories,
    manifest_bytes,
    windows_target,
) -> None:
    if archive_format == "zip":
        with zipfile.ZipFile(archive_stream) as archive:
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
                windows_target,
            )
    else:
        with ExitStack() as stack:
            tar_stream = archive_stream
            if archive_format == "tar.zst":
                temporary = stack.enter_context(
                    tempfile.TemporaryDirectory(prefix="fork-tar-")
                )
                tar_path = Path(temporary) / "archive.tar"
                with tar_path.open("wb") as out:
                    with subprocess.Popen(
                        [*resolve_zstd_command(), "-d", "-q", "-c"],
                        stdin=archive_stream,
                        stdout=subprocess.PIPE,
                    ) as process:
                        assert process.stdout is not None
                        written = 0
                        while chunk := process.stdout.read(
                            min(1024 * 1024, MAX_DECOMPRESSED_TAR_BYTES - written + 1)
                        ):
                            written += len(chunk)
                            if written > MAX_DECOMPRESSED_TAR_BYTES:
                                process.kill()
                                raise ValueError(
                                    "fork archive decompressed size exceeds limit"
                                )
                            out.write(chunk)
                        if process.wait() != 0:
                            raise subprocess.CalledProcessError(
                                process.returncode, process.args
                            )
                tar_stream = stack.enter_context(tar_path.open("rb"))
            archive = stack.enter_context(tarfile.open(fileobj=tar_stream, mode="r:*"))
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
                windows_target,
            )


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
    with open_regular_file(path) as stream:
        return stream_digest(stream)


def stream_digest(stream) -> str:
    digest = hashlib.sha256()
    for chunk in iter(lambda: stream.read(1024 * 1024), b""):
        digest.update(chunk)
    return digest.hexdigest()
