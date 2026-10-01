"""Qualification reconciles uncertain source, role, and destination mutations."""

import json
import os
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import qualification_mutations as mutations
import restore_qualification as qualification
from qualification_checks import sql
from state import ServiceError


@unittest.skipUnless(
    os.environ.get("CODEX_TEST_POSTGRES_STATE")
    and os.environ.get("CODEX_TEST_POSTGRES_RESTORE_STATE"),
    "requires source and empty restore fixtures",
)
class RestoreQualificationCleanupLiveTests(unittest.TestCase):
    def test_uncertain_fixture_mutations_restore_source_and_destination(self):
        source = Path(os.environ["CODEX_TEST_POSTGRES_STATE"])
        destination = Path(os.environ["CODEX_TEST_POSTGRES_RESTORE_STATE"])
        snapshot = (
            "SELECT json_build_object('sequence', to_regclass('codex_storage.codex_restore_qualification_seq')::text, "
            "'roles', (SELECT json_agg(rolname ORDER BY rolname) FROM pg_roles WHERE rolname LIKE 'codex_restore_%'), "
            "'defaults', (SELECT json_agg(a ORDER BY oid) FROM pg_default_acl a WHERE defaclrole='codex_owner'::regrole))::text"
        )
        before = [json.loads(sql(home, snapshot)) for home in (source, destination)]
        for module, marker in (
            (mutations, "CREATE SEQUENCE"),
            (qualification, "BEGIN; CREATE ROLE"),
            (mutations, "ALTER DEFAULT PRIVILEGES"),
        ):
            for outcome in ("committed", "rolled_back"):
                injected = False

                def uncertain_sql(home, statement):
                    nonlocal injected
                    if not injected and marker in statement:
                        injected = True
                        if outcome == "rolled_back":
                            with self.assertRaises(ServiceError):
                                sql(
                                    home,
                                    statement.replace(
                                        "; COMMIT", "; SELECT 1 / 0; COMMIT"
                                    ),
                                )
                        else:
                            sql(home, statement)
                        raise ServiceError("fixture_result_lost")
                    return sql(home, statement)

                with self.subTest(marker=marker, outcome=outcome):
                    with (
                        patch.object(module, "sql", side_effect=uncertain_sql),
                        self.assertRaisesRegex(ServiceError, "fixture_result_lost"),
                    ):
                        qualification.qualify_restore_access(source, destination)
                    self.assertTrue(injected)
                    self.assertEqual(
                        [
                            json.loads(sql(home, snapshot))
                            for home in (source, destination)
                        ],
                        before,
                    )
