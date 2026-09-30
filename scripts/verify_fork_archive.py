"""Read-only validation of a serialized CompleteDotTech Codex candidate."""

import argparse
import os
from pathlib import Path
import sys


scripts_dir = Path(__file__).resolve().parent
sys.path.insert(0, str(scripts_dir))
os.environ.setdefault("CODEX_REPO_ROOT", str(scripts_dir.parent))


def main() -> None:
    from codex_package.fork_archive import verify_fork_archive

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("package_dir", type=Path)
    parser.add_argument("archive", type=Path)
    args = parser.parse_args()
    digest = verify_fork_archive(args.package_dir, args.archive)
    print(f"sha256:{digest}  {args.archive}")


if __name__ == "__main__":
    main()
