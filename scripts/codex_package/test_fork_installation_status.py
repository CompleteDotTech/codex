"""Exercise the public read-only status command against real staged fixtures."""

import json
import subprocess
import sys
import unittest
from unittest.mock import patch
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from codex_package import test_fork_side_by_side
from codex_package.fork_side_by_side import stage_fork_package
from codex_package.fork_side_by_side import read_staged_fork_receipt
from codex_package.fork_identity import verify_fork_package


@unittest.skipUnless(sys.platform == "linux", "Linux descriptor-pinned receipt")
class InstallationStatusTest(unittest.TestCase):
    def setUp(self) -> None:
        test_fork_side_by_side.SideBySideStageTest.setUp(self)

    def status(self, slot: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                sys.executable,
                str(
                    Path(__file__).resolve().parents[1] / "verify_fork_installation.py"
                ),
                str(self.install),
                slot,
            ],
            text=True,
            capture_output=True,
            timeout=10,
            check=False,
        )

    def test_public_status_reports_exact_inactive_receipt_without_changes(self) -> None:
        slot = stage_fork_package(self.package, self.install, self.digest)
        receipt = self.install / "fork-receipts" / (slot.name + ".json")
        before = {
            str(p.relative_to(self.install)): p.read_bytes()
            for p in self.install.rglob("*")
            if p.is_file()
        }
        result = self.status(slot.name)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            json.loads(result.stdout),
            {"status": "verifiedInactive", "receipt": json.loads(receipt.read_text())},
        )
        after = {
            str(p.relative_to(self.install)): p.read_bytes()
            for p in self.install.rglob("*")
            if p.is_file()
        }
        self.assertEqual(after, before)

    def test_missing_receipt_is_reported_without_creating_directories(self) -> None:
        result = self.status(self.digest)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(
            json.loads(result.stdout),
            {"status": "unverified", "action": "manualReconciliation"},
        )
        self.assertEqual(list(self.install.iterdir()), [])

    def test_changed_receipt_and_package_are_preserved_and_fail_closed(self) -> None:
        slot = stage_fork_package(self.package, self.install, self.digest)
        receipt = self.install / "fork-receipts" / (slot.name + ".json")
        for target, replacement in [
            (receipt, b'{"credential":"must-not-echo"}'),
            (slot / "bin/codex.exe", b"modified-user-binary"),
        ]:
            original = target.read_bytes()
            target.write_bytes(replacement)
            result = self.status(slot.name)
            self.assertEqual(result.returncode, 1)
            self.assertEqual(
                json.loads(result.stdout),
                {"status": "unverified", "action": "manualReconciliation"},
            )
            self.assertEqual(result.stderr, "")
            self.assertEqual(target.read_bytes(), replacement)
            target.write_bytes(original)

    def test_symlink_receipt_swap_preserves_foreign_target(self) -> None:
        slot = stage_fork_package(self.package, self.install, self.digest)
        receipt = self.install / "fork-receipts" / (slot.name + ".json")
        foreign = Path(self.temp.name) / "foreign-receipt"
        foreign.write_bytes(receipt.read_bytes())
        receipt.unlink()
        receipt.symlink_to(foreign)
        before = foreign.read_bytes()
        result = self.status(slot.name)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(
            json.loads(result.stdout),
            {"status": "unverified", "action": "manualReconciliation"},
        )
        self.assertTrue(receipt.is_symlink())
        self.assertEqual(foreign.read_bytes(), before)

    def test_receipt_replacement_during_package_verification_is_rejected(self) -> None:
        slot = stage_fork_package(self.package, self.install, self.digest)
        receipt = self.install / "fork-receipts" / (slot.name + ".json")
        replacement = receipt.with_suffix(".replacement")
        replacement.write_bytes(receipt.read_bytes())

        def swapped(*args, **kwargs):
            result = verify_fork_package(*args, **kwargs)
            replacement.replace(receipt)
            return result

        with patch(
            "codex_package.fork_side_by_side.verify_fork_package", side_effect=swapped
        ):
            with self.assertRaisesRegex(ValueError, "receipt changed"):
                read_staged_fork_receipt(self.install, slot.name)
        self.assertTrue(receipt.is_file())
