"""Record-boundary integration through the offline CLI, not a database migration."""

import copy
import hashlib
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from .records import MAX_ROW_BYTES, Fingerprint, encode_row


SENTINEL = "PRIVATE_RECORD_SENTINEL"
CLI = Path(__file__).resolve().parents[1] / "verify_storage_bundle.py"


def json_bytes(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode() + b"\n"


def boundary_row(key=b"a"):
    # Independent serialization oracle; do not use the encoder's budget helper.
    text = SENTINEL + "\x00💾"
    row = {"key": key.hex(), "values": [{"type": "text", "value": text}]}
    text += "x" * (MAX_ROW_BYTES - len(json_bytes(row)))
    row["values"][0]["value"] = text
    return text, json_bytes(row)


def bundle(chunks, source="sqlite"):
    logical = Fingerprint("threads", 1)
    for chunk in chunks:
        for line in chunk.splitlines(keepends=True):
            logical.feed(line)
    dataset = "11111111-1111-4111-8111-111111111111"
    policy = {"id": "threads", "schema": 1, "treatment": "migrate"}
    manifest = {
        "version": 1,
        "operation": "migrate",
        "migration_id": "22222222-2222-4222-8222-222222222222",
        "source": {
            "instance_id": "33333333-3333-4333-8333-333333333333",
            "dataset_id": dataset,
            "generation": 7,
            "backend": source,
        },
        "destination": {
            "instance_id": "44444444-4444-4444-8444-444444444444",
            "dataset_id": dataset,
            "generation": 8,
            "backend": "postgresql" if source == "sqlite" else "sqlite",
        },
        "domains": [
            {
                **policy,
                "count": logical.count,
                "logical_sha256": logical.hexdigest(),
                "chunks": [
                    {
                        "bytes": len(chunk),
                        "records": len(chunk.splitlines()),
                        "sha256": hashlib.sha256(chunk).hexdigest(),
                    }
                    for chunk in chunks
                ],
            }
        ],
    }
    return manifest, {"version": 1, "domains": [policy]}


class RecordBundleBoundaryTests(unittest.TestCase):
    def run_bundle(self, manifest, inventory, payload, expected_code=None):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            contents = {
                "manifest.json": json_bytes(manifest),
                "inventory.json": json_bytes(inventory),
                "payload.jsonl": payload,
            }
            for name, data in contents.items():
                (root / name).write_bytes(data)
            before = {
                name: hashlib.sha256((root / name).read_bytes()).hexdigest()
                for name in contents
            }
            process = subprocess.run(
                [
                    sys.executable,
                    "-B",
                    str(CLI),
                    "--manifest",
                    str(root / "manifest.json"),
                    "--inventory",
                    str(root / "inventory.json"),
                    "--payload",
                    str(root / "payload.jsonl"),
                    "--expected-manifest-sha256",
                    before["manifest.json"],
                ],
                cwd=root,
                capture_output=True,
                timeout=30,
                check=False,
            )
            self.assertEqual(process.stderr, b"")
            self.assertLess(len(process.stdout), 1024)
            self.assertNotIn(SENTINEL.encode(), process.stdout)
            self.assertNotIn(str(root).encode(), process.stdout)
            self.assertEqual(
                sorted(path.name for path in root.iterdir()), sorted(contents)
            )
            self.assertEqual(
                {
                    name: hashlib.sha256((root / name).read_bytes()).hexdigest()
                    for name in contents
                },
                before,
            )
            report = json.loads(process.stdout)
            if expected_code is None:
                expected = {
                    "status": "bundle_verified",
                    "activation_permitted": False,
                    "verified_portable_domains": 1,
                    "records_verified": manifest["domains"][0]["count"],
                    "chunks_verified": len(manifest["domains"][0]["chunks"]),
                    "bytes_verified": len(payload),
                    "excluded_domains": {"absent": 0, "regenerate": 0, "retain": 0},
                }
                self.assertEqual((process.returncode, report), (0, expected))
            else:
                expected = {
                    "status": "rejected",
                    "code": expected_code,
                    "activation_permitted": False,
                }
                self.assertEqual((process.returncode, report), (2, expected))

    def test_exact_boundary_encoder_output_reaches_cli(self):
        text, expected = boundary_row()
        self.assertEqual(len(expected), MAX_ROW_BYTES)
        payload = encode_row(b"a", [text])
        self.assertEqual(payload, expected)
        # These are format labels, not a PostgreSQL/SQLite runtime round trip.
        for source in ("sqlite", "postgresql"):
            with self.subTest(source=source):
                manifest, inventory = bundle([payload], source)
                self.run_bundle(manifest, inventory, payload)

    def test_equal_length_damage_cannot_hide_behind_count_or_raw_digest(self):
        _, payload = boundary_row()
        manifest, inventory = bundle([payload])
        changed = payload.replace(
            b"PRIVATE_RECORD_SENTINEL", b"ALTERED_RECORD_SENTINEL", 1
        )
        self.assertEqual(len(changed), len(payload))
        self.assertNotEqual(changed, payload)
        self.run_bundle(manifest, inventory, changed, "chunk_digest_mismatch")
        manifest["domains"][0]["chunks"][0]["sha256"] = hashlib.sha256(
            changed
        ).hexdigest()
        self.run_bundle(manifest, inventory, changed, "logical_digest_mismatch")

    def test_cli_rejects_malformed_boundary_payloads(self):
        _, payload = boundary_row()
        manifest, inventory = bundle([payload])
        cases = [
            (b"", "truncated_chunk", False),
            (payload[:-1], "invalid_row_boundary", False),
            (payload + b"x", "trailing_payload", False),
            (payload[:-5] + b"x" + payload[-5:], "invalid_row_boundary", True),
        ]
        for candidate, code, revise_chunk in cases:
            with self.subTest(code=code, size=len(candidate)):
                altered = copy.deepcopy(manifest)
                if revise_chunk:
                    chunk = altered["domains"][0]["chunks"][0]
                    chunk["bytes"] = len(candidate)
                    chunk["sha256"] = hashlib.sha256(candidate).hexdigest()
                self.run_bundle(altered, inventory, candidate, code)

    def test_chunk_transition_keeps_key_order_and_boundary_record_checks(self):
        first = encode_row(b"a", ["prior row"])
        text, expected = boundary_row(b"b")
        second = encode_row(b"b", [text])
        self.assertEqual(second, expected)
        manifest, inventory = bundle([first, second])
        self.run_bundle(manifest, inventory, first + second)
        duplicate = second.replace(b'"key":"62"', b'"key":"61"', 1)
        self.assertEqual(len(duplicate), len(second))
        self.assertNotEqual(duplicate, second)
        manifest["domains"][0]["chunks"][1]["sha256"] = hashlib.sha256(
            duplicate
        ).hexdigest()
        self.run_bundle(
            manifest, inventory, first + duplicate, "duplicate_or_unordered_key"
        )
