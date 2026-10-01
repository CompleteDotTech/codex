"""Preview a caller-pinned fork bundle without updating or activating it."""

import argparse
import json
from pathlib import Path

from codex_package.fork_update_preview import preview_fork_update


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("install_root", type=Path)
    parser.add_argument("slot_id")
    parser.add_argument("candidate", type=Path)
    parser.add_argument("expected_manifest_sha256")
    args = parser.parse_args()
    try:
        result = preview_fork_update(
            args.install_root,
            args.slot_id,
            args.candidate,
            args.expected_manifest_sha256,
        )
    except (OSError, ValueError, NotImplementedError, RecursionError):
        print(json.dumps({"status": "blocked", "activationPermitted": False}))
        return 1
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
