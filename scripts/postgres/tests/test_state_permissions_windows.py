"""Native NTFS ACL checks use disposable directories and contain no secrets."""

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
class WindowsPermissionsTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name) / "state"
        for path in (self.home, self.home / "secrets", self.home / "backups"):
            state.private_directory(path)
        state.write_new(self.home / "receipt.json", b"{}")
        state.write_new(self.home / "secrets/runtime.password", b"fixture")
        self.validate(self.home)

    def validate(self, home):
        state._validate_windows_permissions([home, *home.rglob("*")])

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
                        self.validate(self.home)
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
                        self.validate(self.home)
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
            self.validate(restored)
        self.assertEqual(
            state.run(["icacls", str(restored / "secrets/runtime.password")]), acl
        )
        self.validate(self.home)

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
            self.validate(self.home)

    def test_unavailable_acl_inspection_fails_closed(self):
        with patch(
            "state_permissions.run", side_effect=state.ServiceError("command_failed")
        ):
            with self.assertRaisesRegex(
                state.ServiceError, "unverifiable_windows_state_permissions"
            ):
                self.validate(self.home)
