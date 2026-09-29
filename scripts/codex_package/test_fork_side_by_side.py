#!/usr/bin/env python3

import json
import hashlib
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from codex_package.fork_identity import OWNER
from codex_package.fork_identity import seal_fork_package
from codex_package.fork_side_by_side import stage_fork_package
from codex_package.fork_side_by_side import verify_staged_fork_package
from codex_package.fork_side_by_side import owned_child_directory

IDENTITY = {
    "owner": OWNER,
    "declaredBaseCommit": "a" * 40,
    "forkCommit": "b" * 40,
    "patchsetSha256": "sha256:" + "c" * 64,
    "channel": "preview",
    "storageCapabilities": ["sqlite"],
    "postgresSchemaVersions": [],
}


@unittest.skipUnless(sys.platform == "linux", "Linux descriptor-pinned stage")
class SideBySideStageTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        self.package = root / "package"
        self.package.mkdir()
        self.install = root / "home"
        self.install.mkdir(mode=0o700)
        metadata = {
            "layoutVersion": 1,
            "version": "1.2.3",
            "target": "x86_64-pc-windows-msvc",
            "variant": "codex",
            "entrypoint": "bin/codex.exe",
            "resourcesDir": "codex-resources",
            "pathDir": "codex-path",
        }
        (self.package / "codex-package.json").write_text(json.dumps(metadata))
        for name in (
            "bin/codex.exe",
            "bin/codex-code-mode-host.exe",
            "codex-path/rg.exe",
            "codex-resources/codex-command-runner.exe",
            "codex-resources/codex-windows-sandbox-setup.exe",
        ):
            file = self.package / name
            file.parent.mkdir(exist_ok=True)
            file.write_bytes(name.encode())
        seal_fork_package(self.package, IDENTITY)
        self.digest = hashlib.sha256(
            (self.package / "codex-fork-package.json").read_bytes()
        ).hexdigest()

    def test_stage_is_inactive_and_external_receipt_matches(self) -> None:
        slot = stage_fork_package(self.package, self.install, self.digest)
        verify_staged_fork_package(self.install, slot.name)
        self.assertFalse((slot / "receipt.json").exists())
        receipt = self.install / "fork-receipts" / (slot.name + ".json")
        self.assertFalse(json.loads(receipt.read_text())["active"])
        with self.assertRaises(FileExistsError):
            stage_fork_package(self.package, self.install, self.digest)
        (slot / "bin/codex.exe").write_bytes(b"tampered")
        with self.assertRaisesRegex(ValueError, "checksum"):
            verify_staged_fork_package(self.install, slot.name)

    def test_interrupted_copy_leaves_reserved_slot_for_reconciliation(self) -> None:
        with patch(
            "codex_package.fork_side_by_side.shutil.copyfileobj",
            side_effect=OSError("interrupted"),
        ):
            with self.assertRaisesRegex(OSError, "interrupted"):
                stage_fork_package(self.package, self.install, self.digest)
        with self.assertRaises(FileExistsError):
            stage_fork_package(self.package, self.install, self.digest)
        self.assertEqual(list((self.install / "fork-receipts").iterdir()), [])

    def test_existing_unknown_install_is_preserved(self) -> None:
        slot = stage_fork_package(self.package, self.install, self.digest)
        sentinel = slot / "sentinel"
        sentinel.write_text("keep")
        with self.assertRaises(FileExistsError):
            stage_fork_package(self.package, self.install, self.digest)
        self.assertEqual(sentinel.read_text(), "keep")

    @unittest.skipUnless(os.name == "posix", "POSIX permissions")
    def test_shared_root_is_rejected(self) -> None:
        self.install.chmod(0o755)
        with self.assertRaisesRegex(ValueError, "owner-private"):
            stage_fork_package(self.package, self.install, self.digest)

    def test_rejects_unpinned_manifest_and_overlapping_root(self) -> None:
        with self.assertRaisesRegex(ValueError, "authenticated digest"):
            stage_fork_package(self.package, self.install, "0" * 64)
        self.package.chmod(0o700)
        with self.assertRaisesRegex(ValueError, "overlap"):
            stage_fork_package(self.package, self.package, self.digest)

    def test_readback_does_not_create_missing_receipt_directory(self) -> None:
        with self.assertRaises(FileNotFoundError):
            verify_staged_fork_package(self.install, self.digest)
        self.assertFalse((self.install / "fork-receipts").exists())

    def test_restrictive_umask_still_stages_complete_slot(self) -> None:
        program = (
            "import os, sys; from pathlib import Path; "
            "from codex_package.fork_side_by_side import stage_fork_package; "
            "os.umask(0o777); "
            "stage_fork_package(Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3])"
        )
        result = subprocess.run(
            [
                sys.executable,
                "-c",
                program,
                str(self.package),
                str(self.install),
                self.digest,
            ],
            check=False,
            capture_output=True,
            text=True,
            env={**os.environ, "PYTHONPATH": str(Path(__file__).resolve().parents[1])},
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        verify_staged_fork_package(self.install, self.digest)

    def test_failed_opens_close_pinned_descriptors(self) -> None:
        before = len(list(Path("/proc/self/fd").iterdir()))
        with self.assertRaises(FileNotFoundError):
            stage_fork_package(self.package, self.install / "missing", self.digest)
        self.assertEqual(len(list(Path("/proc/self/fd").iterdir())), before)
        (self.install / "fork-receipts").symlink_to(
            self.package, target_is_directory=True
        )
        with self.assertRaises(OSError):
            stage_fork_package(self.package, self.install, self.digest)
        self.assertEqual(len(list(Path("/proc/self/fd").iterdir())), before)

    def test_failed_child_validation_closes_descriptor(self) -> None:
        child = self.install / "shared"
        child.mkdir(mode=0o755)
        child.chmod(0o755)
        parent_fd = os.open(self.install, os.O_RDONLY | os.O_DIRECTORY)
        try:
            before = len(list(Path("/proc/self/fd").iterdir()))
            with self.assertRaisesRegex(ValueError, "owner-private"):
                owned_child_directory(parent_fd, "shared")
            self.assertEqual(len(list(Path("/proc/self/fd").iterdir())), before)
        finally:
            os.close(parent_fd)


if __name__ == "__main__":
    unittest.main()
