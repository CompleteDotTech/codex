"""The issue #2 matrix must expose every pinned source fixture table."""

import unittest
from pathlib import Path
from unittest.mock import patch

from .coverage_matrix import FILES, PRIMARY_PROJECT_EDGES, TABLES, audit_coverage


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
            self.assertEqual(matrix[table]["observed_direct_sql"], modules)
            for module, operations in modules.items():
                # Only check the production prefix: inline tests and test-only
                # helpers may contain SQL that no runtime path executes.
                source = (root / module).read_text(encoding="utf-8")
                production = source.partition("#[cfg(test)]")[0]
                for operation, clause in operations.items():
                    with self.subTest(table=table, module=module, operation=operation):
                        self.assertIn(clause, production)
