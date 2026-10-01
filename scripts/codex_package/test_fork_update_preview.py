"""Exercise the public preview using complete synthetic owned bundle fixtures."""

import hashlib
import json
import shutil
import subprocess
import sys
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from codex_package import test_fork_side_by_side
from codex_package.fork_identity import MANIFEST_NAME
from codex_package.fork_identity import seal_fork_package
from codex_package.fork_identity import verify_fork_package
from codex_package.fork_side_by_side import stage_fork_package
from codex_package.fork_update_preview import preview_fork_update


@unittest.skipUnless(sys.platform == "linux", "Linux descriptor-pinned bundles")
class ForkUpdatePreviewTest(unittest.TestCase):
    def setUp(self) -> None:
        test_fork_side_by_side.SideBySideStageTest.setUp(self)
        self.slot = stage_fork_package(self.package, self.install, self.digest)

    def candidate(self, *, target=None, variant=None, **identity) -> tuple[Path, str]:
        candidate = Path(self.temp.name) / "candidate"
        shutil.copytree(self.package, candidate)
        (candidate / MANIFEST_NAME).unlink()
        metadata_path = candidate / "codex-package.json"
        metadata = json.loads(metadata_path.read_text())
        if target is not None:
            metadata["target"] = target
        if variant is not None:
            metadata["variant"] = variant
            metadata["entrypoint"] = "bin/codex-app-server.exe"
            (candidate / "bin/codex.exe").rename(candidate / "bin/codex-app-server.exe")
        metadata_path.write_text(json.dumps(metadata))
        seal_fork_package(candidate, test_fork_side_by_side.IDENTITY | identity)
        digest = hashlib.sha256((candidate / MANIFEST_NAME).read_bytes()).hexdigest()
        return candidate, digest

    def files(self) -> dict[str, bytes]:
        root = Path(self.temp.name)
        return {
            str(p.relative_to(root)): p.read_bytes()
            for p in root.rglob("*")
            if p.is_file()
        }

    def command(self, candidate: Path, digest: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                sys.executable,
                str(Path(__file__).resolve().parents[1] / "preview_fork_update.py"),
                str(self.install),
                self.slot.name,
                str(candidate),
                digest,
            ],
            text=True,
            capture_output=True,
            timeout=10,
            check=False,
        )

    def expected_report(self, candidate: Path, digest: str) -> dict[str, object]:
        current = json.loads((self.package / MANIFEST_NAME).read_text())
        proposed = json.loads((candidate / MANIFEST_NAME).read_text())
        fields = (
            "owner",
            "channel",
            "target",
            "variant",
            "packageVersion",
            "declaredBaseCommit",
            "forkCommit",
            "patchsetSha256",
        )
        return {
            "status": "sameArtifact"
            if self.slot.name == digest
            else "differentCandidate",
            "trust": "callerPinnedLocalBundle",
            "activationPermitted": False,
            "equalPackageVersion": current["packageVersion"]
            == proposed["packageVersion"],
            "current": {key: current[key] for key in fields},
            "candidate": {key: proposed[key] for key in fields},
        }

    def test_same_artifact_does_not_activate_or_change_files(self) -> None:
        before = self.files()
        result = self.command(self.package, self.digest)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            json.loads(result.stdout), self.expected_report(self.package, self.digest)
        )
        self.assertEqual(self.files(), before)

    def test_equal_numeric_version_different_fork_is_not_up_to_date(self) -> None:
        candidate, digest = self.candidate(forkCommit="d" * 40)
        before = self.files()
        result = self.command(candidate, digest)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            json.loads(result.stdout), self.expected_report(candidate, digest)
        )
        self.assertEqual(self.files(), before)

    def test_channel_change_and_conflicting_source_identity_are_blocked(self) -> None:
        for identity in [
            {"channel": "stable"},
            {"patchsetSha256": "sha256:" + "e" * 64},
        ]:
            with self.subTest(identity=identity):
                candidate, digest = self.candidate(**identity)
                before = self.files()
                result = self.command(candidate, digest)
                self.assertEqual(result.returncode, 1)
                self.assertEqual(
                    json.loads(result.stdout),
                    {"status": "blocked", "activationPermitted": False},
                )
                self.assertEqual(self.files(), before)
                shutil.rmtree(candidate)

    def test_wrong_digest_and_modified_bundle_are_blocked_without_echo(self) -> None:
        candidate, digest = self.candidate(forkCommit="d" * 40)
        for modified in [False, True]:
            with self.subTest(modified=modified):
                if modified:
                    (candidate / "bin/codex.exe").write_bytes(
                        b"credential-must-not-echo"
                    )
                before = self.files()
                result = self.command(candidate, digest if modified else "0" * 64)
                self.assertEqual(result.returncode, 1)
                self.assertEqual(
                    json.loads(result.stdout),
                    {"status": "blocked", "activationPermitted": False},
                )
                self.assertEqual(result.stderr, "")
                self.assertEqual(self.files(), before)

    def test_missing_installed_receipt_prevents_preview(self) -> None:
        receipt = self.install / "fork-receipts" / (self.slot.name + ".json")
        receipt.unlink()
        before = self.files()
        result = self.command(self.package, self.digest)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(
            json.loads(result.stdout),
            {"status": "blocked", "activationPermitted": False},
        )
        self.assertEqual(self.files(), before)

    def test_candidate_directory_swap_during_verification_is_rejected(self) -> None:
        candidate, digest = self.candidate(forkCommit="d" * 40)
        moved = candidate.with_name("original-candidate")
        foreign = candidate.with_name("foreign-candidate")
        shutil.copytree(candidate, foreign)
        before = self.files()

        def swapped(*args, **kwargs):
            result = verify_fork_package(*args, **kwargs)
            candidate.rename(moved)
            foreign.rename(candidate)
            return result

        with patch(
            "codex_package.fork_update_preview.verify_fork_package", side_effect=swapped
        ):
            with self.assertRaisesRegex(ValueError, "changed during preview"):
                preview_fork_update(self.install, self.slot.name, candidate, digest)
        self.assertEqual(
            (moved / MANIFEST_NAME).read_bytes(),
            (candidate / MANIFEST_NAME).read_bytes(),
        )
        self.assertEqual(
            {
                str(p.relative_to(self.install)): p.read_bytes()
                for p in self.install.rglob("*")
                if p.is_file()
            },
            {
                name.removeprefix("home/"): data
                for name, data in before.items()
                if name.startswith("home/")
            },
        )

    def test_complete_valid_target_or_variant_mismatch_is_blocked(self) -> None:
        for package in [
            {"target": "aarch64-pc-windows-msvc"},
            {"variant": "codex-app-server"},
        ]:
            with self.subTest(package=package):
                candidate, digest = self.candidate(**package)
                verification = verify_fork_package(candidate)
                self.assertEqual(
                    verification.manifest["target"],
                    package.get("target", "x86_64-pc-windows-msvc"),
                )
                self.assertEqual(
                    verification.manifest["variant"], package.get("variant", "codex")
                )
                before = self.files()
                result = self.command(candidate, digest)
                self.assertEqual(result.returncode, 1)
                self.assertEqual(
                    json.loads(result.stdout),
                    {"status": "blocked", "activationPermitted": False},
                )
                self.assertEqual(self.files(), before)
                shutil.rmtree(candidate)

    def test_deeply_nested_candidate_manifest_is_redacted_without_mutation(
        self,
    ) -> None:
        candidate, _ = self.candidate(forkCommit="d" * 40)
        manifest = candidate / MANIFEST_NAME
        manifest.write_bytes(b"[" * 100_000 + b"0" + b"]" * 100_000)
        digest = hashlib.sha256(manifest.read_bytes()).hexdigest()
        before = self.files()
        result = self.command(candidate, digest)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(
            json.loads(result.stdout),
            {"status": "blocked", "activationPermitted": False},
        )
        self.assertEqual(result.stderr, "")
        self.assertEqual(self.files(), before)
