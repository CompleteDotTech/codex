"""The issue #2 matrix must expose every pinned source fixture table."""

import unittest
from pathlib import Path
from unittest.mock import patch

from .coverage_matrix import (
    FILES,
    GOAL_EDGES,
    MEMORY_EDGES,
    PRIMARY_PROJECT_EDGES,
    QUEUE_EDGES,
    TABLES,
    audit_coverage,
)


class CoverageMatrixTests(unittest.TestCase):
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
            self.assertIn(table, matrix)
            for module, operations in modules.items():
                source = (root / module).read_text(encoding="utf-8")
                for operation, clause in operations.items():
                    with self.subTest(table=table, module=module, operation=operation):
                        self.assertIn(clause, source)

    def test_queue_edges_match_migrations_and_production_callers(self):
        matrix = audit_coverage()["stores"]["queue_1.sqlite"]
        root = Path(__file__).resolve().parents[2] / "codex-rs"
        for table in QUEUE_EDGES:
            modules = matrix[table]["observed_queue_edges"]
            for module, operations in modules.items():
                source = (
                    (root / module).read_text(encoding="utf-8").replace("\r\n", "\n")
                )
                for operation, clause in operations.items():
                    with self.subTest(table=table, module=module, operation=operation):
                        self.assertIn(clause, source)

    def test_goal_edges_match_migrations_and_production_callers(self):
        matrix = audit_coverage()["stores"]["goals_1.sqlite"]
        root = Path(__file__).resolve().parents[2] / "codex-rs"
        for table in GOAL_EDGES:
            modules = matrix[table]["observed_goal_edges"]
            for module, operations in modules.items():
                source = (
                    (root / module).read_text(encoding="utf-8").replace("\r\n", "\n")
                )
                for operation, clause in operations.items():
                    with self.subTest(table=table, module=module, operation=operation):
                        self.assertIn(clause, source)

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
        for store, tables in matrix["stores"].items():
            if store not in {"memories_1.sqlite", "memories_v2_1.sqlite"}:
                for table in tables.values():
                    self.assertEqual(table["observed_memory_edges"], {})
        for store in ("memories_1.sqlite", "memories_v2_1.sqlite"):
            for table in MEMORY_EDGES:
                modules = matrix["stores"][store][table]["observed_memory_edges"]
                for module, operations in modules.items():
                    # A test-only constructor can precede production methods.
                    # The pinned modules' test-only suffixes start at column zero.
                    source = ("\n" + read_source(module)).split("\n#[cfg(test)]", 1)[0]
                    source = source.replace("\r\n", "\n")
                    for operation, clause in operations.items():
                        self.assertIn(
                            clause,
                            source,
                            f"store={store}, table={table}, module={module}, operation={operation}",
                        )
        modules = matrix["files"]["memory_artifact"]["observed_memory_file_edges"]
        for module, operations in modules.items():
            source = ("\n" + read_source(module)).split("\n#[cfg(test)]", 1)[0]
            source = source.replace("\r\n", "\n")
            for operation, clause in operations.items():
                self.assertIn(clause, source, f"module={module}, operation={operation}")
