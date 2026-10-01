-- A dataset handed back to local storage is retired: every store refuses writes to it from then
-- on, so a client that still points here cannot keep growing a history nobody reads.
ALTER TABLE codex_storage.storage_activation
    DROP CONSTRAINT storage_activation_state_check;

ALTER TABLE codex_storage.storage_activation
    ADD CONSTRAINT storage_activation_state_check
    CHECK (state IN ('open', 'migrating', 'retired'));

UPDATE codex_storage.codex_schema_meta
SET format_version = 19, min_reader_version = 19, min_writer_version = 19
WHERE singleton = TRUE;
