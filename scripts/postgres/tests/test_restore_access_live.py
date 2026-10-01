"""Focused transactional restore checks on two isolated PostgreSQL fixtures."""

import json
import os
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from qualification_checks import checked_backup, command, sql
from qualification_mutations import temporary_sql


@unittest.skipUnless(
    os.environ.get("CODEX_TEST_POSTGRES_STATE")
    and os.environ.get("CODEX_TEST_POSTGRES_FOCUSED_RESTORE_STATE"),
    "requires source and empty restore fixtures",
)
class RestoreAccessLiveTests(unittest.TestCase):
    def test_incompatible_constraints_roll_back_before_clean_retry(self):
        source = Path(os.environ["CODEX_TEST_POSTGRES_STATE"])
        destination = Path(os.environ["CODEX_TEST_POSTGRES_FOCUSED_RESTORE_STATE"])
        command(destination, "up")
        self.assertEqual(
            sql(
                destination,
                "SELECT count(*) FROM pg_class WHERE relnamespace='codex_storage'::regnamespace",
            ),
            "0",
        )
        constraint = "restore_access_extra"
        self.assertEqual(
            sql(
                source, "SELECT to_regtype('codex_storage.restore_access_type') IS NULL"
            ),
            "t",
        )
        self.assertEqual(
            sql(
                source,
                f"SELECT count(*) FROM pg_constraint WHERE connamespace='codex_storage'::regnamespace AND conname='{constraint}'",
            ),
            "0",
        )
        snapshot = (
            "SELECT json_build_object('namespace', "
            "(SELECT row_to_json(n) FROM pg_namespace n WHERE nspname='codex_storage'), "
            "'dependencies', (SELECT json_agg(d ORDER BY classid,objid,objsubid,deptype) "
            "FROM pg_depend d WHERE refclassid='pg_namespace'::regclass "
            "AND refobjid='codex_storage'::regnamespace), "
            "'defaults', (SELECT json_agg(a ORDER BY oid) FROM pg_default_acl a "
            "WHERE defaclrole='codex_owner'::regrole))::text"
        )
        before = json.loads(sql(destination, snapshot))
        protected = (
            "SELECT json_build_object('metadata', (SELECT json_agg(m ORDER BY singleton) "
            "FROM codex_storage.codex_schema_meta m), 'history', (SELECT json_agg(h ORDER BY version) "
            "FROM codex_storage._codex_pg_migrations h))::text"
        )
        expected = json.loads(sql(source, protected))
        self.assertIsNotNone(expected["metadata"])
        self.assertIsNotNone(expected["history"])
        changes = [
            (
                f"ALTER TABLE codex_storage._codex_pg_migrations ADD CONSTRAINT {constraint} {definition}",
                f"ALTER TABLE codex_storage._codex_pg_migrations DROP CONSTRAINT IF EXISTS {constraint}",
            )
            for definition in ("CHECK (version > 0)", "UNIQUE (version)")
        ]
        changes.extend(
            (
                f"ALTER TABLE codex_storage.{table} RENAME CONSTRAINT {name} TO {constraint}",
                f"ALTER TABLE codex_storage.{table} RENAME CONSTRAINT {constraint} TO {name}",
            )
            for table, name in (
                ("_codex_pg_migrations", "_codex_pg_migrations_pkey"),
                ("codex_schema_meta", "codex_schema_meta_pkey"),
                ("codex_schema_meta", "codex_schema_meta_singleton_check"),
            )
        )
        changes.append(
            (
                f"ALTER TABLE codex_storage.codex_schema_meta ADD CONSTRAINT {constraint} CHECK (format_version <= 1)",
                f"ALTER TABLE codex_storage.codex_schema_meta DROP CONSTRAINT IF EXISTS {constraint}",
            )
        )
        changes.extend(
            (
                f"CREATE TYPE codex_storage.restore_access_type AS ({columns}); ALTER TABLE codex_storage.{table} OF codex_storage.restore_access_type",
                f"ALTER TABLE codex_storage.{table} NOT OF; DROP TYPE codex_storage.restore_access_type",
            )
            for table, columns in (
                (
                    "codex_schema_meta",
                    "singleton boolean, format_version integer, min_reader_version integer, min_writer_version integer",
                ),
                (
                    "_codex_pg_migrations",
                    "version bigint, description text, installed_on timestamptz, success boolean, checksum bytea, execution_time bigint",
                ),
            )
        )
        for setup, cleanup in changes:
            with temporary_sql(
                source,
                f"SET LOCAL ROLE codex_owner; {setup}",
                f"SET LOCAL ROLE codex_owner; {cleanup}",
            ):
                backup, archive = checked_backup(source)
            command(
                destination,
                "restore",
                "--archive",
                str(archive),
                "--sha256",
                backup["sha256"],
                "--confirm-empty-destination",
                expected_error="restore_outcome_unconfirmed_inspect_destination",
            )
            self.assertEqual(json.loads(sql(destination, snapshot)), before)
        self.assertEqual(json.loads(sql(source, protected)), expected)
        backup, archive = checked_backup(source)
        command(
            destination,
            "restore",
            "--archive",
            str(archive),
            "--sha256",
            backup["sha256"],
            "--confirm-empty-destination",
        )
        self.assertEqual(json.loads(sql(destination, protected)), expected)
        for statement in (
            "UPDATE codex_storage.codex_schema_meta SET format_version=99",
            "SELECT version FROM codex_storage._codex_pg_migrations",
        ):
            sql(destination, statement, role="runtime", expected_sqlstate="42501")
        self.assertEqual(
            json.loads(
                sql(
                    destination,
                    "SELECT json_agg(h ORDER BY version) FROM codex_storage._codex_pg_migrations h",
                    role="backup",
                )
            ),
            expected["history"],
        )
