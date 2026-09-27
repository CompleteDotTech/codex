"""Read-only command-line verification of operator-owned export snapshots."""

import argparse
import json
from contextlib import ExitStack

from .inputs import open_regular_input
from .manifest import MAX_MANIFEST_BYTES, MAX_TOTAL_BYTES, verify
from .records import ContractError


class _Parser(argparse.ArgumentParser):
    def error(self, _message):
        # argparse's ordinary errors can repeat arbitrary input on stderr.
        raise ContractError("invalid_arguments")


def main(argv=None) -> int:
    parser = _Parser(
        description="Verify a draft storage bundle; never activate or modify storage."
    )
    parser.add_argument("--manifest", required=True)
    parser.add_argument("--payload", required=True)
    parser.add_argument(
        "--inventory",
        required=True,
        help="Trusted inventory, obtained independently of the export",
    )
    parser.add_argument(
        "--expected-manifest-sha256",
        required=True,
        help="Trusted capture/plan digest; not a checksum supplied by the export",
    )
    try:
        args = parser.parse_args(argv)
        with ExitStack() as stack:
            streams = []
            for path, maximum in (
                (args.manifest, MAX_MANIFEST_BYTES),
                (args.inventory, MAX_MANIFEST_BYTES),
                (args.payload, MAX_TOTAL_BYTES),
            ):
                streams.append(stack.enter_context(open_regular_input(path, maximum)))
            manifest, inventory, payload = streams
            report = verify(
                manifest.read(MAX_MANIFEST_BYTES + 1),
                payload,
                args.expected_manifest_sha256,
                inventory.read(MAX_MANIFEST_BYTES + 1),
            )
        print(json.dumps(report, sort_keys=True))
        return 0
    except ContractError as exc:
        print(
            json.dumps(
                {"status": "rejected", "code": str(exc), "activation_permitted": False}
            )
        )
        return 2
    except OSError:
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
