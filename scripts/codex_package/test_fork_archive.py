#!/usr/bin/env python3

import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tarfile
import tempfile
import unittest
import zipfile
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from codex_package.archive import write_archive
from codex_package.archive import resolve_zstd_command
from codex_package.fork_archive import verify_fork_archive
from codex_package.fork_identity import MANIFEST_NAME
from codex_package.fork_identity import OWNER
from codex_package.fork_identity import seal_fork_package


IDENTITY = {
    "owner": OWNER,
    "declaredBaseCommit": "a" * 40,
    "forkCommit": "b" * 40,
    "patchsetSha256": "sha256:" + "c" * 64,
    "channel": "preview",
    "storageCapabilities": ["sqlite"],
    "postgresSchemaVersions": [],
}


class ForkArchiveTest(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.package = self.root / "package"
        self.package.mkdir()
        metadata = {
            "layoutVersion": 1,
            "version": "1.2.3",
            "target": "x86_64-pc-windows-msvc",
            "variant": "codex",
            "entrypoint": "bin/codex.exe",
            "resourcesDir": "codex-resources",
            "pathDir": "codex-path",
        }
        (self.package / "codex-package.json").write_text(json.dumps(metadata))
        for name in (
            "bin/codex.exe",
            "bin/codex-code-mode-host.exe",
            "codex-path/rg.exe",
            "codex-resources/codex-command-runner.exe",
            "codex-resources/codex-windows-sandbox-setup.exe",
        ):
            path = self.package / name
            path.parent.mkdir(exist_ok=True)
            path.write_bytes(name.encode())
            path.chmod(0o755)
        seal_fork_package(self.package, IDENTITY)

    def test_verifies_existing_zip_and_tar_without_writing_sidecars(self) -> None:
        outputs = [self.root / "candidate.zip", self.root / "candidate.tar.gz"]
        for output in outputs:
            write_archive(self.package, output, force=False)
            self.assertEqual(len(verify_fork_archive(self.package, output)), 64)
            self.assertFalse(output.with_name(output.name + ".sha256").exists())

    @unittest.skipUnless(hasattr(os, "mkfifo"), "POSIX FIFO required")
    def test_rejects_fifo_manifest_and_archive_without_blocking(self) -> None:
        output = self.root / "candidate.zip"
        write_archive(self.package, output, force=False)
        manifest = self.package / MANIFEST_NAME
        manifest.unlink()
        os.mkfifo(manifest)
        with self.assertRaisesRegex(ValueError, "regular fork package manifest"):
            verify_fork_archive(self.package, output)
        manifest.unlink()
        seal_fork_package(self.package, IDENTITY)
        output.unlink()
        os.mkfifo(output)
        with self.assertRaisesRegex(ValueError, "not regular"):
            verify_fork_archive(self.package, output)

    @unittest.skipUnless(hasattr(os, "mkfifo"), "POSIX FIFO required")
    def test_archive_path_swap_after_open_keeps_verified_descriptor(self) -> None:
        output = self.root / "candidate.zip"
        moved = self.root / "candidate-original.zip"
        write_archive(self.package, output, force=False)
        from codex_package import fork_archive

        real_open = fork_archive.open_regular_file

        def swap_after_open(path):
            stream = real_open(path)
            if path == output:
                output.rename(moved)
                os.mkfifo(output)
            return stream

        with patch.object(
            fork_archive, "open_regular_file", side_effect=swap_after_open
        ):
            self.assertEqual(len(verify_fork_archive(self.package, output)), 64)

    def test_read_only_cli_reports_archive_digest_from_any_cwd(self) -> None:
        output = self.root / "candidate.zip"
        write_archive(self.package, output, force=False)
        script = Path(__file__).resolve().parents[1] / "verify_fork_archive.py"
        environment = os.environ.copy()
        environment.pop("CODEX_REPO_ROOT", None)
        result = subprocess.run(
            [sys.executable, str(script), str(self.package), str(output)],
            cwd=self.root,
            env=environment,
            text=True,
            capture_output=True,
            check=True,
        )
        self.assertEqual(
            result.stdout.strip(),
            f"sha256:{verify_fork_archive(self.package, output)}  {output}",
        )
        self.assertFalse(output.with_name(output.name + ".sha256").exists())

    def test_verifies_existing_zstd_tar_when_available(self) -> None:
        try:
            resolve_zstd_command()
        except RuntimeError:
            self.skipTest("zstd and DotSlash are unavailable")
        output = self.root / "candidate.tar.zst"
        write_archive(self.package, output, force=False)
        self.assertEqual(len(verify_fork_archive(self.package, output)), 64)

    def test_rejects_zstd_tar_expansion_over_limit(self) -> None:
        try:
            resolve_zstd_command()
        except RuntimeError:
            self.skipTest("zstd and DotSlash are unavailable")
        output = self.root / "candidate.tar.zst"
        write_archive(self.package, output, force=False)
        from codex_package import fork_archive

        with patch.object(fork_archive, "MAX_DECOMPRESSED_TAR_BYTES", 512):
            with self.assertRaisesRegex(ValueError, "decompressed size exceeds limit"):
                verify_fork_archive(self.package, output)

    def test_rejects_serialized_directory_added_after_candidate_check(self) -> None:
        output = self.root / "candidate.tar.gz"
        extra = self.package / "codex-resources/extra"
        extra.mkdir()
        write_archive(self.package, output, force=False)
        extra.rmdir()
        with self.assertRaisesRegex(ValueError, "directory set differs"):
            verify_fork_archive(self.package, output)

    def test_rejects_serialized_bytes_changed_after_candidate_check(self) -> None:
        output = self.root / "candidate.zip"
        runtime = self.package / "bin/codex.exe"
        original = runtime.read_bytes()
        runtime.write_bytes(b"mutated")
        write_archive(self.package, output, force=False)
        runtime.write_bytes(original)
        with self.assertRaisesRegex(ValueError, "byte mismatch"):
            verify_fork_archive(self.package, output)

    def test_rejects_zip_traversal_symlink_and_duplicate(self) -> None:
        for name, mode in (
            ("../escape", stat.S_IFREG | 0o644),
            ("bin/link.exe", stat.S_IFLNK | 0o777),
            ("bin/codex.exe", stat.S_IFREG | 0o644),
        ):
            with self.subTest(name=name):
                output = self.root / "candidate.zip"
                write_archive(self.package, output, force=True)
                info = zipfile.ZipInfo(name)
                info.create_system = 3
                info.external_attr = mode << 16
                with zipfile.ZipFile(output, "a") as archive:
                    archive.writestr(info, b"extra")
                with self.assertRaisesRegex(ValueError, "unsafe|link|duplicate"):
                    verify_fork_archive(self.package, output)

    def test_rejects_tar_hardlink(self) -> None:
        output = self.root / "candidate.tar.gz"
        with tarfile.open(output, "w:gz") as archive:
            for path in sorted(self.package.rglob("*")):
                archive.add(
                    path, arcname=path.relative_to(self.package), recursive=False
                )
            link = tarfile.TarInfo("bin/hardlink.exe")
            link.type = tarfile.LNKTYPE
            link.linkname = "bin/codex.exe"
            archive.addfile(link)
        with self.assertRaisesRegex(ValueError, "link, special"):
            verify_fork_archive(self.package, output)

    @unittest.skipIf(os.name == "nt", "Unix mode archive check requires Unix")
    def test_rejects_serialized_unix_mode_change(self) -> None:
        (self.package / MANIFEST_NAME).unlink()
        for name in (
            "bin/codex.exe",
            "bin/codex-code-mode-host.exe",
            "codex-path/rg.exe",
            "codex-resources/codex-command-runner.exe",
            "codex-resources/codex-windows-sandbox-setup.exe",
        ):
            (self.package / name).unlink()
        metadata = json.loads((self.package / "codex-package.json").read_text())
        metadata["target"] = "x86_64-unknown-linux-gnu"
        metadata["entrypoint"] = "bin/codex"
        (self.package / "codex-package.json").write_text(json.dumps(metadata))
        for name in (
            "bin/codex",
            "bin/codex-code-mode-host",
            "codex-path/rg",
            "codex-resources/bwrap",
        ):
            path = self.package / name
            path.write_bytes(name.encode())
            path.chmod(0o755)
        seal_fork_package(self.package, IDENTITY)
        output = self.root / "candidate.tar.gz"
        runtime = self.package / "bin/codex"
        runtime.chmod(0o644)
        write_archive(self.package, output, force=False)
        runtime.chmod(0o755)
        with self.assertRaisesRegex(ValueError, "Unix mode differs"):
            verify_fork_archive(self.package, output)


if __name__ == "__main__":
    unittest.main()
