#!/usr/bin/env python3

import errno
import json
import os
from pathlib import Path
import stat
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from codex_package.archive import write_archive
from codex_package.fork_archive import verify_fork_archive
from codex_package.fork_archive_publication import publish_verified_fork_archive_linux
from codex_package.fork_archive_publication import verify_publication_receipt
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


@unittest.skipUnless(sys.platform == "linux", "publication requires Linux renameat2")
class ForkArchivePublicationTest(unittest.TestCase):
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
        self.source = self.root / "source.zip"
        write_archive(self.package, self.source, force=False)
        self.parent = self.root / "output"
        self.parent.mkdir()
        self.directory_fd = os.open(self.parent, os.O_RDONLY | os.O_DIRECTORY)
        self.addCleanup(os.close, self.directory_fd)
        self.name = "candidate.zip"

    def publish(self):
        return publish_verified_fork_archive_linux(
            self.package, self.source, self.directory_fd, self.name
        )

    def test_publishes_verified_bytes_with_pinned_receipt(self) -> None:
        receipt = self.publish()
        self.assertEqual(receipt.sha256, verify_fork_archive(self.package, self.source))
        self.assertEqual(
            (self.parent / self.name).read_bytes(), self.source.read_bytes()
        )
        self.assertEqual(list(self.parent.glob(".codex-fork-stage-*")), [])
        verify_publication_receipt(self.directory_fd, receipt)

    def test_existing_and_late_collision_never_replace(self) -> None:
        destination = self.parent / self.name
        destination.write_bytes(b"existing")
        with self.assertRaises(FileExistsError):
            self.publish()
        self.assertEqual(destination.read_bytes(), b"existing")
        destination.unlink()

        from codex_package import fork_archive_publication

        real_rename = fork_archive_publication.rename_noreplace

        def create_competitor(*args):
            destination.write_bytes(b"late competitor")
            return real_rename(*args)

        with patch.object(
            fork_archive_publication, "rename_noreplace", side_effect=create_competitor
        ):
            with self.assertRaises(FileExistsError):
                self.publish()
        self.assertEqual(destination.read_bytes(), b"late competitor")

    def test_parent_path_swap_does_not_redirect_pinned_publication(self) -> None:
        from codex_package import fork_archive_publication

        real_rename = fork_archive_publication.rename_noreplace
        moved = self.root / "moved-output"
        hijack = self.root / "hijack"
        hijack.mkdir()

        def swap_parent(*args):
            self.parent.rename(moved)
            self.parent.symlink_to(hijack, target_is_directory=True)
            return real_rename(*args)

        with patch.object(
            fork_archive_publication, "rename_noreplace", side_effect=swap_parent
        ):
            receipt = self.publish()
        self.assertFalse((hijack / self.name).exists())
        self.assertEqual((moved / self.name).read_bytes(), self.source.read_bytes())
        verify_publication_receipt(self.directory_fd, receipt)

    def test_rejects_destination_inside_sealed_package(self) -> None:
        inside_fd = os.open(self.package / "bin", os.O_RDONLY | os.O_DIRECTORY)
        try:
            with self.assertRaisesRegex(ValueError, "inside the sealed package"):
                publish_verified_fork_archive_linux(
                    self.package, self.source, inside_fd, self.name
                )
        finally:
            os.close(inside_fd)
        self.assertFalse((self.package / "bin" / self.name).exists())

    def test_receipt_detects_late_replacement(self) -> None:
        receipt = self.publish()
        destination = self.parent / self.name
        destination.unlink()
        destination.write_bytes(b"replacement")
        with self.assertRaisesRegex(ValueError, "published (entry|bytes) changed"):
            verify_publication_receipt(self.directory_fd, receipt)

    def test_failed_sync_leaves_ambiguous_name_for_reconciliation(self) -> None:
        from codex_package import fork_archive_publication

        real_fsync = os.fsync

        def fail_directory_sync(fd):
            if stat.S_ISDIR(os.fstat(fd).st_mode):
                raise OSError(errno.EIO, "simulated directory sync failure")
            return real_fsync(fd)

        with patch.object(
            fork_archive_publication.os, "fsync", side_effect=fail_directory_sync
        ):
            with self.assertRaisesRegex(OSError, "simulated"):
                self.publish()
        self.assertEqual(
            (self.parent / self.name).read_bytes(), self.source.read_bytes()
        )

    def test_rejects_source_changed_after_verification(self) -> None:
        from codex_package import fork_archive_publication

        real_verify = verify_fork_archive

        def mutate_after_verification(package, archive):
            digest = real_verify(package, archive)
            archive.write_bytes(b"mutated")
            return digest

        with patch.object(
            fork_archive_publication,
            "verify_fork_archive",
            side_effect=mutate_after_verification,
        ):
            with self.assertRaisesRegex(ValueError, "changed after"):
                self.publish()
        self.assertFalse((self.parent / self.name).exists())

    def test_rejects_shared_directory_and_invalid_name(self) -> None:
        self.parent.chmod(0o777)
        with self.assertRaisesRegex(ValueError, "owner-private"):
            self.publish()
        self.parent.chmod(0o755)
        with self.assertRaisesRegex(ValueError, "single filename"):
            publish_verified_fork_archive_linux(
                self.package, self.source, self.directory_fd, "../candidate.zip"
            )


class UnsupportedPlatformTest(unittest.TestCase):
    @unittest.skipIf(sys.platform == "linux", "Linux-only fail-closed check")
    def test_non_linux_fails_closed_before_accessing_inputs(self) -> None:
        with self.assertRaisesRegex(NotImplementedError, "Linux dir_fd"):
            publish_verified_fork_archive_linux(
                Path("missing"), Path("missing.zip"), -1, "candidate.zip"
            )


if __name__ == "__main__":
    unittest.main()
