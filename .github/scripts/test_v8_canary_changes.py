import contextlib
import io
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from v8_canary_changes import changed_files
from v8_canary_changes import canary_required
from v8_canary_changes import main
from v8_canary_changes import merge_base
from v8_canary_changes import resolved_v8_version
from v8_canary_changes import windows_source_required


class V8CanaryChangesTest(unittest.TestCase):
    def test_resolved_v8_version(self) -> None:
        cargo_lock = b"""\
[[package]]
name = "other"
version = "1.0.0"

[[package]]
name = "v8"
version = "149.2.0"
"""

        self.assertEqual(resolved_v8_version(cargo_lock), "149.2.0")

    def test_unrelated_cargo_manifest_change_does_not_require_source_build(
        self,
    ) -> None:
        self.assertFalse(
            windows_source_required(
                {"codex-rs/Cargo.toml"},
                "149.2.0",
                "149.2.0",
            )
        )

    def test_v8_version_change_requires_source_build(self) -> None:
        self.assertTrue(windows_source_required(set(), "149.2.0", "150.0.0"))

    def test_module_helper_change_requires_source_build(self) -> None:
        self.assertTrue(
            windows_source_required(
                {".github/scripts/rusty_v8_module_bazel.py"},
                "149.2.0",
                "149.2.0",
            )
        )

    def test_shared_ci_setup_changes_require_canary_and_source_build(self) -> None:
        for path in (
            ".github/actions/setup-ci/action.yml",
            ".github/scripts/setup-dev-drive.ps1",
        ):
            with self.subTest(path=path):
                changed_files = {path}
                self.assertTrue(canary_required(changed_files, "149.2.0", "149.2.0"))
                self.assertTrue(
                    windows_source_required(changed_files, "149.2.0", "149.2.0")
                )

    def test_manual_dispatch_requires_source_build(self) -> None:
        self.assertTrue(
            windows_source_required(
                set(),
                "149.2.0",
                "149.2.0",
                force=True,
            )
        )

    def test_changed_files_excludes_changes_made_only_on_base_branch(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.run_git(root, "init", "--initial-branch=main")
            self.run_git(root, "config", "user.name", "Test User")
            self.run_git(root, "config", "user.email", "test@example.com")
            self.run_git(root, "config", "commit.gpgsign", "false")

            self.write_and_commit(root, "initial", "initial.txt")
            common = self.run_git(root, "rev-parse", "HEAD")
            self.run_git(root, "switch", "-c", "feature")
            self.run_git(root, "switch", "main")
            self.write_and_commit(root, "base-only", "base-only.txt")
            base = self.run_git(root, "rev-parse", "HEAD")

            self.run_git(root, "switch", "feature")
            self.write_and_commit(root, "feature-only", "feature-only.txt")
            head = self.run_git(root, "rev-parse", "HEAD")

            self.assertEqual(
                changed_files(base, head, root=root),
                {"feature-only.txt"},
            )
            self.assertEqual(merge_base(base, head, root=root), common)

    def write_and_commit(self, root: Path, contents: str, path: str) -> None:
        (root / path).write_text(contents)
        self.run_git(root, "add", path)
        self.run_git(root, "commit", "-m", contents)

    def run_git(self, root: Path, *args: str) -> str:
        return subprocess.check_output(
            ["git", *args],
            cwd=root,
            stderr=subprocess.PIPE,
            text=True,
        ).strip()


class V8CanaryMetadataTest(unittest.TestCase):
    manifest = """\
[workspace]
members = ["v8-poc"]
resolver = "2"
[workspace.dependencies]
v8 = { version = "=150.4.0", default-features = false }
sqlx = { version = "=0.9.0", features = ["sqlite-bundled"] }
"""
    storage_manifest = manifest.replace(
        '["v8-poc"]', '["v8-poc", "postgres-runtime"]'
    ).replace('["sqlite-bundled"]', '["sqlite-bundled", "postgres"]')

    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.run_git("init", "--initial-branch=main")
        self.run_git("config", "user.name", "Test User")
        self.run_git("config", "user.email", "test@example.com")
        self.run_git("config", "commit.gpgsign", "false")
        (self.root / "codex-rs").mkdir()
        self.write("codex-rs/Cargo.toml", self.manifest)
        self.write(
            "codex-rs/Cargo.lock", '[[package]]\nname = "v8"\nversion = "150.4.0"\n'
        )
        self.commit()
        self.base = self.run_git("rev-parse", "HEAD")

    def test_storage_manifest_does_not_rebuild_v8(self) -> None:
        for contents in (
            self.manifest.replace('["v8-poc"]', '["v8-poc", "postgres-runtime"]'),
            self.manifest.replace(
                '["sqlite-bundled"]', '["sqlite-bundled", "postgres"]'
            ),
            self.storage_manifest,
        ):
            with self.subTest(contents=contents):
                self.write("codex-rs/Cargo.toml", contents)
                self.commit()
                self.assert_metadata(canary=False, windows=False)

    def test_other_manifest_changes_keep_canary(self) -> None:
        for contents in (
            self.storage_manifest.replace('"=150.4.0"', '"=150.5.0"'),
            self.storage_manifest.replace("default-features = false", "features = []"),
            self.storage_manifest.replace('resolver = "2"', 'resolver = "3"'),
            self.storage_manifest.replace('"=0.9.0"', '"=0.10.0"'),
            self.storage_manifest.replace('"postgres"', '"mysql"'),
            self.storage_manifest.replace('"sqlite-bundled", ', ""),
            self.storage_manifest.replace('"v8-poc", ', ""),
            self.storage_manifest + "\n[profile.dev]\nopt-level = 1\n",
            self.storage_manifest + '\n[patch.crates-io]\nv8 = { path = "local" }\n',
            self.storage_manifest + '\n[workspace.dependencies.other]\nversion = "1"\n',
            self.storage_manifest.replace(
                'members = ["v8-poc", "postgres-runtime"]', 'members = "v8-poc"'
            ),
            '[workspace]\nmembers = ["v8-poc"]\ndependencies = "invalid"\n',
            "malformed toml",
        ):
            with self.subTest(contents=contents):
                self.write("codex-rs/Cargo.toml", contents)
                self.commit()
                self.assert_metadata(
                    canary=True, windows=False, reason="codex-rs/Cargo.toml"
                )

    def test_missing_manifest_keeps_canary(self) -> None:
        (self.root / "codex-rs/Cargo.toml").unlink()
        self.commit()
        self.assert_metadata(canary=True, windows=False, reason="codex-rs/Cargo.toml")
        self.base = self.run_git("rev-parse", "HEAD")
        self.write("codex-rs/Cargo.toml", self.storage_manifest)
        self.commit()
        self.assert_metadata(canary=True, windows=False, reason="codex-rs/Cargo.toml")

    def test_storage_and_build_inputs_keep_canary(self) -> None:
        self.write("codex-rs/Cargo.toml", self.storage_manifest)
        self.write("MODULE.bazel", "# new artifact configuration\n")
        self.commit()
        self.assert_metadata(canary=True, windows=False, reason="MODULE.bazel")
        self.write(".github/workflows/v8-canary.yml", "# new build configuration\n")
        self.commit()
        self.assert_metadata(
            canary=True,
            windows=True,
            reason=".github/workflows/v8-canary.yml, MODULE.bazel",
            windows_reason=".github/workflows/v8-canary.yml",
        )

    def test_lock_version_and_manual_dispatch_keep_every_variant(self) -> None:
        self.write("codex-rs/Cargo.toml", self.storage_manifest)
        self.write(
            "codex-rs/Cargo.lock", '[[package]]\nname = "v8"\nversion = "150.5.0"\n'
        )
        self.commit()
        reason = "v8 version changed from 150.4.0 to 150.5.0"
        self.assert_metadata(
            canary=True, windows=True, reason=reason, windows_reason=reason
        )
        self.assert_metadata(
            canary=True,
            windows=True,
            reason="manual workflow dispatch",
            windows_reason="manual workflow dispatch",
            args=["--force"],
        )

    def test_base_only_v8_change_does_not_hide_storage_exception(self) -> None:
        self.run_git("branch", "feature")
        self.write("codex-rs/Cargo.toml", self.manifest.replace("150.4.0", "150.5.0"))
        self.write(
            "codex-rs/Cargo.lock", '[[package]]\nname = "v8"\nversion = "150.5.0"\n'
        )
        self.commit()
        self.base = self.run_git("rev-parse", "HEAD")
        self.run_git("switch", "feature")
        self.write("codex-rs/Cargo.toml", self.storage_manifest)
        self.commit()
        self.assert_metadata(canary=False, windows=False)

    def assert_metadata(
        self,
        *,
        canary: bool,
        windows: bool,
        reason: str = "no relevant changes",
        windows_reason: str = "no relevant changes",
        args: list[str] | None = None,
    ) -> None:
        output = io.StringIO()
        with (
            patch.object(
                sys,
                "argv",
                [
                    "v8_canary_changes.py",
                    *(args or ["--base", self.base, "--head", "HEAD"]),
                ],
            ),
            contextlib.redirect_stdout(output),
        ):
            main(root=self.root)
        self.assertEqual(
            dict(line.split("=", 1) for line in output.getvalue().splitlines()),
            {
                "canary_required": str(canary).lower(),
                "canary_reason": reason,
                "windows_source_required": str(windows).lower(),
                "windows_source_reason": windows_reason,
            },
        )

    def write(self, path: str, contents: str) -> None:
        destination = self.root / path
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(contents, encoding="utf-8")

    def commit(self) -> None:
        self.run_git("add", "-A")
        self.run_git("commit", "-m", "fixture change")

    def run_git(self, *args: str) -> str:
        return subprocess.check_output(
            ["git", *args], cwd=self.root, stderr=subprocess.PIPE, text=True
        ).strip()


if __name__ == "__main__":
    unittest.main()
