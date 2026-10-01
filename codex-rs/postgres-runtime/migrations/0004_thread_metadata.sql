-- Paths identify the source host's files; readers must not resolve them locally.
CREATE TABLE codex_storage.threads (
    id UUID PRIMARY KEY,
    originator TEXT,
    creator_user_id TEXT,
    creator_account_id TEXT,
    origin_rollout_path TEXT NOT NULL,
    created_at_ms BIGINT NOT NULL,
    updated_at_ms BIGINT NOT NULL,
    recency_at_ms BIGINT NOT NULL,
    source TEXT NOT NULL,
    history_mode TEXT NOT NULL,
    thread_source TEXT,
    agent_nickname TEXT,
    agent_role TEXT,
    agent_path TEXT,
    model_provider TEXT NOT NULL,
    model TEXT,
    reasoning_effort TEXT,
    origin_cwd TEXT NOT NULL,
    cli_version TEXT NOT NULL,
    title TEXT NOT NULL,
    name TEXT,
    preview TEXT,
    sandbox_policy TEXT NOT NULL,
    approval_mode TEXT NOT NULL,
    tokens_used BIGINT NOT NULL DEFAULT 0,
    first_user_message TEXT,
    archived_at_s BIGINT,
    thread_section_id TEXT,
    section_position BIGINT,
    section_entered_at_ms BIGINT,
    project_id TEXT,
    daybreak_enabled BOOLEAN,
    git_sha TEXT,
    git_branch TEXT,
    git_origin_url TEXT
);

CREATE INDEX idx_threads_recency_id
    ON codex_storage.threads (recency_at_ms DESC, id DESC);

UPDATE codex_storage.codex_schema_meta
SET format_version = 4, min_reader_version = 4, min_writer_version = 4
WHERE singleton = TRUE;
