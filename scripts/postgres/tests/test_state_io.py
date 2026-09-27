"""Exercise durable files and lock ownership without credentials or Docker."""

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import state
import state_io


class StateIoTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name)

    def test_locked_operation_does_not_delete_other_owners_lock(self):
        marker = self.home / ".operation.lock"
        marker.write_text("another owner")
        with self.assertRaisesRegex(state.ServiceError, "operation_locked"):
            with state.operation_lock(self.home):
                self.fail("lock unexpectedly acquired")
        self.assertEqual(marker.read_text(), "another owner")

    def test_operation_lock_released_after_own_failure(self):
        with self.assertRaises(RuntimeError):
            with state.operation_lock(self.home):
                raise RuntimeError("operation interrupted")
        self.assertFalse((self.home / ".operation.lock").exists())

    def test_lock_owner_change_preserves_replacement(self):
        with self.assertRaisesRegex(state.ServiceError, "lock_ownership_changed"):
            with state.operation_lock(self.home):
                (self.home / ".operation.lock").write_text("replacement owner")
        self.assertEqual(
            (self.home / ".operation.lock").read_text(), "replacement owner"
        )

    def test_missing_active_lock_returns_fixed_diagnostic(self):
        for operation_fails in (False, True):
            with self.subTest(operation_fails=operation_fails):
                with self.assertRaisesRegex(
                    state.ServiceError, "^lock_ownership_changed_preserved$"
                ):
                    with state.operation_lock(self.home):
                        (self.home / ".operation.lock").unlink()
                        if operation_fails:
                            raise RuntimeError("operation failed")
                self.assertFalse((self.home / ".operation.lock").exists())

    def test_write_new_never_overwrites(self):
        file = self.home / "protected"
        state.write_new(file, b"old")
        with self.assertRaises(FileExistsError):
            state.write_new(file, b"new")
        self.assertEqual(file.read_bytes(), b"old")

    def test_failure_before_receipt_replace_preserves_old(self):
        file = self.home / "receipt.json"
        state.publish_json(file, {"old": True})
        before = file.read_bytes()
        with patch("state_io.os.replace", side_effect=OSError("disk failure")):
            with self.assertRaises(OSError):
                state.publish_json(file, {"new": True})
        self.assertEqual(file.read_bytes(), before)

    def test_oversized_receipt_is_rejected_before_writing(self):
        file = self.home / "receipt.json"
        state.publish_json(file, {"old": True})
        before = {path.name: path.read_bytes() for path in self.home.iterdir()}
        with self.assertRaisesRegex(state.ServiceError, "^receipt_too_large$"):
            state.publish_json(file, {"large": "x" * state_io.MAX_RECEIPT_BYTES})
        self.assertEqual(
            before, {path.name: path.read_bytes() for path in self.home.iterdir()}
        )

    def test_state_inside_source_tree_is_refused(self):
        target = Path(state.__file__).parent / "live"
        with self.assertRaisesRegex(state.ServiceError, "outside_source"):
            state.state_path(str(target))

    def test_subprocess_errors_are_redacted(self):
        with patch("state_io.subprocess.run", side_effect=OSError("secret content")):
            with self.assertRaisesRegex(
                state.ServiceError, "^command_unavailable_or_timed_out$"
            ):
                state.run(["missing-program"])

    def test_discarded_native_output_does_not_require_utf8(self):
        result = subprocess.CompletedProcess(["native-command"], 0, b"path-\xe9", b"")
        with patch("state_io.subprocess.run", return_value=result):
            self.assertEqual(state.run(["native-command"], discard_output=True), "")
            with self.assertRaises(UnicodeDecodeError):
                state.run(["machine-readable-command"])

    @unittest.skipIf(os.name == "nt", "POSIX symlink fixture")
    def test_symlink_state_path_is_refused(self):
        target = self.home / "linked"
        target.symlink_to(self.home, target_is_directory=True)
        with self.assertRaisesRegex(state.ServiceError, "symlink_state_path"):
            state.state_path(str(target))
