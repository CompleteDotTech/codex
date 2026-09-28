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

    def test_other_view_owners_and_non_view_rules_are_rejected(self):
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
        self.assertEqual(
            sql(destination, "SELECT to_regnamespace('codex_restore_proxy') IS NULL"),
            "t",
        )
        for setup, object_kind, probe, expected in (
            (
                "CREATE SCHEMA codex_restore_proxy; "
                "GRANT USAGE ON SCHEMA codex_restore_proxy TO codex_runtime; "
                "GRANT USAGE, CREATE ON SCHEMA codex_restore_proxy TO codex_backup; "
                "CREATE VIEW codex_restore_proxy.proxy AS SELECT version FROM codex_storage._codex_pg_migrations; "
                "ALTER VIEW codex_restore_proxy.proxy OWNER TO codex_backup; "
                "GRANT SELECT ON codex_restore_proxy.proxy TO codex_runtime",
                "VIEW",
                "SELECT count(*) FROM codex_restore_proxy.proxy",
                str(len(before["history"])),
            ),
            (
                "CREATE SCHEMA codex_restore_proxy; "
                "GRANT USAGE ON SCHEMA codex_restore_proxy TO codex_runtime; "
                "CREATE VIEW codex_restore_proxy.proxy AS SELECT * FROM codex_storage.codex_schema_meta; "
                "GRANT SELECT, UPDATE ON codex_restore_proxy.proxy TO codex_runtime",
                "VIEW",
                "BEGIN; UPDATE codex_restore_proxy.proxy SET format_version=2 RETURNING format_version; ROLLBACK",
                "2",
            ),
            (
                "SET LOCAL ROLE codex_owner; CREATE SCHEMA codex_restore_proxy; "
                "GRANT USAGE ON SCHEMA codex_restore_proxy TO codex_runtime; "
                "CREATE TABLE codex_restore_proxy.proxy (id integer); "
                "INSERT INTO codex_restore_proxy.proxy VALUES (1); "
                "CREATE RULE proxy_update AS ON UPDATE TO codex_restore_proxy.proxy "
                "DO ALSO UPDATE codex_storage.codex_schema_meta SET format_version=2; "
                "GRANT SELECT, UPDATE ON codex_restore_proxy.proxy TO codex_runtime",
                "TABLE",
                "BEGIN; UPDATE codex_restore_proxy.proxy SET id=2; "
                "SELECT format_version FROM codex_storage.codex_schema_meta; ROLLBACK",
                "2",
            ),
        ):
            with temporary_sql(
                destination,
                setup,
                f"DROP {object_kind} IF EXISTS codex_restore_proxy.proxy; "
                "DROP SCHEMA IF EXISTS codex_restore_proxy",
            ):
                for forbidden in (
                    "SELECT version FROM codex_storage._codex_pg_migrations",
                    "UPDATE codex_storage.codex_schema_meta SET format_version=2",
                ):
                    sql(
                        destination,
                        forbidden,
                        role="runtime",
                        expected_sqlstate="42501",
                    )
                self.assertEqual(sql(destination, probe, role="runtime"), expected)
                sql(destination, guard, expected_sqlstate="42501")
            self.assertEqual(json.loads(sql(destination, rows)), before)
