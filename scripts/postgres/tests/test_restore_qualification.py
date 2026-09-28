"""Offline control-flow checks for protected restore qualification."""

from pathlib import Path
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import restore_qualification as qualification
from state import ServiceError


class RestoreQualificationTests(unittest.TestCase):
    def test_cleanup_uses_catalog_guards_for_fresh_destinations(self):
        with patch.object(qualification, "sql", return_value="") as query:
            qualification._cleanup_roles(Path("destination"))
        statement = query.call_args.args[1]
        self.assertIn("IF EXISTS (SELECT 1 FROM pg_roles", statement)
        self.assertIn("DROP ROLE codex_restore_inherited", statement)
        self.assertIn("DROP ROLE codex_restore_assumable", statement)

    def test_wrapper_preserves_failure_and_cleans_retained_fixture(self):
        failure = ServiceError("qualification_failed")
        with (
            patch.object(
                qualification,
                "_qualify_restore_access",
                side_effect=failure,
            ),
            patch.object(qualification, "_cleanup_roles") as cleanup,
        ):
            with self.assertRaisesRegex(ServiceError, "qualification_failed"):
                qualification.qualify_restore_access(
                    Path("source"), Path("destination")
                )
        cleanup.assert_called_once_with(Path("destination"))

    def test_wrapper_reports_cleanup_failure_after_success(self):
        cleanup_failure = ServiceError("cleanup_failed")
        with (
            patch.object(qualification, "_qualify_restore_access", return_value={}),
            patch.object(qualification, "_cleanup_roles", side_effect=cleanup_failure),
        ):
            with self.assertRaisesRegex(ServiceError, "cleanup_failed"):
                qualification.qualify_restore_access(
                    Path("source"), Path("destination")
                )
