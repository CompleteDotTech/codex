"""Bounded, draft v1 byte-artifact records; no runtime storage or activation.

The caller supplies an independently trusted identity, length, and digest for
one captured artifact. This module neither captures a live store nor classifies
portable files. It never parses, normalizes, or decompresses artifact payloads.
"""

import base64
import hashlib
import hmac
import tempfile
from dataclasses import dataclass
from typing import BinaryIO, Iterator

from .manifest import HASH_PATTERN
from .records import MAX_ROW_BYTES, ContractError, encode_row, integer, parse_json
from .records import require, token, validate_row

CHUNK_BYTES = 64 << 10
MAX_ARTIFACT_BYTES = 64 << 20
FORMAT = "CDTX-artifact-chunk-v1"


@dataclass(frozen=True)
class ArtifactSpec:
    """Independent capture metadata; artifact_id is opaque, not a filesystem path."""

    artifact_id: str
    byte_length: int
    sha256: str

    def validate(self) -> None:
        token(self.artifact_id, HASH_PATTERN)
        integer(self.byte_length, 0, MAX_ARTIFACT_BYTES)
        token(self.sha256, HASH_PATTERN)


def _chunk_row(spec: ArtifactSpec, offset: int, payload: bytes) -> bytes:
    key = bytes.fromhex(spec.artifact_id) + offset.to_bytes(8, "big")
    return encode_row(
        key,
        [
            FORMAT,
            spec.artifact_id,
            spec.byte_length,
            spec.sha256,
            offset,
            payload,
        ],
    )


def encode_artifact(source: BinaryIO, spec: ArtifactSpec) -> Iterator[bytes]:
    """Authenticate a private copy before yielding canonical bounded byte rows.

    Short reads do not change chunk boundaries. The caller retains ownership of
    source. Exhaust or close the iterator to release its private temporary file;
    an incomplete export must never be treated as a completed artifact transfer.
    """
    spec.validate()
    try:
        with tempfile.TemporaryFile(mode="w+b") as captured:
            size, digest = 0, hashlib.sha256()
            while True:
                requested = min(CHUNK_BYTES, spec.byte_length - size + 1)
                block = source.read(requested)
                require(
                    type(block) is bytes and len(block) <= requested,
                    "invalid_binary_stream",
                )
                if not block:
                    break
                size += len(block)
                require(size <= spec.byte_length, "artifact_length_mismatch")
                require(captured.write(block) == len(block), "artifact_io_failed")
                digest.update(block)
            require(size == spec.byte_length, "artifact_length_mismatch")
            require(
                hmac.compare_digest(digest.hexdigest(), spec.sha256),
                "artifact_digest_mismatch",
            )
            captured.seek(0)
            for offset in range(0, max(1, size), CHUNK_BYTES):
                length = min(CHUNK_BYTES, size - offset)
                payload = captured.read(length)
                require(len(payload) == length, "artifact_io_failed")
                yield _chunk_row(spec, offset, payload)
    except ContractError:
        raise
    except (OSError, ValueError):
        raise ContractError("artifact_io_failed") from None


def audit_artifact_chunks(source: BinaryIO, spec: ArtifactSpec) -> dict:
    """Verify one complete fragment stream without writing an exported artifact.

    This receipt cannot authorize cutover. It proves neither the completeness of
    a dataset nor that the supplied identity/digest describes its current owner.
    It is not a rollout decoder, PostgreSQL importer, or reverse exporter.
    """
    spec.validate()
    count = max(1, (spec.byte_length + CHUNK_BYTES - 1) // CHUNK_BYTES)
    digest = hashlib.sha256()
    try:
        for index in range(count):
            line = source.readline(MAX_ROW_BYTES + 1)
            require(
                type(line) is bytes and len(line) <= MAX_ROW_BYTES,
                "invalid_binary_stream",
            )
            require(line.endswith(b"\n"), "artifact_record_truncated")
            _, canonical = validate_row(line)
            values = parse_json(canonical, MAX_ROW_BYTES)["values"]
            require(
                len(values) == 6 and values[-1]["type"] == "bytes",
                "artifact_record_mismatch",
            )
            payload = base64.b64decode(values[-1]["value"], validate=True)
            offset = index * CHUNK_BYTES
            require(
                len(payload) == min(CHUNK_BYTES, spec.byte_length - offset),
                "artifact_record_mismatch",
            )
            require(
                canonical == _chunk_row(spec, offset, payload),
                "artifact_record_mismatch",
            )
            digest.update(payload)
        trailing = source.read(1)
        require(type(trailing) is bytes and len(trailing) <= 1, "invalid_binary_stream")
        require(not trailing, "artifact_trailing_data")
        require(
            hmac.compare_digest(digest.hexdigest(), spec.sha256),
            "artifact_digest_mismatch",
        )
    except ContractError:
        raise
    except (OSError, ValueError):
        raise ContractError("artifact_io_failed") from None
    return {
        "status": "artifact_audited",
        "scope": "single_artifact_chunk_stream",
        "format_version": 1,
        "artifact_id": spec.artifact_id,
        "bytes": spec.byte_length,
        "sha256": digest.hexdigest(),
        "records": count,
        "activation_permitted": False,
    }
