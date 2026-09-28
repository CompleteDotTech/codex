"""Current primary-schema evidence from source SQL on disposable databases."""

import json
import sqlite3
import tempfile
import unittest
from contextlib import closing
from pathlib import Path

from .legacy_primary_test_support import make_legacy_primary
from .source_catalog import build_fixture_policy, verified_migrations


class PrimaryStateFixtureTests(unittest.TestCase):
    def test_legacy_primary_rows_survive_current_source_migrations(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "state_5.sqlite"
            make_legacy_primary(path, version=9)
            scripts = verified_migrations("state_5.sqlite")
            self.assertEqual(len(scripts), 58)

            with closing(sqlite3.connect(path)) as db:
                db.execute("PRAGMA foreign_keys=ON")
                before = db.execute(
                    "SELECT id,title,tokens_used FROM threads ORDER BY id"
                ).fetchall()
                for script in scripts[9:]:
                    db.executescript(script.decode("utf-8"))
                after = db.execute(
                    "SELECT id,title,tokens_used FROM threads ORDER BY id"
                ).fetchall()
                self.assertEqual(after, before)
                self.assertEqual(
                    db.execute(
                        "SELECT name,description FROM thread_dynamic_tools "
                        "WHERE thread_id='active' ORDER BY position"
                    ).fetchall(),
                    [("first", "é"), ("second", "雪")],
                )
                tables = {
                    row[0]
                    for row in db.execute(
                        "SELECT name FROM sqlite_schema WHERE type='table'"
                    )
                }
                self.assertIn("thread_attachments", tables)
                self.assertNotIn("logs", tables)
                self.assertNotIn("_sqlx_migrations", tables)
                self.assertEqual(
                    tables,
                    set(
                        json.loads(build_fixture_policy("state_5.sqlite", version=58))[
                            "tables"
                        ]
                    ),
                )

    def test_current_schema_has_no_live_autoincrement_column(self):
        with sqlite3.connect(":memory:") as db:
            for script in verified_migrations("state_5.sqlite"):
                db.executescript(script.decode("utf-8"))
            rows = db.execute(
                "SELECT name,sql FROM sqlite_schema WHERE type='table'"
            ).fetchall()
            self.assertFalse(
                [(name, sql) for name, sql in rows if sql and "AUTOINCREMENT" in sql]
            )
            self.assertIn("sqlite_sequence", {name for name, _ in rows})


if __name__ == "__main__":
    unittest.main()
