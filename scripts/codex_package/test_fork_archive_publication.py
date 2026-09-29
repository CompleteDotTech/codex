#!/usr/bin/env python3

import errno
import json
import os
from pathlib import Path
import shutil
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
        cross_device_root = Path("/dev/shm")
        if (
            not cross_device_root.is_dir()
            or os.stat(cross_device_root).st_dev == os.stat(self.package).st_dev
        ):
            self.skipTest("distinct writable filesystem unavailable")
        output_temporary = tempfile.TemporaryDirectory(dir=cross_device_root)
        self.addCleanup(output_temporary.cleanup)
        self.parent = Path(output_temporary.name)
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
        moved = self.parent.with_name(self.parent.name + "-moved")
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
        self.parent.unlink()
        moved.rename(self.parent)

    def test_rejects_destination_inside_sealed_package(self) -> None:
        (self.package / "bin").chmod(0o700)
        inside_fd = os.open(self.package / "bin", os.O_RDONLY | os.O_DIRECTORY)
        try:
            with self.assertRaisesRegex(ValueError, "different filesystem"):
                publish_verified_fork_archive_linux(
                    self.package, self.source, inside_fd, self.name
                )
        finally:
            os.close(inside_fd)
        self.assertFalse((self.package / "bin" / self.name).exists())

    def test_rejects_same_filesystem_even_outside_package(self) -> None:
        sibling = self.root / "same-device-output"
        sibling.mkdir()
        sibling.chmod(0o700)
        same_fd = os.open(sibling, os.O_RDONLY | os.O_DIRECTORY)
        try:
            with self.assertRaisesRegex(ValueError, "different filesystem"):
                publish_verified_fork_archive_linux(
                    self.package, self.source, same_fd, self.name
                )
        finally:
            os.close(same_fd)
        self.assertFalse((sibling / self.name).exists())

    def test_cross_device_parent_cannot_move_into_package_before_publish(self) -> None:
        from codex_package import fork_archive_publication

        real_rename = fork_archive_publication.rename_noreplace
        attempted = False

        def try_move_into_package(*args):
            nonlocal attempted
            attempted = True
            with self.assertRaises(OSError) as context:
                self.parent.rename(self.package / "bin/output")
            self.assertEqual(context.exception.errno, errno.EXDEV)
            return real_rename(*args)

        with patch.object(
            fork_archive_publication,
            "rename_noreplace",
            side_effect=try_move_into_package,
        ):
            receipt = self.publish()
        self.assertTrue(attempted)
        verify_publication_receipt(self.directory_fd, receipt)

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

        def mutate_after_verification(package, archive, **kwargs):
            digest = real_verify(package, archive, **kwargs)
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
        for mode in (0o755, 0o777):
            with self.subTest(mode=mode):
                self.parent.chmod(mode)
                with self.assertRaisesRegex(ValueError, "owner-private"):
                    self.publish()
        self.parent.chmod(0o700)
        with self.assertRaisesRegex(ValueError, "single filename"):
            publish_verified_fork_archive_linux(
                self.package, self.source, self.directory_fd, "../candidate.zip"
            )

    def test_package_path_swap_cannot_verify_other_package(self) -> None:
        from codex_package import fork_archive_publication

        # B lives on the destination filesystem; A remains open through its fd.
        shutil.copytree(self.package, self.parent, dirs_exist_ok=True)
        self.parent.chmod(0o700)
        (self.parent / "codex-fork-package.json").unlink()
        (self.parent / "bin/codex.exe").write_bytes(b"alternate binary")
        seal_fork_package(self.parent, IDENTITY)
        alternate_archive = self.root / "alternate.zip"
        write_archive(self.parent, alternate_archive, force=False)
        original_package = self.package
        moved_package = self.root / "package-moved"
        real_verify = verify_fork_archive

        def swap_before_verify(package, archive, **kwargs):
            original_package.rename(moved_package)
            original_package.symlink_to(self.parent, target_is_directory=True)
            return real_verify(package, archive, **kwargs)

        try:
            with patch.object(
                fork_archive_publication,
                "verify_fork_archive",
                side_effect=swap_before_verify,
            ):
                with self.assertRaisesRegex(
                    ValueError, "byte mismatch|checksum differs"
                ):
                    publish_verified_fork_archive_linux(
                        original_package,
                        alternate_archive,
                        self.directory_fd,
                        self.name,
                    )
            self.assertFalse((self.parent / self.name).exists())
        finally:
            if original_package.is_symlink():
                original_package.unlink()
                moved_package.rename(original_package)


class UnsupportedPlatformTest(unittest.TestCase):
    @unittest.skipIf(sys.platform == "linux", "Linux-only fail-closed check")
    def test_non_linux_fails_closed_before_accessing_inputs(self) -> None:
        with self.assertRaisesRegex(NotImplementedError, "Linux dir_fd"):
            publish_verified_fork_archive_linux(
                Path("missing"), Path("missing.zip"), -1, "candidate.zip"
            )


if __name__ == "__main__":
    unittest.main()
