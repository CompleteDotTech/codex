import sqlite3
import tempfile
import unittest
from contextlib import closing
from pathlib import Path

from .snapshot_test_support import apply_source_sql, make_fixture


class SourceFixtureTests(unittest.TestCase):
    def test_revision_migration_backfills_and_tracks_legacy_queue_mutations(self):
        with closing(sqlite3.connect(":memory:")) as db:
            apply_source_sql(db, "queue_0001.sql")
            db.execute(
                "INSERT INTO queued_items VALUES (?,?,?,?,?,?)",
                ("old", "thread", "{}", 0, 1, 1),
            )
            db.commit()
            apply_source_sql(db, "queue_0002.sql")
            initial = db.execute("SELECT * FROM queued_thread_revisions").fetchall()
            db.execute("UPDATE queued_items SET payload_json='[]'")
            updated = db.execute("SELECT * FROM queued_thread_revisions").fetchall()
            db.execute("DELETE FROM queued_items")
            deleted = db.execute("SELECT * FROM queued_thread_revisions").fetchall()
            self.assertEqual(
                [initial, updated, deleted],
                [[(1, "thread")], [(2, "thread")], [(3, "thread")]],
            )

    def test_board_constraints_tombstones_and_opt_outs_survive_reopen(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "board.sqlite"
            make_fixture(path, "board")
            with closing(sqlite3.connect(path)) as db:
                self.assertEqual(
                    db.execute("SELECT * FROM deleted_boards").fetchall(),
                    [("deleted-root",)],
                )
                self.assertEqual(
                    db.execute("SELECT * FROM subscription_opt_outs").fetchall(),
                    [("root-a", "general", "agent-b")],
                )
                with self.assertRaises(sqlite3.IntegrityError):
                    db.execute(
                        "INSERT INTO subscription_opt_outs VALUES (?,?,?)",
                        ("root-a", "general", "agent-b"),
                    )
