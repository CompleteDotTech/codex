"""Pinned SQL fixture catalog, not a production database compatibility authority.

Only source SQL is reproduced: SQLx bookkeeping and runtime split-store capture
are outside this catalog. Unknown stores/versions fail rather than becoming an
empty schema. Running source migrations on a live Codex home is never supported.
"""

import hashlib
import json
import sqlite3
from contextlib import closing
from pathlib import Path

from .manifest import MAX_MANIFEST_BYTES
from .records import ContractError, encode_row, fields, integer, parse_json, require

BASE = "985cf47a4eb6084b2ff6b30ebdb1216acda85bb4"
FIXTURES = Path(__file__).with_name("fixtures")
# Git tree hashes were read independently from the pinned repository, not derived
# from an incoming database or a mutable export manifest.
DIRECTORIES = {
    "goals": ("goals_migrations", "ac02d6b3a8b186daaaecaad1e703664fb6f62433", 2),
    "logs": ("logs_migrations", "7f1e6883777132af5a19059daad4417f08ff78f9", 2),
    "memory": ("memory_migrations", "a609f8f7674be4dc331da2e05884c920bca90b44", 2),
    "history": (
        "thread_history_migrations",
        "6feafbbdc2c71621a023684ae931b668bb42af4d",
        7,
    ),
    "queue": ("queue_migrations", "af9213d98c38a62fcb6fc0b192e19abe28bbe2fd", 2),
}
STORES = {
    "goals_1.sqlite": "goals",
    "logs_2.sqlite": "logs",
    "memories_1.sqlite": "memory",
    "memories_v2_1.sqlite": "memory",
    "thread_history_1.sqlite": "history",
    "queue_1.sqlite": "queue",
    "agent_message_board_1.sqlite": "board",
}
BOARD_HASH = "ca3d1d2469507160a15df016fb857e827821d6963d3f69e85f930460a45923c5"


def verified_migrations(store: str, *, directory: Path = FIXTURES) -> tuple[bytes, ...]:
    """Authenticate a whole source migration directory before returning any SQL.

    Directory injection exists for isolated fixtures, not downloaded code. No SQL
    runs until every asset is checked against the separately pinned Git tree.
    """
    require(type(store) is str and store in STORES, "unsupported_store_schema")
    prefix = STORES[store]
    provenance = fields(
        parse_json((directory / "PROVENANCE.json").read_bytes(), MAX_MANIFEST_BYTES),
        {"repository", "commit", "assets", "scope"},
    )
    require(
        provenance["repository"] == "CompleteDotTech/codex"
        and provenance["commit"] == BASE,
        "source_provenance_mismatch",
    )
    assets = provenance["assets"]
    require(type(assets) is list and len(assets) <= 256, "source_inventory_mismatch")
    for entry in assets:
        fields(
            entry, {"file", "source_path", "source_blob", "representation", "sha256"}
        )
        require(
            all(type(value) is str and len(value) <= 512 for value in entry.values()),
            "source_inventory_mismatch",
        )
    if prefix == "board":
        entries = [e for e in assets if e["file"] == "board_schema.sql"]
        require(
            len(entries) == 1
            and entries[0]["source_path"]
            == "codex-rs/ext/agent-message-board/src/local.rs"
            and entries[0]["source_blob"] == "6413ae176486f4dce5ca881aef1b0bccdc46afc3"
            and entries[0]["representation"] == "SCHEMA_string_value"
            and entries[0]["sha256"] == BOARD_HASH,
            "source_provenance_mismatch",
        )
        content = (directory / "board_schema.sql").read_bytes()
        require(
            hashlib.sha256(content).hexdigest() == BOARD_HASH, "source_asset_mismatch"
        )
        return (content,)
    source_dir, expected_tree, count = DIRECTORIES[prefix]
    source_parent = "codex-rs/state/" + source_dir + "/"
    entries = [
        entry for entry in assets if entry["source_path"].startswith(source_parent)
    ]
    require(len(entries) == count, "source_inventory_mismatch")
    entries.sort(key=lambda entry: entry["source_path"])
    tree, scripts = bytearray(), []
    for index, entry in enumerate(entries, 1):
        name = entry["source_path"][len(source_parent) :]
        require(
            "/" not in name
            and name.startswith(f"{index:04}_")
            and name.endswith(".sql")
            and entry["file"] == f"{prefix}_{index:04}.sql"
            and entry["representation"] == "entire_file",
            "source_inventory_mismatch",
        )
        script = (directory / entry["file"]).read_bytes()
        digest = hashlib.sha1(
            b"blob " + str(len(script)).encode() + b"\0" + script
        ).digest()
        require(
            digest.hex() == entry["source_blob"]
            and hashlib.sha256(script).hexdigest() == entry["sha256"],
            "source_asset_mismatch",
        )
        tree.extend(b"100644 " + name.encode("utf-8") + b"\0" + digest)
        scripts.append(script)
    actual_tree = hashlib.sha1(
        b"tree " + str(len(tree)).encode() + b"\0" + tree
    ).hexdigest()
    require(actual_tree == expected_tree, "source_tree_mismatch")
    return tuple(scripts)


def build_fixture_policy(store: str, *, version: int) -> bytes:
    """Produce an audit policy from independent source SQL in private memory.

    An explicit version is required. This does not check a Codex binary, reader/
    writer versions, portable files, or the database's freshness/ownership.
    It intentionally does not accept SQLx-created databases as equivalent.
    """
    scripts = verified_migrations(store)
    integer(version, 1, len(scripts))
    try:
        with closing(sqlite3.connect(":memory:")) as connection:
            connection.execute("PRAGMA foreign_keys=ON")
            for script in scripts[:version]:
                connection.executescript(script.decode("utf-8"))
            rows = connection.execute(
                "SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name"
            ).fetchall()
            digest = hashlib.sha256(b"CDTX-sqlite-schema-v1\0")
            for ordinal, row in enumerate(rows, 1):
                record = encode_row(ordinal.to_bytes(8, "big"), row)
                digest.update(len(record).to_bytes(8, "big") + record)
            digest.update(len(rows).to_bytes(8, "big"))
            tables = {row[1]: "migrate" for row in rows if row[0] == "table"}
            return json.dumps(
                {"version": 1, "schema_sha256": digest.hexdigest(), "tables": tables}
            ).encode()
    except sqlite3.Error:
        raise ContractError("source_schema_build_failed") from None
