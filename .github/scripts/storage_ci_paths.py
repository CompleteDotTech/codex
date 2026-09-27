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
        "scripts/audit_session_index.py",
        "scripts/verify_storage_bundle.py",
        ".github/scripts/storage_ci_paths.py",
        ".github/scripts/test_storage_ci_paths.py",
        ".github/scripts/verify_cargo_workspace_manifests.py",
        ".codespellignore",
        ".github/workflows/blocking-ci.yml",
        ".github/workflows/postmerge-ci.yml",
        ".github/workflows/storage-tools.yml",
    }
)
V8_WORKFLOW = ".github/workflows/v8-canary.yml"
# Only this metadata guard may change without scheduling native jobs. Comparing
# the remaining Git blobs preserves full checks for every other V8 edit.
V8_STORAGE_GUARD = b"""          if [[ "${EVENT_NAME}" == "pull_request" ]] && python3 .github/scripts/storage_ci_paths.py --storage-only; then
            echo "canary_required=false" >> "$GITHUB_OUTPUT"
            echo "windows_source_required=false" >> "$GITHUB_OUTPUT"
            exit 0
          fi

"""
V8_GUARD_ANCHOR = (
    b"          # Manual runs have no meaningful before/after range. Force every V8\n"
)


def requires_native_checks(
    paths: list[str], *, v8_workflow_change: tuple[bytes, bytes] | None = None
) -> bool:
    """Unknown/mixed changes run native checks; only named CI glue is exempt."""
    for path in paths:
        if path.startswith(TOOL_PREFIXES) or path in TOOL_FILES:
            continue
        if path == V8_WORKFLOW and v8_workflow_change is not None:
            before, after = v8_workflow_change
            guarded_anchor = V8_STORAGE_GUARD + V8_GUARD_ANCHOR
            if (
                before.count(guarded_anchor) <= 1
                and after.count(guarded_anchor) <= 1
                and before.count(guarded_anchor) + after.count(guarded_anchor) == 1
                and before.replace(guarded_anchor, V8_GUARD_ANCHOR)
                == after.replace(guarded_anchor, V8_GUARD_ANCHOR)
            ):
                continue
        return True
    return not paths


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


def changed_paths(
    base: str, head: str, event: str, *, root: Path | None = None
) -> tuple[str, list[str]]:
    """PRs compare their merge base; pushes compare the exact before/after pair."""
    if event == "pull_request":
        base = subprocess.check_output(
            ["git", "merge-base", base, head], cwd=root, text=True
        ).strip()
    result = subprocess.run(
        ["git", "diff", "--name-only", "--no-renames", "-z", base, head],
        cwd=root,
        check=True,
        capture_output=True,
    )
    paths = (
        result.stdout.decode("utf-8", errors="surrogateescape").rstrip("\0").split("\0")
    )
    return base, paths


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--require", action="store_true")
    mode.add_argument("--storage-only", action="store_true")
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
        base, paths = changed_paths(base, head, os.environ.get("EVENT_NAME", ""))
        v8_workflow_change = None
        if V8_WORKFLOW in paths:
            try:
                v8_workflow_change = tuple(
                    subprocess.check_output(
                        ["git", "show", f"{revision}:{V8_WORKFLOW}"],
                        stderr=subprocess.PIPE,
                    )
                    for revision in (base, head)
                )
            except subprocess.CalledProcessError:
                # Added/deleted/renamed workflows cannot use the guard exception.
                # If either blob is unavailable, retain the full native checks.
                v8_workflow_change = None
        native = requires_native_checks(paths, v8_workflow_change=v8_workflow_change)
    if args.storage_only:
        raise SystemExit(1 if native else 0)
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
        output.write(f"native={str(native).lower()}\n")
    print(f"Native build checks required: {native}")


if __name__ == "__main__":
    main()
