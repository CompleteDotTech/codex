-- Inactive thread timestamp allocation. SQLite keeps a process-local high-water mark so hot
-- writes get unique, monotonic millisecond timestamps for cursor ordering. Remote clients share
-- one mark per namespace, advanced inside the write that uses it, so ordering stays unique
-- across hosts.
CREATE TABLE codex_storage.thread_timestamp_marks (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    updated_at_ms BIGINT NOT NULL DEFAULT 0 CHECK (updated_at_ms >= 0),
    recency_at_ms BIGINT NOT NULL DEFAULT 0 CHECK (recency_at_ms >= 0)
);

INSERT INTO codex_storage.thread_timestamp_marks (singleton) VALUES (TRUE);

UPDATE codex_storage.codex_schema_meta
SET format_version = 12, min_reader_version = 12, min_writer_version = 12
WHERE singleton = TRUE;
