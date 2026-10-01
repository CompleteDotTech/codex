-- Inactive ownership record. Future writers must validate the token in the same
-- transaction as their data mutation; this table alone fences no current path.
CREATE TABLE codex_storage.thread_writer_ownership (
    thread_id UUID PRIMARY KEY,
    token BIGINT NOT NULL CHECK (token >= 0),
    owner_id UUID,
    lease_until TIMESTAMPTZ,
    CONSTRAINT thread_writer_ownership_lease_pair_check
        CHECK ((owner_id IS NULL) = (lease_until IS NULL))
);

UPDATE codex_storage.codex_schema_meta
SET format_version = 6, min_reader_version = 6, min_writer_version = 6
WHERE singleton = TRUE;
