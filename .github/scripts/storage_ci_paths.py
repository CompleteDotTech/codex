#!/usr/bin/env python3
"""Select CI for standalone storage tools without relaxing checks for Rust code."""

import argparse
import json
import os
from pathlib import Path
import runpy
import subprocess


NATIVE_JOBS = frozenset({"bazel", "rust-ci", "sdk", "rust-ci-full", "v8-canary"})
TOOL_PREFIXES = ("scripts/storage_contract/", "scripts/postgres/")
TOOL_FILES = frozenset(
    {
        "scripts/audit_sqlite_snapshot.py",
        "scripts/verify_storage_bundle.py",
        ".github/scripts/storage_ci_paths.py",
        ".github/scripts/test_storage_ci_paths.py",
        ".github/workflows/blocking-ci.yml",
        ".github/workflows/postmerge-ci.yml",
        ".github/workflows/storage-tools.yml",
    }
)


def requires_native_checks(paths: list[str]) -> bool:
    """Unknown/mixed changes run native checks; only named CI glue is exempt."""
    return not paths or any(
        not (path.startswith(TOOL_PREFIXES) or path in TOOL_FILES) for path in paths
    )


def required_dependencies(needs: dict) -> dict:
    """Accept a skip only when the successful classifier deliberately chose it."""
    changed = needs.get("changed", {})
    if changed.get("result") != "success":
        return needs
    if changed.get("outputs", {}).get("native") != "false":
        return needs
    return {
        name: value
        for name, value in needs.items()
        if name not in NATIVE_JOBS or value.get("result") != "skipped"
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--require", action="store_true")
    args = parser.parse_args()
    if args.require:
        os.environ["NEEDS"] = json.dumps(
            required_dependencies(json.loads(os.environ["NEEDS"]))
        )
        runpy.run_path(
            str(Path(__file__).with_name("check_ci_results.py")), run_name="__main__"
        )
        return

    base, head = os.environ["BASE_SHA"], os.environ["HEAD_SHA"]
    if not base or set(base) == {"0"}:
        native = True
    else:
        result = subprocess.run(
            ["git", "diff", "--name-only", "--no-renames", "-z", base, head],
            check=True,
            capture_output=True,
        )
        paths = (
            result.stdout.decode("utf-8", errors="surrogateescape")
            .rstrip("\0")
            .split("\0")
        )
        native = requires_native_checks(paths)
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
        output.write(f"native={str(native).lower()}\n")
    print(f"Native build checks required: {native}")


if __name__ == "__main__":
    main()
