"""Offline artifact contracts; not Codex resume or PostgreSQL qualification."""

import base64
import hashlib
import io
import json
import subprocess
import sys
import tempfile
import unittest
from dataclasses import replace
from pathlib import Path
from unittest import mock

from . import artifact_chunks
from .artifact_chunks import CHUNK_BYTES, MAX_ARTIFACT_BYTES, ArtifactSpec
from .artifact_chunks import audit_artifact_chunks, encode_artifact
from .manifest import verify
from .records import MAX_ROW_BYTES, ContractError, Fingerprint, validate_row


def specification(payload: bytes) -> ArtifactSpec:
    return ArtifactSpec("01" * 32, len(payload), hashlib.sha256(payload).hexdigest())


class ShortReader(io.BytesIO):
    def __init__(self, payload):
        super().__init__(payload)
        self.requests = []

    def read(self, size=-1):
        self.requests.append(size)
        return super().read(min(size, 137))


class ArtifactChunksTests(unittest.TestCase):
    def encoded(self, payload):
        return b"".join(encode_artifact(io.BytesIO(payload), specification(payload)))

    def rejects(self, stream, spec, code):
        with self.assertRaisesRegex(ContractError, "^" + code + "$"):
            audit_artifact_chunks(io.BytesIO(stream), spec)

    def test_short_reads_keep_stable_records_and_preserve_arbitrary_bytes(self):
        payload = bytes(range(256)) * 513 + "e\u0301\u00e9\U0001f30d".encode()
        source = ShortReader(payload)
        rows = list(encode_artifact(source, specification(payload)))
        self.assertEqual(b"".join(rows), self.encoded(payload))
        recovered = b"".join(
            base64.b64decode(json.loads(row)["values"][-1]["value"]) for row in rows
        )
        self.assertEqual(recovered, payload)
        keys = [validate_row(row)[0] for row in rows]
        self.assertEqual(keys, sorted(set(keys)))
        self.assertTrue(all(0 < size <= CHUNK_BYTES for size in source.requests))
        self.assertFalse(source.closed)

    def test_empty_is_one_verified_record_not_an_absent_artifact(self):
        spec = specification(b"")
        self.assertEqual(
            audit_artifact_chunks(io.BytesIO(self.encoded(b"")), spec),
            {
                "status": "artifact_audited",
                "scope": "single_artifact_chunk_stream",
                "format_version": 1,
                "artifact_id": spec.artifact_id,
                "bytes": 0,
                "sha256": spec.sha256,
                "records": 1,
                "activation_permitted": False,
            },
        )
        self.rejects(b"", spec, "artifact_record_truncated")

    def test_large_raw_json_is_not_parsed_or_normalized(self):
        payload = (
            b'{"timestamp":"legacy","value":1.234567890123456789012345,'
            b'"unknown":{"text":"' + b"x" * (16 << 20) + b'"}}\n'
        )
        spec = specification(payload)
        with tempfile.TemporaryFile() as transfer:
            for row in encode_artifact(io.BytesIO(payload), spec):
                self.assertLess(len(row), MAX_ROW_BYTES)
                transfer.write(row)
            transfer.seek(0)
            receipt = audit_artifact_chunks(transfer, spec)
        self.assertEqual(receipt["sha256"], hashlib.sha256(payload).hexdigest())
        self.assertEqual(receipt["bytes"], len(payload))
        self.assertFalse(receipt["activation_permitted"])

    def test_fragments_verify_through_the_unchanged_v1_bundle_contract(self):
        payload = b"a" * (CHUNK_BYTES + 1)
        rows = list(encode_artifact(io.BytesIO(payload), specification(payload)))
        logical = Fingerprint("artifact.fragments", 1)
        for row in rows:
            logical.feed(row)
        identity = {
            "instance_id": "00000000-0000-0000-0000-000000000001",
            "dataset_id": "00000000-0000-0000-0000-000000000002",
            "generation": 1,
            "backend": "sqlite",
        }
        rule = {"id": "artifact.fragments", "schema": 1, "treatment": "migrate"}
        manifest = {
            "version": 1,
            "operation": "migrate",
            "migration_id": "00000000-0000-0000-0000-000000000003",
            "source": identity,
            "destination": dict(
                identity,
                backend="postgresql",
                generation=2,
                instance_id="00000000-0000-0000-0000-000000000004",
            ),
            "domains": [
                dict(
                    rule,
                    count=len(rows),
                    logical_sha256=logical.hexdigest(),
                    chunks=[
                        {
                            "bytes": len(row),
                            "records": 1,
                            "sha256": hashlib.sha256(row).hexdigest(),
                        }
                        for row in rows
                    ],
                )
            ],
        }
        data = json.dumps(manifest).encode()
        result = verify(
            data,
            io.BytesIO(b"".join(rows)),
            hashlib.sha256(data).hexdigest(),
            json.dumps({"version": 1, "domains": [rule]}).encode(),
        )
        self.assertEqual(
            result,
            {
                "status": "bundle_verified",
                "activation_permitted": False,
                "verified_portable_domains": 1,
                "records_verified": len(rows),
                "chunks_verified": len(rows),
                "bytes_verified": sum(map(len, rows)),
                "excluded_domains": {"retain": 0, "regenerate": 0, "absent": 0},
            },
        )

    def test_source_authentication_finishes_before_first_record(self):
        payload = b"private fixture content"
        spec = replace(specification(payload), sha256="00" * 32)
        rows = encode_artifact(io.BytesIO(payload), spec)
        with self.assertRaisesRegex(ContractError, "^artifact_digest_mismatch$"):
            next(rows)

    def test_truncated_or_extended_source_emits_nothing(self):
        spec = specification(b"abcd")
        for payload in (b"abc", b"abcde"):
            with self.subTest(length=len(payload)):
                with self.assertRaisesRegex(
                    ContractError, "^artifact_length_mismatch$"
                ):
                    next(encode_artifact(io.BytesIO(payload), spec))

    def test_reorder_duplicate_omission_and_trailing_bytes_are_rejected(self):
        payload = b"a" * CHUNK_BYTES + b"b" * CHUNK_BYTES + b"c"
        rows = self.encoded(payload).splitlines(keepends=True)
        spec = specification(payload)
        for changed in (rows[::-1], [rows[0], rows[0], rows[2]]):
            self.rejects(b"".join(changed), spec, "artifact_record_mismatch")
        self.rejects(b"".join(rows[:-1]), spec, "artifact_record_truncated")
        self.rejects(b"".join(rows) + rows[-1], spec, "artifact_trailing_data")
        self.rejects(b"".join(rows) + b"x", spec, "artifact_trailing_data")
        self.rejects(b"".join(rows)[:-1], spec, "artifact_record_truncated")

    def test_equal_length_payload_corruption_changes_the_verified_digest(self):
        payload = b"same record count and length"
        row = json.loads(self.encoded(payload))
        row["values"][-1]["value"] = base64.b64encode(b"X" + payload[1:]).decode()
        self.rejects(
            (json.dumps(row) + "\n").encode(),
            specification(payload),
            "artifact_digest_mismatch",
        )

    def test_metadata_and_portable_key_are_bound_to_the_independent_spec(self):
        payload = b"data"
        original = json.loads(self.encoded(payload))
        for index, value in (
            (0, "future-format"),
            (1, "02" * 32),
            (2, "5"),
            (3, "00" * 32),
            (4, "1"),
        ):
            row = json.loads(json.dumps(original))
            row["values"][index]["value"] = value
            self.rejects(
                (json.dumps(row) + "\n").encode(),
                specification(payload),
                "artifact_record_mismatch",
            )
        original["key"] = "02" * 40
        self.rejects(
            (json.dumps(original) + "\n").encode(),
            specification(payload),
            "artifact_record_mismatch",
        )

    def test_invalid_spec_is_rejected_before_any_read(self):
        spec = specification(b"")
        for field, value in (
            ("byte_length", True),
            ("byte_length", -1),
            ("byte_length", MAX_ARTIFACT_BYTES + 1),
            ("artifact_id", "private/path"),
            ("sha256", "A" * 64),
        ):
            stream = mock.Mock()
            with self.assertRaises(ContractError):
                next(encode_artifact(stream, replace(spec, **{field: value})))
            stream.read.assert_not_called()

    def test_bad_streams_and_io_failures_have_payload_free_errors(self):
        spec = specification(b"a")
        for result in ("private content", b"aaa"):
            source = mock.Mock(read=mock.Mock(return_value=result))
            with self.assertRaisesRegex(ContractError, "^invalid_binary_stream$"):
                next(encode_artifact(source, spec))
        source = mock.Mock(read=mock.Mock(side_effect=OSError("private/path")))
        with self.assertRaisesRegex(ContractError, "^artifact_io_failed$"):
            next(encode_artifact(source, spec))
        source = mock.Mock(readline=mock.Mock(side_effect=OSError("private/path")))
        with self.assertRaisesRegex(ContractError, "^artifact_io_failed$"):
            audit_artifact_chunks(source, spec)
        self.rejects(b"x" * (MAX_ROW_BYTES + 1), spec, "invalid_binary_stream")

    def test_cancellation_closes_only_the_private_capture(self):
        payload = b"x" * (CHUNK_BYTES + 1)
        source = io.BytesIO(payload)
        temporary = tempfile.TemporaryFile()
        with mock.patch.object(
            artifact_chunks.tempfile, "TemporaryFile", return_value=temporary
        ):
            rows = encode_artifact(source, specification(payload))
            next(rows)
            self.assertFalse(temporary.closed)
            rows.close()
        self.assertTrue(temporary.closed)
        self.assertFalse(source.closed)

    def test_separate_process_export_and_audit_preserve_the_input(self):
        payload = b"isolated\x00captured artifact" * 8192
        spec = specification(payload)
        script = """
import json, sys
from pathlib import Path
sys.path.insert(0, sys.argv[1])
from storage_contract.artifact_chunks import ArtifactSpec, encode_artifact, audit_artifact_chunks
spec = ArtifactSpec(sys.argv[4], int(sys.argv[5]), sys.argv[6])
with open(sys.argv[2], 'rb') as source, open(sys.argv[3], 'xb') as output:
    for row in encode_artifact(source, spec):
        output.write(row)
with open(sys.argv[3], 'rb') as source:
    print(json.dumps(audit_artifact_chunks(source, spec)))
"""
        with tempfile.TemporaryDirectory() as directory:
            source, output = (
                Path(directory) / "capture.bin",
                Path(directory) / "rows.jsonl",
            )
            source.write_bytes(payload)
            sentinel = Path(directory) / "unrelated-backup"
            sentinel.write_bytes(b"preserve me")
            run = subprocess.run(
                [
                    sys.executable,
                    "-c",
                    script,
                    str(Path(__file__).resolve().parents[1]),
                    str(source),
                    str(output),
                    spec.artifact_id,
                    str(spec.byte_length),
                    spec.sha256,
                ],
                capture_output=True,
                text=True,
                timeout=20,
                check=False,
            )
            self.assertEqual((run.returncode, run.stderr), (0, ""))
            self.assertEqual(json.loads(run.stdout)["sha256"], spec.sha256)
            self.assertFalse(json.loads(run.stdout)["activation_permitted"])
            self.assertEqual(source.read_bytes(), payload)
            self.assertEqual(sentinel.read_bytes(), b"preserve me")
