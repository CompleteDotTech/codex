-- Inactive queue persistence mirroring the SQLite queue store. Queue writes take the counter
-- row lock, so change versions become visible in commit order and readers cannot miss a
-- change that commits late.
CREATE TABLE codex_storage.queued_items (
    id TEXT COLLATE "C" PRIMARY KEY,
    thread_id UUID NOT NULL REFERENCES codex_storage.threads (id) ON DELETE CASCADE,
    payload_json TEXT NOT NULL,
    queue_order BIGINT NOT NULL,
    created_at_ms BIGINT NOT NULL,
    updated_at_ms BIGINT NOT NULL
);

CREATE UNIQUE INDEX queued_items_thread_order_idx
    ON codex_storage.queued_items (thread_id, queue_order);

CREATE TABLE codex_storage.queue_change_counter (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    version BIGINT NOT NULL CHECK (version >= 0)
);

INSERT INTO codex_storage.queue_change_counter (singleton, version) VALUES (TRUE, 0);

CREATE TABLE codex_storage.queued_thread_revisions (
    thread_id UUID PRIMARY KEY REFERENCES codex_storage.threads (id) ON DELETE CASCADE,
    revision BIGINT NOT NULL
);

CREATE INDEX queued_thread_revisions_revision_idx
    ON codex_storage.queued_thread_revisions (revision);

UPDATE codex_storage.codex_schema_meta
SET format_version = 8, min_reader_version = 8, min_writer_version = 8
WHERE singleton = TRUE;
