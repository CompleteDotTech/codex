"""Goal/log SQL migration, relational integrity, and artifact-audit regressions."""

import hashlib
import json
import sqlite3
import tempfile
import unittest
from contextlib import closing
from pathlib import Path

from .extended_fixture_support import make_extended_fixture
from .records import ContractError
from .source_catalog import build_fixture_policy, verified_migrations
from .sqlite_snapshot import audit_snapshot


class GoalsLogsFixtureTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def audit(self, path, store):
        policy = build_fixture_policy(store, version=len(verified_migrations(store)))
        with path.open("rb") as stream:
            return audit_snapshot(
                stream, hashlib.sha256(path.read_bytes()).hexdigest(), policy
            )

    def test_goal_accounting_and_deferral_survive_source_upgrade(self):
        path = self.root / "goals_1.sqlite"
        make_extended_fixture(path, path.name)
        with closing(sqlite3.connect(path)) as db:
            self.assertEqual(
                db.execute("SELECT * FROM thread_goals").fetchall(),
                [
                    (
                        "t",
                        "goal",
                        "雪 / é / é",
                        "active",
                        None,
                        9007199254740993,
                        123,
                        1700000000001,
                        1700000000002,
                    )
                ],
            )
            self.assertEqual(
                db.execute(
                    "SELECT * FROM thread_goal_continuation_deferrals"
                ).fetchall(),
                [("t",)],
            )
        self.assertFalse(self.audit(path, path.name)["activation_permitted"])

    def test_goal_delete_cascades_only_its_deferral(self):
        path = self.root / "goals_1.sqlite"
        make_extended_fixture(path, path.name)
        with closing(sqlite3.connect(path)) as db, db:
            db.execute("PRAGMA foreign_keys=ON")
            db.execute(
                "INSERT INTO thread_goals SELECT 'other',goal_id,objective,status,token_budget,"
                "tokens_used,time_used_seconds,created_at_ms,updated_at_ms FROM thread_goals"
            )
            db.execute(
                "INSERT INTO thread_goal_continuation_deferrals VALUES ('other')"
            )
            db.execute("DELETE FROM thread_goals WHERE thread_id='t'")
            self.assertEqual(
                db.execute(
                    "SELECT * FROM thread_goal_continuation_deferrals"
                ).fetchall(),
                [("other",)],
            )

    def test_invalid_goal_status_is_not_accepted(self):
        path = self.root / "goals_1.sqlite"
        make_extended_fixture(path, path.name)
        with closing(sqlite3.connect(path)) as db:
            before = db.execute("SELECT * FROM thread_goals").fetchall()
            with self.assertRaises(sqlite3.IntegrityError):
                db.execute("UPDATE thread_goals SET status='not-a-status'")
            self.assertEqual(
                db.execute("SELECT * FROM thread_goals").fetchall(), before
            )

    def test_goal_accounting_corruption_changes_logical_evidence_without_count_change(
        self,
    ):
        path = self.root / "goals_1.sqlite"
        make_extended_fixture(path, path.name)
        before = self.audit(path, path.name)
        with closing(sqlite3.connect(path)) as db, db:
            db.execute("UPDATE thread_goals SET tokens_used=tokens_used+1")
        after = self.audit(path, path.name)
        self.assertEqual(
            [(t["table"], t["rows"]) for t in before["tables"]],
            [(t["table"], t["rows"]) for t in after["tables"]],
        )
        self.assertNotEqual(before["tables"], after["tables"])

    def test_orphaned_deferral_is_detected_with_matching_byte_digest(self):
        path = self.root / "goals_1.sqlite"
        make_extended_fixture(path, path.name)
        with closing(sqlite3.connect(path)) as db, db:
            db.execute("PRAGMA foreign_keys=OFF")
            db.execute(
                "INSERT INTO thread_goal_continuation_deferrals VALUES ('orphan')"
            )
        with self.assertRaisesRegex(ContractError, "snapshot_foreign_key_failed"):
            self.audit(path, path.name)

    def test_log_body_rename_preserves_complete_row(self):
        path = self.root / "logs_2.sqlite"
        make_extended_fixture(path, path.name)
        with closing(sqlite3.connect(path)) as db:
            self.assertEqual(
                db.execute("SELECT * FROM logs").fetchall(),
                [
                    (
                        500,
                        1700000000,
                        999999999,
                        "INFO",
                        "fixture",
                        "雪\x00é",
                        None,
                        "source.rs",
                        12,
                        "t",
                        "process",
                        999,
                    )
                ],
            )
            self.assertEqual(
                db.execute("SELECT name,seq FROM sqlite_sequence").fetchall(),
                [("logs", 500)],
            )

    def test_log_null_body_and_timestamp_tie_survive(self):
        path = self.root / "logs_2.sqlite"
        make_extended_fixture(path, path.name)
        with closing(sqlite3.connect(path)) as db, db:
            db.execute(
                "INSERT INTO logs(ts,ts_nanos,level,target) VALUES (1700000000,999999999,'INFO','fixture')"
            )
            self.assertEqual(
                db.execute(
                    "SELECT id,feedback_log_body FROM logs ORDER BY ts DESC,ts_nanos DESC,id DESC"
                ).fetchall(),
                [(501, None), (500, "雪\x00é")],
            )
        self.audit(path, path.name)

    def test_retained_logs_must_be_explicit_not_silently_dropped(self):
        path = self.root / "logs_2.sqlite"
        make_extended_fixture(path, path.name)
        before = self.audit(path, path.name)
        with closing(sqlite3.connect(path)) as db, db:
            db.execute("DELETE FROM logs")
        after = self.audit(path, path.name)
        self.assertNotEqual(before["tables"], after["tables"])
        self.assertEqual(
            [t for t in after["tables"] if t["table"] == "sqlite_sequence"],
            [t for t in before["tables"] if t["table"] == "sqlite_sequence"],
        )

    def test_backup_readback_preserves_goals_and_log_high_watermark(self):
        for store in ("goals_1.sqlite", "logs_2.sqlite"):
            with self.subTest(store=store):
                path, backup = self.root / store, self.root / (store + ".backup")
                make_extended_fixture(path, store)
                if store == "logs_2.sqlite":
                    with closing(sqlite3.connect(path)) as db, db:
                        db.execute(
                            "UPDATE sqlite_sequence SET seq=9007199254740993 WHERE name='logs'"
                        )
                before = self.audit(path, store)
                with (
                    closing(sqlite3.connect(path)) as source,
                    closing(sqlite3.connect(backup)) as target,
                ):
                    source.backup(target)
                after = self.audit(backup, store)
                self.assertEqual(before["tables"], after["tables"])
                if store == "logs_2.sqlite":
                    with closing(sqlite3.connect(backup)) as db, db:
                        db.execute(
                            "INSERT INTO logs(ts,ts_nanos,level,target) VALUES (1,0,'INFO','fixture')"
                        )
                        self.assertEqual(
                            db.execute("SELECT max(id) FROM logs").fetchone(),
                            (9007199254740994,),
                        )

    def test_older_source_policy_cannot_approve_newer_schema(self):
        for store in ("goals_1.sqlite", "logs_2.sqlite"):
            with self.subTest(store=store):
                path = self.root / store
                make_extended_fixture(path, store)
                with (
                    path.open("rb") as stream,
                    self.assertRaisesRegex(ContractError, "snapshot_schema_mismatch"),
                ):
                    audit_snapshot(
                        stream,
                        hashlib.sha256(path.read_bytes()).hexdigest(),
                        build_fixture_policy(store, version=1),
                    )

    def test_existing_fixture_destination_is_never_modified(self):
        path = self.root / "existing.sqlite"
        path.write_bytes(b"unrelated user-owned fixture sentinel")
        with self.assertRaises(FileExistsError):
            make_extended_fixture(path, "goals_1.sqlite")
        self.assertEqual(path.read_bytes(), b"unrelated user-owned fixture sentinel")
