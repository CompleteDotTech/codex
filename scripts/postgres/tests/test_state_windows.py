"""Native NTFS permission and certificate persistence checks, without Docker."""

import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import state


@unittest.skipUnless(os.name == "nt", "requires native Windows ACLs")
class WindowsStateTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name) / "state"
        self.receipt = state.initialize(
            self.home, "codex-pg-windows", "postgres:17.11-bookworm", 55432, []
        )

    def test_native_initialization_and_repeat_preserve_complete_state(self):
        before = {
            path.relative_to(self.home): path.read_bytes()
            for path in self.home.rglob("*")
            if path.is_file()
        }
        self.assertEqual(state.load(self.home), self.receipt)
        self.assertEqual(
            state.initialize(
                self.home, "codex-pg-windows", "postgres:17.11-bookworm", 55432, []
            ),
            self.receipt,
        )
        self.assertEqual(
            before,
            {
                path.relative_to(self.home): path.read_bytes()
                for path in self.home.rglob("*")
                if path.is_file()
            },
        )

    def test_non_ascii_path_initialization_and_repeat_preserve_complete_state(self):
        home = Path(self.temp.name) / "\u00e9tat"
        receipt = state.initialize(
            home, "codex-pg-unicode", "postgres:17.11-bookworm", 55432, []
        )
        before = {
            path.relative_to(home): path.read_bytes()
            for path in home.rglob("*")
            if path.is_file()
        }
        self.assertEqual(state.load(home), receipt)
        self.assertEqual(
            state.initialize(
                home, "codex-pg-unicode", "postgres:17.11-bookworm", 55432, []
            ),
            receipt,
        )
        self.assertEqual(
            before,
            {
                path.relative_to(home): path.read_bytes()
                for path in home.rglob("*")
                if path.is_file()
            },
        )

    def test_load_checks_native_permissions_without_repair(self):
        target = self.home / "receipt.json"
        state.run(["icacls", str(target), "/grant", "*S-1-1-0:M"], discard_output=True)
        with self.assertRaisesRegex(state.ServiceError, "windows_state_permissions"):
            state.load(self.home)
        state.run(["icacls", str(target), "/remove:g", "*S-1-1-0"], discard_output=True)
        self.assertEqual(state.load(self.home), self.receipt)

    def test_load_fails_closed_when_native_inspection_is_unavailable(self):
        with patch(
            "windows_state.validate_handle",
            side_effect=OSError("inspection unavailable"),
        ):
            with self.assertRaisesRegex(
                state.ServiceError, "incomplete_or_invalid_state"
            ):
                state.load(self.home)

    def test_load_pins_root_and_receipt_until_consumption_finishes(self):
        parse = state.json.loads

        def inspect(encoded):
            with self.assertRaises(PermissionError):
                self.home.rename(self.home.with_name("moved"))
            with self.assertRaises(PermissionError):
                (self.home / "receipt.json").write_bytes(b"replacement")
            return parse(encoded)

        with patch("state.json.loads", side_effect=inspect):
            self.assertEqual(state.load(self.home), self.receipt)
