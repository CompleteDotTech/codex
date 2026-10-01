-- The activation row names the dataset it publishes and when, so a client that restarts after a
-- crash can tell whether the local authority records and this store agree about the same cutover.
ALTER TABLE codex_storage.storage_activation
    ADD COLUMN dataset_id TEXT,
    ADD COLUMN activated_at_ms BIGINT;

UPDATE codex_storage.codex_schema_meta
SET format_version = 18, min_reader_version = 18, min_writer_version = 18
WHERE singleton = TRUE;
