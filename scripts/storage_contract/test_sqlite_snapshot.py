import hashlib
import io
import json
import sqlite3
import tempfile
import unittest
from contextlib import closing
from pathlib import Path
from unittest.mock import patch

from .records import ContractError
from .snapshot_test_support import make_fixture, policy_for
from .sqlite_snapshot import AuditLimits, audit_snapshot


class SnapshotTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.path = self.root / "queue.sqlite"
        self.policy = make_fixture(self.path, "queue")

    def audit(self, path=None, policy=None, limits=AuditLimits()):
        data = (path or self.path).read_bytes()
        return audit_snapshot(
            io.BytesIO(data),
            hashlib.sha256(data).hexdigest(),
            self.policy if policy is None else policy,
            limits,
        )

    def mutate(self, sql):
        with closing(sqlite3.connect(self.path)) as connection, connection:
            connection.executescript(sql)

    def test_queue_backfill_deletion_revision_and_sequence_are_audited(self):
        report = self.audit()
        self.assertEqual(
            [(t["table"], t["rows"]) for t in report["tables"]],
            [
                ("queued_items", 1),
                ("queued_thread_revisions", 2),
                ("sqlite_sequence", 1),
            ],
        )
        self.assertFalse(report["activation_permitted"])
        self.assertEqual(report["scope"], "single_sqlite_backup_artifact")

    def test_board_tombstones_opt_outs_and_sequence_are_included(self):
        path = self.root / "board.sqlite"
        policy = make_fixture(path, "board")
        report = self.audit(path, policy)
        self.assertEqual(
            [(t["table"], t["rows"]) for t in report["tables"]],
            [
                ("channels", 1),
                ("deleted_boards", 1),
                ("posts", 1),
                ("sqlite_sequence", 1),
                ("subscription_opt_outs", 1),
                ("subscriptions", 1),
            ],
        )

    def test_file_digest_rejects_changed_payload_before_inspection(self):
        old = self.path.read_bytes()
        self.mutate("UPDATE queued_items SET payload_json='changed'")
        with self.assertRaisesRegex(ContractError, "snapshot_digest_mismatch"):
            audit_snapshot(
                io.BytesIO(self.path.read_bytes()),
                hashlib.sha256(old).hexdigest(),
                self.policy,
            )

    def test_equal_row_count_mutation_changes_logical_fingerprint(self):
        before = self.audit()
        self.mutate("UPDATE queued_items SET payload_json='changed'")
        after = self.audit()
        self.assertEqual(before["tables"][0]["rows"], after["tables"][0]["rows"])
        self.assertNotEqual(
            before["tables"][0]["logical_sha256"], after["tables"][0]["logical_sha256"]
        )

    def test_deleted_generated_ids_remain_part_of_evidence(self):
        path = self.root / "board.sqlite"
        policy = make_fixture(path, "board")
        before = self.audit(path, policy)
        with closing(sqlite3.connect(path)) as connection, connection:
            connection.execute("UPDATE sqlite_sequence SET seq=81 WHERE name='posts'")
        after = self.audit(path, policy)
        self.assertNotEqual(before["tables"][3], after["tables"][3])
        self.assertEqual(before["tables"][2], after["tables"][2])

    def test_policy_cannot_omit_empty_or_optional_tables(self):
        policy = json.loads(self.policy)
        del policy["tables"]["queued_thread_revisions"]
        with self.assertRaisesRegex(ContractError, "table_inventory_mismatch"):
            self.audit(policy=json.dumps(policy).encode())

    def test_policy_cannot_add_absent_table(self):
        policy = json.loads(self.policy)
        policy["tables"]["uncreated_feature"] = "migrate"
        with self.assertRaisesRegex(ContractError, "table_inventory_mismatch"):
            self.audit(policy=json.dumps(policy).encode())

    def test_unknown_table_or_trigger_blocks_schema_compatibility(self):
        for sql in (
            "CREATE TABLE hidden_state(secret TEXT)",
            "CREATE TRIGGER secret_trigger AFTER DELETE ON queued_items BEGIN SELECT 1; END",
        ):
            with self.subTest(sql=sql):
                original = self.path.read_bytes()
                self.mutate(sql)
                with self.assertRaisesRegex(ContractError, "snapshot_schema_mismatch"):
                    self.audit()
                self.path.write_bytes(original)

    def test_dropped_table_blocks_compatibility(self):
        self.mutate("DROP TABLE queued_thread_revisions")
        with self.assertRaisesRegex(ContractError, "snapshot_schema_mismatch"):
            self.audit()

    def test_explicit_retention_and_regeneration_are_not_fingerprinted(self):
        policy = json.loads(self.policy)
        policy["tables"]["queued_items"] = "retain"
        policy["tables"]["queued_thread_revisions"] = "regenerate"
        tables = self.audit(policy=json.dumps(policy).encode())["tables"]
        self.assertEqual(
            [(t["treatment"], t["rows"], t["logical_sha256"] is None) for t in tables],
            [("retain", 1, True), ("regenerate", 2, True), ("migrate", 1, False)],
        )

    def test_unsupported_views_fail_without_querying_them(self):
        self.mutate("CREATE VIEW v AS SELECT load_extension('never-load-this')")
        with closing(sqlite3.connect(self.path)) as connection, connection:
            policy = policy_for(connection)
        with self.assertRaisesRegex(ContractError, "unsupported_sqlite_schema"):
            self.audit(policy=policy)

    def test_virtual_text_in_ordinary_table_schema_is_supported(self):
        path = self.root / "ordinary.sqlite"
        with closing(sqlite3.connect(path)) as connection, connection:
            connection.execute(
                "CREATE TABLE virtual_settings(virtual_mode INTEGER, "
                "description TEXT DEFAULT 'virtual machine')"
            )
            connection.execute("INSERT INTO virtual_settings(virtual_mode) VALUES (1)")
            policy = policy_for(connection)
        report = self.audit(path, policy)
        self.assertEqual(
            [(table["table"], table["rows"]) for table in report["tables"]],
            [("virtual_settings", 1)],
        )
        self.assertIsNotNone(report["tables"][0]["logical_sha256"])

    def test_virtual_tables_with_commented_ddl_are_rejected(self):
        path = self.root / "virtual.sqlite"
        with closing(sqlite3.connect(path)) as connection, connection:
            try:
                connection.execute(
                    "CREATE /*comment*/ VIRTUAL /*comment*/ TABLE search "
                    "USING fts5(value)"
                )
            except sqlite3.OperationalError as error:
                if "no such module: fts5" in str(error):
                    self.skipTest("SQLite was built without FTS5")
                raise
            policy = policy_for(connection)
        with self.assertRaisesRegex(ContractError, "unsupported_sqlite_schema"):
            self.audit(path, policy)

    def test_foreign_key_orphans_block_verification(self):
        path = self.root / "orphans.sqlite"
        with closing(sqlite3.connect(path)) as connection, connection:
            connection.executescript(
                "CREATE TABLE p(id INTEGER PRIMARY KEY);"
                "CREATE TABLE c(parent INTEGER REFERENCES p(id));"
                "INSERT INTO c VALUES (1);"
            )
            policy = policy_for(connection)
        with self.assertRaisesRegex(ContractError, "snapshot_foreign_key_failed"):
            self.audit(path, policy)

    def test_empty_database_is_distinct_from_missing_or_zero_byte_file(self):
        path = self.root / "empty.sqlite"
        with closing(sqlite3.connect(path)) as connection, connection:
            connection.execute("VACUUM")
            policy = policy_for(connection)
        self.assertEqual(self.audit(path, policy)["tables"], [])
        empty = b""
        with self.assertRaisesRegex(ContractError, "invalid_sqlite_header"):
            audit_snapshot(io.BytesIO(empty), hashlib.sha256(empty).hexdigest(), policy)

    def test_input_file_is_not_modified(self):
        before = (self.path.read_bytes(), self.path.stat().st_mtime_ns)
        self.audit()
        self.assertEqual(before, (self.path.read_bytes(), self.path.stat().st_mtime_ns))
        self.assertEqual(sorted(p.name for p in self.root.iterdir()), ["queue.sqlite"])

    def test_short_reads_are_supported(self):
        class Partial(io.BytesIO):
            def read(self, size=-1):
                return super().read(min(31, size))

        data = self.path.read_bytes()
        result = audit_snapshot(
            Partial(data), hashlib.sha256(data).hexdigest(), self.policy
        )
        self.assertEqual(result, self.audit())

    def test_size_row_and_vm_budgets_fail_closed(self):
        cases = [
            (AuditLimits(max_bytes=512), "snapshot_too_large"),
            (AuditLimits(max_rows=1), "snapshot_too_many_rows"),
            (AuditLimits(max_vm_steps=1), "sqlite_audit_failed"),
            (AuditLimits(max_encoded_bytes=32), "sqlite_encoded_budget_exhausted"),
        ]
        for limits, code in cases:
            with (
                self.subTest(limits=limits),
                self.assertRaisesRegex(ContractError, code),
            ):
                self.audit(limits=limits)

    def test_invalid_limits_and_policies_are_rejected(self):
        with self.assertRaisesRegex(ContractError, "invalid_integer"):
            self.audit(limits=AuditLimits(max_rows=True))
        for changes in (
            {"version": True},
            {"extra": 1},
            {"tables": {"a": "absent"}},
            {"tables": {"unsafe;name": "migrate"}},
        ):
            value = json.loads(self.policy)
            value.update(changes)
            with self.subTest(changes=changes), self.assertRaises(ContractError):
                self.audit(policy=json.dumps(value).encode())

    def test_corrupt_and_truncated_images_do_not_leak_payload(self):
        original = self.path.read_bytes()
        for data in (b"private-data", original[:512], original[:16] + b"secret" * 500):
            with (
                self.subTest(length=len(data)),
                self.assertRaises(ContractError) as error,
            ):
                audit_snapshot(
                    io.BytesIO(data), hashlib.sha256(data).hexdigest(), self.policy
                )
            self.assertNotIn("secret", str(error.exception))
            self.assertNotIn("private-data", str(error.exception))

    def test_stage_write_failure_leaves_source_intact(self):
        data = self.path.read_bytes()
        with patch(
            "storage_contract.sqlite_snapshot.Path.open", side_effect=OSError("full")
        ):
            with self.assertRaises(OSError):
                audit_snapshot(
                    io.BytesIO(data), hashlib.sha256(data).hexdigest(), self.policy
                )
        self.assertEqual(self.path.read_bytes(), data)

    def test_row_multiset_is_order_independent_and_duplicate_sensitive(self):
        digests = []
        values = [(1,), (1.0,), ("1",), (b"1",), (None,), ("雪",), (1,)]
        for index, rows in enumerate((values, list(reversed(values)), values[:-1])):
            path = self.root / f"values-{index}.sqlite"
            with closing(sqlite3.connect(path)) as connection, connection:
                connection.execute("CREATE TABLE values_table(value)")
                connection.executemany("INSERT INTO values_table VALUES (?)", rows)
                policy = policy_for(connection)
            digests.append(self.audit(path, policy)["tables"][0]["logical_sha256"])
        self.assertEqual(digests[0], digests[1])
        self.assertNotEqual(digests[0], digests[2])

    def test_vacuum_and_physical_layout_do_not_change_logical_content(self):
        before = self.audit()["tables"]
        self.mutate("VACUUM")
        self.assertEqual(before, self.audit()["tables"])

    def test_invalid_unicode_and_oversized_cells_fail_without_content(self):
        for value in ("CAST(x'ff' AS TEXT)", "zeroblob(1100000)"):
            path = self.root / "bad.sqlite"
            path.unlink(missing_ok=True)
            with closing(sqlite3.connect(path)) as connection, connection:
                connection.execute("CREATE TABLE bad(value)")
                connection.execute(f"INSERT INTO bad VALUES ({value})")
                policy = policy_for(connection)
            with self.subTest(value=value), self.assertRaises(ContractError):
                self.audit(path, policy)
