"""Run the offline command in another process for each available source schema."""

import hashlib
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from .extended_fixture_support import make_extended_fixture
from .snapshot_test_support import make_fixture
from .source_catalog import STORES, build_fixture_policy, verified_migrations

SCRIPT = Path(__file__).resolve().parents[1] / "audit_sqlite_snapshot.py"


class CatalogCliTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def create(self, store):
        path = self.root / store
        if store == "queue_1.sqlite":
            make_fixture(path, "queue")
        elif store == "agent_message_board_1.sqlite":
            make_fixture(path, "board")
        else:
            make_extended_fixture(path, store)
        return path

    def invoke(self, path, policy, digest):
        return subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--snapshot",
                str(path),
                "--policy",
                str(policy),
                "--expected-sha256",
                digest,
            ],
            cwd=self.root,
            capture_output=True,
            text=True,
            timeout=15,
        )

    def test_all_seven_source_schemas_are_audited_without_input_changes(self):
        for store in STORES:
            with self.subTest(store=store):
                path = self.create(store)
                policy = self.root / (store + ".policy.json")
                policy.write_bytes(
                    build_fixture_policy(store, version=len(verified_migrations(store)))
                )
                before = {p.name: p.read_bytes() for p in self.root.iterdir()}
                digest = hashlib.sha256(path.read_bytes()).hexdigest()
                result = self.invoke(path, policy, digest)
                self.assertEqual((result.returncode, result.stderr), (0, ""))
                report = json.loads(result.stdout)
                self.assertEqual(
                    (
                        report["status"],
                        report["scope"],
                        report["activation_permitted"],
                        report["schema_sha256"],
                        report["file_sha256"],
                    ),
                    (
                        "snapshot_audited",
                        "single_sqlite_backup_artifact",
                        False,
                        json.loads(policy.read_bytes())["schema_sha256"],
                        digest,
                    ),
                )
                self.assertEqual(
                    before, {p.name: p.read_bytes() for p in self.root.iterdir()}
                )
                self.assertNotIn("host-a", result.stdout)
                self.assertNotIn("雪", result.stdout)

    def test_same_schema_different_memory_generation_receipt_is_rejected(self):
        first = self.create("memories_1.sqlite")
        second = self.create("memories_v2_1.sqlite")
        policy = self.root / "policy.json"
        policy.write_bytes(build_fixture_policy(first.name, version=2))
        result = self.invoke(
            second, policy, hashlib.sha256(first.read_bytes()).hexdigest()
        )
        self.assertEqual(
            (result.returncode, result.stderr, json.loads(result.stdout)),
            (
                2,
                "",
                {
                    "status": "rejected",
                    "code": "snapshot_digest_mismatch",
                    "activation_permitted": False,
                },
            ),
        )

    def test_newer_schemas_are_not_approved_by_previous_version_policies(self):
        for store in (
            "goals_1.sqlite",
            "logs_2.sqlite",
            "memories_1.sqlite",
            "memories_v2_1.sqlite",
            "thread_history_1.sqlite",
        ):
            with self.subTest(store=store):
                path = self.create(store)
                policy = self.root / (store + ".policy.json")
                policy.write_bytes(
                    build_fixture_policy(
                        store, version=len(verified_migrations(store)) - 1
                    )
                )
                result = self.invoke(
                    path, policy, hashlib.sha256(path.read_bytes()).hexdigest()
                )
                self.assertEqual(
                    (result.returncode, result.stderr, json.loads(result.stdout)),
                    (
                        2,
                        "",
                        {
                            "status": "rejected",
                            "code": "snapshot_schema_mismatch",
                            "activation_permitted": False,
                        },
                    ),
                )
