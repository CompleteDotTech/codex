"""Verify a draft manifest and concatenated, record-aligned JSONL chunks.

This checks an immutable export artifact, not source completeness, authority,
writer fences, runtime compatibility, or permission to activate a backend.
"""

import hashlib
import hmac
import uuid
from typing import BinaryIO

from .records import (
    MAX_ROW_BYTES,
    Fingerprint,
    fields,
    integer,
    parse_json,
    require,
    token,
)

MAX_MANIFEST_BYTES = 1 << 20
MAX_DOMAINS = 256
MAX_CHUNKS = 4096
MAX_CHUNK_BYTES = 64 << 20
MAX_TOTAL_BYTES = 1 << 40
ID_PATTERN = r"[a-z][a-z0-9_.-]{0,127}"
HASH_PATTERN = r"[0-9a-f]{64}"
TREATMENTS = {"migrate", "regenerate", "retain", "absent"}


def _uuid(value: object) -> str:
    text = token(value, r"[0-9a-f-]{36}")
    try:
        valid = str(uuid.UUID(text)) == text
    except ValueError:
        valid = False
    require(valid, "invalid_identity")
    return text


def load_inventory(data: bytes) -> dict[str, dict]:
    """Load caller-trusted domain policy, never policy supplied by an export."""
    value = fields(parse_json(data, MAX_MANIFEST_BYTES), {"version", "domains"})
    require(
        type(value["version"]) is int and value["version"] == 1, "unsupported_inventory"
    )
    rules = value["domains"]
    require(type(rules) is list and 0 < len(rules) <= MAX_DOMAINS, "invalid_inventory")
    result = {}
    for rule in rules:
        fields(rule, {"id", "schema", "treatment"})
        name = token(rule["id"], ID_PATTERN)
        integer(rule["schema"], 1)
        require(
            type(rule["treatment"]) is str and rule["treatment"] in TREATMENTS,
            "invalid_treatment",
        )
        require(name not in result, "duplicate_domain")
        result[name] = dict(rule)
    return result


def _identity(value: object) -> dict:
    fields(value, {"instance_id", "dataset_id", "generation", "backend"})
    _uuid(value["instance_id"])
    _uuid(value["dataset_id"])
    integer(value["generation"], 1)
    require(
        type(value["backend"]) is str and value["backend"] in {"sqlite", "postgresql"},
        "unsupported_backend",
    )
    return value


def validate_manifest(data: bytes, expected_sha256: str, inventory: bytes) -> dict:
    """Validate exact version, plan binding, trusted coverage, and resource caps."""
    require(len(data) <= MAX_MANIFEST_BYTES, "manifest_too_large")
    token(expected_sha256, HASH_PATTERN)
    require(
        hmac.compare_digest(hashlib.sha256(data).hexdigest(), expected_sha256),
        "manifest_digest_mismatch",
    )
    policy = load_inventory(inventory)
    manifest = fields(
        parse_json(data, MAX_MANIFEST_BYTES),
        {"version", "operation", "migration_id", "source", "destination", "domains"},
    )
    require(
        type(manifest["version"]) is int and manifest["version"] == 1,
        "unsupported_manifest",
    )
    require(manifest["operation"] == "migrate", "unsupported_operation")
    _uuid(manifest["migration_id"])
    source, destination = (
        _identity(manifest["source"]),
        _identity(manifest["destination"]),
    )
    require(source["backend"] != destination["backend"], "invalid_direction")
    require(
        source["instance_id"] != destination["instance_id"],
        "identical_storage_instance",
    )
    require(
        source["dataset_id"] == destination["dataset_id"], "dataset_identity_mismatch"
    )
    require(
        destination["generation"] == source["generation"] + 1, "generation_mismatch"
    )
    domains = manifest["domains"]
    require(
        type(domains) is list and 0 < len(domains) <= MAX_DOMAINS, "invalid_domains"
    )
    seen, total_chunks, total_bytes = set(), 0, 0
    for domain in domains:
        fields(
            domain, {"id", "schema", "treatment", "count", "logical_sha256", "chunks"}
        )
        name = token(domain["id"], ID_PATTERN)
        require(name not in seen, "duplicate_domain")
        seen.add(name)
        integer(domain["schema"], 1)
        require(
            name in policy
            and domain["schema"] == policy[name]["schema"]
            and domain["treatment"] == policy[name]["treatment"],
            "inventory_mismatch",
        )
        integer(domain["count"])
        chunks = domain["chunks"]
        require(type(chunks) is list and len(chunks) <= MAX_CHUNKS, "invalid_chunks")
        total_chunks += len(chunks)
        require(total_chunks <= MAX_CHUNKS, "too_many_chunks")
        if domain["treatment"] != "migrate":
            require(
                domain["count"] == 0
                and domain["logical_sha256"] is None
                and not chunks,
                "excluded_domain_has_payload",
            )
            continue
        token(domain["logical_sha256"], HASH_PATTERN)
        records = 0
        for chunk in chunks:
            fields(chunk, {"bytes", "records", "sha256"})
            integer(chunk["bytes"], 1, MAX_CHUNK_BYTES)
            integer(chunk["records"], 1)
            token(chunk["sha256"], HASH_PATTERN)
            records += chunk["records"]
            total_bytes += chunk["bytes"]
            require(total_bytes <= MAX_TOTAL_BYTES, "bundle_too_large")
        require(records == domain["count"], "manifest_count_mismatch")
    require(seen == set(policy), "inventory_mismatch")
    return manifest


def verify(
    manifest_data: bytes, payload: BinaryIO, expected_sha256: str, inventory: bytes
) -> dict:
    """Stream verification with no data extraction, connections, writes or activation.

    Payload is the concatenation of each domain's chunks in manifest order.
    Chunk boundaries must fall between complete newline-terminated records.
    Inputs must be immutable, operator-owned export snapshots, not live stores.
    """
    manifest = validate_manifest(manifest_data, expected_sha256, inventory)
    verified_records, verified_domains, chunks_verified, bytes_verified = 0, 0, 0, 0
    excluded = {key: 0 for key in ("retain", "regenerate", "absent")}
    for domain in manifest["domains"]:
        treatment = domain["treatment"]
        if treatment != "migrate":
            excluded[treatment] += 1
            continue
        logical = Fingerprint(domain["id"], domain["schema"])
        for chunk in domain["chunks"]:
            remaining, records, digest = chunk["bytes"], 0, hashlib.sha256()
            while remaining:
                line = payload.readline(min(remaining, MAX_ROW_BYTES + 1))
                require(
                    type(line) is bytes and 0 < len(line) <= remaining,
                    "truncated_chunk",
                )
                remaining -= len(line)
                digest.update(line)
                logical.feed(line)
                records += 1
                require(records <= chunk["records"], "chunk_count_mismatch")
            require(records == chunk["records"], "chunk_count_mismatch")
            require(
                hmac.compare_digest(digest.hexdigest(), chunk["sha256"]),
                "chunk_digest_mismatch",
            )
            chunks_verified += 1
            bytes_verified += chunk["bytes"]
        require(logical.count == domain["count"], "domain_count_mismatch")
        require(
            hmac.compare_digest(logical.hexdigest(), domain["logical_sha256"]),
            "logical_digest_mismatch",
        )
        verified_records += logical.count
        verified_domains += 1
    require(payload.read(1) == b"", "trailing_payload")
    return {
        "status": "bundle_verified",
        "activation_permitted": False,
        "verified_portable_domains": verified_domains,
        "records_verified": verified_records,
        "chunks_verified": chunks_verified,
        "bytes_verified": bytes_verified,
        "excluded_domains": excluded,
    }
