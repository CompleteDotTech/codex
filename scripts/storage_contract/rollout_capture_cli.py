"""Path-free preview of rollout files in a user-supplied snapshot home."""

import json
import os

from .cli import _Parser
from .records import ContractError, require
from .rollout_capture_preview import preview


def main(argv=None):
    parser = _Parser(
        description="Preview rollout snapshot ambiguity; never capture or activate."
    )
    parser.add_argument("--snapshot-home", required=True)
    parser.add_argument("--compressed-header-helper")
    try:
        args = parser.parse_args(argv)
        require("\0" not in args.snapshot_home, "invalid_input_path")
        if args.compressed_header_helper is not None:
            require("\0" not in args.compressed_header_helper, "invalid_helper_path")
            require(
                os.path.isabs(args.compressed_header_helper),
                "invalid_helper_path",
            )
        print(
            json.dumps(
                preview(
                    args.snapshot_home,
                    compressed_header_helper=args.compressed_header_helper,
                ),
                sort_keys=True,
            )
        )
        return 0
    except ContractError as exc:
        print(
            json.dumps(
                {
                    "status": "rejected",
                    "code": str(exc),
                    "capture_complete": False,
                    "activation_permitted": False,
                }
            )
        )
        return 2
    except (OSError, ValueError, RuntimeError):
        print(
            json.dumps(
                {
                    "status": "rejected",
                    "code": "input_unavailable",
                    "capture_complete": False,
                    "activation_permitted": False,
                }
            )
        )
        return 3
