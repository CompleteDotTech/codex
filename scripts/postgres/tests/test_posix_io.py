"""Real namespace changes must not redirect access or admit a second operation."""

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import state_io
import posix_io


@unittest.skipUnless(os.name == "posix", "POSIX descriptor-relative access")
class PosixIoTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.home = self.root / "selected"
        self.outside = self.root / "outside"
        self.home.mkdir(mode=0o700)
        self.outside.mkdir(mode=0o700)
        self.addCleanup(posix_io.guard_path(self.home).unlink, missing_ok=True)

    def test_symlink_inserted_after_lstat_is_rejected(self):
        original = Path.lstat
        moved = self.root / "moved"

        def swap(path, *args, **kwargs):
            result = original(path, *args, **kwargs)
            if path == self.home and not moved.exists():
                path.rename(moved)
                path.symlink_to(self.outside, target_is_directory=True)
            return result

        with patch.object(Path, "lstat", swap):
            with self.assertRaisesRegex(state_io.ServiceError, "symlink_state_path"):
                state_io.state_path(str(self.home))
        self.assertEqual(list(self.outside.iterdir()), [])

    def test_parent_swap_after_open_does_not_redirect_exclusive_write(self):
        original = os.open
        moved = self.root / "moved"

        def swap(path, flags, *args, **kwargs):
            descriptor = original(path, flags, *args, **kwargs)
            if path == self.home.name and flags == posix_io.DIRECTORY_FLAGS:
                self.home.rename(moved)
                self.home.symlink_to(self.outside, target_is_directory=True)
            return descriptor

        with patch.object(posix_io.os, "open", swap):
            state_io.write_new(self.home / "credential", b"protected")
        self.assertEqual((moved / "credential").read_bytes(), b"protected")
        self.assertEqual(list(self.outside.iterdir()), [])

    def test_moved_and_recreated_state_keeps_original_guard(self):
        child = """
import sys
from pathlib import Path
from state_io import ServiceError, operation_lock
try:
    with operation_lock(Path(sys.argv[1])):
        raise SystemExit(1)
except ServiceError as exc:
    raise SystemExit(0 if str(exc).startswith('operation_locked') else 2)
"""
        with self.assertRaisesRegex(state_io.ServiceError, "lock_ownership_changed"):
            with state_io.operation_lock(self.home):
                self.home.rename(self.root / "moved")
                self.home.mkdir(mode=0o700)
                result = subprocess.run(
                    [sys.executable, "-c", child, str(self.home)],
                    env=dict(
                        os.environ, PYTHONPATH=str(Path(state_io.__file__).parent)
                    ),
                    capture_output=True,
                    timeout=10,
                )
                self.assertEqual((result.returncode, result.stderr), (0, b""))
        with state_io.operation_lock(self.home):
            self.assertTrue((self.home / ".operation.lock").exists())

    def test_writable_nonsticky_ancestor_is_rejected(self):
        self.home.chmod(0o777)
        with self.assertRaisesRegex(state_io.ServiceError, "unsafe_state_ancestor"):
            state_io.write_new(self.home / "credential", b"protected")
        self.assertEqual(list(self.home.iterdir()), [])
