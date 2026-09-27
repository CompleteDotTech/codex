"""Historical primary SQL and offline-audit tests; not Codex/SQLx qualification."""

import hashlib
import json
import shutil
import sqlite3
import tempfile
import unittest
from contextlib import closing
from pathlib import Path
from unittest.mock import patch

from . import legacy_primary_test_support
from .legacy_primary_test_support import make_legacy_primary
from .records import ContractError
from .sqlite_snapshot import audit_snapshot


class LegacyPrimaryFixturesTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.path = Path(self.directory.name) / "legacy.sqlite"
        self.policy = make_legacy_primary(self.path)

    def audit(self, policy=None):
        digest = hashlib.sha256(self.path.read_bytes()).hexdigest()
        with self.path.open("rb") as source:
            return audit_snapshot(
                source, digest, self.policy if policy is None else policy
            )

    def rows(self, sql):
        with closing(sqlite3.connect(self.path)) as connection:
            return connection.execute(sql).fetchall()

    def test_complete_threads_survive_reopen_and_backfill(self):
        active = self.rows("SELECT * FROM threads WHERE id='active'")[0]
        self.assertEqual(
            active,
            (
                "active",
                "C:\\fixture\\workspace/rollout.jsonl",
                1700000000,
                1700000001,
                "cli",
                "fixture",
                "C:\\fixture\\workspace",
                "é / é / 雪",
                "{}",
                "never",
                (1 << 53) + 1,
                1,
                0,
                None,
                None,
                None,
                None,
                "",
                "é / é / 雪",
            ),
        )
        self.assertEqual(
            self.rows(
                "SELECT id,first_user_message,archived,archived_at FROM threads ORDER BY id"
            ),
            [
                ("active", "é / é / 雪", 0, None),
                ("archived", "", 1, 1700000002),
                ("empty-title", "", 0, None),
            ],
        )

    def test_complete_memory_and_job_records_remain_in_legacy_store(self):
        self.assertEqual(
            self.rows("SELECT * FROM stage1_outputs"),
            [("active", 1700000001, "memory\0é / é / 雪", "summary", 1700000003, None)],
        )
        self.assertEqual(
            self.rows("SELECT * FROM jobs"),
            [
                (
                    "stage1",
                    "active",
                    "running",
                    "origin-worker",
                    "origin-token",
                    1700000004,
                    None,
                    1700000099,
                    None,
                    3,
                    None,
                    (1 << 53) + 1,
                    None,
                )
            ],
        )
        self.assertFalse(self.audit()["activation_permitted"])

    def test_tool_position_order_and_raw_payloads(self):
        self.assertEqual(
            self.rows("SELECT * FROM thread_dynamic_tools ORDER BY position"),
            [
                ("active", 0, "first", "é", '{"type":"string"}'),
                ("active", 1, "second", "雪", '{"type":"object"}'),
            ],
        )

    def test_log_nulls_nanos_and_consumed_id_survive_reopen(self):
        self.assertEqual(
            self.rows("SELECT * FROM logs"),
            [
                (
                    30,
                    1700000005,
                    999999999,
                    "INFO",
                    "fixture",
                    "log\0雪",
                    None,
                    None,
                    None,
                    "active",
                )
            ],
        )
        self.assertEqual(self.rows("SELECT * FROM sqlite_sequence"), [("logs", 80)])
        with closing(sqlite3.connect(self.path)) as connection, connection:
            row = connection.execute(
                "INSERT INTO logs (ts,ts_nanos,level,target) VALUES (0,0,'INFO','next')"
            )
            self.assertEqual(row.lastrowid, 81)

    def test_backfill_metadata_and_all_prefix_inventories(self):
        self.assertEqual(
            self.rows("SELECT * FROM backfill_state"),
            [(1, "pending", None, None, 1700000030)],
        )
        expected = {
            "threads",
            "thread_dynamic_tools",
            "stage1_outputs",
            "jobs",
            "logs",
            "sqlite_sequence",
        }
        for version in range(6, 10):
            with self.subTest(version=version):
                path = self.path.with_name(f"prefix-{version}.sqlite")
                policy = make_legacy_primary(path, version)
                self.assertEqual(
                    set(json.loads(policy)["tables"]),
                    expected | ({"backfill_state"} if version >= 8 else set()),
                )
                with path.open("rb") as source:
                    report = audit_snapshot(
                        source, hashlib.sha256(path.read_bytes()).hexdigest(), policy
                    )
                self.assertFalse(report["activation_permitted"])
                self.assertEqual(
                    sum(t["rows"] for t in report["tables"]), 9 + (version >= 8)
                )

    def test_foreign_keys_and_compound_uniqueness(self):
        with closing(sqlite3.connect(self.path)) as connection:
            connection.execute("PRAGMA foreign_keys=ON")
            for sql in (
                "INSERT INTO thread_dynamic_tools VALUES ('active',0,'x','x','{}')",
                "INSERT INTO thread_dynamic_tools VALUES ('missing',0,'x','x','{}')",
                "INSERT INTO stage1_outputs VALUES ('missing',0,'x','x',0,NULL)",
                "INSERT INTO jobs SELECT * FROM jobs",
                "INSERT INTO backfill_state VALUES (2,'pending',NULL,NULL,0)",
            ):
                with self.subTest(sql=sql), self.assertRaises(sqlite3.IntegrityError):
                    connection.execute(sql)
            connection.rollback()

    def test_cascade_does_not_imply_logs_or_jobs_are_cascaded(self):
        with closing(sqlite3.connect(self.path)) as connection, connection:
            connection.execute("PRAGMA foreign_keys=ON")
            connection.execute("DELETE FROM threads WHERE id='active'")
        self.assertEqual(self.rows("SELECT * FROM thread_dynamic_tools"), [])
        self.assertEqual(self.rows("SELECT * FROM stage1_outputs"), [])
        self.assertEqual(len(self.rows("SELECT * FROM jobs")), 1)
        self.assertEqual(len(self.rows("SELECT * FROM logs")), 1)
        self.assertFalse(self.audit()["activation_permitted"])

    def test_equal_counts_do_not_hide_changed_legacy_payloads(self):
        mutations = (
            ("threads", "UPDATE threads SET title='changed' WHERE id='active'"),
            ("stage1_outputs", "UPDATE stage1_outputs SET raw_memory='changed'"),
            ("jobs", "UPDATE jobs SET ownership_token='changed'"),
            ("logs", "UPDATE logs SET message='changed'"),
            (
                "thread_dynamic_tools",
                "UPDATE thread_dynamic_tools SET input_schema='{}' WHERE position=0",
            ),
            ("sqlite_sequence", "UPDATE sqlite_sequence SET seq=81 WHERE name='logs'"),
            ("backfill_state", "UPDATE backfill_state SET last_watermark='changed'"),
        )
        for table, sql in mutations:
            with self.subTest(table=table):
                before = {t["table"]: t for t in self.audit()["tables"]}
                with closing(sqlite3.connect(self.path)) as connection, connection:
                    connection.execute(sql)
                after = {t["table"]: t for t in self.audit()["tables"]}
                self.assertEqual(before[table]["rows"], after[table]["rows"])
                self.assertNotEqual(
                    before[table]["logical_sha256"], after[table]["logical_sha256"]
                )
                self.assertEqual(
                    {k: v for k, v in before.items() if k != table},
                    {k: v for k, v in after.items() if k != table},
                )

    def test_omitted_domains_cannot_be_a_successful_inventory(self):
        for table in json.loads(self.policy)["tables"]:
            policy = json.loads(self.policy)
            del policy["tables"][table]
            with (
                self.subTest(table=table),
                self.assertRaisesRegex(ContractError, "^table_inventory_mismatch$"),
            ):
                self.audit(json.dumps(policy).encode())

    def test_retained_and_regenerated_tables_are_explicit_not_transferred(self):
        policy = json.loads(self.policy)
        policy["tables"].update({"jobs": "retain", "backfill_state": "regenerate"})
        report = self.audit(json.dumps(policy).encode())
        tables = {t["table"]: t for t in report["tables"]}
        for table, treatment in (("jobs", "retain"), ("backfill_state", "regenerate")):
            self.assertEqual(
                tables[table],
                {
                    "table": table,
                    "treatment": treatment,
                    "rows": 1,
                    "logical_sha256": None,
                },
            )
        self.assertFalse(report["activation_permitted"])
        self.assertEqual(
            self.rows("SELECT ownership_token FROM jobs"), [("origin-token",)]
        )

    def test_stale_schema_policy_is_rejected(self):
        old = make_legacy_primary(self.path.with_name("version6.sqlite"), 6)
        with self.assertRaisesRegex(ContractError, "^snapshot_schema_mismatch$"):
            self.audit(old)

    def test_broken_relationship_is_rejected(self):
        with closing(sqlite3.connect(self.path)) as connection, connection:
            connection.execute("UPDATE stage1_outputs SET thread_id='missing'")
        with self.assertRaisesRegex(ContractError, "^snapshot_foreign_key_failed$"):
            self.audit()

    def test_existing_destination_is_not_overwritten(self):
        before = self.path.read_bytes()
        with self.assertRaises(FileExistsError):
            make_legacy_primary(self.path)
        self.assertEqual(self.path.read_bytes(), before)

    def test_unsupported_versions_do_not_create_fixtures(self):
        for version in (True, "9", 0, 5, 10, 58):
            path = self.path.with_name("invalid.sqlite")
            with (
                self.subTest(version=version),
                self.assertRaisesRegex(
                    ValueError, "^unsupported_legacy_fixture_version$"
                ),
            ):
                make_legacy_primary(path, version)
            self.assertFalse(path.exists())

    def test_changed_or_missing_source_is_rejected_before_destination_creation(self):
        source = Path(self.directory.name) / "source"
        source.mkdir()
        for name, _blob in legacy_primary_test_support.MIGRATIONS:
            shutil.copyfile(legacy_primary_test_support.SOURCE / name, source / name)
        last = source / legacy_primary_test_support.MIGRATIONS[-1][0]
        last.write_bytes(last.read_bytes() + b"-- changed\n")
        destination = self.path.with_name("uncreated.sqlite")
        with patch.object(legacy_primary_test_support, "SOURCE", source):
            with self.assertRaisesRegex(
                AssertionError, "^legacy_fixture_source_changed$"
            ):
                make_legacy_primary(destination, 6)
            self.assertFalse(destination.exists())
            last.unlink()
            with self.assertRaises(FileNotFoundError):
                make_legacy_primary(destination, 6)
            self.assertFalse(destination.exists())

    def test_windows_checkout_line_endings_preserve_pinned_fixture(self):
        source = Path(self.directory.name) / "crlf-source"
        source.mkdir()
        for name, _blob in legacy_primary_test_support.MIGRATIONS:
            data = (legacy_primary_test_support.SOURCE / name).read_bytes()
            (source / name).write_bytes(
                data.replace(b"\r\n", b"\n").replace(b"\n", b"\r\n")
            )
        with patch.object(legacy_primary_test_support, "SOURCE", source):
            policy = make_legacy_primary(self.path.with_name("crlf.sqlite"))
        self.assertEqual(policy, self.policy)
