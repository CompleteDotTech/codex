"""Metadata fixtures, not real SQLx execution or a complete Codex state fixture."""

import hashlib
import json
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from .records import ContractError
from .sqlx_history import MAX_MIGRATIONS, SQLX_STORES, audit_history

ROOT = Path(__file__).resolve().parents[2]
# Equivalent DDL to SQLx 0.9.0's default SQLite migration-history table.
HISTORY_SCHEMA = """
CREATE TABLE _sqlx_migrations (
    version BIGINT PRIMARY KEY,
    description TEXT NOT NULL,
    installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    success BOOLEAN NOT NULL,
    checksum BLOB NOT NULL,
    execution_time BIGINT NOT NULL
);
"""
SOURCE_BLOBS = {
    38: (
        "0038_external_agent_config_imports.sql",
        "74ae0435f83536a17abf24b663d3be9264aebd2c",
    ),
    39: ("0039_threads_recency_at.sql", "ccbf79f05fab905ed655f35c215f55a66f3c0f2a"),
}


class SqlxHistoryTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.scripts = {}
        for version, (name, blob) in SOURCE_BLOBS.items():
            # Git's Windows checkout may write CRLF; the pinned blob uses LF.
            data = (
                (ROOT / "codex-rs/state/migrations" / name)
                .read_bytes()
                .replace(b"\r\n", b"\n")
            )
            actual = hashlib.sha1(b"blob " + str(len(data)).encode() + b"\0" + data)
            if actual.hexdigest() != blob:
                raise AssertionError(
                    "pinned source SQL changed; review fixture provenance"
                )
            cls.scripts[version] = data
        cls.expected = {
            v: hashlib.sha384(sql).digest() for v, sql in cls.scripts.items()
        }

    def database(self, *, versions=(38, 39), elapsed=0):
        connection = sqlite3.connect(":memory:")
        self.addCleanup(connection.close)
        connection.executescript(HISTORY_SCHEMA)
        for version in versions:
            connection.execute(
                "INSERT INTO _sqlx_migrations VALUES (?, ?, ?, 1, ?, ?)",
                (
                    version,
                    "private description",
                    "2026-09-27 00:00:00",
                    self.expected[version],
                    elapsed,
                ),
            )
        connection.commit()
        return connection

    def check(self, connection, **kwargs):
        return audit_history(
            connection, store="state_5.sqlite", expected=self.expected, **kwargs
        )

    def rejects(self, connection, code):
        before = connection.total_changes
        with self.assertRaisesRegex(ContractError, "^" + code + "$"):
            self.check(connection)
        self.assertEqual(connection.total_changes, before)

    def test_success_is_history_only_and_never_activation(self):
        connection = self.database(versions=(39, 38))
        self.assertEqual(
            self.check(connection),
            {
                "format_version": 1,
                "store": "state_5.sqlite",
                "matched_history_versions": [38, 39],
                "timing_unrecorded_versions": [],
                "activation_permitted": False,
            },
        )
        # SQLx skip() can record checksums without applying application DDL.
        self.assertEqual(
            connection.execute(
                "SELECT name FROM sqlite_schema WHERE name = 'threads'"
            ).fetchall(),
            [],
        )

    def test_negative_one_timing_is_not_dirty(self):
        connection = self.database(elapsed=-1)
        self.assertEqual(self.check(connection)["timing_unrecorded_versions"], [38, 39])
        connection.execute(
            "UPDATE _sqlx_migrations SET execution_time = ?", (-(1 << 63),)
        )
        self.assertEqual(self.check(connection)["timing_unrecorded_versions"], [])

    def test_every_sqlx_store_name_and_both_memory_generations(self):
        connection = self.database()
        for store in sorted(SQLX_STORES):
            with self.subTest(store=store):
                report = audit_history(connection, store=store, expected=self.expected)
                self.assertEqual(report["store"], store)
                self.assertIs(report["activation_permitted"], False)
        for store in (
            "agent_message_board_1.sqlite",
            "state_6.sqlite",
            "private://secret",
            [],
            None,
        ):
            with self.subTest(store=store):
                with self.assertRaisesRegex(ContractError, "^unsupported_sqlx_store$"):
                    audit_history(connection, store=store, expected=self.expected)

    def test_absent_optional_or_empty_history_is_not_success(self):
        missing = sqlite3.connect(":memory:")
        self.addCleanup(missing.close)
        self.rejects(missing, "missing_sqlx_history_table")
        self.rejects(self.database(versions=()), "sqlx_history_inventory_mismatch")
        self.rejects(self.database(versions=(38,)), "sqlx_history_inventory_mismatch")

    def test_unknown_future_migration_is_not_ignored(self):
        connection = self.database()
        connection.execute(
            "INSERT INTO _sqlx_migrations VALUES (999, '', '', 1, ?, 0)", (b"x" * 48,)
        )
        self.rejects(connection, "sqlx_history_inventory_mismatch")

    def test_known_migration_bytes_are_not_normalized(self):
        connection = self.database()
        for sql in (
            self.scripts[38] + b"\n",
            self.scripts[38].replace(b"\n", b"\r\n"),
            b"\xef\xbb\xbf" + self.scripts[38],
        ):
            with self.subTest(checksum=hashlib.sha384(sql).hexdigest()):
                connection.execute(
                    "UPDATE _sqlx_migrations SET checksum = ? WHERE version = 38",
                    (hashlib.sha384(sql).digest(),),
                )
                self.rejects(connection, "sqlx_history_checksum_mismatch")

    def test_dirty_and_non_boolean_success_are_rejected(self):
        for value in (0, 2, -1, 0.5, "private://secret"):
            with self.subTest(value=value):
                connection = self.database()
                connection.execute("UPDATE _sqlx_migrations SET success = ?", (value,))
                self.rejects(connection, "sqlx_history_not_successful")

    def test_checksum_type_and_length_are_not_coerced(self):
        connection = self.database()
        for value in (b"", b"x" * 32, b"x" * 49, self.expected[38].hex(), 38):
            with self.subTest(value_type=type(value).__name__):
                connection.execute("UPDATE _sqlx_migrations SET checksum = ?", (value,))
                self.rejects(connection, "invalid_checksum")
        connection.execute(
            "UPDATE _sqlx_migrations SET checksum = zeroblob(8 * 1024 * 1024)"
        )
        self.rejects(connection, "invalid_checksum")

    def test_version_and_elapsed_types_are_validated(self):
        for column in ("version", "execution_time"):
            for value in ("private://secret", 0.5):
                with self.subTest(column=column, value=value):
                    connection = self.database()
                    connection.execute(
                        f"UPDATE _sqlx_migrations SET {column} = ? WHERE version = 38",
                        (value,),
                    )
                    self.rejects(connection, "invalid_integer")
        connection = self.database()
        connection.execute(
            "UPDATE _sqlx_migrations SET version = NULL WHERE version = 38"
        )
        self.rejects(connection, "invalid_integer")

    def test_metadata_types_are_validated_without_reporting_values(self):
        for column in ("description", "installed_on"):
            with self.subTest(column=column):
                connection = self.database()
                connection.execute(
                    f"UPDATE _sqlx_migrations SET {column} = ?", (b"private secret",)
                )
                self.rejects(connection, "invalid_sqlx_history_metadata")

    def test_legacy_recency_is_reported_without_repairing_source(self):
        connection = self.database(versions=(38,))
        connection.execute(
            "UPDATE _sqlx_migrations SET checksum = ?", (self.expected[39],)
        )
        before = connection.execute("SELECT * FROM _sqlx_migrations").fetchall()
        self.rejects(connection, "legacy_recency_repair_required")
        self.assertEqual(
            connection.execute("SELECT * FROM _sqlx_migrations").fetchall(), before
        )
        connection.execute(
            "INSERT INTO _sqlx_migrations VALUES (39, '', '', 1, ?, 0)",
            (self.expected[39],),
        )
        self.rejects(connection, "sqlx_history_checksum_mismatch")

    def test_legacy_recency_detection_is_primary_store_and_source_specific(self):
        connection = self.database(versions=(38,))
        connection.execute(
            "UPDATE _sqlx_migrations SET checksum = ?", (self.expected[39],)
        )
        with self.assertRaisesRegex(ContractError, "^sqlx_history_inventory_mismatch$"):
            audit_history(connection, store="queue_1.sqlite", expected=self.expected)
        other_expected = {**self.expected, 39: b"x" * 48}
        with self.assertRaisesRegex(ContractError, "^sqlx_history_inventory_mismatch$"):
            audit_history(connection, store="state_5.sqlite", expected=other_expected)

    def test_expected_history_is_nonempty_bounded_and_strict(self):
        connection = self.database()
        bad = (
            {},
            [],
            {True: b"x" * 48},
            {0: b"x" * 48},
            {1 << 63: b"x" * 48},
            {1: b"x" * 32},
            {1: "a" * 96},
            {1: bytearray(48)},
            dict.fromkeys(range(1, MAX_MIGRATIONS + 2), b"x" * 48),
        )
        for expected in bad:
            with self.subTest(kind=type(expected).__name__, count=len(expected)):
                with self.assertRaises(ContractError):
                    audit_history(connection, store="state_5.sqlite", expected=expected)

    def test_invalid_connection_is_redacted(self):
        with self.assertRaisesRegex(ContractError, "^invalid_sqlx_connection$"):
            self.check("postgresql://private-secret")

        connection = self.database()
        connection.text_factory = bytes
        self.rejects(connection, "invalid_sqlx_connection")

    def test_record_budget_is_enforced(self):
        connection = self.database(versions=())
        connection.executemany(
            "INSERT INTO _sqlx_migrations VALUES (?, '', '', 1, ?, 0)",
            ((i, b"x" * 48) for i in range(1, MAX_MIGRATIONS + 2)),
        )
        self.rejects(connection, "sqlx_history_too_large")

    def test_schema_drift_and_views_are_rejected(self):
        connection = self.database()
        connection.execute("ALTER TABLE _sqlx_migrations ADD COLUMN extra TEXT")
        self.rejects(connection, "unsupported_sqlx_history_schema")
        connection.execute("ALTER TABLE _sqlx_migrations RENAME TO actual")
        connection.execute("CREATE VIEW _sqlx_migrations AS SELECT * FROM actual")
        self.rejects(connection, "missing_sqlx_history_table")

    def test_temporary_shadow_and_row_factory_cannot_replace_main_history(self):
        connection = self.database()
        connection.execute("CREATE TEMP TABLE _sqlx_migrations (unrelated TEXT)")
        connection.row_factory = lambda _cursor, _row: "not a database row"
        self.assertEqual(self.check(connection)["matched_history_versions"], [38, 39])
        self.assertIsNotNone(connection.row_factory)

    def test_connection_ownership_and_read_failures(self):
        connection = self.database()
        connection.execute(
            "UPDATE _sqlx_migrations SET description = 'caller transaction'"
        )
        self.assertTrue(connection.in_transaction)
        self.check(connection)
        self.assertTrue(connection.in_transaction)
        connection.rollback()
        self.assertEqual(
            connection.execute("SELECT description FROM _sqlx_migrations").fetchall(),
            [("private description",), ("private description",)],
        )
        connection.set_authorizer(lambda *_args: sqlite3.SQLITE_DENY)
        self.rejects(connection, "sqlx_history_read_failed")
        connection.set_authorizer(None)
        connection.close()
        with self.assertRaisesRegex(ContractError, "^sqlx_history_read_failed$"):
            self.check(connection)

    def test_actual_recency_sql_backfills_and_preserves_old_insert_semantics(self):
        connection = sqlite3.connect(":memory:")
        self.addCleanup(connection.close)
        connection.executescript("""
            CREATE TABLE threads (id TEXT PRIMARY KEY, updated_at INTEGER,
                updated_at_ms INTEGER, archived INTEGER, cwd TEXT, preview TEXT);
            INSERT INTO threads VALUES ('old', 2, 2123, 0, '/fixture', 'visible');
        """)
        connection.executescript(self.scripts[39].decode())
        connection.execute(
            "INSERT INTO threads VALUES ('writer', 3, NULL, 0, '/fixture', 'visible', 0, 0)"
        )
        self.assertEqual(
            connection.execute(
                "SELECT id, recency_at, recency_at_ms FROM threads ORDER BY id"
            ).fetchall(),
            [("old", 2, 2123), ("writer", 3, 3000)],
        )

    def test_crash_after_bookkeeping_commit_and_read_only_process_audit(self):
        # This emulates SQLx's committed bookkeeping shape; it does not run SQLx.
        writer = """
import os, sqlite3, sys
from storage_contract.test_sqlx_history import HISTORY_SCHEMA
c = sqlite3.connect(sys.argv[1])
c.executescript(HISTORY_SCHEMA)
c.execute("INSERT INTO _sqlx_migrations VALUES (38, 'private', '', 1, ?, -1)",
          (bytes.fromhex(sys.argv[2]),))
c.commit()
os._exit(7)
"""
        reader = """
import json, sqlite3, sys
from pathlib import Path
from storage_contract.sqlx_history import audit_history
c = sqlite3.connect(Path(sys.argv[1]).as_uri() + '?mode=ro', uri=True)
print(json.dumps(audit_history(c, store='state_5.sqlite',
                             expected={38: bytes.fromhex(sys.argv[2])})))
c.close()
"""
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "captured.sqlite"
            args = [str(path), self.expected[38].hex()]
            first = subprocess.run(
                [sys.executable, "-c", writer, *args],
                cwd=ROOT / "scripts",
                capture_output=True,
                timeout=15,
            )
            self.assertEqual(
                (first.returncode, first.stdout, first.stderr), (7, b"", b"")
            )
            before = hashlib.sha256(path.read_bytes()).digest()
            result = subprocess.run(
                [sys.executable, "-c", reader, *args],
                cwd=ROOT / "scripts",
                capture_output=True,
                timeout=15,
            )
            self.assertEqual((result.returncode, result.stderr), (0, b""))
            self.assertEqual(
                json.loads(result.stdout),
                {
                    "format_version": 1,
                    "store": "state_5.sqlite",
                    "matched_history_versions": [38],
                    "timing_unrecorded_versions": [38],
                    "activation_permitted": False,
                },
            )
            self.assertEqual(hashlib.sha256(path.read_bytes()).digest(), before)
