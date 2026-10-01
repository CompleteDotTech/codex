"""Synthetic old-primary versus split-store transition evidence for issue #2.

These tests execute pinned source SQL in disposable files. They do not model
SQLx bookkeeping, transfer old rows, or determine a supported source version.
"""

import sqlite3
import tempfile
import unittest
from contextlib import closing
from pathlib import Path

from .legacy_primary_test_support import make_legacy_primary
from .source_catalog import verified_migrations


def apply_prefix(connection, store, start, stop):
    scripts = verified_migrations(store)
    for script in scripts[start:stop]:
        connection.executescript(script.decode("utf-8"))


def table_rows(path, table):
    with closing(sqlite3.connect(path)) as connection:
        present = connection.execute(
            "SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?", (table,)
        ).fetchone()
        if not present:
            return None
        return connection.execute(f"SELECT * FROM {table} ORDER BY rowid").fetchall()


def domain_inventory(primary, logs, memories):
    """Expose presence separately from zero rows at each physical location."""
    paths = {"primary": primary, "logs": logs, "memories": memories}
    domains = {
        "primary": ("logs", "stage1_outputs", "jobs"),
        "logs": ("logs",),
        "memories": ("stage1_outputs", "jobs"),
    }
    return {
        f"{location}.{table}": (
            {"state": "absent", "count": None}
            if not path.exists() or (rows := table_rows(path, table)) is None
            else {"state": "present", "count": len(rows)}
        )
        for location, path in paths.items()
        for table in domains[location]
    }


class SplitStoreFixtureTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.primary = self.root / "state_5.sqlite"
        self.logs = self.root / "logs_2.sqlite"
        self.memories = self.root / "memories_1.sqlite"

    def make_primary(self, prefix):
        make_legacy_primary(self.primary, version=9)
        with closing(sqlite3.connect(self.primary)) as connection:
            connection.execute("PRAGMA foreign_keys=ON")
            apply_prefix(connection, "state_5.sqlite", 9, prefix)

    def make_split_logs(self):
        with closing(sqlite3.connect(self.logs)) as connection:
            apply_prefix(connection, "logs_2.sqlite", 0, 2)
            connection.execute(
                "INSERT INTO logs (id,ts,ts_nanos,level,target,feedback_log_body,estimated_bytes) "
                "VALUES (91,1700000100,5,'INFO','split','new log',7)"
            )
            connection.commit()

    def make_split_memories(self):
        with closing(sqlite3.connect(self.memories)) as connection:
            apply_prefix(connection, "memories_1.sqlite", 0, 2)
            connection.execute(
                "INSERT INTO stage1_outputs "
                "(thread_id,source_updated_at,raw_memory,rollout_summary,generated_at) "
                "VALUES ('split',1700000100,'new memory','new summary',1700000101)"
            )
            connection.execute(
                "INSERT INTO jobs (kind,job_key,status,retry_remaining) "
                "VALUES ('stage1','split','pending',2)"
            )
            connection.commit()

    def test_log_drop_keeps_distinct_split_rows_and_exposes_old_loss(self):
        self.make_primary(22)
        self.make_split_logs()
        self.make_split_memories()
        before = domain_inventory(self.primary, self.logs, self.memories)
        old_logs = table_rows(self.primary, "logs")
        split_logs = table_rows(self.logs, "logs")
        old_memory = table_rows(self.primary, "stage1_outputs")
        self.assertEqual(before["primary.logs"], {"state": "present", "count": 1})
        self.assertEqual(before["logs.logs"], {"state": "present", "count": 1})
        self.assertNotEqual(old_logs, split_logs)

        with closing(sqlite3.connect(self.primary)) as connection:
            apply_prefix(connection, "state_5.sqlite", 22, 23)

        after = domain_inventory(self.primary, self.logs, self.memories)
        self.assertEqual(after["primary.logs"], {"state": "absent", "count": None})
        self.assertEqual(table_rows(self.logs, "logs"), split_logs)
        self.assertEqual(table_rows(self.primary, "stage1_outputs"), old_memory)
        self.assertEqual(
            after["memories.stage1_outputs"], before["memories.stage1_outputs"]
        )

    def test_memory_drop_keeps_distinct_split_rows_and_exposes_old_loss(self):
        self.make_primary(34)
        self.make_split_memories()
        before = domain_inventory(self.primary, self.logs, self.memories)
        old_output = table_rows(self.primary, "stage1_outputs")
        old_job = table_rows(self.primary, "jobs")
        split_output = table_rows(self.memories, "stage1_outputs")
        split_job = table_rows(self.memories, "jobs")
        self.assertNotEqual(old_output, split_output)
        self.assertNotEqual(old_job, split_job)
        self.assertEqual(before["logs.logs"], {"state": "absent", "count": None})
        self.assertEqual(before["primary.stage1_outputs"]["count"], 1)
        self.assertEqual(before["primary.jobs"]["count"], 1)

        with closing(sqlite3.connect(self.primary)) as connection:
            apply_prefix(connection, "state_5.sqlite", 34, 35)

        after = domain_inventory(self.primary, self.logs, self.memories)
        self.assertEqual(
            after["primary.stage1_outputs"], {"state": "absent", "count": None}
        )
        self.assertEqual(after["primary.jobs"], {"state": "absent", "count": None})
        self.assertEqual(table_rows(self.memories, "stage1_outputs"), split_output)
        self.assertEqual(table_rows(self.memories, "jobs"), split_job)

    def test_optional_split_files_are_absent_even_with_old_primary_rows(self):
        self.make_primary(22)
        inventory = domain_inventory(self.primary, self.logs, self.memories)
        self.assertEqual(inventory["primary.logs"]["count"], 1)
        self.assertEqual(inventory["primary.stage1_outputs"]["count"], 1)
        self.assertEqual(inventory["primary.jobs"]["count"], 1)
        for domain in ("logs.logs", "memories.stage1_outputs", "memories.jobs"):
            self.assertEqual(inventory[domain], {"state": "absent", "count": None})
        self.assertFalse(self.logs.exists())
        self.assertFalse(self.memories.exists())

    def test_existing_empty_split_store_is_distinct_from_absent_store(self):
        self.make_primary(22)
        self.make_split_logs()
        with closing(sqlite3.connect(self.logs)) as connection:
            connection.execute("DELETE FROM logs")
            connection.commit()
        inventory = domain_inventory(self.primary, self.logs, self.memories)
        self.assertEqual(inventory["logs.logs"], {"state": "present", "count": 0})
        self.assertEqual(
            inventory["memories.stage1_outputs"], {"state": "absent", "count": None}
        )
        self.assertEqual(inventory["primary.logs"]["count"], 1)
