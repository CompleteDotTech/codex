"""Archive mutation cleanup must preserve source fixtures on every outcome."""

from pathlib import Path
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import restore_archive_cases as cases
import qualification_mutations as mutations
from state import ServiceError


class RestoreArchiveCasesTests(unittest.TestCase):
    def test_setup_failure_still_attempts_source_cleanup(self):
        for cleanup_failure in (None, ServiceError("cleanup_failed")):
            with (
                self.subTest(cleanup_failure=cleanup_failure),
                patch.object(
                    mutations,
                    "sql",
                    side_effect=[
                        ServiceError("setup_outcome_unknown"),
                        cleanup_failure,
                    ],
                ) as sql,
                patch.object(cases, "checked_backup") as backup,
                self.assertRaisesRegex(ServiceError, "setup_outcome_unknown"),
                cases.mutated_archive(Path("source"), "MUTATE", "RESTORE"),
            ):
                self.fail("uncertain setup must not reach the restore probe")
            self.assertEqual(
                [call.args[1] for call in sql.call_args_list],
                [
                    "BEGIN; SET LOCAL ROLE codex_owner; MUTATE; COMMIT",
                    "BEGIN; SET LOCAL ROLE codex_owner; RESTORE; COMMIT",
                ],
            )
            backup.assert_not_called()

    def test_capture_failure_still_restores_source(self):
        with (
            patch.object(mutations, "sql") as sql,
            patch.object(
                cases, "checked_backup", side_effect=ServiceError("capture_failed")
            ),
            self.assertRaisesRegex(ServiceError, "capture_failed"),
            cases.mutated_archive(Path("source"), "MUTATE", "RESTORE"),
        ):
            self.fail("failed capture must not reach the restore probe")
        self.assertEqual(
            [call.args[1] for call in sql.call_args_list],
            [
                "BEGIN; SET LOCAL ROLE codex_owner; MUTATE; COMMIT",
                "BEGIN; SET LOCAL ROLE codex_owner; RESTORE; COMMIT",
            ],
        )

    def test_probe_failure_survives_cleanup_failure(self):
        outcomes = [None, ServiceError("cleanup_failed")]
        with (
            patch.object(mutations, "sql", side_effect=outcomes),
            patch.object(cases, "checked_backup", return_value=("receipt", "archive")),
            self.assertRaisesRegex(ServiceError, "probe_failed"),
            cases.mutated_archive(Path("source"), "MUTATE", "RESTORE"),
        ):
            raise ServiceError("probe_failed")

    def test_success_cannot_hide_cleanup_failure(self):
        outcomes = [None, ServiceError("cleanup_failed")]
        with (
            patch.object(mutations, "sql", side_effect=outcomes),
            patch.object(cases, "checked_backup", return_value=("receipt", "archive")),
            self.assertRaisesRegex(ServiceError, "cleanup_failed"),
            cases.mutated_archive(Path("source"), "MUTATE", "RESTORE") as captured,
        ):
            self.assertEqual(captured, ("receipt", "archive"))
