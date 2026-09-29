"""Offline control-flow checks for protected restore qualification."""

from pathlib import Path
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import restore_qualification as qualification
from state import ServiceError


class RestoreQualificationTests(unittest.TestCase):
    def test_wrapper_preserves_failure_and_cleans_retained_fixture(self):
        failure = ServiceError("qualification_failed")

        def fail_after_roles(source, destination, owned_roles):
            owned_roles.append(True)
            raise failure

        with (
            patch.object(
                qualification,
                "_qualify_restore_access",
                side_effect=fail_after_roles,
            ),
            patch.object(qualification, "_cleanup_roles") as cleanup,
        ):
            with self.assertRaisesRegex(ServiceError, "qualification_failed"):
                qualification.qualify_restore_access(
                    Path("source"), Path("destination")
                )
        cleanup.assert_called_once_with(Path("destination"))

    def test_wrapper_preserves_primary_failure_when_cleanup_fails(self):
        failure = KeyboardInterrupt()

        def fail_after_roles(source, destination, owned_roles):
            owned_roles.append(True)
            raise failure

        for cleanup_failure in (ServiceError(), RuntimeError(), KeyboardInterrupt()):
            with (
                patch.object(
                    qualification,
                    "_qualify_restore_access",
                    side_effect=fail_after_roles,
                ),
                patch.object(
                    qualification, "_cleanup_roles", side_effect=cleanup_failure
                ),
            ):
                with self.assertRaises(BaseException) as raised:
                    qualification.qualify_restore_access(
                        Path("source"), Path("destination")
                    )
            self.assertIs(raised.exception, failure)

    def test_wrapper_reports_cleanup_failure_after_success(self):
        cleanup_failure = ServiceError("cleanup_failed")
        with (
            patch.object(
                qualification,
                "_qualify_restore_access",
                side_effect=lambda s, d, roles: roles.append(True) or {},
            ),
            patch.object(qualification, "_cleanup_roles", side_effect=cleanup_failure),
        ):
            with self.assertRaisesRegex(ServiceError, "cleanup_failed"):
                qualification.qualify_restore_access(
                    Path("source"), Path("destination")
                )
