"""Read-only SQLx 0.9 history checks on an independently captured SQLite backup.

This is an offline contract helper, not a migrator, schema validator, writer
fence, live-file reader, or binary-compatibility/cutover authority. The caller
must authenticate and bound the standalone backup and independently authenticate
its complete expected migration inventory before opening the connection.
"""

import sqlite3
from contextlib import closing

from .records import ContractError, integer, require

MAX_MIGRATIONS = 1024
SQLX_STORES = frozenset(
    {
        "state_5.sqlite",
        "goals_1.sqlite",
        "logs_2.sqlite",
        "memories_1.sqlite",
        "memories_v2_1.sqlite",
        "thread_history_1.sqlite",
        "queue_1.sqlite",
    }
)
# SHA-384 of exact source blob ccbf79f05fab905ed655f35c215f55a66f3c0f2a.
# See SQLX_HISTORY.md. Recognition requests repair; it never performs the repair.
_RECENCY_CHECKSUM = bytes.fromhex(
    "c217b3d14c08c23603f485af11452ee2b68f65fa8dbaf215f33f53b608c092be8738de"
    "3f63d6a38f120edea5af247c17"
)
_COLUMNS = [
    (0, "version", "BIGINT", 0, None, 1, 0),
    (1, "description", "TEXT", 1, None, 0, 0),
    (2, "installed_on", "TIMESTAMP", 1, "CURRENT_TIMESTAMP", 0, 0),
    (3, "success", "BOOLEAN", 1, None, 0, 0),
    (4, "checksum", "BLOB", 1, None, 0, 0),
    (5, "execution_time", "BIGINT", 1, None, 0, 0),
]
# Never materialize descriptions, timestamps, malformed large checksums, or
# non-integer version/status/timing values in Python. main defeats temp shadowing.
_READ_HISTORY = """
SELECT CASE WHEN typeof(version) = 'integer' THEN version END,
       CASE WHEN typeof(success) = 'integer' THEN success END,
       CASE WHEN typeof(checksum) = 'blob' AND length(checksum) = 48
            THEN checksum END,
       CASE WHEN typeof(execution_time) = 'integer' THEN execution_time END,
       typeof(description), typeof(installed_on)
FROM main._sqlx_migrations LIMIT ?
"""


def audit_history(
    connection: sqlite3.Connection, *, store: str, expected: dict[int, bytes]
) -> dict:
    """Compare captured bookkeeping with an independent complete checksum map.

    Checksums are SHA-384 of exact SQL UTF-8 bytes, without newline/BOM rewriting.
    Caller-supplied expected history is a trust input, not derived from this DB.
    No SQL is applied and no row is changed. Successful history checks still need
    independent schema, data, file, capture/fence and exact-binary verification.
    Absent optional stores require a separate inventory decision, not an empty
    successful history. The independent message board does not use this format.
    """
    require(isinstance(connection, sqlite3.Connection), "invalid_sqlx_connection")
    require(connection.text_factory is str, "invalid_sqlx_connection")
    require(type(store) is str and store in SQLX_STORES, "unsupported_sqlx_store")
    require(
        type(expected) is dict and 0 < len(expected) <= MAX_MIGRATIONS,
        "invalid_expected_history",
    )
    for version, checksum in expected.items():
        integer(version, 1)
        require(type(checksum) is bytes and len(checksum) == 48, "invalid_checksum")
    expected = expected.copy()
    try:
        with closing(connection.cursor()) as cursor:
            cursor.row_factory = None
            cursor.execute(
                "SELECT type FROM main.sqlite_schema WHERE name = '_sqlx_migrations'"
            )
            require(cursor.fetchmany(2) == [("table",)], "missing_sqlx_history_table")
            cursor.execute("PRAGMA main.table_xinfo('_sqlx_migrations')")
            require(cursor.fetchmany(7) == _COLUMNS, "unsupported_sqlx_history_schema")
            cursor.execute(_READ_HISTORY, (MAX_MIGRATIONS + 1,))
            rows = cursor.fetchall()
    except sqlite3.Error:
        raise ContractError("sqlx_history_read_failed") from None
    require(len(rows) <= MAX_MIGRATIONS, "sqlx_history_too_large")
    found, timing_unrecorded = {}, []
    for version, success, checksum, elapsed, description_type, timestamp_type in rows:
        integer(version, 1)
        require(version not in found, "duplicate_migration_version")
        require(success == 1, "sqlx_history_not_successful")
        require(type(checksum) is bytes and len(checksum) == 48, "invalid_checksum")
        integer(elapsed, -(1 << 63))
        require(
            description_type == "text" and timestamp_type == "text",
            "invalid_sqlx_history_metadata",
        )
        found[version] = checksum
        # SQLx writes -1 inside the SQL transaction, then timing AFTER commit.
        # It also uses -1 for skip(). Neither this value nor a checksum proves DDL.
        if elapsed == -1:
            timing_unrecorded.append(version)
    require(
        not (
            store == "state_5.sqlite"
            and expected.get(39) == _RECENCY_CHECKSUM
            and found.get(38) == _RECENCY_CHECKSUM
            and 39 not in found
        ),
        "legacy_recency_repair_required",
    )
    require(set(found) == set(expected), "sqlx_history_inventory_mismatch")
    require(found == expected, "sqlx_history_checksum_mismatch")
    return {
        "format_version": 1,
        "store": store,
        "matched_history_versions": sorted(found),
        "timing_unrecorded_versions": sorted(timing_unrecorded),
        "activation_permitted": False,
    }
