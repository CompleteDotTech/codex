"""Seed only an explicitly owned empty restore-test namespace, without Rust bootstrap."""

import argparse
import hashlib
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from qualification_checks import sql


def seed(state):
    migration = (
        Path(__file__).parent / "fixtures/0001_codex_storage_metadata.sql"
    ).read_bytes()
    checksum = hashlib.sha384(migration).hexdigest()
    sql(
        state,
        f"""BEGIN;
SET LOCAL ROLE codex_owner;
DO $empty$ BEGIN
    IF EXISTS (SELECT 1 FROM pg_class WHERE relnamespace='codex_storage'::regnamespace)
       OR EXISTS (SELECT 1 FROM pg_proc WHERE pronamespace='codex_storage'::regnamespace)
       OR EXISTS (SELECT 1 FROM pg_type WHERE typnamespace='codex_storage'::regnamespace) THEN
        RAISE EXCEPTION 'restore fixture namespace must be empty';
    END IF;
END $empty$;
CREATE TABLE codex_storage._codex_pg_migrations (
    version BIGINT PRIMARY KEY,
    description TEXT NOT NULL,
    installed_on TIMESTAMPTZ NOT NULL DEFAULT now(),
    success BOOLEAN NOT NULL,
    checksum BYTEA NOT NULL,
    execution_time BIGINT NOT NULL
);
{migration.decode("utf-8")}
INSERT INTO codex_storage._codex_pg_migrations
    (version,description,success,checksum,execution_time)
VALUES (1,'codex storage metadata',TRUE,decode('{checksum}','hex'),0);
REVOKE ALL ON codex_storage.codex_schema_meta FROM codex_runtime;
GRANT SELECT ON codex_storage.codex_schema_meta TO codex_runtime;
REVOKE ALL ON codex_storage._codex_pg_migrations FROM codex_runtime, codex_backup;
GRANT SELECT ON codex_storage._codex_pg_migrations TO codex_backup;
COMMIT;""",
    )
    metadata = sql(
        state,
        "SELECT singleton::text || ':' || format_version || ':' || min_reader_version || ':' || min_writer_version FROM codex_storage.codex_schema_meta",
    )
    history = sql(
        state,
        "SELECT version::text || ':' || description || ':' || success::text || ':' || encode(checksum,'hex') FROM codex_storage._codex_pg_migrations",
    )
    if (
        metadata != "true:1:1:1"
        or history != f"1:codex storage metadata:true:{checksum}"
    ):
        raise RuntimeError("restore fixture readback differs from canonical migration")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--state", type=Path, required=True)
    parser.add_argument("--owned-disposable", action="store_true", required=True)
    args = parser.parse_args()
    seed(args.state)
