"""Both memory-generation SQL artifacts, without claiming worker-lease safety."""

import hashlib
import sqlite3
import tempfile
import unittest
from contextlib import closing
from pathlib import Path

from .extended_fixture_support import make_extended_fixture
from .records import ContractError
from .source_catalog import build_fixture_policy, verified_migrations
from .sqlite_snapshot import audit_snapshot


class MemoryFixtureTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def audit(self, path, store):
        with path.open("rb") as stream:
            return audit_snapshot(
                stream,
                hashlib.sha256(path.read_bytes()).hexdigest(),
                build_fixture_policy(store, version=2),
            )

    def test_populated_output_and_job_survive_consolidation_upgrade_in_both_versions(
        self,
    ):
        for store in ("memories_1.sqlite", "memories_v2_1.sqlite"):
            with self.subTest(store=store):
                path = self.root / store
                make_extended_fixture(path, store)
                with closing(sqlite3.connect(path)) as db:
                    self.assertEqual(
                        db.execute("SELECT * FROM stage1_outputs").fetchall(),
                        [
                            (
                                "t",
                                1700000000,
                                "memory " + store,
                                "summary",
                                None,
                                1700000001,
                                None,
                                None,
                                1,
                                1700000000,
                            )
                        ],
                    )
                    self.assertEqual(
                        db.execute("SELECT * FROM jobs").fetchall(),
                        [
                            (
                                "phase2",
                                "global",
                                "running",
                                "host-a",
                                "lease-a",
                                1700000000,
                                None,
                                1700000030,
                                None,
                                2,
                                None,
                                1700000000,
                                1699999999,
                            )
                        ],
                    )
                    self.assertEqual(
                        db.execute("SELECT * FROM consolidation_progress").fetchall(),
                        [(1, 31)],
                    )
                self.audit(path, store)

    def test_same_schema_versions_are_not_interchangeable_artifacts(self):
        first, second = (
            self.root / "memories_1.sqlite",
            self.root / "memories_v2_1.sqlite",
        )
        make_extended_fixture(first, first.name)
        make_extended_fixture(second, second.name)
        a, b = self.audit(first, first.name), self.audit(second, second.name)
        self.assertEqual(a["schema_sha256"], b["schema_sha256"])
        self.assertNotEqual(a["tables"], b["tables"])
        with (
            second.open("rb") as stream,
            self.assertRaisesRegex(ContractError, "snapshot_digest_mismatch"),
        ):
            audit_snapshot(
                stream,
                hashlib.sha256(first.read_bytes()).hexdigest(),
                build_fixture_policy(first.name, version=2),
            )

    def test_mutating_one_version_does_not_change_the_other(self):
        first, second = (
            self.root / "memories_1.sqlite",
            self.root / "memories_v2_1.sqlite",
        )
        make_extended_fixture(first, first.name)
        make_extended_fixture(second, second.name)
        before = self.audit(first, first.name)
        with closing(sqlite3.connect(second)) as db, db:
            db.execute("DELETE FROM stage1_outputs")
            db.execute("UPDATE consolidation_progress SET max_thread_count=0")
        self.assertEqual(self.audit(first, first.name), before)

    def test_absent_version_is_not_created_by_policy_building(self):
        store = "memories_v2_1.sqlite"
        path = self.root / store
        build_fixture_policy(store, version=2)
        self.assertFalse(path.exists())
        with closing(sqlite3.connect(path)) as db:
            for script in verified_migrations(store):
                db.executescript(script.decode())
        report = self.audit(path, store)
        self.assertEqual(
            [(t["table"], t["rows"]) for t in report["tables"]],
            [("consolidation_progress", 1), ("jobs", 0), ("stage1_outputs", 0)],
        )

    def test_consolidation_singleton_constraint_rejects_second_row(self):
        path = self.root / "memories_1.sqlite"
        make_extended_fixture(path, path.name)
        with closing(sqlite3.connect(path)) as db:
            with self.assertRaises(sqlite3.IntegrityError):
                db.execute("INSERT INTO consolidation_progress VALUES (2,99)")
            self.assertEqual(
                db.execute("SELECT * FROM consolidation_progress").fetchall(), [(1, 31)]
            )

    def test_job_primary_key_constraint_preserves_existing_ownership_fields(self):
        path = self.root / "memories_1.sqlite"
        make_extended_fixture(path, path.name)
        with closing(sqlite3.connect(path)) as db:
            before = db.execute("SELECT * FROM jobs").fetchall()
            with self.assertRaises(sqlite3.IntegrityError):
                db.execute("INSERT INTO jobs SELECT * FROM jobs")
            self.assertEqual(db.execute("SELECT * FROM jobs").fetchall(), before)

    def test_changed_lease_fields_are_detected_not_reissued_or_activated(self):
        path = self.root / "memories_1.sqlite"
        make_extended_fixture(path, path.name)
        before = self.audit(path, path.name)
        with closing(sqlite3.connect(path)) as db, db:
            db.execute(
                "UPDATE jobs SET ownership_token='lease-b',worker_id='host-b',lease_until=1700000060"
            )
        after = self.audit(path, path.name)
        self.assertNotEqual(before["tables"], after["tables"])
        self.assertEqual(
            [t for t in before["tables"] if t["table"] != "jobs"],
            [t for t in after["tables"] if t["table"] != "jobs"],
        )
        self.assertFalse(after["activation_permitted"])

    def test_memory_nulls_and_large_integer_watermarks_survive_backup(self):
        for store in ("memories_1.sqlite", "memories_v2_1.sqlite"):
            with self.subTest(store=store):
                path, backup = self.root / store, self.root / (store + ".backup")
                make_extended_fixture(path, store)
                with closing(sqlite3.connect(path)) as db, db:
                    db.execute(
                        "UPDATE jobs SET input_watermark=9223372036854775807,last_error=NULL"
                    )
                    db.execute(
                        "UPDATE stage1_outputs SET usage_count=0,last_usage=NULL"
                    )
                before = self.audit(path, store)
                with (
                    closing(sqlite3.connect(path)) as source,
                    closing(sqlite3.connect(backup)) as target,
                ):
                    source.backup(target)
                self.assertEqual(self.audit(backup, store)["tables"], before["tables"])

    def test_legacy_policy_rejects_added_consolidation_state(self):
        path = self.root / "memories_v2_1.sqlite"
        make_extended_fixture(path, path.name)
        with (
            path.open("rb") as stream,
            self.assertRaisesRegex(ContractError, "snapshot_schema_mismatch"),
        ):
            audit_snapshot(
                stream,
                hashlib.sha256(path.read_bytes()).hexdigest(),
                build_fixture_policy(path.name, version=1),
            )
