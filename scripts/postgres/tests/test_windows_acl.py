"""Native ACL creation/validation; run on every supported Windows Python."""

import ctypes
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from state_io import ServiceError

if os.name == "nt":
    import windows_acl as acl

    mkdir = acl.bind(acl.kernel32, "CreateDirectoryW", acl.W.BOOL, acl.W.LPCWSTR, acl.P)
    open_file = acl.bind(
        acl.kernel32,
        "CreateFileW",
        acl.W.HANDLE,
        acl.W.LPCWSTR,
        acl.W.DWORD,
        acl.W.DWORD,
        acl.P,
        acl.W.DWORD,
        acl.W.DWORD,
        acl.W.HANDLE,
    )
    set_security = acl.bind(
        acl.advapi32,
        "SetFileSecurityW",
        acl.W.BOOL,
        acl.W.LPCWSTR,
        acl.W.DWORD,
        acl.P,
    )


@unittest.skipUnless(os.name == "nt", "requires native Windows security descriptors")
class WindowsAclTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def make_private(self, name):
        path = self.root / name
        with acl.private_attributes() as attributes:
            acl.checked(mkdir(str(path), ctypes.byref(attributes)))
        return path

    def validate(self, path):
        handle = open_file(str(path), 0x20000, 3, None, 3, 0x02200000, None)
        if handle == ctypes.c_void_p(-1).value:
            raise ctypes.WinError(ctypes.get_last_error())
        try:
            acl.validate_handle(handle)
        finally:
            acl.close_handle(handle)

    def set_dacl(self, path, sddl, *, protected=True):
        descriptor = acl.P()
        acl.checked(acl.convert_descriptor(sddl, 1, ctypes.byref(descriptor), None))
        try:
            flags = 0x4 | (0x80000000 if protected else 0x20000000)
            acl.checked(set_security(str(path), flags, descriptor))
        finally:
            acl.local_free(descriptor)

    def test_creation_is_private_even_under_inheritable_everyone_parent(self):
        self.set_dacl(self.root, "D:P(A;OICI;FA;;;WD)")
        private = self.make_private("private")
        self.validate(private)
        # An ordinary mkdir under this same parent demonstrably remains unsafe.
        inherited = self.root / "inherited"
        inherited.mkdir()
        with self.assertRaises(ServiceError):
            self.validate(inherited)

    def test_current_user_and_system_descriptor_survives_native_readback(self):
        self.validate(self.make_private("private"))

    def test_unprotected_dacl_is_rejected_even_when_current_aces_are_trusted(self):
        parent = self.make_private("parent")
        child = parent / "child"
        child.mkdir()
        with self.assertRaises(ServiceError):
            self.validate(child)

    def test_untrusted_read_grants_are_refused_without_repair(self):
        private = self.make_private("private")
        sid = acl.current_sid()
        for trustee in ("WD", "AU"):
            with self.subTest(trustee=trustee):
                self.set_dacl(private, f"D:P(A;;FA;;;{sid})(A;;FR;;;{trustee})")
                with self.assertRaises(ServiceError):
                    self.validate(private)
                with self.assertRaises(ServiceError):
                    self.validate(private)

    def test_null_dacl_is_rejected(self):
        private = self.make_private("private")
        self.set_dacl(private, "D:NO_ACCESS_CONTROL")
        with self.assertRaises(ServiceError):
            self.validate(private)

    def test_acl_query_failure_is_not_treated_as_private(self):
        private = self.make_private("private")
        with patch.object(acl, "get_security", return_value=5):
            with self.assertRaises(OSError):
                self.validate(private)
