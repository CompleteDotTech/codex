"""Real SQLite WAL/backup and offline CLI boundaries, not Codex migration."""

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

from .legacy_primary_test_support import make_legacy_primary

CLI = Path(__file__).resolve().parents[1] / "audit_sqlite_snapshot.py"


class LegacyPrimaryProcessTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.path = self.root / "legacy.sqlite"
        self.policy = self.root / "policy.json"
        self.policy.write_bytes(make_legacy_primary(self.path))
        self.home = self.root / "isolated-codex-home"
        self.home.mkdir()
        (self.home / "sentinel").write_bytes(b"unrelated")

    def cli(self, path, expected=None):
        result = subprocess.run(
            [
                sys.executable,
                str(CLI),
                "--snapshot",
                str(path),
                "--policy",
                str(self.policy),
                "--expected-sha256",
                expected
                if expected is not None
                else hashlib.sha256(path.read_bytes()).hexdigest(),
            ],
            env={**os.environ, "CODEX_HOME": str(self.home)},
            capture_output=True,
            text=True,
            timeout=15,
            check=False,
        )
        self.assertEqual(result.stderr, "")
        self.assertEqual(
            {p.name: p.read_bytes() for p in self.home.iterdir()},
            {"sentinel": b"unrelated"},
        )
        report = json.loads(result.stdout)
        self.assertFalse(report["activation_permitted"])
        return result, report

    def test_crashed_writer_requires_supported_backup_including_wal_commit(self):
        with closing(sqlite3.connect(self.path)) as connection:
            self.assertEqual(
                connection.execute("PRAGMA journal_mode=WAL").fetchone(), ("wal",)
            )
        writer = subprocess.run(
            [
                sys.executable,
                "-c",
                """
import os, sqlite3, sys
connection = sqlite3.connect(sys.argv[1])
connection.execute('PRAGMA wal_autocheckpoint=0')
connection.execute("INSERT INTO logs (ts,ts_nanos,level,target,message) "
                   "VALUES (0,0,'INFO','fixture','private-WAL-payload')")
connection.commit()
os._exit(0)  # Only this disposable writer exits, without connection cleanup.
""",
                str(self.path),
            ],
            capture_output=True,
            text=True,
            timeout=15,
            check=False,
        )
        self.assertEqual((writer.returncode, writer.stdout, writer.stderr), (0, "", ""))
        self.assertGreater(Path(str(self.path) + "-wal").stat().st_size, 32)
        with closing(
            sqlite3.connect(self.path.as_uri() + "?mode=ro&immutable=1", uri=True)
        ) as stale:
            self.assertEqual(
                stale.execute("SELECT count(*) FROM logs").fetchone(), (1,)
            )
        before = {p.name: p.read_bytes() for p in self.root.iterdir() if p.is_file()}
        result, report = self.cli(self.path)
        self.assertEqual(
            (result.returncode, report["code"]), (2, "sqlite_sidecars_present")
        )
        self.assertEqual(
            before, {p.name: p.read_bytes() for p in self.root.iterdir() if p.is_file()}
        )
        backup = self.root / "backup.sqlite"
        with backup.open("xb"):
            pass
        with (
            closing(sqlite3.connect(self.path)) as source,
            closing(sqlite3.connect(backup)) as target,
        ):
            source.backup(target)
            self.assertEqual(
                target.execute("SELECT id,message FROM logs ORDER BY id").fetchall(),
                [(30, "log\0雪"), (81, "private-WAL-payload")],
            )
        captured = backup.read_bytes()
        result, report = self.cli(backup)
        self.assertEqual((result.returncode, report["status"]), (0, "snapshot_audited"))
        self.assertEqual(
            {t["table"]: t["rows"] for t in report["tables"]},
            {
                "backfill_state": 1,
                "jobs": 1,
                "logs": 2,
                "sqlite_sequence": 1,
                "stage1_outputs": 1,
                "thread_dynamic_tools": 2,
                "threads": 3,
            },
        )
        self.assertNotIn("private-WAL-payload", result.stdout)
        self.assertNotIn("origin-token", result.stdout)
        self.assertEqual(backup.read_bytes(), captured)

    def test_cli_rejects_changed_bytes_before_sqlite_parsing(self):
        captured = self.path.read_bytes()
        self.path.write_bytes(captured[:-1] + bytes([captured[-1] ^ 1]))
        tampered = self.path.read_bytes()
        result, report = self.cli(self.path, hashlib.sha256(captured).hexdigest())
        self.assertEqual(
            (result.returncode, report["code"]), (2, "snapshot_digest_mismatch")
        )
        self.assertEqual(self.path.read_bytes(), tampered)

    def test_cli_rejects_earlier_schema_policy(self):
        self.policy.write_bytes(make_legacy_primary(self.root / "prefix6.sqlite", 6))
        before = self.path.read_bytes()
        result, report = self.cli(self.path)
        self.assertEqual(
            (result.returncode, report["code"]), (2, "snapshot_schema_mismatch")
        )
        self.assertEqual(self.path.read_bytes(), before)
