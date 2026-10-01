#!/usr/bin/env python3
"""Run the workspace Rust formatter within Windows' command-line limit.

Target selection follows cargo-fmt bundled with Rust 1.95: canonical paths,
deduplicated in metadata order, sorted by path, then grouped by edition.
"""

import json
import os
from pathlib import Path
import subprocess
import sys


WINDOWS_COMMAND_LIMIT = 32767


def command_units(arguments: list[str]) -> int:
    """CreateProcess counts UTF-16 units, including the terminating NUL."""
    return len(subprocess.list2cmdline(arguments).encode("utf-16-le")) // 2 + 1


def workspace_targets(metadata: dict, cwd: Path) -> dict[str, list[str]]:
    if Path(metadata["workspace_root"]).resolve(strict=True) != cwd.resolve(
        strict=True
    ):
        raise ValueError("Rust formatting must run at the workspace root")
    targets: dict[str, str] = {}
    for package in metadata["packages"]:
        for target in package["targets"]:
            path = Path(target["src_path"])
            try:
                canonical = str(path.resolve(strict=True))
            except OSError:
                canonical = str(path)
            # cargo-fmt's BTreeSet compares only paths; first insertion wins.
            targets.setdefault(canonical, target["edition"])
    if not targets:
        raise ValueError("No Rust targets found in the workspace")
    groups: dict[str, list[str]] = {}
    for path in sorted(targets):
        groups.setdefault(targets[path], []).append(path)
    return dict(sorted(groups.items()))


def batches(executable: str, files: list[str], options: list[str]) -> list[list[str]]:
    result: list[list[str]] = []
    current: list[str] = []
    for path in files:
        if command_units([executable, path, *options]) > WINDOWS_COMMAND_LIMIT:
            raise ValueError(
                f"Rust target exceeds the Windows command-line limit: {path}"
            )
        if (
            current
            and command_units([executable, *current, path, *options])
            > WINDOWS_COMMAND_LIMIT
        ):
            result.append([executable, *current, *options])
            current = []
        current.append(path)
    if current:
        result.append([executable, *current, *options])
    return result


def run(cwd: Path, options: list[str]) -> int:
    metadata_args = ["cargo", "metadata", "--no-deps", "--format-version", "1"]
    try:
        metadata = json.loads(
            subprocess.check_output([*metadata_args, "--offline"], cwd=cwd)
        )
    except (subprocess.CalledProcessError, json.JSONDecodeError):
        # Preserve cargo-fmt's ordinary metadata fallback for normal users.
        metadata = json.loads(subprocess.check_output(metadata_args, cwd=cwd))
    # Match cargo-fmt's RUSTFMT override, otherwise use the selected toolchain.
    executable = os.environ.get("RUSTFMT")
    if executable is None:
        executable = subprocess.check_output(
            ["rustup", "which", "rustfmt"],
            cwd=cwd,
            text=True,
        ).strip()
    commands = []
    for edition, files in workspace_targets(metadata, cwd).items():
        commands.extend(batches(executable, files, ["--edition", edition, *options]))
    first_failure = 0
    for command in commands:
        status = subprocess.run(command, cwd=cwd, check=False).returncode
        if status and not first_failure:
            first_failure = status
    return first_failure


if __name__ == "__main__":
    try:
        sys.exit(run(Path.cwd(), sys.argv[1:]))
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        print(f"Rust formatter failed: {error}", file=sys.stderr)
        sys.exit(1)
