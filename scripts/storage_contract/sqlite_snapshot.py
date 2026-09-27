"""Read-only auditing of an independently authenticated SQLite backup artifact.

This is not a live-store snapshotter, exporter, migration controller, or backend.
It cannot establish a cross-store fence, freshness, or permission to activate.
"""

import hashlib
import hmac
import sqlite3
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import BinaryIO

from .manifest import HASH_PATTERN, MAX_MANIFEST_BYTES
from .records import MAX_CELLS, MAX_ROW_BYTES, ContractError, encode_row
from .records import fields, integer, parse_json, require, token

MAX_SCHEMA_OBJECTS = 1024
IDENTIFIER = r"[A-Za-z_][A-Za-z0-9_]{0,127}"


@dataclass(frozen=True)
class AuditLimits:
    """Per-artifact upper bounds; increasing them is an operator decision."""

    max_bytes: int = 64 << 20
    max_rows: int = 250_000
    max_vm_steps: int = 10_000_000
    max_encoded_bytes: int = 128 << 20

    def validate(self) -> None:
        integer(self.max_bytes, 512, 1 << 30)
        integer(self.max_rows, 1, 10_000_000)
        integer(self.max_vm_steps, 1, 1_000_000_000)
        integer(self.max_encoded_bytes, 1, 1 << 30)


def load_snapshot_policy(data: bytes) -> dict:
    """Load independently trusted schema and table treatments, never infer them."""
    policy = fields(
        parse_json(data, MAX_MANIFEST_BYTES), {"version", "schema_sha256", "tables"}
    )
    require(
        type(policy["version"]) is int and policy["version"] == 1,
        "unsupported_snapshot_policy",
    )
    token(policy["schema_sha256"], HASH_PATTERN)
    tables = policy["tables"]
    require(
        type(tables) is dict and len(tables) <= MAX_SCHEMA_OBJECTS,
        "invalid_table_inventory",
    )
    for name, treatment in tables.items():
        token(name, IDENTIFIER)
        require(
            type(treatment) is str and treatment in {"migrate", "retain", "regenerate"},
            "invalid_table_treatment",
        )
    return policy


def _authorize(action, _arg1, arg2, _database, _trigger):
    if action in {sqlite3.SQLITE_SELECT, sqlite3.SQLITE_READ}:
        return sqlite3.SQLITE_OK
    if action == sqlite3.SQLITE_FUNCTION and arg2 in {"count", "cdtx_row"}:
        return sqlite3.SQLITE_OK
    if action == sqlite3.SQLITE_PRAGMA and _arg1 in {
        "integrity_check",
        "foreign_key_check",
        "table_xinfo",
    }:
        return sqlite3.SQLITE_OK
    return sqlite3.SQLITE_DENY


def audit_snapshot(
    source: BinaryIO,
    expected_sha256: str,
    policy_data: bytes,
    limits: AuditLimits = AuditLimits(),
) -> dict:
    """Authenticate, stage, and audit one immutable, operator-owned backup.

    The source is only read, and is not closed here. A private temporary copy
    prevents SQLite from opening source-side WAL, journals, or other files.
    The expected digest must come from an independent trusted capture receipt.
    In particular, hashing a live main database file does not make it a backup.
    """
    limits.validate()
    token(expected_sha256, HASH_PATTERN)
    policy = load_snapshot_policy(policy_data)
    with tempfile.TemporaryDirectory(prefix="codex-snapshot-audit-") as directory:
        target = Path(directory) / "snapshot.sqlite"
        size, digest = 0, hashlib.sha256()
        with target.open("xb") as output:
            while True:
                block = source.read(min(64 << 10, limits.max_bytes - size + 1))
                require(type(block) is bytes, "invalid_binary_stream")
                if not block:
                    break
                size += len(block)
                require(size <= limits.max_bytes, "snapshot_too_large")
                digest.update(block)
                output.write(block)
        require(
            hmac.compare_digest(digest.hexdigest(), expected_sha256),
            "snapshot_digest_mismatch",
        )
        with target.open("rb") as header:
            require(header.read(16) == b"SQLite format 3\0", "invalid_sqlite_header")
        try:
            connection = sqlite3.connect(
                target.as_uri() + "?mode=ro&immutable=1", uri=True
            )
            try:
                return _inspect(connection, policy, digest.hexdigest(), size, limits)
            finally:
                connection.close()
        except (sqlite3.Error, UnicodeError, OverflowError):
            raise ContractError("sqlite_audit_failed") from None


def _inspect(
    connection: sqlite3.Connection,
    policy: dict,
    file_hash: str,
    size: int,
    limits: AuditLimits,
) -> dict:
    connection.execute("PRAGMA query_only=ON")
    connection.execute("PRAGMA trusted_schema=OFF")
    connection.execute("PRAGMA temp_store=FILE")
    connection.execute("PRAGMA cache_size=-1024")
    # Python 3.10 lacks setlimit; the authenticated artifact byte cap still
    # bounds input there. On newer runtimes also enforce SQLite's row limit.
    if hasattr(connection, "setlimit"):
        connection.setlimit(sqlite3.SQLITE_LIMIT_LENGTH, MAX_ROW_BYTES)
        connection.setlimit(sqlite3.SQLITE_LIMIT_SQL_LENGTH, MAX_ROW_BYTES)
    steps = 0
    interval = min(1000, limits.max_vm_steps)

    def progress():
        nonlocal steps
        steps += interval
        return int(steps >= limits.max_vm_steps)

    connection.set_progress_handler(progress, interval)
    connection.set_authorizer(_authorize)
    objects = connection.execute(
        "SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name"
    ).fetchmany(MAX_SCHEMA_OBJECTS + 1)
    require(len(objects) <= MAX_SCHEMA_OBJECTS, "too_many_schema_objects")
    schema_hash = hashlib.sha256(b"CDTX-sqlite-schema-v1\0")
    tables = []
    for index, row in enumerate(objects, 1):
        record = encode_row(index.to_bytes(8, "big"), row)
        schema_hash.update(len(record).to_bytes(8, "big") + record)
        kind, name, _table_name, sql = row
        require(kind != "view", "unsupported_sqlite_schema")
        if kind == "table":
            token(name, IDENTIFIER)
            # SQLite normalizes virtual-table DDL to this leading phrase;
            # ordinary identifiers and defaults can also contain "virtual".
            require(
                sql is not None and not sql.upper().startswith("CREATE VIRTUAL TABLE"),
                "unsupported_sqlite_schema",
            )
            tables.append(name)
    schema_hash.update(len(objects).to_bytes(8, "big"))
    require(
        hmac.compare_digest(schema_hash.hexdigest(), policy["schema_sha256"]),
        "snapshot_schema_mismatch",
    )
    require(set(tables) == set(policy["tables"]), "table_inventory_mismatch")
    require(
        connection.execute("PRAGMA integrity_check").fetchone() == ("ok",),
        "snapshot_integrity_failed",
    )
    require(
        connection.execute("PRAGMA foreign_key_check").fetchone() is None,
        "snapshot_foreign_key_failed",
    )
    callback_error = []
    encoded_bytes = 0

    def canonical_row(*values):
        nonlocal encoded_bytes
        try:
            record = encode_row(b"\x01", values)
            encoded_bytes += len(record)
            require(
                encoded_bytes <= limits.max_encoded_bytes,
                "sqlite_encoded_budget_exhausted",
            )
            return record
        except ContractError as exc:
            callback_error.append(str(exc))
            raise

    connection.create_function("cdtx_row", -1, canonical_row, deterministic=True)
    result, total = [], 0
    for name in tables:
        columns = connection.execute(f'PRAGMA table_xinfo("{name}")').fetchall()
        require(
            0 < len(columns) <= MAX_CELLS and all(row[6] == 0 for row in columns),
            "unsupported_sqlite_columns",
        )
        for column in columns:
            token(column[1], IDENTIFIER)
        count = connection.execute(f'SELECT count(*) FROM "{name}"').fetchone()[0]
        total += count
        require(total <= limits.max_rows, "snapshot_too_many_rows")
        treatment = policy["tables"][name]
        logical_hash = None
        if treatment == "migrate":
            encoded_name = name.encode("ascii")
            logical = hashlib.sha256(
                b"CDTX-sqlite-rows-v1\0"
                + len(encoded_name).to_bytes(4, "big")
                + encoded_name
            )
            selections = ",".join(f'"{column[1]}"' for column in columns)
            try:
                rows = connection.execute(
                    f'SELECT cdtx_row({selections}) AS record FROM "{name}" ORDER BY record'
                )
                observed = 0
                for (record,) in rows:
                    logical.update(len(record).to_bytes(8, "big") + record)
                    observed += 1
            except sqlite3.Error:
                if callback_error:
                    raise ContractError(callback_error[0]) from None
                raise
            require(observed == count, "snapshot_count_changed")
            logical.update(observed.to_bytes(8, "big"))
            logical_hash = logical.hexdigest()
        result.append(
            {
                "table": name,
                "treatment": treatment,
                "rows": count,
                "logical_sha256": logical_hash,
            }
        )
    return {
        "status": "snapshot_audited",
        "scope": "single_sqlite_backup_artifact",
        "activation_permitted": False,
        "file_sha256": file_hash,
        "bytes": size,
        "schema_sha256": schema_hash.hexdigest(),
        "tables": result,
    }
