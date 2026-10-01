CREATE TABLE codex_storage.external_agent_config_imports (
    import_id TEXT PRIMARY KEY,
    provider_id TEXT,
    completed_at_ms BIGINT NOT NULL,
    successes TEXT NOT NULL,
    failures TEXT NOT NULL
);

CREATE INDEX idx_external_agent_config_imports_history
    ON codex_storage.external_agent_config_imports (completed_at_ms DESC, import_id ASC);

UPDATE codex_storage.codex_schema_meta
SET format_version = 3, min_reader_version = 3, min_writer_version = 3
WHERE singleton = TRUE;
