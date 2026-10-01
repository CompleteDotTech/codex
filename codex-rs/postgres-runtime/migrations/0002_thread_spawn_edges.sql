CREATE TABLE codex_storage.thread_spawn_edges (
    parent_thread_id UUID NOT NULL,
    child_thread_id UUID PRIMARY KEY,
    status TEXT NOT NULL CHECK (status IN ('open', 'closed'))
);

CREATE INDEX idx_thread_spawn_edges_parent_status
    ON codex_storage.thread_spawn_edges (parent_thread_id, status, child_thread_id);

UPDATE codex_storage.codex_schema_meta
SET format_version = 2, min_reader_version = 2, min_writer_version = 2
WHERE singleton = TRUE;
