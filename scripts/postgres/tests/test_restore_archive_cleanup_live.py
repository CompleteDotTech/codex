"""Source cleanup after uncertain setup outcomes, against an isolated server."""

import json
import os
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import restore_archive_cases as cases
import qualification_mutations as mutations
from qualification_checks import sql
from state import ServiceError


@unittest.skipUnless(
    os.environ.get("CODEX_TEST_POSTGRES_STATE"), "requires PostgreSQL fixture"
)
class RestoreArchiveCleanupLiveTests(unittest.TestCase):
    def test_setup_failure_preserves_source_for_committed_and_rolled_back_changes(self):
        source = Path(os.environ["CODEX_TEST_POSTGRES_STATE"])
        snapshot = (
            "SELECT json_build_object('relations', "
            "(SELECT json_agg(row_to_json(r) ORDER BY relname) FROM "
            "(SELECT relname, relpersistence FROM pg_class WHERE "
            "relnamespace='codex_storage'::regnamespace AND relkind='r') r), "
            "'constraints', (SELECT json_agg(pg_get_constraintdef(oid) ORDER BY conname) "
            "FROM pg_constraint WHERE connamespace='codex_storage'::regnamespace), "
            "'metadata', (SELECT json_agg(m ORDER BY singleton) "
            "FROM codex_storage.codex_schema_meta m), 'history', "
            "(SELECT json_agg(h ORDER BY version) FROM codex_storage._codex_pg_migrations h))::text"
        )
        before = json.loads(sql(source, snapshot))
        self.assertIsNotNone(before["metadata"])
        self.assertIsNotNone(before["history"])
        self.assertEqual(
            sql(
                source,
                "SELECT to_regclass('codex_storage.restore_cleanup_probe') IS NULL",
            ),
            "t",
        )
        relation = "codex_storage._codex_pg_migrations"
        for setup, cleanup in (
            (
                f"ALTER TABLE {relation} RENAME TO restore_cleanup_probe",
                "ALTER TABLE codex_storage.restore_cleanup_probe RENAME TO _codex_pg_migrations",
            ),
            (
                f"ALTER TABLE {relation} DROP CONSTRAINT _codex_pg_migrations_pkey",
                f"ALTER TABLE {relation} ADD CONSTRAINT _codex_pg_migrations_pkey PRIMARY KEY (version)",
            ),
            (
                f"ALTER TABLE {relation} SET UNLOGGED",
                f"ALTER TABLE {relation} SET LOGGED",
            ),
        ):
            for outcome in ("committed", "rolled_back"):
                calls = []

                def uncertain_sql(home, statement):
                    calls.append(statement)
                    if len(calls) == 1:
                        if outcome == "rolled_back":
                            # PostgreSQL must execute the mutation, then abort
                            # before COMMIT, to exercise the other uncertain case.
                            with self.assertRaises(ServiceError):
                                sql(
                                    home,
                                    statement.replace(
                                        "; COMMIT", "; SELECT 1 / 0; COMMIT"
                                    ),
                                )
                        else:
                            sql(home, statement)
                        raise ServiceError("setup_result_lost")
                    return sql(home, statement)

                with self.subTest(setup=setup, outcome=outcome):
                    with (
                        patch.object(mutations, "sql", side_effect=uncertain_sql),
                        patch.object(cases, "checked_backup") as backup,
                        self.assertRaisesRegex(ServiceError, "setup_result_lost"),
                        cases.mutated_archive(source, setup, cleanup),
                    ):
                        self.fail("uncertain setup must not publish an archive")
                    backup.assert_not_called()
                    self.assertEqual(len(calls), 2)
                    self.assertEqual(json.loads(sql(source, snapshot)), before)
