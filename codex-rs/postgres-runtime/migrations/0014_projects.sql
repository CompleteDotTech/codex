-- Inactive project persistence mirroring the SQLite project tables. Project writes lock the
-- projects table in share row exclusive mode, serializing them like SQLite immediate
-- transactions while leaving readers and thread writes unblocked.
CREATE TABLE codex_storage.projects (
    id TEXT NOT NULL PRIMARY KEY,
    name TEXT NOT NULL,
    metadata TEXT NOT NULL DEFAULT '{}',
    position BIGINT NOT NULL,
    created_at_ms BIGINT NOT NULL,
    updated_at_ms BIGINT NOT NULL
);

CREATE TABLE codex_storage.project_roots (
    project_id TEXT NOT NULL REFERENCES codex_storage.projects (id) ON DELETE CASCADE,
    position BIGINT NOT NULL,
    path TEXT NOT NULL,
    PRIMARY KEY (project_id, position)
);

CREATE TABLE codex_storage.project_idempotency_keys (
    key TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    created_at_ms BIGINT NOT NULL
);

ALTER TABLE codex_storage.threads
    ADD CONSTRAINT threads_project_id_fkey
    FOREIGN KEY (project_id) REFERENCES codex_storage.projects (id) ON DELETE SET NULL;

CREATE INDEX idx_projects_position
    ON codex_storage.projects (position ASC, id ASC);

CREATE INDEX idx_threads_project_id
    ON codex_storage.threads (project_id, (archived_at_s IS NULL), created_at_ms DESC, id DESC)
    WHERE project_id IS NOT NULL;

UPDATE codex_storage.codex_schema_meta
SET format_version = 14, min_reader_version = 14, min_writer_version = 14
WHERE singleton = TRUE;
