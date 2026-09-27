"""Native private creation, retained object identity, and bounded handle reads."""

import ctypes
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from state_io import ServiceError

if os.name == "nt":
    import windows_state as native
    import test_windows_acl as acl_tests


@unittest.skipUnless(os.name == "nt", "requires native Windows handles")
class WindowsStateTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.parent = Path(self.temp.name)
        self.root = self.parent / "private"
        native.create_directory(self.root)

    def test_private_creation_and_bounded_reads_preserve_unicode_and_binary_data(self):
        file = self.root / "état-秘密"
        payload = b"secret\0bytes"
        native.write_new(file, payload)
        with native.pinned_paths() as scope:
            scope.validate(self.root, directory=True)
            self.assertEqual(scope.read(file, 3), payload[:4])
            self.assertEqual(scope.read(file, 16384), payload)

    def test_existing_objects_are_never_overwritten_or_reconfigured(self):
        file = self.root / "secret"
        native.write_new(file, b"original")
        with self.assertRaises(FileExistsError):
            native.create_directory(self.root)
        with self.assertRaises(FileExistsError):
            native.write_new(file, b"replacement")
        with native.pinned_paths() as scope:
            self.assertEqual(scope.read(file, 32), b"original")

    def test_validated_reads_never_reopen_the_path(self):
        file = self.root / "secret"
        native.write_new(file, b"original")
        with native.pinned_paths() as scope:
            scope.validate(file, directory=False)
            with patch.object(
                Path, "read_bytes", side_effect=AssertionError("reopened")
            ):
                with patch.object(Path, "open", side_effect=AssertionError("reopened")):
                    self.assertEqual(scope.read(file, 32), b"original")

    def test_native_names_cannot_truncate_or_select_alternate_streams(self):
        file = self.root / "secret"
        native.write_new(file, b"original")
        with native.pinned_paths() as scope:
            for name in (str(file) + "\0suffix", str(file) + ":stream"):
                with self.subTest(name=repr(name)), self.assertRaises(ServiceError):
                    scope.read(name, 32)
            self.assertEqual(scope.read(file, 32), b"original")

    def test_pins_block_another_process_replacing_file_or_ancestor_until_closed(self):
        file = self.root / "secret"
        candidate = self.root / "candidate"
        native.write_new(file, b"original")
        native.write_new(candidate, b"replacement")
        attack = """import os,sys
try:
    os.replace(sys.argv[1],sys.argv[2])
except PermissionError:
    sys.exit(0)
sys.exit(1)
"""
        with native.pinned_paths() as scope:
            scope.validate(file, directory=False)
            for source, destination in (
                (candidate, file),
                (self.root, self.parent / "moved"),
            ):
                result = subprocess.run(
                    [sys.executable, "-c", attack, str(source), str(destination)],
                    capture_output=True,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
            with self.assertRaises(PermissionError):
                file.write_bytes(b"changed")
            self.assertEqual(scope.read(file, 32), b"original")
        os.replace(candidate, file)
        with native.pinned_paths() as scope:
            self.assertEqual(scope.read(file, 32), b"replacement")

    def test_junction_ancestor_is_refused_before_reading_target(self):
        file = self.root / "secret"
        native.write_new(file, b"secret")
        link = self.parent / "junction"
        get_system = native.bind(
            native.kernel32,
            "GetSystemDirectoryW",
            native.W.UINT,
            native.W.LPWSTR,
            native.W.UINT,
        )
        buffer = ctypes.create_unicode_buffer(32768)
        length = get_system(buffer, len(buffer))
        self.assertTrue(0 < length < len(buffer))
        subprocess.run(
            [
                str(Path(buffer.value) / "cmd.exe"),
                "/d",
                "/c",
                "mklink",
                "/J",
                str(link),
                str(self.root),
            ],
            capture_output=True,
            check=True,
        )
        self.addCleanup(link.rmdir)
        with native.pinned_paths() as scope, patch.object(native, "read_file") as read:
            with self.assertRaises(ServiceError):
                scope.read(link / "secret", 32)
            read.assert_not_called()

    def test_unprotected_file_and_unsafe_acl_are_refused_without_reading(self):
        file = self.root / "inherited"
        file.write_bytes(b"must not be read")
        with native.pinned_paths() as scope, patch.object(native, "read_file") as read:
            with self.assertRaises(ServiceError):
                scope.read(file, 32)
            read.assert_not_called()
        acl_tests.WindowsAclTests.set_dacl(self, file, "D:P(A;;FA;;;WD)")
        with native.pinned_paths() as scope:
            with self.assertRaises(ServiceError):
                scope.read(file, 32)

    def test_write_and_flush_failures_delete_only_the_new_owned_file(self):
        for function in ("write_file", "flush_file"):
            file = self.root / function
            with self.subTest(function=function):
                with patch.object(native, function, return_value=False):
                    ctypes.set_last_error(5)
                    with self.assertRaises(OSError):
                        native.write_new(file, b"payload")
                self.assertFalse(file.exists())
        self.assertEqual(list(self.root.iterdir()), [])

    def test_private_creation_ignores_planted_acl_executables_in_current_directory(
        self,
    ):
        for executable in ("powershell.exe", "icacls.exe"):
            (self.parent / executable).write_bytes(b"not a trusted Windows executable")
        code = """from pathlib import Path
import windows_state as native
root=Path('private').absolute()
native.create_directory(root/'nested')
native.write_new(root/'secret',b'protected')
with native.pinned_paths() as scope:
    assert scope.read(root/'secret',32)==b'protected'
"""
        env = dict(os.environ, PYTHONPATH=str(Path(native.__file__).parent))
        result = subprocess.run(
            [sys.executable, "-c", code],
            cwd=self.parent,
            env=env,
            capture_output=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_private_write_refuses_an_unprotected_parent(self):
        parent = self.root / "inherited"
        parent.mkdir()
        with self.assertRaises(ServiceError):
            native.write_new(parent / "secret", b"protected")
        self.assertFalse((parent / "secret").exists())

    def test_source_tree_identity_is_excluded_at_each_actual_operation(self):
        secret = self.root / "secret"
        native.write_new(secret, b"original")
        module = self.root / "scripts" / "postgres" / "windows_state.py"
        with (
            patch.object(native, "__file__", str(module)),
            native.pinned_paths() as scope,
        ):
            for operation in (
                lambda: native.create_directory(self.root / "child"),
                lambda: native.write_new(self.root / "new-file", b"new"),
                lambda: scope.read(secret, 32),
            ):
                with self.assertRaisesRegex(
                    ServiceError, "state_must_be_outside_source_tree"
                ):
                    operation()
        self.assertEqual(sorted(path.name for path in self.root.iterdir()), ["secret"])
