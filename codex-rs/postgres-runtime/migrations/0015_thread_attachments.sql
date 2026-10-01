-- Inactive thread attachment persistence mirroring the SQLite attachment table. Attachment
-- writes lock the owning thread row, so the per-thread limit and the identity uniqueness hold
-- under concurrent writers.
CREATE TABLE codex_storage.thread_attachments (
    id TEXT COLLATE "C" PRIMARY KEY,
    thread_id UUID NOT NULL REFERENCES codex_storage.threads (id) ON DELETE CASCADE,
    attachment_type TEXT NOT NULL,
    identity_key TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    CONSTRAINT thread_attachments_identity_key
        UNIQUE (thread_id, attachment_type, identity_key)
);

CREATE INDEX idx_thread_attachments_thread_created_id
    ON codex_storage.thread_attachments (thread_id, created_at, id);

UPDATE codex_storage.codex_schema_meta
SET format_version = 15, min_reader_version = 15, min_writer_version = 15
WHERE singleton = TRUE;
