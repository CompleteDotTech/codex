"""Verify descriptor ownership, real chown, and fail-closed root durability."""

import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import test_state
import state
import posix_io


@unittest.skipUnless(os.name == "posix", "POSIX directory durability")
class StateRootDurabilityTests(unittest.TestCase):
    def test_parent_sync_failure_stops_before_creating_credentials(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "state"
            with patch("state.sync_directory", side_effect=OSError("disk failure")):
                with self.assertRaises(OSError):
                    state.initialize(
                        path, "codex-pg-sync", "postgres:17.11-bookworm", 55432, []
                    )
            self.assertEqual(list(path.iterdir()), [])


@unittest.skipUnless(
    os.name == "posix" and os.geteuid() == 0, "requires root for real chown"
)
class PosixOwnerTests(unittest.TestCase):
    setUpClass = classmethod(test_state.StateTests.setUpClass.__func__)
    tearDownClass = classmethod(test_state.StateTests.tearDownClass.__func__)
    setUp = test_state.StateTests.setUp

    def test_untrusted_owners_are_rejected_without_repair(self):
        for path in (
            self.home,
            self.home / "secrets",
            self.home / "backups",
            self.home / "receipt.json",
            self.home / "secrets/runtime.password",
        ):
            with self.subTest(path=path.name):
                os.chown(path, 65534, -1)
                try:
                    with self.assertRaisesRegex(
                        state.ServiceError, "insecure|unsafe_state_ancestor"
                    ):
                        state.load(self.home)
                    self.assertEqual(path.lstat().st_uid, 65534)
                finally:
                    os.chown(path, 0, -1)

    def test_owner_is_checked_on_the_file_actually_opened(self):
        target = self.home / "secrets/runtime.password"
        content = target.read_bytes()
        original_open = os.open

        def replace_before_open(path, flags, *args, **kwargs):
            if path == target.name:
                target.unlink()
                target.write_bytes(content)
                target.chmod(0o600)
                os.chown(target, 65534, -1)
            return original_open(path, flags, *args, **kwargs)

        with patch.object(posix_io.os, "open", side_effect=replace_before_open):
            with self.assertRaisesRegex(state.ServiceError, "insecure_secret_file"):
                state.load(self.home)


@unittest.skipUnless(os.name == "posix", "requires POSIX descriptors")
class PosixDescriptorOwnerTests(unittest.TestCase):
    setUpClass = classmethod(test_state.StateTests.setUpClass.__func__)
    tearDownClass = classmethod(test_state.StateTests.tearDownClass.__func__)
    setUp = test_state.StateTests.setUp

    def test_untrusted_descriptor_owners_are_rejected_without_repair(self):
        original_fstat = os.fstat
        for path in (
            self.home,
            self.home / "secrets",
            self.home / "backups",
            self.home / "receipt.json",
            self.home / "secrets/runtime.password",
        ):
            expected = path.stat()

            def untrusted_metadata(descriptor):
                metadata = original_fstat(descriptor)
                if (metadata.st_dev, metadata.st_ino) == (
                    expected.st_dev,
                    expected.st_ino,
                ):
                    fields = list(metadata)
                    fields[4] = os.geteuid() + 100000
                    return os.stat_result(fields)
                return metadata

            with self.subTest(path=path.name):
                with patch.object(posix_io.os, "fstat", side_effect=untrusted_metadata):
                    with self.assertRaisesRegex(
                        state.ServiceError, "insecure|unsafe_state_ancestor"
                    ):
                        state.load(self.home)
                self.assertEqual(path.stat().st_uid, expected.st_uid)

    def test_owner_check_uses_replacement_descriptor_metadata(self):
        target = self.home / "secrets/runtime.password"
        replacement = target.with_name("replacement.password")
        replacement.write_bytes(target.read_bytes())
        replacement.chmod(0o600)
        expected = replacement.stat()
        original_open, original_fstat = os.open, os.fstat

        def replace_before_open(path, flags, *args, **kwargs):
            if path == target.name:
                replacement.replace(target)
            return original_open(path, flags, *args, **kwargs)

        def untrusted_metadata(descriptor):
            metadata = original_fstat(descriptor)
            if (metadata.st_dev, metadata.st_ino) == (
                expected.st_dev,
                expected.st_ino,
            ):
                fields = list(metadata)
                fields[4] = os.geteuid() + 100000
                return os.stat_result(fields)
            return metadata

        with (
            patch.object(posix_io.os, "open", side_effect=replace_before_open),
            patch.object(posix_io.os, "fstat", side_effect=untrusted_metadata),
        ):
            with self.assertRaisesRegex(state.ServiceError, "insecure_secret_file"):
                state.load(self.home)
