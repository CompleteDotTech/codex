"""Audit an offline SQLite backup artifact; no live capture or activation."""

import json
import os
from pathlib import Path

from .cli import _Parser
from .inputs import open_regular_input
from .manifest import MAX_MANIFEST_BYTES
from .records import ContractError, require
from .sqlite_snapshot import AuditLimits, audit_snapshot


def _reject_sidecars(path: Path) -> None:
    require(
        not any(
            os.path.lexists(str(path) + suffix)
            for suffix in ("-wal", "-shm", "-journal")
        ),
        "sqlite_sidecars_present",
    )


def main(argv=None) -> int:
    parser = _Parser(
        description="Audit one offline SQLite backup; never migrate or activate."
    )
    parser.add_argument("--snapshot", required=True)
    parser.add_argument(
        "--policy",
        required=True,
        help="Independent trusted schema hash and explicit table inventory",
    )
    parser.add_argument(
        "--expected-sha256",
        required=True,
        help="Digest from an independent capture receipt, not from the backup",
    )
    try:
        args = parser.parse_args(argv)
        require("\0" not in args.snapshot, "invalid_input_path")
        path = Path(args.snapshot).resolve(strict=True)
        _reject_sidecars(path)
        with open_regular_input(str(path), AuditLimits().max_bytes) as source:
            with open_regular_input(args.policy, MAX_MANIFEST_BYTES) as policy:
                report = audit_snapshot(
                    source, args.expected_sha256, policy.read(MAX_MANIFEST_BYTES + 1)
                )
        _reject_sidecars(path)
        print(json.dumps(report, sort_keys=True))
        return 0
    except ContractError as exc:
        print(
            json.dumps(
                {"status": "rejected", "code": str(exc), "activation_permitted": False}
            )
        )
        return 2
    except (OSError, ValueError, RuntimeError):
        print(
            json.dumps(
                {
                    "status": "rejected",
                    "code": "input_unavailable",
                    "activation_permitted": False,
                }
            )
        )
        return 3
