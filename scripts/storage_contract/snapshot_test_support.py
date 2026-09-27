"""Disposable SQLite schema fixtures; no Codex public-consumer behavior claimed."""

import hashlib
import json
import sqlite3
from contextlib import closing
from pathlib import Path

from .records import encode_row

FIXTURES = Path(__file__).with_name("fixtures")


def apply_source_sql(connection, name):
    provenance = json.loads((FIXTURES / "PROVENANCE.json").read_bytes())
    entry = next(item for item in provenance["assets"] if item["file"] == name)
    data = (FIXTURES / name).read_bytes()
    if hashlib.sha256(data).hexdigest() != entry["sha256"]:
        raise AssertionError("fixture_source_changed")
    if entry["representation"] == "entire_file":
        blob = b"blob " + str(len(data)).encode() + b"\0" + data
        if hashlib.sha1(blob).hexdigest() != entry["source_blob"]:
            raise AssertionError("fixture_blob_changed")
    connection.executescript(data.decode("utf-8"))


def policy_for(connection, treatments=None):
    # Independent fixture-side construction of the documented schema digest.
    rows = connection.execute(
        "SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name"
    ).fetchall()
    digest = hashlib.sha256(b"CDTX-sqlite-schema-v1\0")
    for ordinal, row in enumerate(rows, 1):
        record = encode_row(ordinal.to_bytes(8, "big"), row)
        digest.update(len(record).to_bytes(8, "big") + record)
    digest.update(len(rows).to_bytes(8, "big"))
    tables = {row[1]: "migrate" for row in rows if row[0] == "table"}
    tables.update(treatments or {})
    return json.dumps(
        {"version": 1, "schema_sha256": digest.hexdigest(), "tables": tables}
    ).encode()


def make_fixture(path, kind):
    with closing(sqlite3.connect(path)) as connection, connection:
        if kind == "queue":
            apply_source_sql(connection, "queue_0001.sql")
            # Populate the old schema before revision migration: exercise backfill.
            connection.execute(
                "INSERT INTO queued_items VALUES (?,?,?,?,?,?)",
                (
                    "q1",
                    "thread-a",
                    '{"fixture":"before-revision-migration"}',
                    0,
                    1000,
                    1000,
                ),
            )
            connection.commit()
            apply_source_sql(connection, "queue_0002.sql")
            connection.execute(
                "INSERT INTO queued_items VALUES (?,?,?,?,?,?)",
                ("q2", "thread-b", '{"fixture":"雪"}', 0, 2000, 2000),
            )
            connection.execute("DELETE FROM queued_items WHERE id='q2'")
        elif kind == "board":
            apply_source_sql(connection, "board_schema.sql")
            connection.execute(
                "INSERT INTO deleted_boards VALUES (?)", ("deleted-root",)
            )
            connection.execute(
                "INSERT INTO channels VALUES (?,?,?,?,?,?)",
                (
                    "root-a",
                    "general",
                    "general",
                    "2026-09-27T00:00:00Z",
                    1000,
                    "agent-a",
                ),
            )
            connection.execute(
                "INSERT INTO posts VALUES (?,?,?,?,?,?,?,?,?,?,?)",
                (
                    30,
                    "root-a",
                    "post-a",
                    "general",
                    "post-a",
                    "agent-a",
                    1000,
                    "fixture",
                    '{"fixture":"é / é / 雪"}',
                    "request-a",
                    "{}",
                ),
            )
            connection.execute(
                "INSERT INTO subscriptions VALUES (?,?,?)",
                ("root-a", "general", "agent-a"),
            )
            connection.execute(
                "INSERT INTO subscription_opt_outs VALUES (?,?,?)",
                ("root-a", "general", "agent-b"),
            )
            # A consumed and deleted ID must not disappear from sequence evidence.
            connection.execute("UPDATE sqlite_sequence SET seq=80 WHERE name='posts'")
        else:
            raise AssertionError("unknown_fixture")
        connection.commit()
        policy = policy_for(connection)
    return policy
