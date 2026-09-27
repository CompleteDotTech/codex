import hashlib
import json
import os
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from contextlib import closing
from pathlib import Path

from .snapshot_test_support import make_fixture

SCRIPT = Path(__file__).resolve().parents[1] / "audit_sqlite_snapshot.py"


class SnapshotCliTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.path = self.root / "snapshot.sqlite"
        self.policy = self.root / "policy.json"
        self.policy.write_bytes(make_fixture(self.path, "queue"))
        self.digest = hashlib.sha256(self.path.read_bytes()).hexdigest()

    def run_cli(self, *extra, snapshot=None, digest=None):
        return subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--snapshot",
                str(snapshot or self.path),
                "--policy",
                str(self.policy),
                "--expected-sha256",
                digest or self.digest,
                *extra,
            ],
            cwd=self.root,
            capture_output=True,
            text=True,
            timeout=10,
        )

    def test_success_is_read_only_and_never_authorizes_activation(self):
        original = {p.name: p.read_bytes() for p in self.root.iterdir()}
        result = self.run_cli()
        self.assertEqual((result.returncode, result.stderr), (0, ""))
        report = json.loads(result.stdout)
        self.assertEqual(
            (report["status"], report["activation_permitted"]),
            ("snapshot_audited", False),
        )
        self.assertEqual(
            original, {p.name: p.read_bytes() for p in self.root.iterdir()}
        )
        self.assertNotIn("before-revision-migration", result.stdout)

    def test_bad_hash_is_redacted(self):
        result = self.run_cli(
            digest="postgresql://secret-user:secret-password@private-host/db"
        )
        self.assertEqual(result.returncode, 2)
        self.assertEqual(
            json.loads(result.stdout),
            {
                "status": "rejected",
                "code": "invalid_token",
                "activation_permitted": False,
            },
        )
        self.assertNotIn("secret", result.stdout + result.stderr)

    def test_unknown_argument_does_not_echo_its_value(self):
        result = self.run_cli("--private-password", "do-not-print-this")
        self.assertEqual(result.returncode, 2)
        self.assertNotIn("do-not-print-this", result.stdout + result.stderr)
        self.assertEqual(json.loads(result.stdout)["code"], "invalid_arguments")

    def test_missing_input_has_no_traceback_or_private_path(self):
        path = self.root / "private-password-missing.sqlite"
        result = self.run_cli(snapshot=path)
        self.assertEqual(result.returncode, 3)
        self.assertNotIn("private-password", result.stdout + result.stderr)
        self.assertEqual(result.stderr, "")

    def test_directory_input_is_rejected_without_blocking(self):
        result = self.run_cli(snapshot=self.root)
        self.assertEqual(result.returncode, 2)
        self.assertEqual(json.loads(result.stdout)["code"], "input_not_regular_file")

    def test_each_sqlite_sidecar_is_rejected_even_if_empty(self):
        for suffix in ("-wal", "-shm", "-journal"):
            sidecar = Path(str(self.path) + suffix)
            sidecar.write_bytes(b"")
            with self.subTest(suffix=suffix):
                result = self.run_cli()
                self.assertEqual(result.returncode, 2)
                self.assertEqual(
                    json.loads(result.stdout)["code"], "sqlite_sidecars_present"
                )
            sidecar.unlink()

    def test_corrupt_backup_with_updated_checksum_is_still_rejected(self):
        self.path.write_bytes(b"SQLite format 3\0" + b"private-content" * 100)
        result = self.run_cli(digest=hashlib.sha256(self.path.read_bytes()).hexdigest())
        self.assertEqual(result.returncode, 2)
        self.assertNotIn("private-content", result.stdout + result.stderr)
        self.assertFalse(json.loads(result.stdout)["activation_permitted"])

    def test_crashed_writer_wal_is_refused_and_sqlite_backup_includes_committed_row(
        self,
    ):
        # Only this disposable child exits. No live Codex session is touched.
        writer = """
import os, sqlite3, sys
connection = sqlite3.connect(sys.argv[1])
connection.execute('PRAGMA journal_mode=WAL')
connection.execute('PRAGMA wal_autocheckpoint=0')
connection.execute('INSERT INTO queued_items VALUES (?,?,?,?,?,?)',
                   ('q3', 'thread-a', '{"fixture":"WAL-only"}', 1, 3000, 3000))
connection.commit()
os._exit(0)
"""
        subprocess.run(
            [sys.executable, "-c", writer, str(self.path)],
            check=True,
            capture_output=True,
            timeout=10,
        )
        self.assertGreater(Path(str(self.path) + "-wal").stat().st_size, 0)
        rejected = self.run_cli()
        self.assertEqual(json.loads(rejected.stdout)["code"], "sqlite_sidecars_present")
        target = self.root / "backup.sqlite"
        with closing(
            sqlite3.connect(self.path.as_uri() + "?mode=ro", uri=True)
        ) as source:
            with closing(sqlite3.connect(target)) as destination:
                source.backup(destination)
        result = self.run_cli(
            snapshot=target, digest=hashlib.sha256(target.read_bytes()).hexdigest()
        )
        self.assertEqual((result.returncode, result.stderr), (0, ""))
        report = json.loads(result.stdout)
        self.assertEqual(report["tables"][0]["rows"], 2)
        self.assertFalse(report["activation_permitted"])

    @unittest.skipUnless(hasattr(os, "mkfifo"), "POSIX FIFO operation")
    def test_fifo_does_not_hang(self):
        fifo = self.root / "input.fifo"
        os.mkfifo(fifo)
        result = self.run_cli(snapshot=fifo)
        self.assertEqual(result.returncode, 2)
        self.assertEqual(json.loads(result.stdout)["code"], "input_not_regular_file")

    @unittest.skipUnless(os.name == "posix", "POSIX symlink-loop fixture")
    def test_symlink_loop_is_redacted(self):
        link = self.root / "private-loop.sqlite"
        link.symlink_to(link.name)
        result = self.run_cli(snapshot=link)
        self.assertEqual(result.returncode, 3)
        self.assertNotIn("private-loop", result.stdout + result.stderr)
        self.assertEqual(result.stderr, "")
