"""Test retained host records with an incomplete, pinned source-SQL subset.

This is not a primary-store policy, full migration prefix, or runtime adapter test.
"""

import hashlib
import json
import os
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from contextlib import closing
from pathlib import Path
from unittest import mock

from .records import ContractError, encode_row
from .sqlite_snapshot import audit_snapshot

ASSETS = Path(__file__).parent / "fixtures" / "host_local"
# Independently read at fork/main 395c622d; never infer pins from fixture bytes.
PINS = {
    "0001_threads.sql": "7063ce11a45be508749bfe70de23bc64ac2a03d0",
    "0024_remote_control_enrollments.sql": "970db9ef201072387e87e063ff7e7731342f83e8",
    "0037_remote_control_enrollments_enabled.sql": "abb422384ea578892ff9875c356f43f787b752d1",
    "0038_external_agent_config_imports.sql": "74ae0435f83536a17abf24b663d3be9264aebd2c",
    "0044_external_agent_config_imports_provider_id.sql": "2fc16008a9dcd4d806173d91aaf1fdde24bb9c73",
}
HOST_MARKER = "fixture-host-only:"
CHILD = """
import json
import sys
from pathlib import Path
from storage_contract.records import ContractError
from storage_contract.sqlite_snapshot import audit_snapshot
try:
    with open(sys.argv[1], "rb") as source:
        result = audit_snapshot(source, sys.argv[2], Path(sys.argv[3]).read_bytes())
except ContractError as error:
    print(json.dumps({"error": str(error)}))
    sys.exit(2)
print(json.dumps(result, sort_keys=True))
"""


def create_schema(connection: sqlite3.Connection, *, legacy: bool = False) -> None:
    for name, expected in PINS.items():
        if legacy and name.startswith("0044_"):
            continue
        # Git's Windows checkout may write CRLF; authenticate the LF Git blob.
        data = (ASSETS / name).read_bytes().replace(b"\r\n", b"\n")
        header = b"blob " + str(len(data)).encode("ascii") + b"\0"
        if hashlib.sha1(header + data).hexdigest() != expected:
            raise AssertionError("source fixture Git blob mismatch")
        connection.executescript(data.decode("utf-8"))


def trusted_fixture_policy(*, legacy: bool = False) -> bytes:
    # Policy comes from independently pinned SQL, never from the input database.
    with closing(sqlite3.connect(":memory:")) as connection:
        create_schema(connection, legacy=legacy)
        rows = connection.execute(
            "SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name"
        ).fetchall()
    digest = hashlib.sha256(b"CDTX-sqlite-schema-v1\0")
    for ordinal, row in enumerate(rows, 1):
        record = encode_row(ordinal.to_bytes(8, "big"), row)
        digest.update(len(record).to_bytes(8, "big") + record)
    digest.update(len(rows).to_bytes(8, "big"))
    return json.dumps(
        {
            "version": 1,
            "schema_sha256": digest.hexdigest(),
            "tables": {
                "threads": "migrate",
                "remote_control_enrollments": "retain",
                "external_agent_config_imports": "retain",
            },
        }
    ).encode("utf-8")


class HostLocalFixtureTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory(prefix="codex-host-local-fixture-")
        self.addCleanup(directory.cleanup)
        self.path = Path(directory.name) / "subset.sqlite"
        self.policy = trusted_fixture_policy()
        with closing(sqlite3.connect(self.path)) as connection:
            create_schema(connection)
            connection.execute(
                "INSERT INTO threads (id,rollout_path,created_at,updated_at,source,"
                "model_provider,cwd,title,sandbox_policy,approval_mode,tokens_used) "
                "VALUES (?,?,?,?,?,?,?,?,?,?,?)",
                (
                    "thread-1",
                    "sessions/fixture.jsonl",
                    1,
                    2,
                    "cli",
                    "fixture",
                    "/tmp/p",
                    "portable control",
                    "{}",
                    "never",
                    (1 << 53) + 7,
                ),
            )
            connection.execute(
                "INSERT INTO remote_control_enrollments (websocket_url,account_id,"
                "app_server_client_name,server_id,environment_id,server_name,updated_at) "
                "VALUES (?,?,?,?,?,?,?)",
                (
                    "wss://fixture.invalid",
                    HOST_MARKER + "account",
                    "client-a",
                    HOST_MARKER + "server",
                    "environment-a",
                    "host-a",
                    17,
                ),
            )
            # Match the Rust record fields, but do not claim Rust deserialization.
            successes = [
                {
                    "item_type": "fixture",
                    "cwd": r"C:\host-only\workspace",
                    "source": r"C:\host-only\config.toml",
                    "target": None,
                    "title": None,
                }
            ]
            failures = [
                {
                    "item_type": "fixture",
                    "error_type": None,
                    "sub_error_type": None,
                    "failure_stage": "fixture",
                    "message": HOST_MARKER + "failure",
                    "cwd": "/host-only/workspace",
                    "source": "/host-only/config.toml",
                }
            ]
            connection.execute(
                "INSERT INTO external_agent_config_imports "
                "(import_id,completed_at_ms,successes,failures) VALUES (?,?,?,?)",
                (
                    HOST_MARKER + "import",
                    1700000000123,
                    json.dumps(successes),
                    json.dumps(failures),
                ),
            )
            connection.commit()

    def change(self, sql, parameters=()):
        with closing(sqlite3.connect(self.path)) as connection:
            connection.execute(sql, parameters)
            connection.commit()

    def read_one(self, sql):
        with closing(sqlite3.connect(self.path)) as connection:
            return connection.execute(sql).fetchone()

    def audit(self, policy=None):
        before = self.path.read_bytes()
        try:
            with self.path.open("rb") as source:
                result = audit_snapshot(
                    source,
                    hashlib.sha256(before).hexdigest(),
                    self.policy if policy is None else policy,
                )
                self.assertFalse(source.closed)
            return result
        finally:
            self.assertEqual(self.path.read_bytes(), before)

    def tables(self, result):
        return {table["table"]: table for table in result["tables"]}

    def child(self, expected):
        policy_path = self.path.with_suffix(".policy.json")
        policy_path.write_bytes(self.policy)
        environment = dict(os.environ)
        environment["PYTHONPATH"] = str(Path(__file__).resolve().parents[1])
        return subprocess.run(
            [sys.executable, "-c", CHILD, str(self.path), expected, str(policy_path)],
            env=environment,
            capture_output=True,
            text=True,
            timeout=20,
            check=False,
        )

    def test_retained_rows_are_counted_without_value_fingerprints(self):
        result = self.audit()
        tables = self.tables(result)
        for name in ("remote_control_enrollments", "external_agent_config_imports"):
            self.assertEqual(
                tables[name],
                {
                    "table": name,
                    "treatment": "retain",
                    "rows": 1,
                    "logical_sha256": None,
                },
            )
        self.assertEqual(tables["threads"]["rows"], 1)
        self.assertRegex(tables["threads"]["logical_sha256"], r"^[0-9a-f]{64}$")
        self.assertFalse(result["activation_permitted"])
        self.assertNotIn(HOST_MARKER, json.dumps(result))

    def test_windows_checkout_line_endings_preserve_pinned_schema(self):
        with tempfile.TemporaryDirectory() as directory:
            assets = Path(directory)
            for name in PINS:
                data = (ASSETS / name).read_bytes().replace(b"\r\n", b"\n")
                (assets / name).write_bytes(data.replace(b"\n", b"\r\n"))
            with mock.patch(__name__ + ".ASSETS", assets):
                self.assertEqual(trusted_fixture_policy(), self.policy)

    def test_host_values_never_enter_typed_row_encoding(self):
        with mock.patch(
            "storage_contract.sqlite_snapshot.encode_row", wraps=encode_row
        ) as encoder:
            self.audit()
        encoded_arguments = repr(encoder.call_args_list)
        self.assertIn("portable control", encoded_arguments)
        self.assertNotIn(HOST_MARKER, encoded_arguments)
        self.assertNotIn("host-only", encoded_arguments)

    def test_host_changes_change_file_digest_not_portable_rows(self):
        before = self.audit()
        self.change(
            "UPDATE remote_control_enrollments SET server_name=?",
            (HOST_MARKER + "changed",),
        )
        self.change("UPDATE external_agent_config_imports SET failures=?", ("[]",))
        after = self.audit()
        self.assertNotEqual(before["file_sha256"], after["file_sha256"])
        self.assertEqual(before["tables"], after["tables"])

    def test_portable_control_row_changes_are_detected(self):
        before = self.tables(self.audit())["threads"]["logical_sha256"]
        self.change("UPDATE threads SET title=?", ("changed e\u0301",))
        after = self.tables(self.audit())["threads"]["logical_sha256"]
        self.assertNotEqual(before, after)

    def test_integer_precision_survives_reopen_and_affects_digest(self):
        self.assertEqual(
            self.read_one("SELECT tokens_used FROM threads"), ((1 << 53) + 7,)
        )
        before = self.tables(self.audit())["threads"]["logical_sha256"]
        self.change("UPDATE threads SET tokens_used=?", ((1 << 53) + 8,))
        self.assertNotEqual(
            before, self.tables(self.audit())["threads"]["logical_sha256"]
        )

    def test_enrollment_enabled_preserves_null_false_and_true(self):
        query = "SELECT remote_control_enabled FROM remote_control_enrollments"
        self.assertEqual(self.read_one(query), (None,))
        for value in (0, 1, None):
            with self.subTest(value=value):
                self.change(
                    "UPDATE remote_control_enrollments SET remote_control_enabled=?",
                    (value,),
                )
                self.assertEqual(self.read_one(query), (value,))
                self.audit()

    def test_composite_enrollment_identity_is_not_account_only(self):
        columns = (
            "websocket_url,account_id,app_server_client_name,server_id,"
            "environment_id,server_name,updated_at"
        )
        with self.assertRaises(sqlite3.IntegrityError):
            self.change(
                f"INSERT INTO remote_control_enrollments ({columns}) "
                f"SELECT {columns} FROM remote_control_enrollments"
            )
        self.change(
            f"INSERT INTO remote_control_enrollments ({columns}) "
            "SELECT websocket_url,account_id,?,server_id,environment_id,"
            "server_name,updated_at FROM remote_control_enrollments",
            ("client-b",),
        )
        rows = self.tables(self.audit())["remote_control_enrollments"]["rows"]
        self.assertEqual(rows, 2)

    def test_provider_identity_preserves_null_and_present_values(self):
        query = "SELECT provider_id FROM external_agent_config_imports"
        self.assertEqual(self.read_one(query), (None,))
        before = self.audit()
        self.change(
            "UPDATE external_agent_config_imports SET provider_id=?",
            (HOST_MARKER + "provider",),
        )
        self.assertEqual(self.read_one(query), (HOST_MARKER + "provider",))
        after = self.audit()
        self.assertNotEqual(before["file_sha256"], after["file_sha256"])
        self.assertEqual(before["tables"], after["tables"])

    def test_legacy_policy_rejects_provider_schema_instead_of_silently_dropping_it(
        self,
    ):
        with self.assertRaisesRegex(ContractError, "^snapshot_schema_mismatch$"):
            self.audit(trusted_fixture_policy(legacy=True))

    def test_unknown_primary_table_cannot_be_silently_ignored(self):
        self.change("CREATE TABLE unclassified_primary_data (payload TEXT)")
        with self.assertRaisesRegex(ContractError, "^snapshot_schema_mismatch$"):
            self.audit()

    def test_changed_host_schema_is_rejected(self):
        self.change("ALTER TABLE external_agent_config_imports ADD COLUMN extra TEXT")
        with self.assertRaisesRegex(ContractError, "^snapshot_schema_mismatch$"):
            self.audit()

    def test_omitted_retained_table_is_rejected(self):
        policy = json.loads(self.policy)
        del policy["tables"]["external_agent_config_imports"]
        with self.assertRaisesRegex(ContractError, "^table_inventory_mismatch$"):
            self.audit(json.dumps(policy).encode())

    def test_separate_process_verifies_without_modifying_backup(self):
        before = self.path.read_bytes()
        process = self.child(hashlib.sha256(before).hexdigest())
        self.assertEqual(process.returncode, 0, process.stderr)
        self.assertEqual(json.loads(process.stdout), self.audit())
        self.assertEqual(self.path.read_bytes(), before)
        self.assertNotIn(HOST_MARKER, process.stdout + process.stderr)

    def test_stale_receipt_is_rejected_in_separate_process(self):
        old_digest = hashlib.sha256(self.path.read_bytes()).hexdigest()
        self.change(
            "UPDATE remote_control_enrollments SET server_name=?",
            (HOST_MARKER + "new",),
        )
        before = self.path.read_bytes()
        process = self.child(old_digest)
        self.assertEqual(process.returncode, 2, process.stderr)
        self.assertEqual(
            json.loads(process.stdout), {"error": "snapshot_digest_mismatch"}
        )
        self.assertEqual(self.path.read_bytes(), before)
        self.assertNotIn(HOST_MARKER, process.stdout + process.stderr)
