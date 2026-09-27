"""Subprocess CLI redaction checks; no Docker or database mocks counted as integration."""

import contextlib
import io
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import manage
import test_state


class ManageTests(unittest.TestCase):
    setUpClass = classmethod(test_state.StateTests.setUpClass.__func__)
    tearDownClass = classmethod(test_state.StateTests.tearDownClass.__func__)
    setUp = test_state.StateTests.setUp

    def test_cli_argument_errors_do_not_echo_secret(self):
        result = subprocess.run(
            [
                sys.executable,
                str(Path(manage.__file__)),
                "--state",
                str(self.home),
                "--password",
                "DO_NOT_ECHO_SECRET",
            ],
            capture_output=True,
        )
        self.assertEqual(result.returncode, 2)
        self.assertNotIn(b"DO_NOT_ECHO_SECRET", result.stdout + result.stderr)

    def test_cli_state_errors_do_not_echo_payloads(self):
        (self.home / "receipt.json").write_text("PRIVATE_RECEIPT_CONTENT")
        output, error = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(output), contextlib.redirect_stderr(error):
            self.assertEqual(manage.main(["--state", str(self.home), "status"]), 2)
        self.assertNotIn("PRIVATE_RECEIPT_CONTENT", error.getvalue())

    def test_pin_reads_receipt_after_acquiring_operation_lock(self):
        initial = {"image_digest": None}
        pinned = {"image_digest": "sha256:established"}
        current = initial

        @contextlib.contextmanager
        def acquire_lock(path):
            nonlocal current
            # Another command finishes pinning before this command acquires the lock.
            current = pinned
            yield

        output = io.StringIO()
        with (
            patch.object(manage, "operation_lock", acquire_lock),
            patch.object(manage, "load", side_effect=lambda path: current),
            patch.object(manage, "pin", return_value=pinned) as pin,
            contextlib.redirect_stdout(output),
        ):
            self.assertEqual(manage.main(["--state", str(self.home), "pin"]), 0)
        pin.assert_called_once_with(self.home, pinned)
