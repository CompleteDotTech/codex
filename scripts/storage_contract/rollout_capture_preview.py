"""Bounded, path-free preview of rollout files in a user-supplied snapshot.

This does not prove that the snapshot was fenced or that all durable files were
captured. Compressed headers remain uninspected with Python's 3.10 stdlib.
"""

import datetime
import json
import os
import re
import stat
import uuid
from collections import defaultdict
from pathlib import Path

from .records import require

MAX_ENTRIES = 100_000
MAX_DIRECTORIES = 10_000
MAX_HEADER_BYTES = 64 * 1024
MAX_EXAMPLES = 32
NAME = re.compile(
    r"rollout-(\d{4}-\d{2}-\d{2}T\d{2}-\d{2}-\d{2})-"
    r"([0-9a-f-]{36})(?:_([0-9a-f-]{36}))?\.jsonl(\.zst)?"
)


def _uuid(value):
    if type(value) is not str:
        return None
    try:
        return value if str(uuid.UUID(value)) == value else None
    except ValueError:
        return None


def _name_parts(name):
    match = NAME.fullmatch(name)
    if match is None:
        return None
    try:
        datetime.datetime.strptime(match[1], "%Y-%m-%dT%H-%M-%S")
    except ValueError:
        return None
    thread_id = _uuid(match[2])
    rollout_id = _uuid(match[3]) if match[3] else thread_id
    if thread_id is None or rollout_id is None:
        return None
    return thread_id, rollout_id, "compressed" if match[4] else "plain", match[1][:10]


def _header(path):
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(path, flags)
    try:
        require(stat.S_ISREG(os.fstat(descriptor).st_mode), "input_not_regular_file")
        with os.fdopen(descriptor, "rb", closefd=False) as stream:
            data = stream.read(MAX_HEADER_BYTES + 1)
    finally:
        os.close(descriptor)
    for line in data[:MAX_HEADER_BYTES].splitlines():
        if not line or len(line) >= MAX_HEADER_BYTES:
            continue
        try:
            record = json.loads(line)
        except (UnicodeDecodeError, ValueError):
            continue
        if type(record) is not dict or record.get("type") != "session_meta":
            continue
        payload = record.get("payload")
        if type(payload) is not dict:
            return None
        thread_id = _uuid(payload.get("id"))
        history_base = payload.get("history_base")
        if history_base is None:
            return thread_id, None
        if type(history_base) is not dict:
            return None
        ancestor = _uuid(history_base.get("thread_id"))
        return (thread_id, ancestor) if ancestor is not None else None
    return None


def _examples(values):
    ordered = sorted(values)
    return {"count": len(ordered), "examples": ordered[:MAX_EXAMPLES]}


def preview(snapshot_home):
    """Report physical copies and direct plain-header ancestry without paths."""
    home = Path(snapshot_home)
    require(stat.S_ISDIR(os.lstat(home).st_mode), "snapshot_not_directory")
    files = defaultdict(list)
    lineage = set()
    invalid_headers = set()
    unknown_entries = 0
    compressed_headers_unknown = 0
    scanned_entries = 0
    scanned_directories = 0
    collection_present = {}

    for collection, maximum_depth in (("active", 3), ("archive", 0)):
        root = home / ("sessions" if collection == "active" else "archived_sessions")
        try:
            root_mode = os.lstat(root).st_mode
        except FileNotFoundError:
            collection_present[collection] = False
            continue
        require(stat.S_ISDIR(root_mode), "collection_not_directory")
        collection_present[collection] = True
        stack = [(root, 0)]
        while stack:
            directory, depth = stack.pop()
            require(stat.S_ISDIR(os.lstat(directory).st_mode), "collection_changed")
            scanned_directories += 1
            require(scanned_directories <= MAX_DIRECTORIES, "too_many_directories")
            with os.scandir(directory) as entries:
                for entry in entries:
                    scanned_entries += 1
                    require(scanned_entries <= MAX_ENTRIES, "too_many_entries")
                    mode = entry.stat(follow_symlinks=False).st_mode
                    if stat.S_ISDIR(mode):
                        if depth < maximum_depth:
                            stack.append((Path(entry.path), depth + 1))
                        else:
                            unknown_entries += 1
                        continue
                    if not stat.S_ISREG(mode):
                        unknown_entries += 1
                        continue
                    parts = _name_parts(entry.name)
                    if parts is None or (collection == "active" and depth != 3):
                        unknown_entries += 1
                        continue
                    thread_id, rollout_id, representation, date = parts
                    if collection == "active" and directory.relative_to(
                        root
                    ).parts != tuple(date.split("-")):
                        unknown_entries += 1
                        continue
                    files[rollout_id].append((collection, representation))
                    if representation == "compressed":
                        compressed_headers_unknown += 1
                        continue
                    header = _header(entry.path)
                    if header is None or header[0] != thread_id:
                        invalid_headers.add(rollout_id)
                    elif header[1] is not None:
                        lineage.add((rollout_id, header[1]))

    duplicate_ids = {
        rollout_id for rollout_id, copies in files.items() if len(copies) > 1
    }
    sibling_ids = {
        rollout_id
        for rollout_id, copies in files.items()
        if any(
            (where, "plain") in copies and (where, "compressed") in copies
            for where in ("active", "archive")
        )
    }
    active_archive_ids = {
        rollout_id
        for rollout_id, copies in files.items()
        if {where for where, _ in copies} == {"active", "archive"}
    }
    missing = {
        f"{child}:{ancestor}" for child, ancestor in lineage if ancestor not in files
    }
    ambiguous = {
        f"{child}:{ancestor}"
        for child, ancestor in lineage
        if ancestor in duplicate_ids
    }
    return {
        "status": "partial",
        "capture_complete": False,
        "activation_permitted": False,
        "collection_present": collection_present,
        "scanned_entries": scanned_entries,
        "scanned_directories": scanned_directories,
        "canonical_files": sum(map(len, files.values())),
        "unknown_entries": unknown_entries,
        "invalid_plain_headers": _examples(invalid_headers),
        "compressed_headers_unknown": compressed_headers_unknown,
        "duplicate_rollout_ids": _examples(duplicate_ids),
        "plain_compressed_siblings": _examples(sibling_ids),
        "active_archive_copies": _examples(active_archive_ids),
        "missing_direct_ancestors": _examples(missing),
        "ambiguous_direct_ancestors": _examples(ambiguous),
    }
