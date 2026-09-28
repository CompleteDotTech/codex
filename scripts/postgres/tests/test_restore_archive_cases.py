"""Archive mutation cleanup must preserve source fixtures on every outcome."""

from pathlib import Path
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import restore_archive_cases as cases
from state import ServiceError


class RestoreArchiveCasesTests(unittest.TestCase):
    def test_capture_failure_still_restores_source(self):
        with (
            patch.object(cases, "sql") as sql,
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
            patch.object(cases, "sql", side_effect=outcomes),
            patch.object(cases, "checked_backup", return_value=("receipt", "archive")),
            self.assertRaisesRegex(ServiceError, "probe_failed"),
            cases.mutated_archive(Path("source"), "MUTATE", "RESTORE"),
        ):
            raise ServiceError("probe_failed")

    def test_success_cannot_hide_cleanup_failure(self):
        outcomes = [None, ServiceError("cleanup_failed")]
        with (
            patch.object(cases, "sql", side_effect=outcomes),
            patch.object(cases, "checked_backup", return_value=("receipt", "archive")),
            self.assertRaisesRegex(ServiceError, "cleanup_failed"),
            cases.mutated_archive(Path("source"), "MUTATE", "RESTORE") as captured,
        ):
            self.assertEqual(captured, ("receipt", "archive"))
