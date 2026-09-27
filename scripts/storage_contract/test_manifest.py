import copy
import hashlib
import json
import unittest
import uuid

from .manifest import MAX_CHUNK_BYTES, validate_manifest
from .records import ContractError, Fingerprint, encode_row

# These are synthetic contract domains, not a substitute for a table-level source inventory.
STORES = (
    "state",
    "logs",
    "goals",
    "memories",
    "memories_v2",
    "queue",
    "thread_history",
    "agent_message_board",
    "rollouts.active",
    "rollouts.archived",
    "rollouts.compressed",
    "rollouts.legacy",
    "rollouts.fork_refs",
    "artifacts.memory",
    "artifacts.attachments",
)


def json_bytes(value):
    return json.dumps(value, separators=(",", ":"), sort_keys=True).encode()


def make_bundle():
    dataset = str(uuid.uuid4())
    source = {
        "dataset_id": dataset,
        "instance_id": str(uuid.uuid4()),
        "generation": 4,
        "backend": "sqlite",
    }
    destination = {
        **source,
        "instance_id": str(uuid.uuid4()),
        "generation": 5,
        "backend": "postgresql",
    }
    manifest = {
        "version": 1,
        "operation": "migrate",
        "migration_id": str(uuid.uuid4()),
        "source": source,
        "destination": destination,
        "domains": [],
    }
    policy, payloads = {"version": 1, "domains": []}, []
    specifications = [(name, "migrate") for name in STORES] + [
        ("empty_store", "migrate"),
        ("session_index", "regenerate"),
        ("host.credentials", "retain"),
        ("host.device_identity", "retain"),
        ("host.installation_receipts", "retain"),
        ("host.settings", "retain"),
        ("host.journals", "retain"),
        ("host.backups", "retain"),
        ("absent_optional_store", "absent"),
    ]
    for name, treatment in specifications:
        rule = {"id": name, "schema": 1, "treatment": treatment}
        domain = {**rule, "count": 0, "logical_sha256": None, "chunks": []}
        policy["domains"].append(rule)
        if treatment == "migrate":
            logical = Fingerprint(name, 1)
            for key in [] if name == "empty_store" else [b"a", b"b"]:
                line = encode_row(key, [name, 9223372036854775807, b"\x00\xff", "é💾"])
                logical.feed(line)
                domain["chunks"].append(
                    {
                        "bytes": len(line),
                        "records": 1,
                        "sha256": hashlib.sha256(line).hexdigest(),
                    }
                )
                payloads.append(line)
            domain["count"], domain["logical_sha256"] = (
                logical.count,
                logical.hexdigest(),
            )
        manifest["domains"].append(domain)
    return manifest, b"".join(payloads), policy


def check(manifest, policy):
    raw = json_bytes(manifest)
    return validate_manifest(raw, hashlib.sha256(raw).hexdigest(), json_bytes(policy))


class ManifestTests(unittest.TestCase):
    def setUp(self):
        self.manifest, _, self.policy = make_bundle()

    def test_complete_manifest_and_exclusions(self):
        self.assertEqual(check(self.manifest, self.policy), self.manifest)

    def test_reverse_direction_uses_same_contract(self):
        self.manifest["source"]["backend"] = "postgresql"
        self.manifest["destination"]["backend"] = "sqlite"
        self.assertEqual(check(self.manifest, self.policy), self.manifest)

    def test_absent_empty_and_retained_are_distinct(self):
        domain = self.manifest["domains"][-1]
        domain["treatment"] = "retain"
        with self.assertRaisesRegex(ContractError, "inventory_mismatch"):
            check(self.manifest, self.policy)

    def test_missing_extra_duplicate_or_reclassified_domain_fails(self):
        for mutation in ("missing", "extra", "duplicate", "reclassify"):
            manifest = copy.deepcopy(self.manifest)
            if mutation == "missing":
                manifest["domains"].pop(
                    7
                )  # The independent message board cannot disappear.
            elif mutation == "extra":
                domain = copy.deepcopy(manifest["domains"][0])
                domain["id"] = "unclassified"
                manifest["domains"].append(domain)
            elif mutation == "duplicate":
                manifest["domains"].append(copy.deepcopy(manifest["domains"][0]))
            else:
                manifest["domains"][0]["treatment"] = "retain"
            with self.subTest(mutation=mutation), self.assertRaises(ContractError):
                check(manifest, self.policy)

    def test_retained_domain_cannot_smuggle_payload(self):
        self.manifest["domains"][-1]["chunks"] = copy.deepcopy(
            self.manifest["domains"][0]["chunks"]
        )
        with self.assertRaisesRegex(ContractError, "excluded_domain_has_payload"):
            check(self.manifest, self.policy)

    def test_stale_manifest_digest_is_rejected(self):
        raw = json_bytes(self.manifest)
        with self.assertRaisesRegex(ContractError, "manifest_digest_mismatch"):
            validate_manifest(raw, "0" * 64, json_bytes(self.policy))

    def test_bad_counts_sizes_schema_and_identity_fail(self):
        for field, value in (
            ("count", True),
            ("count", 0),
            ("schema", 2),
            ("schema", True),
        ):
            manifest = copy.deepcopy(self.manifest)
            manifest["domains"][0][field] = value
            with (
                self.subTest(field=field, value=value),
                self.assertRaises(ContractError),
            ):
                check(manifest, self.policy)
        self.manifest["domains"][0]["chunks"][0]["bytes"] = MAX_CHUNK_BYTES + 1
        with self.assertRaises(ContractError):
            check(self.manifest, self.policy)

    def test_operation_version_and_identity_are_not_config_switches(self):
        for field, value in (
            ("operation", "attach"),
            ("operation", "initialize"),
            ("version", 2),
            ("version", True),
            ("migration_id", "secret"),
        ):
            manifest = copy.deepcopy(self.manifest)
            manifest[field] = value
            with self.subTest(field=field), self.assertRaises(ContractError):
                check(manifest, self.policy)
        for field, value in (
            ("dataset_id", str(uuid.uuid4())),
            ("generation", 9),
            ("generation", True),
            ("backend", "sqlite"),
        ):
            manifest = copy.deepcopy(self.manifest)
            manifest["destination"][field] = value
            with self.subTest(field=field), self.assertRaises(ContractError):
                check(manifest, self.policy)

    def test_unknown_fields_and_duplicate_json_keys_fail(self):
        raw = json_bytes(self.manifest)
        bad = raw[:-1] + b',"version":1}'
        with self.assertRaisesRegex(ContractError, "duplicate_field"):
            validate_manifest(
                bad, hashlib.sha256(bad).hexdigest(), json_bytes(self.policy)
            )
        self.manifest["password"] = "DO_NOT_PRINT_PRIVATE_VALUE"
        with self.assertRaises(ContractError) as caught:
            check(self.manifest, self.policy)
        self.assertNotIn(self.manifest["password"], str(caught.exception))

    def test_malformed_field_mutations_return_controlled_errors(self):
        paths = [
            ("version",),
            ("operation",),
            ("migration_id",),
            ("domains",),
            ("source",),
            ("source", "generation"),
            ("source", "backend"),
            ("domains", 0, "id"),
            ("domains", 0, "schema"),
            ("domains", 0, "treatment"),
            ("domains", 0, "count"),
            ("domains", 0, "logical_sha256"),
            ("domains", 0, "chunks"),
        ]
        values = [None, [], {}, True, -1, 1 << 64, "PRIVATE_SENTINEL"]
        for path in paths:
            for value in values:
                manifest = copy.deepcopy(self.manifest)
                node = manifest
                for part in path[:-1]:
                    node = node[part]
                node[path[-1]] = value
                with (
                    self.subTest(path=path, value=value),
                    self.assertRaises(ContractError) as caught,
                ):
                    check(manifest, self.policy)
                self.assertNotIn("PRIVATE_SENTINEL", str(caught.exception))

    def test_duplicate_inventory_domains_are_rejected(self):
        self.policy["domains"].append(copy.deepcopy(self.policy["domains"][0]))
        with self.assertRaisesRegex(ContractError, "duplicate_domain"):
            check(self.manifest, self.policy)

    def test_schema_boolean_in_trusted_inventory_is_rejected(self):
        self.policy["domains"][0]["schema"] = True
        with self.assertRaisesRegex(ContractError, "invalid_integer"):
            check(self.manifest, self.policy)
