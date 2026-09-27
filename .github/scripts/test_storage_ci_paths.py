"""Check route selection and fail-closed aggregation for storage-tool changes."""

import unittest

from storage_ci_paths import required_dependencies
from storage_ci_paths import requires_native_checks


class StorageCiPathsTests(unittest.TestCase):
    def test_tooling_and_workflow_changes_use_lightweight_checks(self):
        for paths in (
            ["scripts/storage_contract/manifest.py"],
            ["scripts/postgres/compose.yaml", "scripts/postgres/tests/test_state.py"],
            ["scripts/audit_sqlite_snapshot.py", "scripts/verify_storage_bundle.py"],
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
