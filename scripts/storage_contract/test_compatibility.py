"""Synthetic compatibility axes; no package or server is qualified here."""

import hashlib
import json
import unittest

from .compatibility import evaluate_compatibility
from .records import ContractError


def encoded(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode()


class CompatibilityTests(unittest.TestCase):
    def setUp(self):
        self.descriptor = {
            "version": 1,
            "fork_revision": "a" * 40,
            "upstream_base": "b" * 40,
            "package_sha256": "c" * 64,
            "target": "linux-x86_64",
            "domain_schemas": [
                {"id": "threads", "read": [1, 2], "write": [2]},
                {"id": "rollouts", "read": [3], "write": [3]},
            ],
            "backend_tuples": [
                {"backend": "sqlite", "server_major": None},
                {"backend": "postgresql", "server_major": 17},
            ],
            "protocol_versions": ["v2"],
            "daemon_versions": ["d1"],
            "rollout_formats": ["jsonl2"],
            "artifact_formats": ["blob1"],
            "required_capabilities": ["storage-authority", "writer-fence"],
        }
        self.observed = {
            "operation": "write",
            "fork_revision": "a" * 40,
            "upstream_base": "b" * 40,
            "package_sha256": "c" * 64,
            "target": "linux-x86_64",
            "domains": [
                {"id": "threads", "schema": 2},
                {"id": "rollouts", "schema": 3},
            ],
            "backend": "sqlite",
            "server_major": None,
            "protocol_version": "v2",
            "daemon_version": "d1",
            "rollout_format": "jsonl2",
            "artifact_format": "blob1",
            "capabilities": ["storage-authority", "writer-fence"],
            "migration_phase": "idle",
        }

    def evaluate(self):
        raw = encoded(self.descriptor)
        return evaluate_compatibility(
            raw, hashlib.sha256(raw).hexdigest(), encoded(self.observed)
        )

    def test_matching_synthetic_axes_only_allow_planning(self):
        result = self.evaluate()
        self.assertEqual(result["status"], "compatible_for_planning")
        self.assertEqual(result["missing_evidence"], [])
        self.assertFalse(result["activation_permitted"])
        self.assertFalse(result["observations_verified"])
        self.assertIn("native_package_qualification", result["remaining_gates"])

    def test_reader_support_does_not_grant_writer_or_update_support(self):
        self.observed["domains"][0]["schema"] = 1
        self.observed["operation"] = "read"
        self.assertEqual(self.evaluate()["status"], "compatible_for_planning")
        for operation in ("write", "update"):
            with self.subTest(operation=operation):
                self.observed["operation"] = operation
                self.assertIn(
                    "writer_schema_unsupported", self.evaluate()["missing_evidence"]
                )

    def test_exact_package_and_upstream_identity_are_independent_axes(self):
        for axis, value in (("package_sha256", "d" * 64), ("upstream_base", "e" * 40)):
            with self.subTest(axis=axis):
                original = self.observed[axis]
                self.observed[axis] = value
                self.assertIn(f"{axis}_mismatch", self.evaluate()["missing_evidence"])
                self.observed[axis] = original

    def test_protocol_daemon_formats_capabilities_and_backend_each_refuse(self):
        cases = (
            ("protocol_version", "v3", "protocol_version_unsupported"),
            ("daemon_version", "d2", "daemon_version_unsupported"),
            ("rollout_format", "jsonl3", "rollout_format_unsupported"),
            ("artifact_format", "blob2", "artifact_format_unsupported"),
            ("capabilities", ["storage-authority"], "capability_missing"),
        )
        for axis, value, reason in cases:
            with self.subTest(axis=axis):
                original = self.observed[axis]
                self.observed[axis] = value
                self.assertIn(reason, self.evaluate()["missing_evidence"])
                self.observed[axis] = original
        self.observed["backend"] = "postgresql"
        self.observed["server_major"] = 16
        self.assertIn(
            "backend_version_unsupported", self.evaluate()["missing_evidence"]
        )

    def test_in_progress_or_unknown_migration_blocks_decision(self):
        self.observed["migration_phase"] = "transferring"
        self.assertIn("migration_in_progress", self.evaluate()["missing_evidence"])
        self.observed["migration_phase"] = "future_phase"
        with self.assertRaisesRegex(ContractError, "unknown_migration_phase"):
            self.evaluate()

    def test_upstream_restore_always_needs_actual_target_qualification(self):
        self.observed["operation"] = "upstream_restore"
        result = self.evaluate()
        self.assertEqual(result["status"], "refused")
        self.assertIn("native_target_qualification_missing", result["missing_evidence"])

    def test_domain_omission_and_unknown_descriptor_fields_fail(self):
        self.observed["domains"].pop()
        self.assertIn("domain_set_mismatch", self.evaluate()["missing_evidence"])
        self.descriptor["credential"] = "never display"
        with self.assertRaises(ContractError) as failure:
            self.evaluate()
        self.assertNotIn("never display", str(failure.exception))

    def test_independent_digest_rejects_modified_descriptor(self):
        trusted = encoded(self.descriptor)
        self.descriptor["protocol_versions"].append("v3")
        with self.assertRaisesRegex(ContractError, "descriptor_digest_mismatch"):
            evaluate_compatibility(
                encoded(self.descriptor),
                hashlib.sha256(trusted).hexdigest(),
                encoded(self.observed),
            )
