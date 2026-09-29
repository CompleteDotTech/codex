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

    def test_fifo_manifest_fails_without_blocking(self) -> None:
        manifest = self.package / "codex-fork-package.json"
        manifest.unlink()
        os.mkfifo(manifest)
        program = (
            "import sys; from pathlib import Path; "
            "from codex_package.fork_side_by_side import stage_fork_package; "
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
            timeout=5,
            env={**os.environ, "PYTHONPATH": str(Path(__file__).resolve().parents[1])},
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not a regular file", result.stderr)

    def test_source_file_swapped_to_fifo_before_copy_fails(self) -> None:
        program = """
import os
import sys
from pathlib import Path
from unittest.mock import patch
from codex_package.fork_identity import verify_fork_package
from codex_package.fork_side_by_side import stage_fork_package
package, install, digest = Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3]
source = package / 'bin' / 'codex.exe'
def swap(*args, **kwargs):
    verification = verify_fork_package(*args, **kwargs)
    source.unlink()
    os.mkfifo(source)
    return verification
with patch('codex_package.fork_side_by_side.verify_fork_package', side_effect=swap):
    stage_fork_package(package, install, digest)
"""
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
            timeout=5,
            env={**os.environ, "PYTHONPATH": str(Path(__file__).resolve().parents[1])},
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not regular", result.stderr)
        self.assertEqual(list((self.install / "fork-receipts").iterdir()), [])

    def test_missing_or_modified_receipt_blocks_readback(self) -> None:
        slot = stage_fork_package(self.package, self.install, self.digest)
        receipt = self.install / "fork-receipts" / (slot.name + ".json")
        original = receipt.read_text()
        receipt.write_text(original.replace('"active": false', '"active": true'))
        with self.assertRaisesRegex(ValueError, "receipt"):
            verify_staged_fork_package(self.install, slot.name)
        receipt.unlink()
        with self.assertRaises(FileNotFoundError):
            verify_staged_fork_package(self.install, slot.name)

    def test_linux_modes_and_nested_zsh_survive_staging(self) -> None:
        package = self.package.parent / "linux-package"
        package.mkdir()
        metadata = {
            "layoutVersion": 1,
            "version": "1.2.3",
            "target": "x86_64-unknown-linux-gnu",
            "variant": "codex",
            "entrypoint": "bin/codex",
            "resourcesDir": "codex-resources",
            "pathDir": "codex-path",
        }
        (package / "codex-package.json").write_text(json.dumps(metadata))
        for name in (
            "bin/codex",
            "bin/codex-code-mode-host",
            "codex-path/rg",
            "codex-resources/bwrap",
            "codex-resources/zsh/bin/zsh",
        ):
            path = package / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(name.encode())
            path.chmod(0o755)
        for name in (
            "bin",
            "codex-path",
            "codex-resources",
            "codex-resources/zsh",
            "codex-resources/zsh/bin",
        ):
            (package / name).chmod(0o755)
        seal_fork_package(package, IDENTITY)
        digest = hashlib.sha256(
            (package / "codex-fork-package.json").read_bytes()
        ).hexdigest()
        slot = stage_fork_package(package, self.install, digest)
        verify_staged_fork_package(self.install, digest)
        self.assertEqual(
            (slot / "codex-resources/zsh/bin").stat().st_mode & 0o777, 0o755
        )
        self.assertEqual(
            (slot / "codex-resources/zsh/bin/zsh").stat().st_mode & 0o777, 0o755
        )


if __name__ == "__main__":
    unittest.main()
