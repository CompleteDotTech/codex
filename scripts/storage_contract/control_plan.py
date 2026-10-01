"""Validate an offline v1 operation plan and preview exclusions.

This is a contract check, not a confirmation, source probe, writer fence,
credential resolver, migration controller, or authority to activate storage.
"""

import hashlib
import hmac

from .manifest import (
    HASH_PATTERN,
    MAX_MANIFEST_BYTES,
    _identity,
    _uuid,
    load_inventory,
    validate_manifest,
)
from .records import fields, parse_json, require, token

OPERATIONS = {"initialize_new", "migrate_local", "attach_existing"}
HOST_RETAINED = frozenset(
    {
        "host.credentials",
        "host.device_identity",
        "host.remote_control_enrollments",
        "host.workspace_checkout",
        "host.installation_receipts",
        "host.settings",
        "host.journals",
        "host.backups",
    }
)
PLAN_FIELDS = {
    "version",
    "operation",
    "operation_id",
    "owner_host_id",
    "source",
    "destination",
    "inventory_sha256",
    "manifest_sha256",
    "destination_occupancy",
    "local_history_present",
}


def preview_plan(
    plan_data: bytes,
    expected_plan_sha256: str,
    trusted_inventory: bytes,
    manifest_data: bytes | None = None,
) -> dict:
    """Check exact plan shape and return a payload-free, non-authorizing preview.

    The caller must obtain the expected plan digest and trusted inventory from
    an independent authority. Occupancy and local-history fields are assertions,
    not probes; runtime must revalidate them under ownership before any action.
    """
    require(len(plan_data) <= MAX_MANIFEST_BYTES, "plan_too_large")
    require(len(trusted_inventory) <= MAX_MANIFEST_BYTES, "inventory_too_large")
    token(expected_plan_sha256, HASH_PATTERN)
    require(
        hmac.compare_digest(
            hashlib.sha256(plan_data).hexdigest(), expected_plan_sha256
        ),
        "plan_digest_mismatch",
    )
    plan = fields(parse_json(plan_data, MAX_MANIFEST_BYTES), PLAN_FIELDS)
    require(type(plan["version"]) is int and plan["version"] == 1, "unsupported_plan")
    operation = plan["operation"]
    require(type(operation) is str and operation in OPERATIONS, "unsupported_operation")
    _uuid(plan["operation_id"])
    _uuid(plan["owner_host_id"])
    token(plan["inventory_sha256"], HASH_PATTERN)
    require(
        hmac.compare_digest(
            hashlib.sha256(trusted_inventory).hexdigest(), plan["inventory_sha256"]
        ),
        "inventory_digest_mismatch",
    )
    inventory = load_inventory(trusted_inventory)
    require(
        all(
            name in inventory and inventory[name]["treatment"] == "retain"
            for name in HOST_RETAINED
        ),
        "host_exclusion_missing",
    )
    require(type(plan["local_history_present"]) is bool, "invalid_local_history")
    occupancy = plan["destination_occupancy"]
    require(
        type(occupancy) is str and occupancy in {"empty", "existing"},
        "invalid_occupancy",
    )
    destination = _identity(plan["destination"])
    require(destination["backend"] == "postgresql", "unsupported_destination")

    if operation == "migrate_local":
        source = _identity(plan["source"])
        require(source["backend"] == "sqlite", "unsupported_source")
        require(occupancy == "empty", "destination_not_empty")
        require(manifest_data is not None, "manifest_required")
        token(plan["manifest_sha256"], HASH_PATTERN)
        manifest = validate_manifest(
            manifest_data, plan["manifest_sha256"], trusted_inventory
        )
        require(
            manifest["migration_id"] == plan["operation_id"]
            and manifest["source"] == source
            and manifest["destination"] == destination,
            "manifest_plan_mismatch",
        )
        local_history_action = "capture_required"
    else:
        require(plan["source"] is None, "implicit_source_import")
        require(
            plan["manifest_sha256"] is None and manifest_data is None,
            "implicit_source_import",
        )
        if operation == "initialize_new":
            require(occupancy == "empty", "destination_not_empty")
            require(destination["generation"] == 1, "invalid_initial_generation")
        else:
            require(occupancy == "existing", "remote_dataset_missing")
        local_history_action = "preserved_not_imported"

    exclusions = [
        {"id": name, "treatment": rule["treatment"]}
        for name, rule in sorted(inventory.items())
        if rule["treatment"] != "migrate"
    ]
    return {
        "status": "draft_plan_preview",
        "activation_permitted": False,
        "source_observation_verified": False,
        "operation": operation,
        "destination_dataset_id": destination["dataset_id"],
        "destination_generation": destination["generation"],
        "local_history_present_assertion": plan["local_history_present"],
        "local_history_action": local_history_action,
        "planned_migrate_domains": sum(
            rule["treatment"] == "migrate" for rule in inventory.values()
        )
        if operation == "migrate_local"
        else 0,
        "verified_migrated_records": 0,
        "excluded_domains": exclusions,
        "not_imported_domains": sorted(
            name for name, rule in inventory.items() if rule["treatment"] == "migrate"
        )
        if operation != "migrate_local"
        else [],
    }
