"""Adversarial offline plan binding and exclusion preview tests."""

import copy
import hashlib
import json
import unittest
import uuid

from .control_plan import HOST_RETAINED, preview_plan
from .manifest import MAX_MANIFEST_BYTES
from .records import ContractError
from .test_manifest import make_bundle


def encoded(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode()


class ControlPlanTests(unittest.TestCase):
    def setUp(self):
        self.manifest, _, self.inventory = make_bundle()
        for name in HOST_RETAINED:
            if name not in {rule["id"] for rule in self.inventory["domains"]}:
                rule = {"id": name, "schema": 1, "treatment": "retain"}
                self.inventory["domains"].append(rule)
                self.manifest["domains"].append(
                    {**rule, "count": 0, "logical_sha256": None, "chunks": []}
                )
        self.plan = {
            "version": 1,
            "operation": "migrate_local",
            "operation_id": self.manifest["migration_id"],
            "owner_host_id": str(uuid.uuid4()),
            "source": copy.deepcopy(self.manifest["source"]),
            "destination": copy.deepcopy(self.manifest["destination"]),
            "inventory_sha256": hashlib.sha256(encoded(self.inventory)).hexdigest(),
            "manifest_sha256": hashlib.sha256(encoded(self.manifest)).hexdigest(),
            "destination_occupancy": "empty",
            "local_history_present": True,
        }

    def preview(self, *, include_manifest=True):
        raw = encoded(self.plan)
        return preview_plan(
            raw,
            hashlib.sha256(raw).hexdigest(),
            encoded(self.inventory),
            encoded(self.manifest) if include_manifest else None,
        )

    def test_migration_is_bound_to_manifest_and_exclusions_are_visible(self):
        report = self.preview()
        excluded = {row["id"]: row["treatment"] for row in report["excluded_domains"]}
        self.assertTrue(HOST_RETAINED <= excluded.keys())
        self.assertTrue(all(excluded[name] == "retain" for name in HOST_RETAINED))
        self.assertEqual(report["local_history_action"], "capture_required")
        self.assertFalse(report["activation_permitted"])
        self.assertFalse(report["source_observation_verified"])
        self.assertEqual(report["verified_migrated_records"], 0)
        self.assertEqual(report["not_imported_domains"], [])

    def test_migration_rejects_changed_identity_or_manifest_digest(self):
        for field in ("source", "destination", "migration_id"):
            with self.subTest(field=field):
                original = copy.deepcopy(self.manifest)
                if field == "migration_id":
                    self.manifest[field] = str(uuid.uuid4())
                else:
                    self.manifest[field]["instance_id"] = str(uuid.uuid4())
                self.plan["manifest_sha256"] = hashlib.sha256(
                    encoded(self.manifest)
                ).hexdigest()
                with self.assertRaisesRegex(ContractError, "manifest_plan_mismatch"):
                    self.preview()
                self.manifest = original
        self.plan["manifest_sha256"] = "0" * 64
        with self.assertRaisesRegex(ContractError, "manifest_digest_mismatch"):
            self.preview()

    def test_changed_plan_bytes_reject_the_original_trusted_digest(self):
        original = encoded(self.plan)
        expected_digest = hashlib.sha256(original).hexdigest()
        self.plan["owner_host_id"] = str(uuid.uuid4())
        for name, changed in (
            ("owner_host", encoded(self.plan)),
            ("whitespace", original + b"\n"),
        ):
            with self.subTest(change=name):
                with self.assertRaisesRegex(ContractError, "^plan_digest_mismatch$"):
                    preview_plan(
                        changed,
                        expected_digest,
                        encoded(self.inventory),
                        encoded(self.manifest),
                    )

    def test_initialize_and_attach_preserve_local_history_without_import(self):
        self.plan["source"] = None
        self.plan["manifest_sha256"] = None
        self.plan["operation"] = "initialize_new"
        self.plan["destination"]["generation"] = 1
        initialized = self.preview(include_manifest=False)
        self.assertEqual(initialized["local_history_action"], "preserved_not_imported")
        self.assertTrue(initialized["local_history_present_assertion"])
        self.assertEqual(initialized["planned_migrate_domains"], 0)
        self.assertIn("rollouts.active", initialized["not_imported_domains"])
        self.assertIn("state", initialized["not_imported_domains"])
        self.assertEqual(initialized["verified_migrated_records"], 0)

        self.plan["operation"] = "attach_existing"
        self.plan["destination_occupancy"] = "existing"
        self.plan["destination"]["generation"] = 7
        attached = self.preview(include_manifest=False)
        self.assertEqual(attached["destination_generation"], 7)
        self.assertEqual(attached["local_history_action"], "preserved_not_imported")
        self.assertEqual(attached["planned_migrate_domains"], 0)
        self.assertIn("rollouts.active", attached["not_imported_domains"])

        self.plan["source"] = self.manifest["source"]
        with self.assertRaisesRegex(ContractError, "implicit_source_import"):
            self.preview(include_manifest=False)

    def test_occupied_destination_and_hidden_manifest_are_rejected(self):
        self.plan["destination_occupancy"] = "existing"
        with self.assertRaisesRegex(ContractError, "destination_not_empty"):
            self.preview()
        self.plan["destination_occupancy"] = "empty"
        self.plan["operation"] = "initialize_new"
        self.plan["source"] = None
        self.plan["manifest_sha256"] = None
        self.plan["destination"]["generation"] = 1
        with self.assertRaisesRegex(ContractError, "implicit_source_import"):
            self.preview()

    def test_host_exclusions_and_independent_inventory_cannot_be_reclassified(self):
        rule = next(
            rule
            for rule in self.inventory["domains"]
            if rule["id"] == "host.credentials"
        )
        rule["treatment"] = "migrate"
        self.plan["inventory_sha256"] = hashlib.sha256(
            encoded(self.inventory)
        ).hexdigest()
        with self.assertRaisesRegex(ContractError, "host_exclusion_missing"):
            self.preview()

    def test_unknown_fields_and_untrusted_digests_fail_closed(self):
        self.plan["credential"] = "do not echo this value"
        with self.assertRaises(ContractError) as failure:
            self.preview()
        self.assertNotIn("do not echo this value", str(failure.exception))
        del self.plan["credential"]
        self.plan["inventory_sha256"] = "0" * 64
        with self.assertRaisesRegex(ContractError, "inventory_digest_mismatch"):
            self.preview()

    def test_oversized_inventory_is_rejected_before_hashing_or_parsing(self):
        raw = encoded(self.plan)
        with self.assertRaisesRegex(ContractError, "inventory_too_large"):
            preview_plan(
                raw,
                hashlib.sha256(raw).hexdigest(),
                b"x" * (MAX_MANIFEST_BYTES + 1),
                encoded(self.manifest),
            )
