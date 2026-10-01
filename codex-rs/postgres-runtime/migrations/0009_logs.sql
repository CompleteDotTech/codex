-- Inactive runtime log persistence mirroring the SQLite log store. Log ids come from a
-- counter row so ids become visible in commit order and readers that poll for newer ids
-- cannot skip a row that commits late.
CREATE TABLE codex_storage.logs (
    id BIGINT PRIMARY KEY,
    ts BIGINT NOT NULL,
    ts_nanos BIGINT NOT NULL,
    level TEXT NOT NULL,
    target TEXT NOT NULL,
    feedback_log_body TEXT,
    module_path TEXT,
    file TEXT,
    line BIGINT,
    thread_id TEXT,
    process_uuid TEXT,
    estimated_bytes BIGINT NOT NULL DEFAULT 0
);

CREATE INDEX logs_ts_idx
    ON codex_storage.logs (ts DESC, ts_nanos DESC, id DESC);

CREATE INDEX logs_thread_id_ts_idx
    ON codex_storage.logs (thread_id, ts DESC, ts_nanos DESC, id DESC);

CREATE INDEX logs_threadless_process_ts_idx
    ON codex_storage.logs (process_uuid, ts DESC, ts_nanos DESC, id DESC)
    WHERE thread_id IS NULL;

CREATE TABLE codex_storage.log_id_counter (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    last_id BIGINT NOT NULL CHECK (last_id >= 0)
);

INSERT INTO codex_storage.log_id_counter (singleton, last_id) VALUES (TRUE, 0);

UPDATE codex_storage.codex_schema_meta
SET format_version = 9, min_reader_version = 9, min_writer_version = 9
WHERE singleton = TRUE;
