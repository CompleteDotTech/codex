-- Inactive canonical rollout storage. Each row is one rollout line exactly as the recorder
-- serialized it, so payloads and field order survive a round trip. Positions are dense and
-- 0-based per thread, which makes an append a compare-and-extend on the next position and lets
-- a retried append after an ambiguous commit be recognized instead of duplicated.
CREATE TABLE codex_storage.thread_rollout_lines (
    thread_id UUID NOT NULL REFERENCES codex_storage.threads (id) ON DELETE CASCADE,
    position BIGINT NOT NULL CHECK (position >= 0),
    ordinal BIGINT,
    line TEXT NOT NULL,
    PRIMARY KEY (thread_id, position)
);

CREATE INDEX idx_thread_rollout_lines_ordinal
    ON codex_storage.thread_rollout_lines (thread_id, ordinal)
    WHERE ordinal IS NOT NULL;

UPDATE codex_storage.codex_schema_meta
SET format_version = 16, min_reader_version = 16, min_writer_version = 16
WHERE singleton = TRUE;
