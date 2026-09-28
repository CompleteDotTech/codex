"""Offline, path-free relocation disposition for captured source metadata."""

import hashlib
import hmac
import re
from pathlib import PurePosixPath, PureWindowsPath

from .manifest import HASH_PATTERN, MAX_MANIFEST_BYTES, _uuid
from .records import fields, parse_json, require, token

KINDS = {"thread_cwd", "project_root", "rollout_path"}
FLAVORS = {"windows": PureWindowsPath, "posix": PurePosixPath}
MAX_ENTRIES = 256


def _path(text, flavor):
    require(type(flavor) is str and flavor in FLAVORS, "invalid_path_flavor")
    require(type(text) is str and 0 < len(text) <= 4096, "invalid_path")
    require("\x00" not in text, "invalid_path")
    if flavor == "windows":
        require(not text.startswith(("\\\\", "//")), "unsupported_network_path")
        parts = re.split(r"[\\/]", text)
    else:
        require("\\" not in text and not text.startswith("//"), "invalid_path")
        parts = text.split("/")
    require(all(part not in {".", ".."} for part in parts), "path_traversal")
    path = FLAVORS[flavor](text)
    require(path.is_absolute(), "relative_path")
    if flavor == "windows":
        require(bool(re.fullmatch(r"[A-Za-z]:\\", path.anchor)), "invalid_path")
    return path


def _records(value, *, plan):
    require(type(value) is list and len(value) <= MAX_ENTRIES, "invalid_records")
    result = {}
    rollout_ids = set()
    for record in value:
        expected = {"kind", "id", "source_path", "flavor"}
        if plan:
            expected.add("portable_rollout_id")
        fields(record, expected)
        kind = record["kind"]
        require(type(kind) is str and kind in KINDS, "invalid_record_kind")
        key = (kind, token(record["id"], r"[a-z0-9_.-]{1,128}"))
        require(key not in result, "duplicate_record")
        _path(record["source_path"], record["flavor"])
        if plan:
            if kind == "rollout_path":
                rollout_id = _uuid(record["portable_rollout_id"])
                require(rollout_id not in rollout_ids, "duplicate_rollout_identity")
                rollout_ids.add(rollout_id)
            else:
                require(
                    record["portable_rollout_id"] is None, "invalid_rollout_identity"
                )
        result[key] = record
    return result


def _mappings(value):
    require(type(value) is list and len(value) <= MAX_ENTRIES, "invalid_mappings")
    mappings = []
    for mapping in value:
        fields(
            mapping,
            {"source_root", "source_flavor", "target_root", "target_flavor"},
        )
        source = _path(mapping["source_root"], mapping["source_flavor"])
        target = _path(mapping["target_root"], mapping["target_flavor"])
        mappings.append(
            (source, mapping["source_flavor"], target, mapping["target_flavor"])
        )
    return mappings


def preview_relocation(
    plan_data: bytes,
    expected_plan_sha256: str,
    trusted_source_host_id: str,
    trusted_target_host_id: str,
    trusted_dataset_id: str,
    observed_records: list[dict],
) -> dict:
    """Compare a plan to independently captured source rows; grant no path use."""
    require(len(plan_data) <= MAX_MANIFEST_BYTES, "plan_too_large")
    token(expected_plan_sha256, HASH_PATTERN)
    require(
        hmac.compare_digest(
            hashlib.sha256(plan_data).hexdigest(), expected_plan_sha256
        ),
        "plan_digest_mismatch",
    )
    plan = fields(
        parse_json(plan_data, MAX_MANIFEST_BYTES),
        {
            "version",
            "source_host_id",
            "target_host_id",
            "dataset_id",
            "records",
            "mappings",
        },
    )
    require(type(plan["version"]) is int and plan["version"] == 1, "unsupported_plan")
    for field, trusted in (
        ("source_host_id", trusted_source_host_id),
        ("target_host_id", trusted_target_host_id),
        ("dataset_id", trusted_dataset_id),
    ):
        _uuid(trusted)
        require(_uuid(plan[field]) == trusted, "identity_mismatch")
    records = _records(plan["records"], plan=True)
    observed = _records(observed_records, plan=False)
    require(set(records) == set(observed), "source_record_mismatch")
    for key, record in records.items():
        require(
            {name: record[name] for name in observed[key]} == observed[key],
            "source_record_mismatch",
        )
    mappings = _mappings(plan["mappings"])
    dispositions = []
    for (kind, record_id), record in sorted(records.items()):
        if kind == "rollout_path":
            dispositions.append(
                {
                    "kind": kind,
                    "id": record_id,
                    "disposition": "portable_id_source_path_only",
                }
            )
            continue
        source = _path(record["source_path"], record["flavor"])
        matches = []
        for root, source_flavor, target, target_flavor in mappings:
            if source_flavor != record["flavor"]:
                continue
            try:
                relative = source.relative_to(root)
            except ValueError:
                continue
            candidate = target.joinpath(*relative.parts)
            _path(str(candidate), target_flavor)
            matches.append(candidate)
        require(len(matches) <= 1, "ambiguous_mapping")
        dispositions.append(
            {
                "kind": kind,
                "id": record_id,
                "disposition": "mapping_candidate" if matches else "unresolved",
            }
        )
    return {
        "status": "offline_relocation_preview",
        "activation_permitted": False,
        "resume_permitted": False,
        "source_observation_verified": False,
        "dispositions": dispositions,
    }
