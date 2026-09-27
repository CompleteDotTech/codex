import copy
import hashlib
import io
import json
import unittest
import uuid

from .manifest import MAX_CHUNK_BYTES, validate_manifest, verify
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


def check(manifest, payload, policy):
    raw = json_bytes(manifest)
    return verify(
        raw, io.BytesIO(payload), hashlib.sha256(raw).hexdigest(), json_bytes(policy)
    )


class ManifestTests(unittest.TestCase):
    def setUp(self):
        self.manifest, self.payload, self.policy = make_bundle()

    def test_complete_synthetic_bundle_and_exclusions(self):
        report = check(self.manifest, self.payload, self.policy)
        self.assertEqual(
            report,
            {
                "status": "bundle_verified",
                "activation_permitted": False,
                "verified_portable_domains": len(STORES) + 1,
                "records_verified": 2 * len(STORES),
                "chunks_verified": 2 * len(STORES),
                "bytes_verified": len(self.payload),
                "excluded_domains": {"retain": 6, "regenerate": 1, "absent": 1},
            },
        )

    def test_reverse_direction_uses_same_contract(self):
        self.manifest["source"]["backend"] = "postgresql"
        self.manifest["destination"]["backend"] = "sqlite"
        self.assertEqual(
            check(self.manifest, self.payload, self.policy)["status"], "bundle_verified"
        )

    def test_absent_empty_and_retained_are_distinct(self):
        domain = self.manifest["domains"][-1]
        domain["treatment"] = "retain"
        with self.assertRaisesRegex(ContractError, "inventory_mismatch"):
            check(self.manifest, self.payload, self.policy)

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
                check(manifest, self.payload, self.policy)

    def test_retained_domain_cannot_smuggle_payload(self):
        self.manifest["domains"][-1]["chunks"] = copy.deepcopy(
            self.manifest["domains"][0]["chunks"]
        )
        with self.assertRaisesRegex(ContractError, "excluded_domain_has_payload"):
            check(self.manifest, self.payload, self.policy)

    def test_same_count_corruption_with_forged_chunk_hash_fails_logical_verification(
        self,
    ):
        size = self.manifest["domains"][0]["chunks"][0]["bytes"]
        changed = self.payload[:size].replace(b"state", b"stale")
        self.assertEqual(len(changed), size)
        self.manifest["domains"][0]["chunks"][0]["sha256"] = hashlib.sha256(
            changed
        ).hexdigest()
        with self.assertRaisesRegex(ContractError, "logical_digest_mismatch"):
            check(self.manifest, changed + self.payload[size:], self.policy)

    def test_raw_checksum_is_verified_independently(self):
        size = self.manifest["domains"][0]["chunks"][0]["bytes"]
        changed = self.payload[:size].replace(b"state", b"stale")
        with self.assertRaisesRegex(ContractError, "chunk_digest_mismatch"):
            check(self.manifest, changed + self.payload[size:], self.policy)

    def test_truncation_and_trailing_data_fail(self):
        for payload in (self.payload[:-1], self.payload + b"\n", b""):
            with self.subTest(length=len(payload)), self.assertRaises(ContractError):
                check(self.manifest, payload, self.policy)

    def test_every_interrupted_byte_of_a_record_fails(self):
        size = self.manifest["domains"][0]["chunks"][0]["bytes"]
        for cut in range(size):
            with self.subTest(cut=cut), self.assertRaises(ContractError):
                check(self.manifest, self.payload[:cut], self.policy)

    def test_chunk_boundary_must_not_split_a_record(self):
        self.manifest["domains"][0]["chunks"][0]["bytes"] -= 1
        with self.assertRaisesRegex(ContractError, "invalid_row_boundary"):
            check(self.manifest, self.payload, self.policy)

    def test_duplicate_key_across_chunks_fails(self):
        chunks = self.manifest["domains"][0]["chunks"]
        size0, size1 = chunks[0]["bytes"], chunks[1]["bytes"]
        chunks[1] = copy.deepcopy(chunks[0])
        payload = self.payload[:size0] * 2 + self.payload[size0 + size1 :]
        with self.assertRaisesRegex(ContractError, "duplicate_or_unordered_key"):
            check(self.manifest, payload, self.policy)

    def test_stale_manifest_digest_rejects_before_reading_payload(self):
        raw = json_bytes(self.manifest)

        class NoReads:
            def readline(self, _size):
                raise AssertionError("payload must not be read")

        with self.assertRaisesRegex(ContractError, "manifest_digest_mismatch"):
            verify(raw, NoReads(), "0" * 64, json_bytes(self.policy))

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
                check(manifest, self.payload, self.policy)
        self.manifest["domains"][0]["chunks"][0]["bytes"] = MAX_CHUNK_BYTES + 1
        with self.assertRaises(ContractError):
            check(self.manifest, self.payload, self.policy)

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
                check(manifest, self.payload, self.policy)
        for field, value in (
            ("dataset_id", str(uuid.uuid4())),
            ("generation", 9),
            ("generation", True),
            ("backend", "sqlite"),
        ):
            manifest = copy.deepcopy(self.manifest)
            manifest["destination"][field] = value
            with self.subTest(field=field), self.assertRaises(ContractError):
                check(manifest, self.payload, self.policy)

    def test_unknown_fields_and_duplicate_json_keys_fail(self):
        raw = json_bytes(self.manifest)
        bad = raw[:-1] + b',"version":1}'
        with self.assertRaisesRegex(ContractError, "duplicate_field"):
            validate_manifest(
                bad, hashlib.sha256(bad).hexdigest(), json_bytes(self.policy)
            )
        self.manifest["password"] = "DO_NOT_PRINT_PRIVATE_VALUE"
        with self.assertRaises(ContractError) as caught:
            check(self.manifest, self.payload, self.policy)
        self.assertNotIn(self.manifest["password"], str(caught.exception))

    def test_empty_fingerprint_is_not_arbitrary(self):
        domain = next(d for d in self.manifest["domains"] if d["id"] == "empty_store")
        domain["logical_sha256"] = "0" * 64
        with self.assertRaisesRegex(ContractError, "logical_digest_mismatch"):
            check(self.manifest, self.payload, self.policy)

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
                    check(manifest, self.payload, self.policy)
                self.assertNotIn("PRIVATE_SENTINEL", str(caught.exception))

    def test_duplicate_inventory_domains_are_rejected(self):
        self.policy["domains"].append(copy.deepcopy(self.policy["domains"][0]))
        with self.assertRaisesRegex(ContractError, "duplicate_domain"):
            check(self.manifest, self.payload, self.policy)

    def test_schema_boolean_in_trusted_inventory_is_rejected(self):
        self.policy["domains"][0]["schema"] = True
        with self.assertRaisesRegex(ContractError, "invalid_integer"):
            check(self.manifest, self.payload, self.policy)
