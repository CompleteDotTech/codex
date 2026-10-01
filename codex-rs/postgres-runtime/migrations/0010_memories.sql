-- Inactive generated-memory persistence mirroring the SQLite memory store. Memory work takes
-- the consolidation progress row lock first, so job claims, completions and output rewrites
-- run one at a time and cannot deadlock across tables.
ALTER TABLE codex_storage.threads
    ADD COLUMN memory_mode TEXT NOT NULL DEFAULT 'enabled';

CREATE TABLE codex_storage.memory_stage1_outputs (
    thread_id UUID PRIMARY KEY,
    source_updated_at BIGINT NOT NULL,
    raw_memory TEXT NOT NULL,
    rollout_summary TEXT NOT NULL,
    rollout_slug TEXT,
    generated_at BIGINT NOT NULL,
    usage_count BIGINT,
    last_usage BIGINT,
    selected_for_phase2 BIGINT NOT NULL DEFAULT 0 CHECK (selected_for_phase2 IN (0, 1)),
    selected_for_phase2_source_updated_at BIGINT
);

CREATE INDEX memory_stage1_outputs_source_updated_idx
    ON codex_storage.memory_stage1_outputs (source_updated_at DESC, thread_id DESC);

CREATE TABLE codex_storage.memory_jobs (
    kind TEXT NOT NULL,
    job_key TEXT NOT NULL,
    status TEXT NOT NULL,
    worker_id TEXT,
    ownership_token TEXT,
    started_at BIGINT,
    finished_at BIGINT,
    lease_until BIGINT,
    retry_at BIGINT,
    retry_remaining BIGINT NOT NULL,
    last_error TEXT,
    input_watermark BIGINT,
    last_success_watermark BIGINT,
    PRIMARY KEY (kind, job_key)
);

CREATE INDEX memory_jobs_kind_status_retry_lease_idx
    ON codex_storage.memory_jobs (kind, status, retry_at, lease_until);

CREATE TABLE codex_storage.memory_consolidation_progress (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    max_thread_count BIGINT NOT NULL DEFAULT 0 CHECK (max_thread_count >= 0)
);

INSERT INTO codex_storage.memory_consolidation_progress (singleton) VALUES (TRUE);

UPDATE codex_storage.codex_schema_meta
SET format_version = 10, min_reader_version = 10, min_writer_version = 10
WHERE singleton = TRUE;
