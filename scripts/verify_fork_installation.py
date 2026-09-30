"""Read-only receipt-bound status for an inactive Linux fork slot.

This reports an authenticated staging receipt, not the running executable or
activation authorization. It never repairs, activates or removes artifacts.
"""

import argparse
import json
from pathlib import Path

from codex_package.fork_side_by_side import read_staged_fork_receipt


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("install_root", type=Path)
    parser.add_argument("slot_id")
    args = parser.parse_args()
    try:
        receipt = read_staged_fork_receipt(args.install_root, args.slot_id)
    except (OSError, ValueError, NotImplementedError):
        # Do not print parser, OS paths or arbitrary receipt contents on failure.
        print(json.dumps({"status": "unverified", "action": "manualReconciliation"}))
        return 1
    print(
        json.dumps({"status": "verifiedInactive", "receipt": receipt}, sort_keys=True)
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
