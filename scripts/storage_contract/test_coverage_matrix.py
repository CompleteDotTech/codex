"""The issue #2 matrix must expose every pinned source fixture table."""

import re
import unittest
from pathlib import Path
from unittest.mock import patch

from .coverage_matrix import (
    FILES,
    GOAL_EDGES,
    MEMORY_EDGES,
    MEMORY_FILE_EDGES,
    MEMORY_VERSION_EDGES,
    PRIMARY_PROJECT_EDGES,
    QUEUE_EDGES,
    TABLES,
    audit_coverage,
)


class CoverageMatrixTests(unittest.TestCase):
    def method_source(self, source, method):
        # Lexical boundaries for these pinned modules, not a Rust parser.
        declarations = list(
            re.finditer(
                r"(?m)^(?:    )?(?:pub(?:\([^)]*\))? )?(?:async )?fn (\w+)\b",
                source,
            )
        )
        selected = [
            i for i, match in enumerate(declarations) if match.group(1) == method
        ]
        self.assertEqual(len(selected), 1)
        index = selected[0]
        end = (
            declarations[index + 1].start()
            if index + 1 < len(declarations)
            else len(source)
        )
        return source[declarations[index].start() : end]

    def test_unmapped_pinned_table_is_rejected(self):
        with patch.dict(TABLES["state_5.sqlite"]):
            TABLES["state_5.sqlite"].pop("threads")
            with self.assertRaisesRegex(ValueError, "table coverage differs"):
                audit_coverage()

    def test_source_anchors_exist_in_checkout(self):
        audit_coverage()
        root = Path(__file__).resolve().parents[2] / "codex-rs"
        sources = {
            source for entries in TABLES.values() for _, source in entries.values()
        } | {source for _, source in FILES.values()}
        for source in sources:
            with self.subTest(source=source):
                self.assertTrue((root / source).exists())

    def test_project_family_edges_match_production_sql_clauses(self):
        matrix = audit_coverage()["stores"]["state_5.sqlite"]
        root = Path(__file__).resolve().parents[2] / "codex-rs"
        for table, modules in PRIMARY_PROJECT_EDGES.items():
            self.assertEqual(matrix[table]["observed_direct_sql"], modules)
            for module, operations in modules.items():
                # Only check the production prefix: inline tests and test-only
                # helpers may contain SQL that no runtime path executes.
                source = (root / module).read_text(encoding="utf-8")
                production = source.partition("#[cfg(test)]")[0]
                for operation, clause in operations.items():
                    with self.subTest(table=table, module=module, operation=operation):
                        self.assertIn(clause, production)

    def test_queue_edges_match_migrations_and_production_callers(self):
        matrix = audit_coverage()["stores"]["queue_1.sqlite"]
        root = Path(__file__).resolve().parents[2] / "codex-rs"
        expected_shape = {
            "queued_items": {
                "state/queue_migrations/0001_queued_items.sql": {
                    "schema",
                    "constraint",
                },
                "state/src/runtime/queued_items.rs": {
                    "enqueue",
                    "list_page",
                    "update",
                    "delete",
                    "reorder",
                    "delete_thread_queue",
                },
                "thread-store/src/queue_store.rs": {"adapter"},
                "ext/queue/src/service.rs": {"consumer"},
                "app-server/src/message_processor.rs": {"factory"},
            },
            "queued_thread_revisions": {
                "state/queue_migrations/0002_queued_thread_revisions.sql": {
                    "schema",
                    "insert_trigger",
                    "update_trigger",
                    "delete_trigger",
                },
                "state/src/runtime/queued_items.rs": {
                    "revision_read",
                    "commit_observation",
                },
                "thread-store/src/queue_store.rs": {"adapter", "commit_observation"},
                "ext/queue/src/service.rs": {"watcher", "commit_observation"},
                "app-server/src/message_processor.rs": {"factory"},
            },
        }
        self.assertEqual(
            {
                table: {
                    module: set(operations) for module, operations in modules.items()
                }
                for table, modules in QUEUE_EDGES.items()
            },
            expected_shape,
        )
        observed = {
            table: row["observed_queue_edges"]
            for table, row in matrix.items()
            if row["observed_queue_edges"]
        }
        self.assertEqual(observed, QUEUE_EDGES)
        for table in expected_shape:
            modules = matrix[table]["observed_queue_edges"]
            for module, operations in modules.items():
                source = (
                    (root / module)
                    .read_text(encoding="utf-8")
                    .split("#[cfg(test)]", 1)[0]
                    .replace("\r\n", "\n")
                )
                for operation, clause in operations.items():
                    with self.subTest(table=table, module=module, operation=operation):
                        bounded_source = source
                        if module == "state/src/runtime/queued_items.rs":
                            method = {
                                "revision_read": "changes_since",
                                "commit_observation": "change_version",
                            }.get(operation, operation)
                            bounded_source = self.method_source(source, method)
                        self.assertIn(clause, bounded_source)

    def test_goal_edges_match_migrations_and_production_callers(self):
        matrix = audit_coverage()["stores"]["goals_1.sqlite"]
        root = Path(__file__).resolve().parents[2] / "codex-rs"
        expected_shape = {
            "thread_goals": {
                "state/goals_migrations/0001_thread_goals.sql": {"schema"},
                "state/src/runtime/goals.rs": {
                    "get_thread_goal",
                    "replace_thread_goal_snapshot",
                    "replace_thread_goal",
                    "insert_thread_goal",
                    "update_thread_goal",
                    "update_active_thread_goal_status",
                    "account_thread_goal_usage",
                    "delete_thread_goal",
                },
                "ext/goal/src/api.rs": {"set_thread_goal", "clear_thread_goal"},
                "ext/goal/src/runtime.rs": {
                    "account_active_goal_progress",
                    "account_idle_goal_progress",
                },
                "app-server/src/request_processors/thread_goal_processor.rs": {
                    "api_set",
                    "canonical_event",
                },
                "app-server/src/request_processors/thread_fork_goal.rs": {
                    "inherit_thread_goal_snapshot"
                },
            },
            "thread_goal_continuation_deferrals": {
                "state/goals_migrations/0002_thread_goal_continuation_deferrals.sql": {
                    "schema",
                    "cascade",
                },
                "state/src/runtime/goals.rs": {
                    "replace_thread_goal_snapshot",
                    "has_thread_goal_continuation_deferral",
                    "clear_thread_goal_continuation_deferral",
                },
                "ext/goal/src/runtime.rs": {"continue_if_idle"},
                "ext/goal/src/extension.rs": {"on_turn_start"},
            },
        }
        self.assertEqual(
            {
                table: {
                    module: set(operations) for module, operations in modules.items()
                }
                for table, modules in GOAL_EDGES.items()
            },
            expected_shape,
        )
        observed = {
            table: row["observed_goal_edges"]
            for table, row in matrix.items()
            if row["observed_goal_edges"]
        }
        self.assertEqual(observed, GOAL_EDGES)
        for table in expected_shape:
            modules = matrix[table]["observed_goal_edges"]
            for module, operations in modules.items():
                source = (
                    (root / module)
                    .read_text(encoding="utf-8")
                    .split("#[cfg(test)]", 1)[0]
                    .replace("\r\n", "\n")
                )
                for operation, clause in operations.items():
                    with self.subTest(table=table, module=module, operation=operation):
                        bounded_source = source
                        if module.endswith(".rs"):
                            method = (
                                "thread_goal_set_inner"
                                if operation in {"api_set", "canonical_event"}
                                else operation
                            )
                            bounded_source = self.method_source(source, method)
                        self.assertIn(clause, bounded_source)

    def test_versioned_memory_and_generated_file_edges_match_source(self):
        root = Path(__file__).resolve().parents[2] / "codex-rs"
        self._assert_memory_edges(
            lambda module: (root / module).read_text(encoding="utf-8")
        )

    def test_memory_edges_reject_rerouting_and_usage_drift(self):
        root = Path(__file__).resolve().parents[2] / "codex-rs"
        mutations = (
            (
                "memory_versions.rs",
                "self.memories.clone()",
                "self.other_memories.clone()",
                "version_selection",
            ),
            (
                "memory_versions.rs",
                ".memories_v2\n",
                ".other_memories\n",
                "version_selection",
            ),
            (
                "memory_versions.rs",
                ".open_memories_v2_db()",
                ".open_state_db()",
                "version_selection",
            ),
            (
                "memories.rs",
                "usage_count = COALESCE(usage_count, 0) + 1,",
                "usage_count = 0,",
                "usage",
            ),
            ("memories.rs", "last_usage = ?\n", "last_usage = NULL\n", "usage"),
            (
                "memories.rs",
                "lease_until = excluded.lease_until,",
                "lease_until = NULL,",
                "lease_claim",
            ),
            (
                "memories.rs",
                "jobs.lease_until <= excluded.started_at",
                "jobs.lease_until > excluded.started_at",
                "lease_claim_guard",
            ),
            (
                "memories.rs",
                "    lease_until = ?,\n    retry_at = NULL,",
                "    lease_until = NULL,\n    retry_at = NULL,",
                "lease_start",
            ),
            (
                "memories.rs",
                "UPDATE jobs\nSET lease_until = ?\nWHERE kind = ? AND job_key = ?",
                "UPDATE jobs\nSET lease_until = NULL\nWHERE kind = ? AND job_key = ?",
                "lease_heartbeat",
            ),
            (
                "memories.rs",
                "AND (status != 'running' OR lease_until IS NULL OR lease_until <= ?)",
                "AND (status != 'running' OR lease_until IS NULL)",
                "lease_start_guard",
            ),
            (
                "memories.rs",
                "UPDATE jobs\nSET lease_until = ?\nWHERE kind = ? AND job_key = ?\n"
                "  AND status = 'running' AND ownership_token = ?",
                "UPDATE jobs\nSET lease_until = ?\nWHERE kind = ? AND job_key = ?\n"
                "  AND status = 'running'",
                "lease_heartbeat",
            ),
            (
                "storage.rs",
                "body.push_str(memory.raw_memory.trim());",
                'body.push_str("No selected memory");',
                "raw_summary_write",
            ),
        )
        for filename, old, new, operation in mutations:
            module = (
                "memories/write/src/storage.rs"
                if filename == "storage.rs"
                else f"state/src/runtime/{filename}"
            )
            source = (root / module).read_text(encoding="utf-8")
            changed = source.replace(old, new, 1)
            self.assertNotEqual(source, changed)
            # Keeping the original in a test-only suffix must not satisfy an edge.
            changed += f"\n#[cfg(test)]\nmod source_copy {{\n{source}\n}}\n"
            with self.subTest(module=module, change=old):
                with self.assertRaisesRegex(AssertionError, f"operation={operation}"):
                    self._assert_memory_edges(
                        lambda path: (
                            changed
                            if path == module
                            else (root / path).read_text(encoding="utf-8")
                        )
                    )

    def _assert_memory_edges(self, read_source):
        matrix = audit_coverage()
        expected_shape = {
            "stage1_outputs": {
                "state/memory_migrations/0001_memories.sql": {"schema"},
                "state/src/runtime/memories.rs": {"read", "write", "usage", "delete"},
                "memories/write/src/runtime.rs": {"writer_selection"},
                "memories/write/src/phase2.rs": {"file_materialization"},
            },
            "jobs": {
                "state/memory_migrations/0001_memories.sql": {"schema"},
                "state/src/runtime/memories.rs": {
                    "claim",
                    "lease_claim",
                    "lease_claim_guard",
                    "lease_start",
                    "lease_heartbeat",
                    "lease_start_guard",
                    "read",
                    "delete",
                },
            },
            "consolidation_progress": {
                "state/memory_migrations/0002_consolidation_progress.sql": {"schema"},
                "state/src/runtime/memories.rs": {"write"},
                "state/src/runtime/memory_readiness.rs": {"read"},
            },
        }
        self.assertEqual(
            {
                table: {
                    module: set(operations) for module, operations in modules.items()
                }
                for table, modules in MEMORY_EDGES.items()
            },
            expected_shape,
        )
        self.assertEqual(
            {
                store: set(operations)
                for store, operations in MEMORY_VERSION_EDGES.items()
            },
            {
                "memories_1.sqlite": {"version_selection"},
                "memories_v2_1.sqlite": {"version_selection"},
            },
        )
        self.assertEqual(
            {
                module: set(operations)
                for module, operations in MEMORY_FILE_EDGES.items()
            },
            {
                "memories/write/src/storage.rs": {
                    "raw_summary_write",
                    "rollout_summary_write",
                },
                "memories/write/src/phase2.rs": {"summary_sync", "v1_raw_file"},
            },
        )
        memory_methods = {
            "stage1_outputs": {
                "read": "list_stage1_outputs_for_global",
                "write": "mark_stage1_job_succeeded",
                "usage": "record_stage1_output_usage",
                "delete": "delete_thread_memory",
            },
            "jobs": {
                "claim": "try_claim_stage1_job",
                "lease_claim": "try_claim_stage1_job",
                "lease_claim_guard": "try_claim_stage1_job",
                "lease_start": "try_claim_global_phase2_job",
                "lease_heartbeat": "heartbeat_global_phase2_job",
                "lease_start_guard": "try_claim_global_phase2_job",
                "read": "try_claim_global_phase2_job",
                "delete": "delete_thread_memory",
            },
            "consolidation_progress": {"write": "mark_global_phase2_job_succeeded"},
        }
        other_methods = {
            "state/src/runtime/memory_readiness.rs": {
                "read": "max_consolidated_thread_count"
            },
            "state/src/runtime/memory_versions.rs": {
                "version_selection": "memories_for_version"
            },
            "memories/write/src/runtime.rs": {"writer_selection": "memory_store"},
            "memories/write/src/phase2.rs": {
                "file_materialization": "sync_phase2_workspace_inputs"
            },
        }
        for store, tables in matrix["stores"].items():
            if store not in {"memories_1.sqlite", "memories_v2_1.sqlite"}:
                for table in tables.values():
                    self.assertEqual(table["observed_memory_edges"], {})
        for store in ("memories_1.sqlite", "memories_v2_1.sqlite"):
            for table in expected_shape:
                modules = matrix["stores"][store][table]["observed_memory_edges"]
                self.assertEqual(
                    modules,
                    {
                        **MEMORY_EDGES[table],
                        "state/src/runtime/memory_versions.rs": MEMORY_VERSION_EDGES[
                            store
                        ],
                    },
                )
                for module, operations in modules.items():
                    # A test-only constructor can precede production methods.
                    # The pinned modules' test-only suffixes start at column zero.
                    source = ("\n" + read_source(module)).split("\n#[cfg(test)]", 1)[0]
                    source = source.replace("\r\n", "\n")
                    for operation, clause in operations.items():
                        bounded_source = source
                        if module.endswith(".rs"):
                            method = (
                                memory_methods[table][operation]
                                if module == "state/src/runtime/memories.rs"
                                else other_methods[module][operation]
                            )
                            bounded_source = self.method_source(source, method)
                        self.assertIn(
                            clause,
                            bounded_source,
                            f"store={store}, table={table}, module={module}, operation={operation}",
                        )
        modules = matrix["files"]["memory_artifact"]["observed_memory_file_edges"]
        self.assertEqual(modules, MEMORY_FILE_EDGES)
        file_methods = {
            "memories/write/src/storage.rs": {
                "raw_summary_write": "rebuild_raw_memories_file",
                "rollout_summary_write": "write_rollout_summary_for_thread",
            },
            "memories/write/src/phase2.rs": {
                "summary_sync": "sync_phase2_workspace_inputs",
                "v1_raw_file": "sync_phase2_workspace_inputs",
            },
        }
        for module, operations in modules.items():
            source = ("\n" + read_source(module)).split("\n#[cfg(test)]", 1)[0]
            source = source.replace("\r\n", "\n")
            for operation, clause in operations.items():
                self.assertIn(
                    clause,
                    self.method_source(source, file_methods[module][operation]),
                    f"module={module}, operation={operation}",
                )
