CREATE TABLE codex_storage.thread_sections (
    id TEXT COLLATE "C" PRIMARY KEY,
    name TEXT NOT NULL,
    appearance TEXT
);

INSERT INTO codex_storage.thread_sections (id, name)
VALUES ('01984de2-8f74-7c91-a3b2-5c5e937cf318', 'Pinned');

ALTER TABLE codex_storage.threads
    ADD CONSTRAINT threads_thread_section_id_fkey
    FOREIGN KEY (thread_section_id)
    REFERENCES codex_storage.thread_sections (id) ON DELETE SET NULL;

CREATE INDEX idx_threads_section_recency
    ON codex_storage.threads (thread_section_id COLLATE "C", recency_at_ms DESC, id DESC)
    WHERE thread_section_id IS NOT NULL;

CREATE INDEX idx_threads_section_position
    ON codex_storage.threads (thread_section_id COLLATE "C", section_position ASC, id ASC)
    WHERE thread_section_id IS NOT NULL;

UPDATE codex_storage.codex_schema_meta
SET format_version = 5, min_reader_version = 5, min_writer_version = 5
WHERE singleton = TRUE;
