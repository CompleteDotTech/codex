#!/usr/bin/env python3

import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from codex_package.fork_identity import MANIFEST_NAME
from codex_package.fork_identity import OWNER
from codex_package.fork_identity import package_tree
from codex_package.fork_identity import safe_name
from codex_package.fork_identity import seal_fork_package
from codex_package.fork_identity import sha256
from codex_package.fork_identity import source_identity
from codex_package.fork_identity import verify_fork_package


IDENTITY = {
    "owner": OWNER,
    "declaredBaseCommit": "a" * 40,
    "forkCommit": "b" * 40,
    "patchsetSha256": "sha256:" + "c" * 64,
    "channel": "preview",
    "storageCapabilities": ["sqlite"],
    "postgresSchemaVersions": [],
}


class ForkPackageIdentityTest(unittest.TestCase):
    def test_source_identity_uses_committed_ancestor_and_fork_head(self) -> None:
        fixture_env = {
            key: value
            for key, value in os.environ.items()
            if not key.startswith("GIT_")
        }
        fixture_env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)
        self.enterContext(patch.dict(os.environ, fixture_env, clear=True))
        repository = Path(self.temp.name) / "source"
        repository.mkdir()
        subprocess.check_call(["git", "init", "--initial-branch=main", str(repository)])
        for key, value in {
            "user.name": "Package identity fixture",
            "user.email": "fixture@example.invalid",
            "commit.gpgsign": "false",
            "core.hooksPath": str(repository / "no-hooks"),
            "core.autocrlf": "false",
        }.items():
            subprocess.check_call(["git", "-C", str(repository), "config", key, value])
        source = repository / "source.txt"
        for contents in ("upstream\n", "fork\n"):
            source.write_text(contents, encoding="utf-8")
            subprocess.check_call(["git", "-C", str(repository), "add", "source.txt"])
            subprocess.check_call(
                ["git", "-C", str(repository), "commit", "-m", contents.strip()]
            )
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
        from codex_package import fork_identity

        with patch.object(fork_identity, "REPO_ROOT", repository):
            identity = source_identity(upstream, "preview")
        self.assertEqual(identity["declaredBaseCommit"], upstream)
        self.assertNotEqual(upstream, fork)
        self.assertTrue(patchset)
        self.assertEqual(identity["forkCommit"], fork)
        self.assertEqual(
            identity["patchsetSha256"], "sha256:" + hashlib.sha256(patchset).hexdigest()
        )

    def test_source_identity_refuses_dirty_worktree(self) -> None:
        with patch("codex_package.fork_identity.git", return_value=b" M source.py\n"):
            with self.assertRaisesRegex(ValueError, "dirty"):
                source_identity("a" * 40, "preview")

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
        verification = verify_fork_package(self.package)
        manifest = verification.manifest
        self.assertEqual(verification.unix_mode_status, "notApplicable")
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
        with self.assertRaisesRegex(ValueError, "unexpected fork package resource"):
            verify_fork_package(self.package)

    def test_rejects_extra_executable_even_when_manifest_lists_it(self) -> None:
        (self.package / MANIFEST_NAME).unlink()
        (self.package / "bin/other.exe").write_bytes(b"unexpected executable")
        with self.assertRaisesRegex(ValueError, "unexpected fork package executable"):
            seal_fork_package(self.package, IDENTITY)

    def test_rejects_extra_empty_directory_before_seal_and_after_seal(self) -> None:
        extra = self.package / "codex-resources/empty"
        extra.mkdir()
        with self.assertRaisesRegex(ValueError, "unexpected or empty directories"):
            verify_fork_package(self.package)
        (self.package / MANIFEST_NAME).unlink()
        with self.assertRaisesRegex(ValueError, "unexpected or empty directories"):
            seal_fork_package(self.package, IDENTITY)

    def test_rejects_unexpected_nonempty_directory_even_if_file_is_listed(self) -> None:
        (self.package / MANIFEST_NAME).unlink()
        extra = self.package / "codex-resources/other"
        extra.mkdir()
        (extra / "helper").write_bytes(b"unexpected resource")
        with self.assertRaisesRegex(ValueError, "unexpected or empty directories"):
            seal_fork_package(self.package, IDENTITY)

    def test_rejects_unsafe_directory_name(self) -> None:
        self.assertFalse(safe_name("codex-resources\\..\\escape"))
        if os.name == "nt":
            self.skipTest("Windows treats backslashes as path separators")
        unsafe = self.package / "codex-resources\\..\\escape"
        try:
            unsafe.mkdir()
        except OSError as error:
            self.skipTest(f"unsafe directory name unavailable: {error}")
        with self.assertRaisesRegex(ValueError, "unsafe path"):
            verify_fork_package(self.package)
        (self.package / MANIFEST_NAME).unlink()
        with self.assertRaisesRegex(ValueError, "unsafe path"):
            seal_fork_package(self.package, IDENTITY)

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
        bwrap = self.package / "codex-resources/bwrap"
        bwrap.chmod(0o755)
        if os.name == "nt":
            files, directories = package_tree(self.package)
            manifest = {
                "manifestVersion": 1,
                **IDENTITY,
                "packageVersion": metadata["version"],
                "target": metadata["target"],
                "variant": metadata["variant"],
                "files": {
                    name: {
                        "sha256": "sha256:" + sha256(path),
                        "unixMode": "0644" if name == "codex-package.json" else "0755",
                    }
                    for name, path in files.items()
                },
                "directories": {name: "0755" for name in directories},
            }
            (self.package / MANIFEST_NAME).write_text(json.dumps(manifest))
        else:
            seal_fork_package(self.package, IDENTITY)
        verification = verify_fork_package(self.package)
        self.assertEqual(verification.manifest["target"], metadata["target"])
        self.assertEqual(
            verification.unix_mode_status,
            "unavailable" if os.name == "nt" else "verified",
        )

    @unittest.skipIf(os.name == "nt", "Unix chmod modes are unavailable on Windows")
    def test_unix_executable_mode_change_fails_verification(self) -> None:
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
        bwrap = self.package / "codex-resources/bwrap"
        bwrap.write_bytes(b"bwrap")
        bwrap.chmod(0o755)
        seal_fork_package(self.package, IDENTITY)
        (self.package / "bin/codex").chmod(0o644)
        with self.assertRaisesRegex(ValueError, "Unix mode differs"):
            verify_fork_package(self.package)
        (self.package / "bin/codex").chmod(0o755)
        manifest_path = self.package / MANIFEST_NAME
        manifest = json.loads(manifest_path.read_text())
        manifest["files"]["bin/codex"]["unixMode"] = "0644"
        manifest_path.write_text(json.dumps(manifest))
        with self.assertRaisesRegex(ValueError, "not executable"):
            verify_fork_package(self.package)

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


if __name__ == "__main__":
    unittest.main()
