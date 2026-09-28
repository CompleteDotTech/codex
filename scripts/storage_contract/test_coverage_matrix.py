"""The issue #2 matrix must expose every pinned source fixture table."""

import unittest
from pathlib import Path
from unittest.mock import patch

from .coverage_matrix import FILES, TABLES, audit_coverage


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
