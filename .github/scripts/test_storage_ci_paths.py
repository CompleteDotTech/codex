"""Check route selection and fail-closed aggregation for storage-tool changes."""

from pathlib import Path
import subprocess
import tempfile
import unittest

from storage_ci_paths import changed_paths
from storage_ci_paths import required_dependencies
from storage_ci_paths import requires_native_checks
from storage_ci_paths import V8_GUARD_ANCHOR
from storage_ci_paths import V8_STORAGE_GUARD
from storage_ci_paths import V8_WORKFLOW


class StorageCiPathsTests(unittest.TestCase):
    def test_pr_ignores_base_only_rust_changes_but_push_uses_exact_endpoints(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)

            def git(*args):
                return subprocess.check_output(
                    ["git", "-c", "commit.gpgsign=false", *args],
                    cwd=root,
                    text=True,
                    stderr=subprocess.PIPE,
                ).strip()

            def commit(path, content):
                file = root / path
                file.parent.mkdir(parents=True, exist_ok=True)
                file.write_text(content, encoding="utf-8")
                git("add", path)
                git("commit", "-m", "fixture")
                return git("rev-parse", "HEAD")

            git("init", "--initial-branch=main")
            git("config", "user.name", "CI fixture")
            git("config", "user.email", "fixture@example.invalid")
            common = commit("initial.txt", "initial")
            git("branch", "storage")
            base = commit("codex-rs/core/src/lib.rs", "base-only Rust change")
            git("switch", "storage")
            head = commit("scripts/postgres/manage.py", "storage-only branch change")

            pr_base, pr_paths = changed_paths(base, head, "pull_request", root=root)
            self.assertEqual(
                (pr_base, pr_paths), (common, ["scripts/postgres/manage.py"])
            )
            self.assertFalse(requires_native_checks(pr_paths))
            push_base, push_paths = changed_paths(base, head, "push", root=root)
            self.assertEqual(
                (push_base, push_paths),
                (base, ["codex-rs/core/src/lib.rs", "scripts/postgres/manage.py"]),
            )
            self.assertTrue(requires_native_checks(push_paths))

    def test_tooling_and_workflow_changes_use_lightweight_checks(self):
        for paths in (
            ["scripts/storage_contract/manifest.py"],
            ["scripts/postgres/compose.yaml", "scripts/postgres/tests/test_state.py"],
            ["scripts/audit_sqlite_snapshot.py", "scripts/verify_storage_bundle.py"],
            [".codespellignore"],
            [".github/scripts/verify_cargo_workspace_manifests.py"],
            [
                ".github/workflows/blocking-ci.yml",
                ".github/workflows/storage-tools.yml",
            ],
            [
                ".github/scripts/storage_ci_paths.py",
                ".github/scripts/test_storage_ci_paths.py",
            ],
            [
                "scripts/storage_contract/catalog.py",
                ".github/workflows/storage-tools.yml",
            ],
        ):
            with self.subTest(paths=paths):
                self.assertFalse(requires_native_checks(paths))

    def test_native_unknown_and_mixed_changes_keep_full_checks(self):
        for path in (
            "codex-rs/core/src/lib.rs",
            "codex-rs/Cargo.lock",
            "MODULE.bazel.lock",
            "defs.bzl",
            "sdk/python/src/codex/__init__.py",
            ".github/actions/setup-ci/action.yml",
            ".github/workflows/rust-ci.yml",
            ".github/workflows/bazel.yml",
            ".github/workflows/sdk.yml",
            ".github/workflows/rust-ci-full.yml",
            ".github/workflows/v8-canary.yml",
            ".github/workflows/unknown.yml",
            ".github/scripts/check_ci_results.py",
            "scripts/postgres_extra.py",
            "scripts/storage_contract_other/manifest.py",
            "unknown/file",
        ):
            for paths in ([path], ["scripts/postgres/manage.py", path]):
                with self.subTest(paths=paths):
                    self.assertTrue(requires_native_checks(paths))
        self.assertTrue(requires_native_checks([]))

    def test_only_exact_v8_metadata_guard_preserves_lightweight_checks(self):
        original = b"name: canary\n" + V8_GUARD_ANCHOR + b"native build matrix\n"
        guarded = original.replace(V8_GUARD_ANCHOR, V8_STORAGE_GUARD + V8_GUARD_ANCHOR)
        for before, after in ((original, guarded), (guarded, original)):
            with self.subTest(before=before):
                self.assertFalse(
                    requires_native_checks(
                        [V8_WORKFLOW], v8_workflow_change=(before, after)
                    )
                )
        for changed in (
            guarded.replace(b"native build matrix", b"different native build matrix"),
            V8_STORAGE_GUARD + original,
            guarded.replace(b"exit 0", b"exit 1"),
            guarded.replace(V8_STORAGE_GUARD, V8_STORAGE_GUARD * 2),
        ):
            with self.subTest(changed=changed):
                self.assertTrue(
                    requires_native_checks(
                        [V8_WORKFLOW], v8_workflow_change=(original, changed)
                    )
                )
        self.assertTrue(
            requires_native_checks(
                [V8_WORKFLOW, "codex-rs/Cargo.toml"],
                v8_workflow_change=(original, guarded),
            )
        )

    def test_only_deliberate_native_skips_are_exempted(self):
        needs = {
            "changed": {"result": "success", "outputs": {"native": "false"}},
            "bazel": {"result": "skipped"},
            "rust-ci-full": {"result": "skipped"},
            "v8-canary": {"result": "skipped"},
            "rust-ci": {"result": "failure"},
            "sdk": {"result": "cancelled"},
            "storage-tools": {"result": "skipped"},
            "repo-checks": {"result": "success"},
        }
        expected = {
            name: value
            for name, value in needs.items()
            if name not in {"bazel", "rust-ci-full", "v8-canary"}
        }
        self.assertEqual(required_dependencies(needs), expected)

    def test_missing_failed_or_full_classifier_never_accepts_skips(self):
        for changed in (
            {},
            {"result": "failure", "outputs": {"native": "false"}},
            {"result": "cancelled", "outputs": {"native": "false"}},
            {"result": "success", "outputs": {}},
            {"result": "success", "outputs": {"native": "true"}},
        ):
            with self.subTest(changed=changed):
                needs = {"changed": changed, "bazel": {"result": "skipped"}}
                self.assertEqual(required_dependencies(needs), needs)


if __name__ == "__main__":
    unittest.main()
