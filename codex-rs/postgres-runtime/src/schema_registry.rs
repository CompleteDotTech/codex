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
