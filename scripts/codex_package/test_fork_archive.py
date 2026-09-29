#!/usr/bin/env python3

import json
import os
from pathlib import Path
import stat
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from codex_package.archive import write_archive
from codex_package.archive import resolve_zstd_command
from codex_package.fork_archive import publish_verified_fork_archives
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
        self.manifest_bytes = (self.package / MANIFEST_NAME).read_bytes()

    def test_publishes_verified_zip_and_tar_with_checksums(self) -> None:
        outputs = [self.root / "candidate.zip", self.root / "candidate.tar.gz"]
        self.assertEqual(
            publish_verified_fork_archives(self.package, outputs, force=False), outputs
        )
        for output in outputs:
            digest = verify_fork_archive(output, self.manifest_bytes)
            self.assertEqual(
                output.with_name(output.name + ".sha256").read_text(),
                f"{digest}  {output.name}\n",
            )

    def test_publishes_verified_zstd_tar_when_available(self) -> None:
        try:
            resolve_zstd_command()
        except RuntimeError:
            self.skipTest("zstd and DotSlash are unavailable")
        output = self.root / "candidate.tar.zst"
        publish_verified_fork_archives(self.package, [output], force=False)
        digest = verify_fork_archive(output, self.manifest_bytes)
        self.assertEqual(
            output.with_name(output.name + ".sha256").read_text(),
            f"{digest}  {output.name}\n",
        )

    def test_rejects_directory_added_between_check_and_archive(self) -> None:
        output = self.root / "candidate.tar.gz"
        original_write = write_archive

        def add_directory_then_write(package, archive, *, force):
            (package / "codex-resources/extra").mkdir()
            original_write(package, archive, force=force)

        with patch(
            "codex_package.fork_archive.write_archive",
            side_effect=add_directory_then_write,
        ):
            with self.assertRaisesRegex(ValueError, "directory set differs"):
                publish_verified_fork_archives(self.package, [output], force=False)
        self.assertFalse(output.exists())

    def test_rejects_source_mutation_between_directory_check_and_archive(self) -> None:
        output = self.root / "candidate.zip"
        original_write = write_archive

        def mutate_then_write(package, archive, *, force):
            (package / "bin/codex.exe").write_bytes(b"mutated")
            original_write(package, archive, force=force)

        with patch(
            "codex_package.fork_archive.write_archive", side_effect=mutate_then_write
        ):
            with self.assertRaisesRegex(ValueError, "byte mismatch"):
                publish_verified_fork_archives(self.package, [output], force=False)
        self.assertFalse(output.exists())
        self.assertFalse(output.with_name(output.name + ".sha256").exists())

    def test_later_archive_failure_publishes_nothing(self) -> None:
        outputs = [self.root / "candidate.zip", self.root / "candidate.tar.gz"]
        original_write = write_archive
        calls = 0

        def mutate_on_second(package, archive, *, force):
            nonlocal calls
            calls += 1
            if calls == 2:
                (package / "bin/codex.exe").write_bytes(b"mutated")
            original_write(package, archive, force=force)

        with patch(
            "codex_package.fork_archive.write_archive", side_effect=mutate_on_second
        ):
            with self.assertRaisesRegex(ValueError, "byte mismatch"):
                publish_verified_fork_archives(self.package, outputs, force=False)
        for output in outputs:
            self.assertFalse(output.exists())
            self.assertFalse(output.with_name(output.name + ".sha256").exists())

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
                    verify_fork_archive(output, self.manifest_bytes)

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
            verify_fork_archive(output, self.manifest_bytes)

    @unittest.skipIf(os.name == "nt", "Unix mode archive check requires Unix")
    def test_rejects_serialized_unix_mode_change(self) -> None:
        manifest = json.loads(self.manifest_bytes)
        manifest["target"] = "x86_64-unknown-linux-gnu"
        for name in manifest["directories"]:
            manifest["directories"][name] = format(
                stat.S_IMODE((self.package / name).stat().st_mode), "04o"
            )
        for name in manifest["files"]:
            manifest["files"][name]["unixMode"] = format(
                stat.S_IMODE((self.package / name).stat().st_mode), "04o"
            )
        # A conflicting Unix claim exercises serialized mode validation.
        manifest["files"]["bin/codex.exe"]["unixMode"] = "0644"
        output = self.root / "candidate.tar.gz"
        write_archive(self.package, output, force=False)
        with self.assertRaisesRegex(ValueError, "Unix mode differs"):
            verify_fork_archive(output, json.dumps(manifest).encode())


if __name__ == "__main__":
    unittest.main()
