-- Inactive goal persistence mirroring the SQLite goal store. Goals follow their thread.
CREATE TABLE codex_storage.thread_goals (
    thread_id UUID PRIMARY KEY REFERENCES codex_storage.threads (id) ON DELETE CASCADE,
    goal_id TEXT COLLATE "C" NOT NULL,
    objective TEXT NOT NULL,
    status TEXT COLLATE "C" NOT NULL CHECK (status IN (
        'active',
        'paused',
        'blocked',
        'usage_limited',
        'budget_limited',
        'complete'
    )),
    token_budget BIGINT,
    tokens_used BIGINT NOT NULL DEFAULT 0,
    time_used_seconds BIGINT NOT NULL DEFAULT 0,
    created_at_ms BIGINT NOT NULL,
    updated_at_ms BIGINT NOT NULL
);

CREATE TABLE codex_storage.thread_goal_continuation_deferrals (
    thread_id UUID PRIMARY KEY
        REFERENCES codex_storage.thread_goals (thread_id) ON DELETE CASCADE
);

UPDATE codex_storage.codex_schema_meta
SET format_version = 7, min_reader_version = 7, min_writer_version = 7
WHERE singleton = TRUE;
