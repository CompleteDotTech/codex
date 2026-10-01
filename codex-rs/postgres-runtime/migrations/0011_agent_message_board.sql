-- Inactive agent message board persistence mirroring the SQLite board. Board writes take the
-- post counter row lock first, so they serialize like the SQLite immediate transactions and
-- deletion tombstones cannot race with concurrent posts.
CREATE TABLE codex_storage.agent_board_deleted (
    board TEXT COLLATE "C" PRIMARY KEY
);

CREATE TABLE codex_storage.agent_board_channels (
    board TEXT COLLATE "C" NOT NULL,
    name TEXT COLLATE "C" NOT NULL,
    name_search TEXT NOT NULL,
    created_at TEXT NOT NULL,
    timestamp BIGINT NOT NULL,
    author TEXT NOT NULL,
    PRIMARY KEY (board, name)
);

CREATE TABLE codex_storage.agent_board_posts (
    seq BIGINT PRIMARY KEY,
    board TEXT COLLATE "C" NOT NULL,
    id TEXT COLLATE "C" NOT NULL,
    channel TEXT COLLATE "C" NOT NULL,
    root TEXT COLLATE "C" NOT NULL,
    author TEXT NOT NULL,
    timestamp BIGINT NOT NULL,
    body_search TEXT NOT NULL,
    payload TEXT NOT NULL,
    request_id TEXT COLLATE "C" NOT NULL,
    request TEXT NOT NULL,
    CONSTRAINT agent_board_posts_id_key UNIQUE (board, id),
    CONSTRAINT agent_board_posts_request_key UNIQUE (board, request_id)
);

CREATE INDEX agent_board_posts_channel_idx
    ON codex_storage.agent_board_posts (board, channel, seq);

CREATE INDEX agent_board_posts_channel_ts_idx
    ON codex_storage.agent_board_posts (board, channel, timestamp, seq);

CREATE INDEX agent_board_posts_roots_idx
    ON codex_storage.agent_board_posts (board, channel, timestamp, seq)
    WHERE id = root;

CREATE INDEX agent_board_posts_root_idx
    ON codex_storage.agent_board_posts (board, root, seq);

CREATE INDEX agent_board_posts_root_ts_idx
    ON codex_storage.agent_board_posts (board, root, timestamp, seq);

CREATE INDEX agent_board_posts_board_ts_idx
    ON codex_storage.agent_board_posts (board, timestamp, seq);

CREATE TABLE codex_storage.agent_board_subscriptions (
    board TEXT COLLATE "C" NOT NULL,
    target TEXT COLLATE "C" NOT NULL,
    agent TEXT COLLATE "C" NOT NULL,
    PRIMARY KEY (board, target, agent)
);

CREATE TABLE codex_storage.agent_board_opt_outs (
    board TEXT COLLATE "C" NOT NULL,
    target TEXT COLLATE "C" NOT NULL,
    agent TEXT COLLATE "C" NOT NULL,
    PRIMARY KEY (board, target, agent)
);

CREATE TABLE codex_storage.agent_board_post_counter (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    last_seq BIGINT NOT NULL CHECK (last_seq >= 0)
);

INSERT INTO codex_storage.agent_board_post_counter (singleton, last_seq) VALUES (TRUE, 0);

UPDATE codex_storage.codex_schema_meta
SET format_version = 11, min_reader_version = 11, min_writer_version = 11
WHERE singleton = TRUE;
