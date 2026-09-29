"""Exercise trigger and nested definer routes into protected storage."""

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
class RestoreTransitiveDefinerLiveTests(unittest.TestCase):
    def setUp(self):
        self.destination = Path(os.environ["CODEX_TEST_POSTGRES_FOCUSED_RESTORE_STATE"])
        self.guard = (
            (Path(__file__).resolve().parents[1] / "container/restore-access.sql")
            .read_text(encoding="utf-8")
            .split("ALTER DEFAULT PRIVILEGES", 1)[0]
        )

    def test_writable_table_trigger_fires_without_runtime_execute(self):
        destination = self.destination
        self.assertEqual(
            sql(destination, "SELECT to_regnamespace('codex_restore_trigger') IS NULL"),
            "t",
        )
        setup = (
            "CREATE SCHEMA codex_restore_trigger; "
            "CREATE TABLE codex_storage.restore_trigger_probe (id integer); "
            "GRANT INSERT ON codex_storage.restore_trigger_probe TO codex_runtime; "
            "CREATE FUNCTION codex_restore_trigger.fire() RETURNS trigger "
            "LANGUAGE plpgsql SECURITY DEFINER AS $$BEGIN "
            "UPDATE codex_storage.codex_schema_meta SET format_version=2; "
            "RETURN NEW; END$$; "
            "REVOKE ALL ON FUNCTION codex_restore_trigger.fire() FROM PUBLIC, codex_runtime; "
            "CREATE TRIGGER restore_probe BEFORE INSERT ON codex_storage.restore_trigger_probe "
            "FOR EACH ROW EXECUTE FUNCTION codex_restore_trigger.fire()"
        )
        cleanup = (
            "DROP TABLE IF EXISTS codex_storage.restore_trigger_probe; "
            "DROP SCHEMA IF EXISTS codex_restore_trigger CASCADE"
        )
        with temporary_sql(destination, setup, cleanup):
            self.assertEqual(
                sql(
                    destination,
                    "SELECT has_function_privilege('codex_runtime', "
                    "'codex_restore_trigger.fire()'::regprocedure, 'EXECUTE')",
                ),
                "f",
            )
            self.assertEqual(
                sql(
                    destination,
                    "BEGIN; INSERT INTO codex_storage.restore_trigger_probe VALUES (1); "
                    "SELECT format_version FROM codex_storage.codex_schema_meta; ROLLBACK",
                    role="runtime",
                ),
                "2",
            )
            sql(destination, self.guard, expected_sqlstate="42501")
            sql(
                destination,
                "DROP TRIGGER restore_probe ON codex_storage.restore_trigger_probe",
            )
            sql(destination, self.guard, expected_sqlstate="00000")

    def test_callable_definer_reaches_privileged_nested_definer(self):
        destination = self.destination
        self.assertEqual(
            sql(
                destination,
                "SELECT to_regnamespace('codex_restore_nested') IS NULL AND NOT EXISTS "
                "(SELECT 1 FROM pg_roles WHERE rolname='codex_restore_middle_owner')",
            ),
            "t",
        )
        setup = (
            "CREATE ROLE codex_restore_middle_owner; "
            "CREATE SCHEMA codex_restore_nested; "
            "GRANT USAGE, CREATE ON SCHEMA codex_restore_nested "
            "TO codex_owner, codex_restore_middle_owner; "
            "GRANT USAGE ON SCHEMA codex_restore_nested TO codex_runtime; "
            "CREATE FUNCTION codex_restore_nested.inner_proxy() RETURNS integer "
            "LANGUAGE plpgsql SECURITY DEFINER AS $$BEGIN "
            "UPDATE codex_storage.codex_schema_meta SET format_version=2; "
            "RETURN 2; END$$; "
            "ALTER FUNCTION codex_restore_nested.inner_proxy() OWNER TO codex_owner; "
            "REVOKE ALL ON FUNCTION codex_restore_nested.inner_proxy() FROM PUBLIC; "
            "GRANT EXECUTE ON FUNCTION codex_restore_nested.inner_proxy() "
            "TO codex_restore_middle_owner; "
            "CREATE FUNCTION codex_restore_nested.middle_proxy() RETURNS integer "
            "LANGUAGE plpgsql SECURITY DEFINER AS $$BEGIN "
            "PERFORM codex_restore_nested.inner_proxy(); RETURN 2; END$$; "
            "ALTER FUNCTION codex_restore_nested.middle_proxy() "
            "OWNER TO codex_restore_middle_owner; "
            "REVOKE ALL ON FUNCTION codex_restore_nested.middle_proxy() FROM PUBLIC; "
            "GRANT EXECUTE ON FUNCTION codex_restore_nested.middle_proxy() TO codex_runtime"
        )
        cleanup = (
            "DROP SCHEMA IF EXISTS codex_restore_nested CASCADE; "
            "DROP ROLE IF EXISTS codex_restore_middle_owner"
        )
        with temporary_sql(destination, setup, cleanup):
            self.assertEqual(
                sql(
                    destination,
                    "SELECT has_function_privilege('codex_runtime', "
                    "'codex_restore_nested.inner_proxy()'::regprocedure, 'EXECUTE')",
                ),
                "f",
            )
            self.assertEqual(
                sql(
                    destination,
                    "BEGIN; SELECT codex_restore_nested.middle_proxy(); "
                    "SELECT format_version FROM codex_storage.codex_schema_meta; ROLLBACK",
                    role="runtime",
                ),
                "2\n2",
            )
            sql(destination, self.guard, expected_sqlstate="42501")
            sql(
                destination,
                "REVOKE EXECUTE ON FUNCTION codex_restore_nested.inner_proxy() "
                "FROM codex_restore_middle_owner",
            )
            sql(destination, self.guard, expected_sqlstate="00000")
