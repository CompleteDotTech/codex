"""Native relative opens must retain the selected filesystem parent identity."""

import ctypes
import os
from pathlib import Path
import struct
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from state_io import ServiceError

if os.name == "nt":
    import windows_paths as paths
    import windows_acl as acl
    from test_windows_acl import open_file


@unittest.skipUnless(os.name == "nt", "requires native Windows relative opens")
class WindowsPathsTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.parent = self.root / "parent"
        self.parent.mkdir()
        self.handle = open_file(str(self.parent), 0x20080, 1, None, 3, 0x02200000, None)
        acl.checked(self.handle != acl.P(-1).value)
        self.addCleanup(acl.close_handle, self.handle)

    def test_unicode_creation_is_private_exclusive_and_bound_to_parent(self):
        leaf = "état-秘密-😀"
        with acl.private_attributes() as private:
            handle = paths.relative_open(
                self.handle,
                leaf,
                0x20080,
                create=True,
                directory=True,
                security=private.descriptor,
            )
        try:
            acl.validate_handle(handle)
            with self.assertRaises(FileExistsError):
                paths.relative_open(
                    self.handle, leaf, 0x20080, create=True, directory=True
                )
        finally:
            acl.close_handle(handle)
        self.assertTrue((self.parent / leaf).is_dir())

    def test_only_single_unambiguous_leaf_names_are_accepted(self):
        for leaf in ("", ".", "..", "../other", r"a\b", "a:b", "a\0b", "a.", "a "):
            with self.subTest(leaf=repr(leaf)), self.assertRaises(ServiceError):
                paths.relative_open(
                    self.handle, leaf, 0x20080, create=True, directory=True
                )
        self.assertEqual(list(self.parent.iterdir()), [])

    def test_parent_changed_to_junction_cannot_redirect_relative_open_or_creation(self):
        target = self.root / "target"
        target.mkdir()
        (target / "existing").write_bytes(b"unrelated")
        substitute = ("\\??\\" + str(target)).encode("utf-16-le")
        display = str(target).encode("utf-16-le")
        names = substitute + b"\0\0" + display + b"\0\0"
        data = (
            struct.pack(
                "<IHHHHHH",
                0xA0000003,
                8 + len(names),
                0,
                0,
                len(substitute),
                len(substitute) + 2,
                len(display),
            )
            + names
        )
        control = acl.bind(
            acl.kernel32,
            "DeviceIoControl",
            acl.W.BOOL,
            acl.W.HANDLE,
            acl.W.DWORD,
            acl.P,
            acl.W.DWORD,
            acl.P,
            acl.W.DWORD,
            acl.P,
            acl.P,
        )
        attributes = open_file(str(self.parent), 0x100, 7, None, 3, 0x02200000, None)
        acl.checked(attributes != acl.P(-1).value)
        count = acl.W.DWORD()
        try:
            buffer = ctypes.create_string_buffer(data)
            acl.checked(
                control(
                    attributes,
                    0x900A4,
                    buffer,
                    len(data),
                    None,
                    0,
                    ctypes.byref(count),
                    None,
                )
            )
            try:
                for leaf, create, directory in (
                    ("existing", False, False),
                    ("new-file", True, False),
                    ("new-directory", True, True),
                ):
                    with self.subTest(leaf=leaf), self.assertRaises(OSError):
                        with acl.private_attributes() as private:
                            handle = paths.relative_open(
                                self.handle,
                                leaf,
                                0x20080,
                                create=create,
                                directory=directory,
                                security=private.descriptor,
                            )
                            acl.close_handle(handle)
                self.assertEqual(
                    sorted(path.name for path in target.iterdir()), ["existing"]
                )
            finally:
                remove = ctypes.create_string_buffer(
                    struct.pack("<IHH", 0xA0000003, 0, 0)
                )
                acl.checked(
                    control(
                        attributes,
                        0x900AC,
                        remove,
                        8,
                        None,
                        0,
                        ctypes.byref(count),
                        None,
                    )
                )
        finally:
            acl.close_handle(attributes)
        self.assertEqual(list(self.parent.iterdir()), [])
