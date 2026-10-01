"""Exercise routine proxies and effective role edges on a restored fixture."""

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
class RestoreRoutineAccessLiveTests(unittest.TestCase):
    def test_definer_functions_procedures_and_aliases_are_rejected(self):
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
            sql(
                destination,
                "SELECT to_regnamespace('codex_restore_routine') IS NULL AND NOT EXISTS "
                "(SELECT 1 FROM pg_roles WHERE rolname='codex_restore_routine_alias')",
            ),
            "t",
        )
        write_body = (
            "RETURNS integer LANGUAGE plpgsql SECURITY DEFINER AS $$BEGIN "
            "EXECUTE 'UPDATE codex_storage.codex_schema_meta SET format_version=2'; "
            "RETURN 2; END$$"
        )
        for owner, caller, kind, definition, invoke, expected, alias in (
            (
                "codex_owner",
                "runtime",
                "FUNCTION",
                write_body,
                "SELECT codex_restore_routine.proxy()",
                "2",
                False,
            ),
            (
                "postgres",
                "runtime",
                "PROCEDURE",
                "LANGUAGE SQL SECURITY DEFINER AS $$UPDATE codex_storage.codex_schema_meta SET format_version=2$$",
                "CALL codex_restore_routine.proxy(); SELECT format_version FROM codex_storage.codex_schema_meta",
                "2",
                False,
            ),
            (
                "codex_backup",
                "runtime",
                "FUNCTION",
                "RETURNS bigint LANGUAGE SQL SECURITY DEFINER AS $$SELECT count(*) FROM codex_storage._codex_pg_migrations$$",
                "SELECT codex_restore_routine.proxy()",
                str(len(before["history"])),
                False,
            ),
            (
                "codex_owner",
                "backup",
                "FUNCTION",
                write_body,
                "SELECT codex_restore_routine.proxy()",
                "2",
                False,
            ),
            (
                "codex_owner",
                "runtime",
                "FUNCTION",
                write_body,
                "SELECT codex_restore_routine.proxy()",
                "2",
                True,
            ),
            (
                "codex_owner",
                "backup",
                "FUNCTION",
                write_body,
                "SELECT codex_restore_routine.proxy()",
                "2",
                True,
            ),
        ):
            with self.subTest(owner=owner, caller=caller, kind=kind, alias=alias):
                grantee = "codex_restore_routine_alias" if alias else f"codex_{caller}"
                setup = (
                    "CREATE SCHEMA codex_restore_routine; "
                    f"GRANT USAGE, CREATE ON SCHEMA codex_restore_routine TO {owner}; "
                    f"CREATE {kind} codex_restore_routine.proxy() {definition}; "
                    f"ALTER {kind} codex_restore_routine.proxy() OWNER TO {owner}; "
                    f"REVOKE ALL ON {kind} codex_restore_routine.proxy() FROM PUBLIC; "
                )
                if alias:
                    setup += (
                        "CREATE ROLE codex_restore_routine_alias; "
                        f"GRANT codex_restore_routine_alias TO codex_{caller} WITH INHERIT FALSE, SET TRUE; "
                    )
                setup += (
                    f"GRANT USAGE ON SCHEMA codex_restore_routine TO {grantee}; "
                    f"GRANT EXECUTE ON {kind} codex_restore_routine.proxy() TO {grantee}"
                )
                cleanup = (
                    f"DROP {kind} IF EXISTS codex_restore_routine.proxy(); "
                    "DROP SCHEMA IF EXISTS codex_restore_routine"
                )
                if alias:
                    cleanup += "; DROP ROLE IF EXISTS codex_restore_routine_alias"
                with temporary_sql(destination, setup, cleanup):
                    sql(
                        destination,
                        "UPDATE codex_storage.codex_schema_meta SET format_version=2",
                        role=caller,
                        expected_sqlstate="42501",
                    )
                    assume = (
                        "SET LOCAL ROLE codex_restore_routine_alias; " if alias else ""
                    )
                    self.assertEqual(
                        sql(
                            destination,
                            f"BEGIN; {assume}{invoke}; ROLLBACK",
                            role=caller,
                        ),
                        expected,
                    )
                    sql(destination, guard, expected_sqlstate="42501")
                    sql(
                        destination,
                        f"REVOKE EXECUTE ON {kind} codex_restore_routine.proxy() FROM {grantee}",
                    )
                    sql(destination, guard, expected_sqlstate="00000")
                self.assertEqual(json.loads(sql(destination, rows)), before)

    def test_owner_graph_ignores_inert_edges_without_weakening_backup_policy(self):
        destination = Path(os.environ["CODEX_TEST_POSTGRES_FOCUSED_RESTORE_STATE"])
        guard = (
            (Path(__file__).resolve().parents[1] / "container/restore-access.sql")
            .read_text(encoding="utf-8")
            .split("ALTER DEFAULT PRIVILEGES", 1)[0]
        )
        self.assertEqual(
            sql(
                destination,
                "SELECT count(*) FROM pg_roles WHERE rolname IN ('codex_restore_inert_sink','codex_restore_inert_admin')",
            ),
            "0",
        )
        for target, edge, administrator, expected in (
            (
                "codex_owner",
                "ADMIN FALSE, INHERIT FALSE, SET FALSE",
                "codex_restore_inert_admin",
                "00000",
            ),
            (
                "codex_owner",
                "ADMIN TRUE, INHERIT FALSE, SET FALSE",
                "codex_restore_inert_admin",
                "42501",
            ),
            (
                "codex_owner",
                "ADMIN FALSE, INHERIT FALSE, SET TRUE",
                "codex_restore_inert_admin",
                "42501",
            ),
            # Restore also promises read-only backup access to ordinary archives.
            # Its dedicated backup role never administers other roles.
            (
                "codex_runtime",
                "ADMIN FALSE, INHERIT FALSE, SET FALSE",
                "codex_backup",
                "42501",
            ),
        ):
            with self.subTest(target=target, edge=edge, administrator=administrator):
                with temporary_sql(
                    destination,
                    "CREATE ROLE codex_restore_inert_sink; CREATE ROLE codex_restore_inert_admin; "
                    f"GRANT {target} TO codex_restore_inert_sink WITH {edge}; "
                    f"GRANT codex_restore_inert_sink TO {administrator} WITH ADMIN TRUE, INHERIT FALSE, SET FALSE",
                    "DROP ROLE IF EXISTS codex_restore_inert_admin; DROP ROLE IF EXISTS codex_restore_inert_sink",
                ):
                    sql(destination, guard, expected_sqlstate=expected)
        sql(destination, guard, expected_sqlstate="00000")
