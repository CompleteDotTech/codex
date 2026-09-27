"""Synthetic records under source SQL; not public Codex payload fixtures."""

import sqlite3
from contextlib import closing

from .source_catalog import verified_migrations


def make_extended_fixture(path, store):
    scripts = verified_migrations(store)
    # Fixtures never reuse or mutate a pre-existing destination, even an empty file.
    with open(path, "xb"):
        pass
    with closing(sqlite3.connect(path)) as connection:
        connection.execute("PRAGMA foreign_keys=ON")
        connection.executescript(scripts[0].decode())
        if store == "goals_1.sqlite":
            connection.execute(
                "INSERT INTO thread_goals VALUES (?,?,?,?,?,?,?,?,?)",
                (
                    "t",
                    "goal",
                    "雪 / é / é",
                    "active",
                    None,
                    9007199254740993,
                    123,
                    1700000000001,
                    1700000000002,
                ),
            )
        elif store == "logs_2.sqlite":
            connection.execute(
                "INSERT INTO logs VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
                (
                    500,
                    1700000000,
                    999999999,
                    "INFO",
                    "fixture",
                    "雪\x00é",
                    None,
                    "source.rs",
                    12,
                    "t",
                    "process",
                    999,
                ),
            )
        elif store in {"memories_1.sqlite", "memories_v2_1.sqlite"}:
            connection.execute(
                "INSERT INTO stage1_outputs VALUES (?,?,?,?,?,?,?,?,?,?)",
                (
                    "t",
                    1700000000,
                    "memory " + store,
                    "summary",
                    None,
                    1700000001,
                    None,
                    None,
                    1,
                    1700000000,
                ),
            )
            connection.execute(
                "INSERT INTO jobs VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
                (
                    "phase2",
                    "global",
                    "running",
                    "host-a",
                    "lease-a",
                    1700000000,
                    None,
                    1700000030,
                    None,
                    2,
                    None,
                    1700000000,
                    1699999999,
                ),
            )
        else:
            raise AssertionError("unsupported_extended_fixture")
        connection.commit()
        for script in scripts[1:]:
            connection.executescript(script.decode())
        if store == "goals_1.sqlite":
            connection.execute(
                "INSERT INTO thread_goal_continuation_deferrals VALUES ('t')"
            )
        elif store in {"memories_1.sqlite", "memories_v2_1.sqlite"}:
            connection.execute("UPDATE consolidation_progress SET max_thread_count=31")
        connection.commit()
