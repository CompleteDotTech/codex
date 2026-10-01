//! One description of what the embedded migrations create in a storage namespace.
//!
//! Bootstrap, named-namespace bootstrap, and both compatibility checks derive their relation
//! allow-lists, runtime grants, supported formats, and per-migration shape checks from here, so a
//! new migration is registered in exactly one place.

use crate::bootstrap::BASE_MIGRATOR;

/// A table created by the migrations and the access its runtime login receives.
///
/// Backup logins only ever receive `SELECT`, and metadata and history are not listed because
/// they are immutable to runtime and handled explicitly by bootstrap.
pub(crate) struct ProtectedTable {
    pub(crate) name: &'static str,
    /// Privileges granted to the runtime login, for example `SELECT, INSERT, UPDATE`.
    pub(crate) runtime_privileges: &'static str,
    /// Primary key and secondary index relations owned by this table.
    pub(crate) indexes: &'static [&'static str],
}

pub(crate) const PROTECTED_TABLES: &[ProtectedTable] = &[
    ProtectedTable {
        name: "thread_spawn_edges",
        runtime_privileges: "SELECT, INSERT, UPDATE, DELETE",
        indexes: &[
            "thread_spawn_edges_pkey",
            "idx_thread_spawn_edges_parent_status",
        ],
    },
    ProtectedTable {
        name: "external_agent_config_imports",
        runtime_privileges: "SELECT, INSERT, UPDATE",
        indexes: &[
            "external_agent_config_imports_pkey",
            "idx_external_agent_config_imports_history",
        ],
    },
    ProtectedTable {
        name: "threads",
        runtime_privileges: "SELECT, INSERT, UPDATE, DELETE",
        indexes: &[
            "threads_pkey",
            "idx_threads_recency_id",
            "idx_threads_section_recency",
            "idx_threads_section_position",
            "idx_threads_project_id",
        ],
    },
    ProtectedTable {
        name: "thread_sections",
        runtime_privileges: "SELECT, INSERT, UPDATE, DELETE",
        indexes: &["thread_sections_pkey"],
    },
    ProtectedTable {
        name: "thread_writer_ownership",
        runtime_privileges: "SELECT, INSERT, UPDATE",
        indexes: &["thread_writer_ownership_pkey"],
    },
    ProtectedTable {
        name: "thread_goals",
        runtime_privileges: "SELECT, INSERT, UPDATE, DELETE",
        indexes: &["thread_goals_pkey"],
    },
    ProtectedTable {
        name: "thread_goal_continuation_deferrals",
        runtime_privileges: "SELECT, INSERT, DELETE",
        indexes: &["thread_goal_continuation_deferrals_pkey"],
    },
    ProtectedTable {
        name: "queued_items",
        runtime_privileges: "SELECT, INSERT, UPDATE, DELETE",
        indexes: &["queued_items_pkey", "queued_items_thread_order_idx"],
    },
    ProtectedTable {
        name: "queue_change_counter",
        runtime_privileges: "SELECT, UPDATE",
        indexes: &["queue_change_counter_pkey"],
    },
    ProtectedTable {
        name: "queued_thread_revisions",
        runtime_privileges: "SELECT, INSERT, UPDATE",
        indexes: &[
            "queued_thread_revisions_pkey",
            "queued_thread_revisions_revision_idx",
        ],
    },
    ProtectedTable {
        name: "logs",
        runtime_privileges: "SELECT, INSERT, DELETE",
        indexes: &[
            "logs_pkey",
            "logs_ts_idx",
            "logs_thread_id_ts_idx",
            "logs_threadless_process_ts_idx",
        ],
    },
    ProtectedTable {
        name: "log_id_counter",
        runtime_privileges: "SELECT, UPDATE",
        indexes: &["log_id_counter_pkey"],
    },
    ProtectedTable {
        name: "memory_stage1_outputs",
        runtime_privileges: "SELECT, INSERT, UPDATE, DELETE",
        indexes: &[
            "memory_stage1_outputs_pkey",
            "memory_stage1_outputs_source_updated_idx",
        ],
    },
    ProtectedTable {
        name: "memory_jobs",
        runtime_privileges: "SELECT, INSERT, UPDATE, DELETE",
        indexes: &[
            "memory_jobs_pkey",
            "memory_jobs_kind_status_retry_lease_idx",
        ],
    },
    ProtectedTable {
        name: "memory_consolidation_progress",
        runtime_privileges: "SELECT, UPDATE",
        indexes: &["memory_consolidation_progress_pkey"],
    },
    ProtectedTable {
        name: "agent_board_deleted",
        runtime_privileges: "SELECT, INSERT",
        indexes: &["agent_board_deleted_pkey"],
    },
    ProtectedTable {
        name: "agent_board_channels",
        runtime_privileges: "SELECT, INSERT, DELETE",
        indexes: &["agent_board_channels_pkey"],
    },
    ProtectedTable {
        name: "agent_board_posts",
        runtime_privileges: "SELECT, INSERT, DELETE",
        indexes: &[
            "agent_board_posts_pkey",
            "agent_board_posts_id_key",
            "agent_board_posts_request_key",
            "agent_board_posts_channel_idx",
            "agent_board_posts_channel_ts_idx",
            "agent_board_posts_roots_idx",
            "agent_board_posts_root_idx",
            "agent_board_posts_root_ts_idx",
            "agent_board_posts_board_ts_idx",
        ],
    },
    ProtectedTable {
        name: "agent_board_subscriptions",
        runtime_privileges: "SELECT, INSERT, DELETE",
        indexes: &["agent_board_subscriptions_pkey"],
    },
    ProtectedTable {
        name: "agent_board_opt_outs",
        runtime_privileges: "SELECT, INSERT, DELETE",
        indexes: &["agent_board_opt_outs_pkey"],
    },
    ProtectedTable {
        name: "agent_board_post_counter",
        runtime_privileges: "SELECT, UPDATE",
        indexes: &["agent_board_post_counter_pkey"],
    },
    ProtectedTable {
        name: "thread_timestamp_marks",
        runtime_privileges: "SELECT, UPDATE",
        indexes: &["thread_timestamp_marks_pkey"],
    },
    ProtectedTable {
        name: "projects",
        runtime_privileges: "SELECT, INSERT, UPDATE, DELETE",
        indexes: &["projects_pkey", "idx_projects_position"],
    },
    ProtectedTable {
        name: "project_roots",
        runtime_privileges: "SELECT, INSERT, UPDATE, DELETE",
        indexes: &["project_roots_pkey"],
    },
    ProtectedTable {
        name: "project_idempotency_keys",
        runtime_privileges: "SELECT, INSERT",
        indexes: &["project_idempotency_keys_pkey"],
    },
];

/// What a migration must look like before its schema qualifier is rewritten for a named
/// namespace. A changed SQL layout needs a new review before identifier substitution.
pub(crate) struct MigrationShape {
    pub(crate) version: i64,
    /// Required prefix of the migration source, when the source has a fixed opening.
    pub(crate) starts_with: Option<&'static str>,
    /// Statements that must appear verbatim.
    pub(crate) contains: &'static [&'static str],
    /// Exact number of `codex_storage.` qualifiers in the source.
    pub(crate) qualified_identifiers: usize,
}

const META_UPDATE: &str = "\nUPDATE codex_storage.codex_schema_meta\n";

pub(crate) const MIGRATION_SHAPES: &[MigrationShape] = &[
    MigrationShape {
        version: 1,
        starts_with: Some("CREATE TABLE codex_storage.codex_schema_meta ("),
        contains: &["\nINSERT INTO codex_storage.codex_schema_meta\n"],
        qualified_identifiers: 2,
    },
    MigrationShape {
        version: 2,
        starts_with: Some("CREATE TABLE codex_storage.thread_spawn_edges ("),
        contains: &["\n    ON codex_storage.thread_spawn_edges (", META_UPDATE],
        qualified_identifiers: 3,
    },
    MigrationShape {
        version: 3,
        starts_with: Some("CREATE TABLE codex_storage.external_agent_config_imports ("),
        contains: &[
            "\n    ON codex_storage.external_agent_config_imports (",
            META_UPDATE,
        ],
        qualified_identifiers: 3,
    },
    MigrationShape {
        version: 4,
        starts_with: None,
        contains: &[
            "\nCREATE TABLE codex_storage.threads (\n",
            "\n    ON codex_storage.threads (recency_at_ms DESC, id DESC);\n",
            META_UPDATE,
        ],
        qualified_identifiers: 3,
    },
    MigrationShape {
        version: 5,
        starts_with: Some("CREATE TABLE codex_storage.thread_sections (\n"),
        contains: &[
            "\nINSERT INTO codex_storage.thread_sections (id, name)\n",
            "\nALTER TABLE codex_storage.threads\n",
            "\n    REFERENCES codex_storage.thread_sections (id) ON DELETE SET NULL;\n",
            "\n    ON codex_storage.threads (thread_section_id COLLATE",
            META_UPDATE,
        ],
        qualified_identifiers: 7,
    },
    MigrationShape {
        version: 6,
        starts_with: Some("-- Inactive ownership record."),
        contains: &[
            "\nCREATE TABLE codex_storage.thread_writer_ownership (\n",
            META_UPDATE,
        ],
        qualified_identifiers: 2,
    },
    MigrationShape {
        version: 7,
        starts_with: Some("-- Inactive goal persistence"),
        contains: &[
            "
CREATE TABLE codex_storage.thread_goals (
",
            "REFERENCES codex_storage.threads (id) ON DELETE CASCADE",
            "
CREATE TABLE codex_storage.thread_goal_continuation_deferrals (
",
            "REFERENCES codex_storage.thread_goals (thread_id) ON DELETE CASCADE",
            META_UPDATE,
        ],
        qualified_identifiers: 5,
    },
    MigrationShape {
        version: 8,
        starts_with: Some("-- Inactive queue persistence"),
        contains: &[
            "
CREATE TABLE codex_storage.queued_items (
",
            "REFERENCES codex_storage.threads (id) ON DELETE CASCADE",
            "
CREATE UNIQUE INDEX queued_items_thread_order_idx
",
            "
CREATE TABLE codex_storage.queue_change_counter (
",
            "
INSERT INTO codex_storage.queue_change_counter (singleton, version) VALUES (TRUE, 0);
",
            "
CREATE TABLE codex_storage.queued_thread_revisions (
",
            "
CREATE INDEX queued_thread_revisions_revision_idx
",
            META_UPDATE,
        ],
        qualified_identifiers: 9,
    },
    MigrationShape {
        version: 9,
        starts_with: Some("-- Inactive runtime log persistence"),
        contains: &[
            "
CREATE TABLE codex_storage.logs (
",
            "
CREATE INDEX logs_ts_idx
",
            "
CREATE INDEX logs_thread_id_ts_idx
",
            "
CREATE INDEX logs_threadless_process_ts_idx
",
            "
CREATE TABLE codex_storage.log_id_counter (
",
            "
INSERT INTO codex_storage.log_id_counter (singleton, last_id) VALUES (TRUE, 0);
",
            META_UPDATE,
        ],
        qualified_identifiers: 7,
    },
    MigrationShape {
        version: 10,
        starts_with: Some("-- Inactive generated-memory persistence"),
        contains: &[
            "
ALTER TABLE codex_storage.threads
",
            "
CREATE TABLE codex_storage.memory_stage1_outputs (
",
            "
CREATE INDEX memory_stage1_outputs_source_updated_idx
",
            "
CREATE TABLE codex_storage.memory_jobs (
",
            "
CREATE INDEX memory_jobs_kind_status_retry_lease_idx
",
            "
CREATE TABLE codex_storage.memory_consolidation_progress (
",
            "
INSERT INTO codex_storage.memory_consolidation_progress (singleton) VALUES (TRUE);
",
            META_UPDATE,
        ],
        qualified_identifiers: 8,
    },
    MigrationShape {
        version: 11,
        starts_with: Some("-- Inactive agent message board persistence"),
        contains: &[
            "
CREATE TABLE codex_storage.agent_board_deleted (
",
            "
CREATE TABLE codex_storage.agent_board_channels (
",
            "
CREATE TABLE codex_storage.agent_board_posts (
",
            "
CREATE INDEX agent_board_posts_channel_idx
",
            "
CREATE INDEX agent_board_posts_channel_ts_idx
",
            "
CREATE INDEX agent_board_posts_roots_idx
",
            "
CREATE INDEX agent_board_posts_root_idx
",
            "
CREATE INDEX agent_board_posts_root_ts_idx
",
            "
CREATE INDEX agent_board_posts_board_ts_idx
",
            "
CREATE TABLE codex_storage.agent_board_subscriptions (
",
            "
CREATE TABLE codex_storage.agent_board_opt_outs (
",
            "
CREATE TABLE codex_storage.agent_board_post_counter (
",
            "
INSERT INTO codex_storage.agent_board_post_counter (singleton, last_seq) VALUES (TRUE, 0);
",
            META_UPDATE,
        ],
        qualified_identifiers: 14,
    },
    MigrationShape {
        version: 12,
        starts_with: Some("-- Inactive thread timestamp allocation"),
        contains: &[
            "
CREATE TABLE codex_storage.thread_timestamp_marks (
",
            "
INSERT INTO codex_storage.thread_timestamp_marks (singleton) VALUES (TRUE);
",
            META_UPDATE,
        ],
        qualified_identifiers: 3,
    },
    MigrationShape {
        version: 13,
        starts_with: Some("-- Queue change records outlive"),
        contains: &[
            "
ALTER TABLE codex_storage.queued_thread_revisions
",
            "
    DROP CONSTRAINT queued_thread_revisions_thread_id_fkey;
",
            META_UPDATE,
        ],
        qualified_identifiers: 2,
    },
    MigrationShape {
        version: 14,
        starts_with: Some("-- Inactive project persistence"),
        contains: &[
            "
CREATE TABLE codex_storage.projects (
",
            "
CREATE TABLE codex_storage.project_roots (
",
            "
CREATE TABLE codex_storage.project_idempotency_keys (
",
            "
ALTER TABLE codex_storage.threads
",
            "
CREATE INDEX idx_projects_position
",
            "
CREATE INDEX idx_threads_project_id
",
            META_UPDATE,
        ],
        qualified_identifiers: 9,
    },
];

/// Relations always present once the metadata migration has run.
const BASE_RELATIONS: &[&str] = &[
    "_codex_pg_migrations",
    "_codex_pg_migrations_pkey",
    "codex_schema_meta",
    "codex_schema_meta_pkey",
];

/// Every relation the embedded migrations may create in a namespace, including indexes.
pub(crate) fn known_relations() -> Vec<&'static str> {
    let mut relations = BASE_RELATIONS.to_vec();
    for table in PROTECTED_TABLES {
        relations.push(table.name);
        relations.extend_from_slice(table.indexes);
    }
    relations
}

/// Whether a recorded schema format is one this build embeds. Format `n` is the state after
/// the first `n` migrations.
pub(crate) fn supported_format(format: i32) -> bool {
    usize::try_from(format)
        .is_ok_and(|applied| (1..=BASE_MIGRATOR.migrations.len()).contains(&applied))
}
