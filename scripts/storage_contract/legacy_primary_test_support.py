"""Populated historical SQL fixtures, not a current-store exporter or authority."""

import hashlib
import sqlite3
from contextlib import closing
from pathlib import Path

from .snapshot_test_support import policy_for

# Exact source bytes at fork main 395c622d693cf8ec1c4769cf71bc4a949da3e017.
# These nine files are only a historical prefix, not the complete migration tree.
SOURCE = Path(__file__).resolve().parents[2] / "codex-rs/state/migrations"
MIGRATIONS = (
    ("0001_threads.sql", "7063ce11a45be508749bfe70de23bc64ac2a03d0"),
    ("0002_logs.sql", "b9a2c681d439968e7fd1bd2b2e30d4e714df6bfb"),
    ("0003_logs_thread_id.sql", "c4badb6885592eeefe8b2630ff22aa3c2dba2d1c"),
    ("0004_thread_dynamic_tools.sql", "0f40b5f800599fde7d470f966a980e33f3462429"),
    ("0005_threads_cli_version.sql", "8891562d90075f58db69219bfd843415ef9c5780"),
    ("0006_memories.sql", "e5e34307fc8737478ad1b8443f16711b69a4befe"),
    ("0007_threads_first_user_message.sql", "5e9a7649bb7743f85645b08d9878569292e4acb7"),
    ("0008_backfill_state.sql", "c9fc1fdeb7118b2cf9e1d2400ff72ddca3989c02"),
    (
        "0009_stage1_outputs_rollout_slug.sql",
        "9b3a1e077da022e5a94f27bfa1abde2e2a9bea1c",
    ),
)


def make_legacy_primary(path: Path, version: int = 9) -> bytes:
    """Create only a new disposable fixture; never open a user's existing store.

    SQL comes from trusted repository test assets; payloads are synthetic. The
    returned policy is fixture evidence, not independent production inventory.
    Leases and paths are captured values, not transferable operating authority.
    """
    if type(version) is not int or not 6 <= version <= 9:
        raise ValueError("unsupported_legacy_fixture_version")
    scripts = []
    for name, expected in MIGRATIONS:
        # Git's Windows checkout may write CRLF; authenticate the LF Git blob.
        data = (SOURCE / name).read_bytes().replace(b"\r\n", b"\n")
        blob = b"blob " + str(len(data)).encode("ascii") + b"\0" + data
        if hashlib.sha1(blob).hexdigest() != expected:
            raise AssertionError("legacy_fixture_source_changed")
        scripts.append(data.decode("utf-8"))
    # Callers own the temporary parent. Retain failures for diagnosis, not cleanup.
    with path.open("xb"):
        pass
    with closing(sqlite3.connect(path)) as connection, connection:
        connection.execute("PRAGMA foreign_keys=ON")
        for sql in scripts[:6]:
            connection.executescript(sql)
        for name, title, user_event, archived, origin in (
            ("active", "é / é / 雪", 1, 0, r"C:\fixture\workspace"),
            ("archived", "not a user message", 0, 1, "/fixture/workspace"),
            ("empty-title", "", 1, 0, "/fixture/empty"),
        ):
            connection.execute(
                "INSERT INTO threads (id,rollout_path,created_at,updated_at,source,"
                "model_provider,cwd,title,sandbox_policy,approval_mode,tokens_used,"
                "has_user_event,archived,archived_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                (
                    name,
                    origin + "/rollout.jsonl",
                    1700000000,
                    1700000001,
                    "cli",
                    "fixture",
                    origin,
                    title,
                    "{}",
                    "never",
                    (1 << 53) + 1,
                    user_event,
                    archived,
                    1700000002 if archived else None,
                ),
            )
        connection.executemany(
            "INSERT INTO thread_dynamic_tools VALUES (?,?,?,?,?)",
            [
                ("active", 1, "second", "雪", '{"type":"object"}'),
                ("active", 0, "first", "é", '{"type":"string"}'),
            ],
        )
        connection.execute(
            "INSERT INTO stage1_outputs VALUES (?,?,?,?,?)",
            ("active", 1700000001, "memory\0é / é / 雪", "summary", 1700000003),
        )
        connection.execute(
            "INSERT INTO jobs VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
            (
                "stage1",
                "active",
                "running",
                "origin-worker",
                "origin-token",
                1700000004,
                None,
                1700000099,
                None,
                3,
                None,
                (1 << 53) + 1,
                None,
            ),
        )
        connection.execute(
            "INSERT INTO logs VALUES (?,?,?,?,?,?,?,?,?,?)",
            (
                30,
                1700000005,
                999999999,
                "INFO",
                "fixture",
                "log\0雪",
                None,
                None,
                None,
                "active",
            ),
        )
        connection.execute(
            "INSERT INTO logs (id,ts,ts_nanos,level,target) VALUES (80,0,0,'INFO','deleted')"
        )
        connection.execute("DELETE FROM logs WHERE id=80")
        connection.commit()
        for sql in scripts[6:version]:
            connection.executescript(sql)
        if version >= 8:
            # Source migration initializes wall time; normalize only test data.
            connection.execute("UPDATE backfill_state SET updated_at=1700000030")
        connection.commit()
        return policy_for(connection)
