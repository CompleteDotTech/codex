"""Verify ownership rejection with real chown and fail-closed root durability."""

import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import test_state
import state
import posix_state


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
                    with self.assertRaisesRegex(state.ServiceError, "insecure"):
                        state.load(self.home)
                    self.assertEqual(path.lstat().st_uid, 65534)
                finally:
                    os.chown(path, 0, -1)

    def test_owner_is_checked_on_the_file_actually_opened(self):
        target = self.home / "secrets/runtime.password"
        content = target.read_bytes()
        original_open = os.open

        def replace_before_open(path, flags, *args, **kwargs):
            if Path(path) == target:
                target.unlink()
                target.write_bytes(content)
                target.chmod(0o600)
                os.chown(target, 65534, -1)
            return original_open(path, flags, *args, **kwargs)

        with patch.object(posix_state.os, "open", side_effect=replace_before_open):
            with self.assertRaisesRegex(state.ServiceError, "insecure_secret_file"):
                state.load(self.home)
