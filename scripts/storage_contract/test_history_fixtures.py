"""History projection SQL behavior, not canonical rollout or context qualification."""

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


class HistoryFixtureTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.path = self.root / "thread_history_1.sqlite"
        make_extended_fixture(self.path, self.path.name)

    def audit(self, path):
        with path.open("rb") as stream:
            return audit_snapshot(
                stream,
                hashlib.sha256(path.read_bytes()).hexdigest(),
                build_fixture_policy(self.path.name, version=7),
            )

    def test_backfilled_type_ordinals_offsets_and_lifecycle_fields_survive(self):
        with closing(sqlite3.connect(self.path)) as db:
            self.assertEqual(
                db.execute("SELECT * FROM thread_items").fetchall(),
                [
                    (
                        "t",
                        "turn",
                        "user",
                        5,
                        1000001,
                        '{"type":"userMessage","fixture":"雪 / é / é"}',
                        "userMessage",
                        5,
                        1000001,
                        1000002,
                    )
                ],
            )
            self.assertEqual(
                db.execute("SELECT * FROM thread_turns").fetchall(),
                [
                    (
                        "t",
                        "turn",
                        4,
                        "completed",
                        None,
                        1000,
                        1002,
                        2000,
                        "user",
                        "agent",
                        512,
                        8,
                        7000,
                    )
                ],
            )
        self.audit(self.path)

    def test_old_writer_zero_default_is_preserved_not_treated_as_fenced(self):
        with closing(sqlite3.connect(self.path)) as db, db:
            db.execute(
                "INSERT INTO thread_items(thread_id,turn_id,item_id,rollout_ordinal,"
                "created_at_ms,item_json,item_type) VALUES ('t','turn','old',6,1000002,'{}','agentMessage')"
            )
            self.assertEqual(
                db.execute(
                    "SELECT item_id,updated_at_ordinal FROM thread_items ORDER BY rollout_ordinal"
                ).fetchall(),
                [("user", 5), ("old", 0)],
            )
        before = self.audit(self.path)
        with closing(sqlite3.connect(self.path)) as db, db:
            db.execute(
                "UPDATE thread_items SET updated_at_ordinal=9 WHERE item_id='old'"
            )
        self.assertNotEqual(self.audit(self.path)["tables"], before["tables"])

    def test_invalid_legacy_json_rolls_back_in_an_explicit_sqlite_transaction(self):
        scripts = verified_migrations(self.path.name)
        with closing(sqlite3.connect(":memory:")) as db:
            db.executescript(scripts[0].decode())
            db.execute(
                "INSERT INTO thread_items VALUES ('t','turn','bad',1,1,'not-json')"
            )
            db.commit()
            before_schema = db.execute(
                "SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY name"
            ).fetchall()
            with self.assertRaises(sqlite3.OperationalError):
                db.executescript(
                    "BEGIN IMMEDIATE;\n" + scripts[1].decode() + "\nCOMMIT;"
                )
            db.rollback()
            self.assertEqual(
                db.execute(
                    "SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY name"
                ).fetchall(),
                before_schema,
            )
            self.assertEqual(
                db.execute("SELECT * FROM thread_items").fetchall(),
                [("t", "turn", "bad", 1, 1, "not-json")],
            )

    def test_realtime_cleanup_is_scoped_to_deleted_projection(self):
        with closing(sqlite3.connect(self.path)) as db, db:
            db.execute(
                "INSERT INTO thread_history_projection_state VALUES ('other',42,3)"
            )
            db.execute(
                "INSERT INTO thread_realtime_items VALUES ('other','r',1,1,'realtime_session_closed','{}')"
            )
            db.execute(
                "DELETE FROM thread_history_projection_state WHERE thread_id='t'"
            )
            self.assertEqual(
                db.execute("SELECT * FROM thread_realtime_items").fetchall(),
                [("other", "r", 1, 1, "realtime_session_closed", "{}")],
            )
            self.assertEqual(
                db.execute("SELECT * FROM thread_history_projection_state").fetchall(),
                [("other", 42, 3)],
            )
        self.audit(self.path)

    def test_duplicate_item_ordinal_in_one_thread_is_rejected(self):
        with closing(sqlite3.connect(self.path)) as db:
            before = db.execute("SELECT * FROM thread_items").fetchall()
            with self.assertRaises(sqlite3.IntegrityError):
                db.execute(
                    "INSERT INTO thread_items(thread_id,turn_id,item_id,rollout_ordinal,created_at_ms,item_json) "
                    "VALUES ('t','other-turn','duplicate',5,2,'{}')"
                )
            self.assertEqual(
                db.execute("SELECT * FROM thread_items").fetchall(), before
            )

    def test_equal_ordinals_on_different_threads_remain_distinct(self):
        with closing(sqlite3.connect(self.path)) as db, db:
            db.execute(
                "INSERT INTO thread_items SELECT 'other',turn_id,item_id,rollout_ordinal,created_at_ms,"
                "item_json,item_type,updated_at_ordinal,started_at_ms,completed_at_ms FROM thread_items"
            )
            self.assertEqual(
                db.execute(
                    "SELECT thread_id,item_id,rollout_ordinal FROM thread_items ORDER BY thread_id"
                ).fetchall(),
                [("other", "user", 5), ("t", "user", 5)],
            )
        self.audit(self.path)

    def test_equal_count_projected_payload_corruption_changes_evidence(self):
        before = self.audit(self.path)
        with closing(sqlite3.connect(self.path)) as db, db:
            db.execute("UPDATE thread_items SET item_json='{}'")
        after = self.audit(self.path)
        self.assertEqual(
            [(t["table"], t["rows"]) for t in before["tables"]],
            [(t["table"], t["rows"]) for t in after["tables"]],
        )
        self.assertNotEqual(before["tables"], after["tables"])

    def test_backup_preserves_every_projection_table(self):
        backup = self.root / "history.backup"
        before = self.audit(self.path)
        with (
            closing(sqlite3.connect(self.path)) as source,
            closing(sqlite3.connect(backup)) as target,
        ):
            source.backup(target)
        self.assertEqual(self.audit(backup)["tables"], before["tables"])

    def test_outdated_policy_rejects_lifecycle_schema(self):
        with (
            self.path.open("rb") as stream,
            self.assertRaisesRegex(ContractError, "snapshot_schema_mismatch"),
        ):
            audit_snapshot(
                stream,
                hashlib.sha256(self.path.read_bytes()).hexdigest(),
                build_fixture_policy(self.path.name, version=6),
            )

    def test_removed_cleanup_trigger_cannot_be_hidden_by_recounting_rows(self):
        with closing(sqlite3.connect(self.path)) as db, db:
            db.execute("DROP TRIGGER thread_realtime_items_projection_cleanup")
        with self.assertRaisesRegex(ContractError, "snapshot_schema_mismatch"):
            self.audit(self.path)
