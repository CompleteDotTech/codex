"""Exercise indirect privileges against a successfully restored fixture."""

import json
import os
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from qualification_checks import sql
from qualification_mutations import temporary_sql


@unittest.skipUnless(
    os.environ.get("CODEX_TEST_POSTGRES_FOCUSED_RESTORE_STATE"),
    "requires the restored focused fixture",
)
class RestoreIndirectAccessLiveTests(unittest.TestCase):
    def test_admin_memberships_and_external_views_are_rejected(self):
        destination = Path(os.environ["CODEX_TEST_POSTGRES_FOCUSED_RESTORE_STATE"])
        guard = (
            (Path(__file__).resolve().parents[1] / "container/restore-access.sql")
            .read_text(encoding="utf-8")
            .split("ALTER DEFAULT PRIVILEGES", 1)[0]
        )
        rows = (
            "SELECT json_build_object('metadata', (SELECT json_agg(m ORDER BY singleton) "
            "FROM codex_storage.codex_schema_meta m), 'history', (SELECT json_agg(h ORDER BY version) "
            "FROM codex_storage._codex_pg_migrations h))::text"
        )
        before = json.loads(sql(destination, rows))
        self.assertIsNotNone(before["metadata"])
        self.assertIsNotNone(before["history"])
        self.assertEqual(
            sql(
                destination,
                "SELECT to_regnamespace('codex_restore_view') IS NULL AND NOT EXISTS "
                "(SELECT 1 FROM pg_roles WHERE rolname='codex_restore_admin_bridge')",
            ),
            "t",
        )
        for setup, cleanup in (
            (
                "GRANT codex_runtime TO codex_backup WITH ADMIN TRUE, INHERIT FALSE, SET FALSE",
                "REVOKE codex_runtime FROM codex_backup",
            ),
            (
                "CREATE ROLE codex_restore_admin_bridge; "
                "GRANT codex_runtime TO codex_restore_admin_bridge WITH INHERIT FALSE, SET TRUE; "
                "GRANT codex_restore_admin_bridge TO codex_backup WITH ADMIN TRUE, INHERIT FALSE, SET FALSE",
                "DROP ROLE IF EXISTS codex_restore_admin_bridge",
            ),
            (
                "CREATE ROLE codex_restore_admin_bridge; "
                "GRANT codex_restore_admin_bridge TO codex_backup WITH INHERIT FALSE, SET TRUE; "
                "GRANT codex_runtime TO codex_restore_admin_bridge WITH ADMIN TRUE, INHERIT FALSE, SET FALSE",
                "DROP ROLE IF EXISTS codex_restore_admin_bridge",
            ),
        ):
            with temporary_sql(destination, setup, cleanup):
                self.assertEqual(
                    sql(
                        destination,
                        "SELECT pg_has_role('codex_backup','codex_runtime','USAGE'), "
                        "pg_has_role('codex_backup','codex_runtime','SET')",
                    ),
                    "f|f",
                )
                sql(destination, guard, expected_sqlstate="42501")

        with temporary_sql(
            destination,
            "SET LOCAL ROLE codex_owner; CREATE SCHEMA codex_restore_view; "
            "GRANT USAGE ON SCHEMA codex_restore_view TO codex_runtime; "
            "CREATE VIEW codex_restore_view.direct AS SELECT * FROM codex_storage.codex_schema_meta; "
            "CREATE VIEW codex_restore_view.nested AS SELECT * FROM codex_restore_view.direct; "
            "GRANT UPDATE(format_version), SELECT ON codex_restore_view.nested TO codex_runtime",
            "DROP VIEW IF EXISTS codex_restore_view.nested; "
            "DROP VIEW IF EXISTS codex_restore_view.direct; DROP SCHEMA IF EXISTS codex_restore_view",
        ):
            sql(
                destination,
                "UPDATE codex_storage.codex_schema_meta SET format_version=2",
                role="runtime",
                expected_sqlstate="42501",
            )
            self.assertEqual(
                sql(
                    destination,
                    "BEGIN; UPDATE codex_restore_view.nested SET format_version=2 "
                    "RETURNING format_version; ROLLBACK",
                    role="runtime",
                ),
                "2",
            )
            sql(destination, guard, expected_sqlstate="42501")
        self.assertEqual(json.loads(sql(destination, rows)), before)
