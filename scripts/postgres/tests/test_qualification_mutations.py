"""Atomic fixture cleanup must run after every uncertain setup outcome."""

from pathlib import Path
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import qualification_mutations as mutations
from state import ServiceError


class QualificationMutationsTests(unittest.TestCase):
    def test_setup_failure_arms_cleanup_without_masking_primary_error(self):
        failure = ServiceError("setup_outcome_unknown")
        for cleanup_failure in (None, ServiceError("cleanup_failed")):
            with (
                self.subTest(cleanup_failure=cleanup_failure),
                patch.object(
                    mutations, "sql", side_effect=[failure, cleanup_failure]
                ) as sql,
                self.assertRaises(ServiceError) as raised,
                mutations.temporary_sql(Path("fixture"), "MUTATE", "RESTORE"),
            ):
                self.fail("uncertain setup must not reach the body")
            self.assertIs(raised.exception, failure)
            self.assertEqual(
                [call.args for call in sql.call_args_list],
                [
                    (Path("fixture"), "BEGIN; MUTATE; COMMIT"),
                    (Path("fixture"), "BEGIN; RESTORE; COMMIT"),
                ],
            )

    def test_body_failure_survives_cleanup_failure(self):
        failure = ServiceError("body_failed")
        with (
            patch.object(
                mutations, "sql", side_effect=[None, ServiceError("cleanup_failed")]
            ),
            self.assertRaises(ServiceError) as raised,
            mutations.temporary_sql(Path("fixture"), "MUTATE", "RESTORE"),
        ):
            raise failure
        self.assertIs(raised.exception, failure)

    def test_success_cannot_hide_cleanup_failure(self):
        failure = ServiceError("cleanup_failed")
        with (
            patch.object(mutations, "sql", side_effect=[None, failure]),
            self.assertRaises(ServiceError) as raised,
            mutations.temporary_sql(Path("fixture"), "MUTATE", "RESTORE"),
        ):
            pass
        self.assertIs(raised.exception, failure)
