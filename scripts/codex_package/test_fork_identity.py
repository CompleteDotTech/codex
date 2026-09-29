#!/usr/bin/env python3

import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from codex_package.fork_identity import MANIFEST_NAME
from codex_package.fork_identity import OWNER
from codex_package.fork_identity import seal_fork_package
from codex_package.fork_identity import source_identity
from codex_package.fork_identity import verify_fork_package
from codex_package.fork_identity import write_archive_checksum


IDENTITY = {
    "owner": OWNER,
    "upstreamCommit": "a" * 40,
    "forkCommit": "b" * 40,
    "patchsetSha256": "sha256:" + "c" * 64,
    "channel": "preview",
    "storageCapabilities": ["sqlite"],
    "postgresSchemaVersions": [],
}


class ForkPackageIdentityTest(unittest.TestCase):
    def test_source_identity_uses_committed_ancestor_and_fork_head(self) -> None:
        repository = Path(__file__).resolve().parents[2]
        upstream = subprocess.check_output(
            ["git", "-C", str(repository), "rev-parse", "HEAD^"], text=True
        ).strip()
        fork = subprocess.check_output(
            ["git", "-C", str(repository), "rev-parse", "HEAD"], text=True
        ).strip()
        patchset = subprocess.check_output(
            [
                "git",
                "-C",
                str(repository),
                "diff",
                "--no-ext-diff",
                "--binary",
                upstream,
                fork,
            ]
        )
        identity = source_identity(upstream, "preview")
        self.assertEqual(identity["forkCommit"], fork)
        self.assertEqual(
            identity["patchsetSha256"], "sha256:" + hashlib.sha256(patchset).hexdigest()
        )

    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.package = Path(self.temp.name)
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
            path = self.package / name
            path.parent.mkdir(exist_ok=True)
            path.write_bytes(name.encode())
            path.chmod(0o755)
        seal_fork_package(self.package, IDENTITY)

    def test_verifies_complete_package_and_sqlite_only_capability(self) -> None:
        manifest = verify_fork_package(self.package)
        self.assertEqual(manifest["storageCapabilities"], ["sqlite"])
        self.assertEqual(manifest["postgresSchemaVersions"], [])
        self.assertEqual(
            set(manifest["files"]),
            {
                "codex-package.json",
                "bin/codex.exe",
                "bin/codex-code-mode-host.exe",
                "codex-path/rg.exe",
                "codex-resources/codex-command-runner.exe",
                "codex-resources/codex-windows-sandbox-setup.exe",
            },
        )

    def test_rejects_changed_or_missing_runtime_files(self) -> None:
        (self.package / "bin/codex.exe").write_bytes(b"different binary")
        with self.assertRaisesRegex(ValueError, "checksum differs"):
            verify_fork_package(self.package)
        (self.package / "bin/codex.exe").unlink()
        with self.assertRaisesRegex(ValueError, "Missing package file"):
            verify_fork_package(self.package)

    def test_rejects_uninventoried_file(self) -> None:
        (self.package / "codex-resources/extra").write_bytes(b"extra")
        with self.assertRaisesRegex(ValueError, "inventory differs"):
            verify_fork_package(self.package)

    def test_rejects_extra_executable_even_when_manifest_lists_it(self) -> None:
        (self.package / MANIFEST_NAME).unlink()
        (self.package / "bin/other.exe").write_bytes(b"unexpected executable")
        seal_fork_package(self.package, IDENTITY)
        with self.assertRaisesRegex(ValueError, "unexpected fork package executable"):
            verify_fork_package(self.package)

    def test_verifies_unix_package_on_windows_without_permission_bits(self) -> None:
        (self.package / MANIFEST_NAME).unlink()
        metadata_path = self.package / "codex-package.json"
        metadata = json.loads(metadata_path.read_text())
        metadata["target"] = "x86_64-unknown-linux-musl"
        metadata["entrypoint"] = "bin/codex"
        metadata_path.write_text(json.dumps(metadata))
        for old, new in (
            ("bin/codex.exe", "bin/codex"),
            ("bin/codex-code-mode-host.exe", "bin/codex-code-mode-host"),
            ("codex-path/rg.exe", "codex-path/rg"),
        ):
            (self.package / old).rename(self.package / new)
        (self.package / "codex-resources/codex-command-runner.exe").unlink()
        (self.package / "codex-resources/codex-windows-sandbox-setup.exe").unlink()
        (self.package / "codex-resources/bwrap").write_bytes(b"bwrap")
        seal_fork_package(self.package, IDENTITY)
        self.assertEqual(
            verify_fork_package(self.package)["target"], metadata["target"]
        )

    def test_rejects_link_inside_package(self) -> None:
        outside = self.package.parent / "outside"
        outside.write_bytes(b"outside")
        self.addCleanup(outside.unlink)
        link = self.package / "codex-resources/link"
        try:
            link.symlink_to(outside)
        except OSError as error:
            self.skipTest(f"symlink unavailable: {error}")
        with self.assertRaisesRegex(ValueError, "link or special file"):
            verify_fork_package(self.package)

    def test_rejects_path_traversal_inventory(self) -> None:
        manifest_path = self.package / MANIFEST_NAME
        manifest = json.loads(manifest_path.read_text())
        manifest["files"]["../outside"] = manifest["files"]["bin/codex.exe"]
        manifest_path.write_text(json.dumps(manifest))
        with self.assertRaisesRegex(ValueError, "unsafe path"):
            verify_fork_package(self.package)

    def test_rejects_unqualified_or_mismatched_manifest(self) -> None:
        manifest_path = self.package / MANIFEST_NAME
        manifest = json.loads(manifest_path.read_text())
        manifest["storageCapabilities"].append("postgresql")
        manifest_path.write_text(json.dumps(manifest))
        with self.assertRaisesRegex(ValueError, "unqualified storage capability"):
            verify_fork_package(self.package)
        manifest["storageCapabilities"] = ["sqlite"]
        manifest["target"] = "aarch64-unknown-linux-musl"
        manifest_path.write_text(json.dumps(manifest))
        with self.assertRaisesRegex(ValueError, "disagrees with package metadata"):
            verify_fork_package(self.package)

    def test_archive_checksum_binds_serialized_bytes(self) -> None:
        archive = self.package.parent / "fork-package.zip"
        archive.write_bytes(b"archive bytes")
        self.addCleanup(archive.unlink)
        checksum = write_archive_checksum(archive)
        self.addCleanup(checksum.unlink)
        self.assertEqual(
            checksum.read_text(),
            f"{hashlib.sha256(b'archive bytes').hexdigest()}  fork-package.zip\n",
        )
        with self.assertRaisesRegex(ValueError, "already exists"):
            write_archive_checksum(archive)


if __name__ == "__main__":
    unittest.main()
