"""Audit immutable name-index captures; never choose names or activate storage."""

import argparse
import hashlib
import json
import os
from typing import BinaryIO

from .inputs import open_regular_input
from .records import ContractError, fields, parse_json, require, token

MAX_INDEX_BYTES = 1 << 40
MAX_INDEX_LINE_BYTES = 1 << 20
UUID_TEXT = r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}"


def verify_session_index(
    stream: BinaryIO, *, expected_sha256: str
) -> dict[str, str | int | bool]:
    """Verify bounded structure and exact bytes against an independent digest.

    The caller must supply a protected offline capture and a separately trusted
    digest. Every physical line contributes to that digest. Repeated IDs, empty
    names, original whitespace and timestamp strings are not reduced or sorted.
    Success neither reconciles SQLite nor proves native reader/version parity.
    """
    token(expected_sha256, r"[0-9a-f]{64}")
    digest = hashlib.sha256()
    total = lines = records = blanks = empty_names = largest = 0
    while True:
        line = stream.readline(min(MAX_INDEX_LINE_BYTES, MAX_INDEX_BYTES - total) + 1)
        if not line:
            break
        total += len(line)
        require(total <= MAX_INDEX_BYTES, "index_too_large")
        require(len(line) <= MAX_INDEX_LINE_BYTES, "index_line_too_large")
        require(line.endswith(b"\n"), "index_incomplete_line")
        digest.update(line)
        lines += 1
        largest = max(largest, len(line))
        if not line.strip(b" \t\r\n"):
            blanks += 1
            continue
        entry = fields(
            parse_json(line, MAX_INDEX_LINE_BYTES), {"id", "thread_name", "updated_at"}
        )
        require(all(type(value) is str for value in entry.values()), "index_field_type")
        token(entry["id"], UUID_TEXT)
        try:
            for value in entry.values():
                value.encode("utf-8")
        except UnicodeError:
            raise ContractError("index_invalid_text") from None
        records += 1
        empty_names += entry["thread_name"] == ""
    actual = digest.hexdigest()
    require(actual == expected_sha256, "source_digest_mismatch")
    return {
        "format_version": 1,
        "status": "verified",
        "scope": "session_index_snapshot_structure_and_bytes",
        "activation_permitted": False,
        "raw_sha256": actual,
        "source_bytes": total,
        "physical_lines": lines,
        "name_records": records,
        "blank_lines": blanks,
        "empty_name_records": empty_names,
        "largest_line_bytes": largest,
    }


def _identity(metadata: os.stat_result) -> tuple[int, ...]:
    return (
        metadata.st_dev,
        metadata.st_ino,
        metadata.st_size,
        metadata.st_mtime_ns,
        metadata.st_ctime_ns,
    )


def audit_session_index_snapshot(path: str, *, expected_sha256: str) -> dict:
    """Read a regular offline capture; metadata checks do not fence live writers."""
    token(expected_sha256, r"[0-9a-f]{64}")
    with open_regular_input(path, MAX_INDEX_BYTES) as stream:
        before = _identity(os.fstat(stream.fileno()))
        report = verify_session_index(stream, expected_sha256=expected_sha256)
        require(before == _identity(os.fstat(stream.fileno())), "source_changed")
    return report


class _Parser(argparse.ArgumentParser):
    def error(self, _message):
        raise ContractError("invalid_arguments")


def main(argv=None) -> int:
    parser = _Parser(description="Audit an offline name index, never live storage.")
    parser.add_argument("--snapshot", required=True)
    parser.add_argument(
        "--expected-sha256",
        required=True,
        help="Digest from a separate trusted capture",
    )
    try:
        args = parser.parse_args(argv)
        report = audit_session_index_snapshot(
            args.snapshot, expected_sha256=args.expected_sha256
        )
    except ContractError as exc:
        report = {"status": "rejected", "code": str(exc), "activation_permitted": False}
        exit_code = 2
    except OSError:
        report = {
            "status": "rejected",
            "code": "input_unavailable",
            "activation_permitted": False,
        }
        exit_code = 3
    else:
        exit_code = 0
    print(json.dumps(report, sort_keys=True))
    return exit_code
