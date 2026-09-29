"""The issue #2 matrix must expose every pinned source fixture table."""

import re
import unittest
from pathlib import Path
from unittest.mock import patch

from .coverage_matrix import (
    FILES,
    GOAL_EDGES,
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
