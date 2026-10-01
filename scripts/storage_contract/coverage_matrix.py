"""Pinned, incomplete source coverage matrix for issue #2 review.

This is an inventory check, not a migration policy or activation gate. A missing
entry fails the audit; an unresolved entry must remain visible to reviewers.
"""

import json

from .source_catalog import STORES, build_fixture_policy, verified_migrations

# Each source table is mapped to its implementation owner and a direct source
# module. Transitive consumers and migration semantics remain unresolved unless
# a later reviewed contract explicitly settles them.
TABLES = {
    "state_5.sqlite": {
        "threads": (5, "state/src/runtime/threads.rs"),
        "thread_dynamic_tools": (5, "state/src/runtime/threads.rs"),
        "thread_spawn_edges": (5, "state/src/runtime/threads.rs"),
        "thread_sections": (5, "state/src/runtime/thread_sections.rs"),
        "thread_attachments": (5, "state/src/runtime/thread_attachments.rs"),
        "projects": (5, "state/src/runtime/projects.rs"),
        "project_roots": (5, "state/src/runtime/projects.rs"),
        "project_idempotency_keys": (5, "state/src/runtime/projects.rs"),
        "backfill_state": (11, "state/src/runtime/backfill.rs"),
        "rollout_migration_state": (11, "state/src/runtime/rollout_migration.rs"),
        "rollout_migration_skipped_rollouts": (
            11,
            "state/src/runtime/rollout_migration.rs",
        ),
        "remote_control_enrollments": (12, "state/src/runtime/remote_control.rs"),
        "external_agent_config_imports": (
            12,
            "state/src/runtime/external_agent_config_imports.rs",
        ),
        "sqlite_sequence": (13, "state/src/migrations.rs"),
    },
    "goals_1.sqlite": {
        "thread_goals": (6, "state/src/runtime/goals.rs"),
        "thread_goal_continuation_deferrals": (6, "state/src/runtime/goals.rs"),
    },
    "logs_2.sqlite": {
        "logs": (8, "state/src/runtime/logs.rs"),
        "sqlite_sequence": (8, "state/logs_migrations"),
    },
    "memories_1.sqlite": {
        "stage1_outputs": (6, "state/src/runtime/memories.rs"),
        "jobs": (6, "state/src/runtime/memories.rs"),
        "consolidation_progress": (6, "state/src/runtime/memories.rs"),
    },
    "memories_v2_1.sqlite": {
        "stage1_outputs": (6, "state/src/runtime/memories.rs"),
        "jobs": (6, "state/src/runtime/memories.rs"),
        "consolidation_progress": (6, "state/src/runtime/memories.rs"),
    },
    "thread_history_1.sqlite": {
        "thread_turns": (
            11,
            "thread-store/src/local/thread_history_materialization.rs",
        ),
        "thread_items": (
            11,
            "thread-store/src/local/thread_history_materialization.rs",
        ),
        "thread_history_projection_state": (
            11,
            "thread-store/src/local/thread_history_materialization.rs",
        ),
        "thread_realtime_items": (
            11,
            "thread-store/src/local/thread_history/realtime.rs",
        ),
    },
    "queue_1.sqlite": {
        "queued_items": (7, "state/src/runtime/queued_items.rs"),
        "queued_thread_revisions": (7, "state/src/runtime/queued_items.rs"),
        "sqlite_sequence": (7, "state/queue_migrations"),
    },
    "agent_message_board_1.sqlite": {
        "channels": (9, "ext/agent-message-board/src/local.rs"),
        "posts": (9, "ext/agent-message-board/src/local.rs"),
        "subscriptions": (9, "ext/agent-message-board/src/local.rs"),
        "subscription_opt_outs": (9, "ext/agent-message-board/src/local.rs"),
        "deleted_boards": (9, "ext/agent-message-board/src/local.rs"),
        "sqlite_sequence": (9, "ext/agent-message-board/src/local.rs"),
    },
}

# File classes have no schema fixture in source_catalog. Their unresolved
# representation and closure must not disappear from a table-only inventory.
FILES = {
    "active_rollout_jsonl": (11, "rollout/src/recorder.rs"),
    "archived_rollout_jsonl": (11, "thread-store/src/local/archive_thread.rs"),
    "compressed_rollout_zst": (11, "rollout/src/compression.rs"),
    "reference_backed_fork": (11, "thread-store/src/local/paginated_fork.rs"),
    "legacy_copied_fork": (11, "core/src/thread_manager.rs"),
    "session_index_jsonl": (11, "rollout/src/session_index.rs"),
    "attachment_payload": (11, "attachment-store/src/lib.rs"),
    "memory_artifact": (6, "memories/write/src/storage.rs"),
}

# Observed production SQL clauses in one primary-state family. These are
# examples of direct operations, not an exhaustive list of call sites.
# In particular, a thread's project binding is written by projects.rs as well
# as the thread implementation, so a table's owning module is insufficient.
PRIMARY_PROJECT_EDGES = {
    "threads": {
        "state/src/runtime/threads.rs": {
            "read": "FROM threads",
            "write": "UPDATE threads",
        },
        "state/src/runtime/projects.rs": {
            "read": "SELECT project_id FROM threads",
            "write": "UPDATE threads SET project_id",
        },
    },
    "projects": {
        "state/src/runtime/projects.rs": {
            "read": "FROM projects",
            "write": "INSERT INTO projects",
        },
    },
    "project_roots": {
        "state/src/runtime/projects.rs": {
            "read": "FROM project_roots",
            "write": "INSERT INTO project_roots",
        },
    },
    "project_idempotency_keys": {
        "state/src/runtime/projects.rs": {
            "read": "FROM project_idempotency_keys",
            "write": "INSERT INTO project_idempotency_keys",
        },
    },
    "thread_sections": {
        "state/src/runtime/threads.rs": {
            "read": "FROM thread_sections",
        },
        "state/src/runtime/thread_sections.rs": {
            "insert": "INSERT INTO thread_sections",
            "write": "UPDATE thread_sections",
        },
        "state/src/runtime/thread_section_order.rs": {
            "read": "FROM thread_sections",
        },
        "state/src/runtime/memories.rs": {
            "read": "FROM thread_sections",
        },
    },
    "thread_attachments": {
        "state/src/runtime/thread_attachments.rs": {
            "read": "FROM thread_attachments",
            "write": "INSERT INTO thread_attachments",
        },
    },
    "thread_dynamic_tools": {
        "state/src/runtime/threads.rs": {
            "write": "DELETE FROM thread_dynamic_tools",
        },
    },
    "thread_spawn_edges": {
        "state/src/runtime/threads.rs": {
            "read": "FROM thread_spawn_edges",
            "write": "INSERT INTO thread_spawn_edges",
        },
    },
}

# Selected direct goal-store and public boundary edges. Goal updates can also
# append canonical rollout items, so SQLite rows alone are not a full capture.
GOAL_EDGES = {
    "thread_goals": {
        "state/goals_migrations/0001_thread_goals.sql": {
            "schema": "CREATE TABLE thread_goals (",
        },
        "state/src/runtime/goals.rs": {
            "get_thread_goal": "FROM thread_goals",
            "replace_thread_goal_snapshot": "INSERT INTO thread_goals (",
            "replace_thread_goal": "INSERT INTO thread_goals (",
            "insert_thread_goal": "INSERT INTO thread_goals (",
            "update_thread_goal": "UPDATE thread_goals",
            "update_active_thread_goal_status": "UPDATE thread_goals",
            "account_thread_goal_usage": "UPDATE thread_goals",
            "delete_thread_goal": "DELETE FROM thread_goals",
        },
        "ext/goal/src/api.rs": {
            "set_thread_goal": ".update_thread_goal(",
            "clear_thread_goal": ".delete_thread_goal(thread_id)",
        },
        "ext/goal/src/runtime.rs": {
            "account_active_goal_progress": ".account_thread_goal_usage(",
            "account_idle_goal_progress": ".account_thread_goal_usage(",
        },
        "app-server/src/request_processors/thread_goal_processor.rs": {
            "api_set": ".set_thread_goal(",
            "canonical_event": "outcome.thread_goal_updated_item()",
        },
        "app-server/src/request_processors/thread_fork_goal.rs": {
            "inherit_thread_goal_snapshot": ".replace_thread_goal_snapshot(&goal)",
        },
    },
    "thread_goal_continuation_deferrals": {
        "state/goals_migrations/0002_thread_goal_continuation_deferrals.sql": {
            "schema": "CREATE TABLE thread_goal_continuation_deferrals (",
            "cascade": "REFERENCES thread_goals(thread_id) ON DELETE CASCADE",
        },
        "state/src/runtime/goals.rs": {
            "replace_thread_goal_snapshot": "INSERT INTO thread_goal_continuation_deferrals (thread_id)",
            "has_thread_goal_continuation_deferral": "FROM thread_goal_continuation_deferrals",
            "clear_thread_goal_continuation_deferral": "DELETE FROM thread_goal_continuation_deferrals WHERE thread_id = ?",
        },
        "ext/goal/src/runtime.rs": {
            "continue_if_idle": ".has_thread_goal_continuation_deferral(self.thread_id())",
        },
        "ext/goal/src/extension.rs": {
            "on_turn_start": ".clear_thread_goal_continuation_deferral(runtime.thread_id())",
        },
    },
}

# Direct queue data and notification edges. The revision table is written by
# SQLite triggers, not by queued_items.rs. These source clauses cover the local
# adapter and service entry points, but not every app-server RPC caller.
QUEUE_EDGES = {
    "queued_items": {
        "state/queue_migrations/0001_queued_items.sql": {
            "schema": "CREATE TABLE queued_items (",
            "constraint": "CREATE UNIQUE INDEX queued_items_thread_order_idx",
        },
        "state/src/runtime/queued_items.rs": {
            "enqueue": "INSERT INTO queued_items (",
            "list_page": "FROM queued_items",
            "update": "UPDATE queued_items",
            "delete": "DELETE FROM queued_items",
            "reorder": "UPDATE queued_items SET queue_order = ?, updated_at_ms = ?",
            "delete_thread_queue": "DELETE FROM queued_items WHERE thread_id = ?",
        },
        "thread-store/src/queue_store.rs": {
            "adapter": "self.queue().list_page(thread_id, offset, limit)",
        },
        "ext/queue/src/service.rs": {
            "consumer": ".list_page(thread_id, offset, limit)",
        },
        "app-server/src/message_processor.rs": {
            "factory": "LocalQueueStore::new(Arc::clone(state_db))",
        },
    },
    "queued_thread_revisions": {
        "state/queue_migrations/0002_queued_thread_revisions.sql": {
            "schema": "CREATE TABLE queued_thread_revisions (",
            "insert_trigger": """CREATE TRIGGER queued_items_revision_after_insert
AFTER INSERT ON queued_items
BEGIN
    INSERT INTO queued_thread_revisions (thread_id)
    VALUES (NEW.thread_id)
    ON CONFLICT(thread_id) DO UPDATE
    SET revision = (SELECT COALESCE(MAX(revision), 0) + 1 FROM queued_thread_revisions);
END;""",
            "update_trigger": """CREATE TRIGGER queued_items_revision_after_update
AFTER UPDATE ON queued_items
BEGIN
    INSERT INTO queued_thread_revisions (thread_id)
    VALUES (NEW.thread_id)
    ON CONFLICT(thread_id) DO UPDATE
    SET revision = (SELECT COALESCE(MAX(revision), 0) + 1 FROM queued_thread_revisions);
END;""",
            "delete_trigger": """CREATE TRIGGER queued_items_revision_after_delete
AFTER DELETE ON queued_items
BEGIN
    INSERT INTO queued_thread_revisions (thread_id)
    VALUES (OLD.thread_id)
    ON CONFLICT(thread_id) DO UPDATE
    SET revision = (SELECT COALESCE(MAX(revision), 0) + 1 FROM queued_thread_revisions);
END;""",
        },
        "state/src/runtime/queued_items.rs": {
            "revision_read": "SELECT thread_id, revision FROM queued_thread_revisions WHERE revision > ",
            "commit_observation": "PRAGMA data_version",
        },
        "thread-store/src/queue_store.rs": {
            "adapter": "self.queue().changes_since(revision, thread_ids)",
            "commit_observation": "self.queue().change_version()",
        },
        "ext/queue/src/service.rs": {
            "watcher": ".changes_since(last_revision, &thread_ids)",
            "commit_observation": "service.queue.change_version().await",
        },
        "app-server/src/message_processor.rs": {
            "factory": "LocalQueueStore::new(Arc::clone(state_db))",
        },
    },
}


def audit_coverage() -> dict:
    """Match every pinned fixture table and expose unfinished decisions."""
    if set(TABLES) != set(STORES):
        raise ValueError("store coverage differs from pinned source catalog")
    stores = {}
    for store, entries in TABLES.items():
        policy = json.loads(
            build_fixture_policy(store, version=len(verified_migrations(store)))
        )
        if set(entries) != set(policy["tables"]):
            raise ValueError(f"table coverage differs from pinned fixture: {store}")
        stores[store] = {
            table: {
                "issue": issue,
                "source": source,
                "fixture_treatment": policy["tables"][table],
                "producer_consumer_audit": "partial",
                "forward_reverse_decision": "unresolved",
                "observed_direct_sql": PRIMARY_PROJECT_EDGES.get(table, {})
                if store == "state_5.sqlite"
                else {},
                "observed_queue_edges": QUEUE_EDGES.get(table, {})
                if store == "queue_1.sqlite"
                else {},
                "observed_goal_edges": GOAL_EDGES.get(table, {})
                if store == "goals_1.sqlite"
                else {},
            }
            for table, (issue, source) in entries.items()
        }
    return {
        "status": "partial",
        "activation_permitted": False,
        "stores": stores,
        "files": {
            name: {
                "issue": issue,
                "source": source,
                "producer_consumer_audit": "partial",
                "forward_reverse_decision": "unresolved",
            }
            for name, (issue, source) in FILES.items()
        },
    }
