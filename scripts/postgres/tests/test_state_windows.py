"""Native NTFS permission and certificate persistence checks, without Docker."""

import ctypes
from ctypes import wintypes
import os
from pathlib import Path
import shutil
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

    def test_weakened_secret_and_receipt_are_rejected_without_repair(self):
        for relative in ("secrets/runtime.password", "receipt.json"):
            with self.subTest(path=relative):
                target = self.home / relative
                before = target.read_bytes()
                state.run(["icacls", str(target), "/grant", "*S-1-1-0:M"])
                acl = state.run(["icacls", str(target)])
                try:
                    with self.assertRaisesRegex(
                        state.ServiceError, "windows_state_permissions"
                    ):
                        state.load(self.home)
                    self.assertEqual(
                        (target.read_bytes(), state.run(["icacls", str(target)])),
                        (before, acl),
                    )
                finally:
                    state.run(["icacls", str(target), "/remove:g", "*S-1-1-0"])

    def test_weakened_directories_are_rejected_without_repair(self):
        for relative in (".", "secrets", "backups"):
            with self.subTest(path=relative):
                target = self.home / relative
                state.run(["icacls", str(target), "/grant", "*S-1-1-0:M"])
                acl = state.run(["icacls", str(target)])
                try:
                    with self.assertRaisesRegex(
                        state.ServiceError, "windows_state_permissions"
                    ):
                        state.load(self.home)
                    self.assertEqual(state.run(["icacls", str(target)]), acl)
                finally:
                    state.run(["icacls", str(target), "/remove:g", "*S-1-1-0"])

    def test_copy_inheriting_broad_access_is_rejected(self):
        parent = Path(self.temp.name) / "shared"
        parent.mkdir()
        state.run(["icacls", str(parent), "/grant", "*S-1-1-0:(OI)(CI)RX"])
        restored = parent / "restored"
        shutil.copytree(self.home, restored)
        acl = state.run(["icacls", str(restored / "secrets/runtime.password")])
        with self.assertRaisesRegex(state.ServiceError, "windows_state_permissions"):
            state.load(restored)
        self.assertEqual(
            state.run(["icacls", str(restored / "secrets/runtime.password")]), acl
        )
        self.assertEqual(state.load(self.home), self.receipt)

    def test_null_dacl_is_rejected(self):
        target = self.home / "receipt.json"
        set_security = ctypes.WinDLL("advapi32").SetNamedSecurityInfoW
        set_security.argtypes = [
            wintypes.LPWSTR,
            wintypes.DWORD,
            wintypes.DWORD,
            ctypes.c_void_p,
            ctypes.c_void_p,
            ctypes.c_void_p,
            ctypes.c_void_p,
        ]
        set_security.restype = wintypes.DWORD
        # SE_FILE_OBJECT, DACL_SECURITY_INFORMATION, and a NULL DACL permit everyone.
        self.assertEqual(set_security(str(target), 1, 4, None, None, None, None), 0)
        with self.assertRaisesRegex(state.ServiceError, "windows_state_permissions"):
            state.load(self.home)

    def test_unavailable_acl_inspection_fails_closed(self):
        with patch("state.run", side_effect=state.ServiceError("command_failed")):
            with self.assertRaisesRegex(
                state.ServiceError, "unverifiable_windows_state_permissions"
            ):
                state.load(self.home)
