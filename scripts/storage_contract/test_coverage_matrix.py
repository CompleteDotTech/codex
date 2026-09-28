"""The issue #2 matrix must expose every pinned source fixture table."""

import unittest
from pathlib import Path
from unittest.mock import patch

from .coverage_matrix import (
    FILES,
    GOAL_EDGES,
    MEMORY_EDGES,
    MEMORY_FILE_EDGES,
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
        matrix = audit_coverage()
        root = Path(__file__).resolve().parents[2] / "codex-rs"
        for store in ("memories_1.sqlite", "memories_v2_1.sqlite"):
            for table in MEMORY_EDGES:
                modules = matrix["stores"][store][table]["observed_memory_edges"]
                for module, operations in modules.items():
                    source = (root / module).read_text(encoding="utf-8")
                    for operation, clause in operations.items():
                        with self.subTest(
                            store=store, table=table, module=module, operation=operation
                        ):
                            self.assertIn(clause, source)
        modules = matrix["files"]["memory_artifact"]["observed_memory_file_edges"]
        for module, operations in modules.items():
            source = (root / module).read_text(encoding="utf-8")
            for operation, clause in operations.items():
                with self.subTest(
                    file="memory_artifact", module=module, operation=operation
                ):
                    self.assertIn(clause, source)
