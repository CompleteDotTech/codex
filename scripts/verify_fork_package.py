"""Read-only validation of a staged CompleteDotTech Codex package directory."""

import argparse
from pathlib import Path

from codex_package.fork_identity import verify_fork_package


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("package_dir", type=Path)
    args = parser.parse_args()
    manifest = verify_fork_package(args.package_dir)
    print(
        f"Verified {manifest['owner']} {manifest['variant']} "
        f"{manifest['packageVersion']} for {manifest['target']} "
        f"at {manifest['forkCommit']}"
    )


if __name__ == "__main__":
    main()
