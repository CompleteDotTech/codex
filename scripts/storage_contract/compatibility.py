"""Offline compatibility prefilter, never a runtime or restore authority."""

import hashlib
import hmac

from .manifest import HASH_PATTERN, ID_PATTERN, MAX_MANIFEST_BYTES
from .records import fields, integer, parse_json, require, token

IDENTITY_PATTERN = r"[a-z0-9_.-]{1,64}"
GIT_SHA_PATTERN = r"[0-9a-f]{40}"
OPERATIONS = {"read", "write", "update", "upstream_restore"}
PHASES = {"idle", "prepared", "transferring", "verifying", "activating"}


def _names(value, pattern=IDENTITY_PATTERN):
    require(type(value) is list and len(value) <= 256, "invalid_capabilities")
    names = [token(item, pattern) for item in value]
    require(len(names) == len(set(names)), "duplicate_capability")
    return set(names)


def _schemas(value, *, descriptor):
    require(type(value) is list and 0 < len(value) <= 256, "invalid_domains")
    result = {}
    for item in value:
        if descriptor:
            fields(item, {"id", "read", "write"})
            read = _schema_versions(item["read"])
            write = _schema_versions(item["write"])
            require(read, "missing_reader_schema")
            entry = (read, write)
        else:
            fields(item, {"id", "schema"})
            entry = integer(item["schema"], 1)
        name = token(item["id"], ID_PATTERN)
        require(name not in result, "duplicate_domain")
        result[name] = entry
    return result


def _schema_versions(value):
    require(type(value) is list and len(value) <= 64, "invalid_schema_set")
    versions = [integer(version, 1) for version in value]
    require(len(versions) == len(set(versions)), "duplicate_schema")
    return set(versions)


def _backend_tuple(value):
    fields(value, {"backend", "server_major"})
    backend = value["backend"]
    require(
        type(backend) is str and backend in {"sqlite", "postgresql"},
        "unsupported_backend",
    )
    if backend == "sqlite":
        require(value["server_major"] is None, "invalid_backend_version")
        return (backend, None)
    return (backend, integer(value["server_major"], 1, 99))


def _backend_tuples(value):
    require(type(value) is list and 0 < len(value) <= 64, "invalid_backend_tuples")
    tuples = [_backend_tuple(item) for item in value]
    require(len(tuples) == len(set(tuples)), "duplicate_backend_tuple")
    return set(tuples)


def evaluate_compatibility(
    descriptor_data: bytes, expected_descriptor_sha256: str, observed_data: bytes
) -> dict:
    """Return missing axes from a claimed observation; never permit activation.

    The expected digest must come from independent package evidence. The
    observation is a caller assertion, not a trusted probe of binaries/data.
    """
    require(len(descriptor_data) <= MAX_MANIFEST_BYTES, "descriptor_too_large")
    require(len(observed_data) <= MAX_MANIFEST_BYTES, "observation_too_large")
    token(expected_descriptor_sha256, HASH_PATTERN)
    require(
        hmac.compare_digest(
            hashlib.sha256(descriptor_data).hexdigest(), expected_descriptor_sha256
        ),
        "descriptor_digest_mismatch",
    )
    descriptor = fields(
        parse_json(descriptor_data, MAX_MANIFEST_BYTES),
        {
            "version",
            "fork_revision",
            "upstream_base",
            "package_sha256",
            "target",
            "domain_schemas",
            "backend_tuples",
            "protocol_versions",
            "daemon_versions",
            "rollout_formats",
            "artifact_formats",
            "required_capabilities",
        },
    )
    require(
        type(descriptor["version"]) is int and descriptor["version"] == 1,
        "unsupported_descriptor",
    )
    token(descriptor["fork_revision"], GIT_SHA_PATTERN)
    token(descriptor["upstream_base"], GIT_SHA_PATTERN)
    token(descriptor["package_sha256"], HASH_PATTERN)
    token(descriptor["target"], IDENTITY_PATTERN)
    schemas = _schemas(descriptor["domain_schemas"], descriptor=True)
    backends = _backend_tuples(descriptor["backend_tuples"])
    supported = {
        axis: _names(descriptor[axis])
        for axis in (
            "protocol_versions",
            "daemon_versions",
            "rollout_formats",
            "artifact_formats",
        )
    }
    required_capabilities = _names(descriptor["required_capabilities"])

    observed = fields(
        parse_json(observed_data, MAX_MANIFEST_BYTES),
        {
            "operation",
            "fork_revision",
            "upstream_base",
            "package_sha256",
            "target",
            "domains",
            "backend",
            "server_major",
            "protocol_version",
            "daemon_version",
            "rollout_format",
            "artifact_format",
            "capabilities",
            "migration_phase",
        },
    )
    operation = observed["operation"]
    require(type(operation) is str and operation in OPERATIONS, "unsupported_operation")
    phase = observed["migration_phase"]
    require(type(phase) is str and phase in PHASES, "unknown_migration_phase")
    actual_schemas = _schemas(observed["domains"], descriptor=False)
    actual_backend = _backend_tuple(
        {"backend": observed["backend"], "server_major": observed["server_major"]}
    )
    actual_capabilities = _names(observed["capabilities"])
    reasons = []
    for axis, pattern in (
        ("fork_revision", GIT_SHA_PATTERN),
        ("upstream_base", GIT_SHA_PATTERN),
        ("package_sha256", HASH_PATTERN),
        ("target", IDENTITY_PATTERN),
    ):
        token(observed[axis], pattern)
        if observed[axis] != descriptor[axis]:
            reasons.append(f"{axis}_mismatch")
    if actual_backend not in backends:
        reasons.append("backend_version_unsupported")
    if set(actual_schemas) != set(schemas):
        reasons.append("domain_set_mismatch")
    for name, version in actual_schemas.items():
        if name in schemas:
            read, write = schemas[name]
            if version not in read:
                reasons.append("reader_schema_unsupported")
            if (
                operation in {"write", "update", "upstream_restore"}
                and version not in write
            ):
                reasons.append("writer_schema_unsupported")
    for observation_axis, descriptor_axis in (
        ("protocol_version", "protocol_versions"),
        ("daemon_version", "daemon_versions"),
        ("rollout_format", "rollout_formats"),
        ("artifact_format", "artifact_formats"),
    ):
        token(observed[observation_axis], IDENTITY_PATTERN)
        if observed[observation_axis] not in supported[descriptor_axis]:
            reasons.append(f"{observation_axis}_unsupported")
    if not required_capabilities <= actual_capabilities:
        reasons.append("capability_missing")
    if phase != "idle":
        reasons.append("migration_in_progress")
    if operation == "upstream_restore":
        reasons.append("native_target_qualification_missing")
    return {
        "status": "compatible_for_planning" if not reasons else "refused",
        "activation_permitted": False,
        "observations_verified": False,
        "operation": operation,
        "missing_evidence": sorted(set(reasons)),
        "remaining_gates": [
            "attested_observation",
            "native_package_qualification",
            "runtime_revalidation",
        ],
    }
