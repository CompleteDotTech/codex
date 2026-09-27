"""Filesystem checks for creating protected service directories."""

import os
from pathlib import Path
import stat
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import state


class PrivateDirectoryTests(unittest.TestCase):
    def test_existing_directory_is_not_reconfigured(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary)
            before = path.stat()
            with self.assertRaises(FileExistsError):
                state.private_directory(path)
            self.assertEqual(
                stat.S_IMODE(path.stat().st_mode), stat.S_IMODE(before.st_mode)
            )

    @unittest.skipIf(os.name == "nt", "POSIX modes")
    def test_new_directory_excludes_other_users(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "private"
            state.private_directory(path)
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o700)
