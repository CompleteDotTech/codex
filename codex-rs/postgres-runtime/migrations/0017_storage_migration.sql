-- Inactive migration bookkeeping. A run records one verified movement of a whole store, its
-- per-domain checkpoints let an interrupted run resume, and the single activation row gates
-- ordinary writers while a migration holds the store.
CREATE TABLE codex_storage.storage_migration_runs (
    run_id UUID PRIMARY KEY,
    direction TEXT NOT NULL CHECK (direction IN ('import', 'export')),
    source_fingerprint TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('running', 'verified', 'failed', 'activated', 'abandoned')),
    started_at_ms BIGINT NOT NULL,
    updated_at_ms BIGINT NOT NULL,
    detail TEXT
);

CREATE TABLE codex_storage.storage_migration_domains (
    run_id UUID NOT NULL REFERENCES codex_storage.storage_migration_runs (run_id) ON DELETE CASCADE,
    domain TEXT NOT NULL,
    resume_cursor TEXT,
    done BOOLEAN NOT NULL DEFAULT FALSE,
    row_count BIGINT NOT NULL DEFAULT 0,
    digest TEXT,
    PRIMARY KEY (run_id, domain)
);

CREATE TABLE codex_storage.storage_activation (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    state TEXT NOT NULL CHECK (state IN ('open', 'migrating')),
    run_id UUID,
    generation BIGINT NOT NULL CHECK (generation >= 0),
    updated_at_ms BIGINT NOT NULL
);

INSERT INTO codex_storage.storage_activation (singleton, state, generation, updated_at_ms)
VALUES (TRUE, 'open', 0, 0);

UPDATE codex_storage.codex_schema_meta
SET format_version = 17, min_reader_version = 17, min_writer_version = 17
WHERE singleton = TRUE;
