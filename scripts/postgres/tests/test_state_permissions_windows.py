"""Native compatibility entry points preserve private ACLs and fail closed."""

import os
from pathlib import Path
import shutil
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import state

if os.name == "nt":
    import windows_state as native
    import test_windows_acl as acl_tests


@unittest.skipUnless(os.name == "nt", "requires native Windows ACLs")
class WindowsPermissionsTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name) / "state"
        for path in (self.home, self.home / "secrets", self.home / "backups"):
            state.private_directory(path)
        # This stage integrates permission entry points; the IO hook follows.
        native.write_new(self.home / "receipt.json", b"{}")
        native.write_new(self.home / "secrets/runtime.password", b"fixture")
        self.validate(self.home)

    def validate(self, home):
        state._validate_windows_permissions([home, *home.rglob("*")])

    def test_weakened_files_and_directories_are_rejected_without_repair(self):
        for relative in (
            "secrets/runtime.password",
            "receipt.json",
            "secrets",
            "backups",
            ".",
        ):
            with self.subTest(path=relative):
                target = self.home / relative
                acl_tests.WindowsAclTests.set_dacl(self, target, "D:P(A;;FA;;;WD)")
                try:
                    with self.assertRaisesRegex(
                        state.ServiceError, "windows_state_permissions"
                    ):
                        self.validate(self.home)
                    # A second native inspection must still see the unsafe ACL.
                    with native.pinned_paths() as scope:
                        with self.assertRaises(state.ServiceError):
                            scope.validate(target)
                finally:
                    sid = acl_tests.acl.current_sid()
                    acl_tests.WindowsAclTests.set_dacl(
                        self, target, f"D:P(A;OICI;FA;;;{sid})(A;OICI;FA;;;SY)"
                    )

    def test_restored_inherited_acl_is_rejected_and_original_remains_usable(self):
        restored = Path(self.temp.name) / "restored"
        shutil.copytree(self.home, restored)
        with self.assertRaisesRegex(state.ServiceError, "windows_state_permissions"):
            self.validate(restored)
        self.validate(self.home)
        with native.pinned_paths() as scope:
            self.assertEqual(
                scope.read(self.home / "secrets/runtime.password", 32), b"fixture"
            )

    def test_null_dacl_is_rejected(self):
        acl_tests.WindowsAclTests.set_dacl(
            self, self.home / "receipt.json", "D:NO_ACCESS_CONTROL"
        )
        with self.assertRaisesRegex(state.ServiceError, "windows_state_permissions"):
            self.validate(self.home)

    def test_unavailable_acl_inspection_fails_closed_without_exposing_details(self):
        with patch.object(
            native, "validate_handle", side_effect=OSError("private diagnostic")
        ):
            with self.assertRaisesRegex(
                state.ServiceError,
                "^insecure_or_unverifiable_windows_state_permissions$",
            ):
                self.validate(self.home)
