#!/usr/bin/env python3

import argparse
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from codex_package.cli import parse_package_version


class PackageVersionTest(unittest.TestCase):
    def test_accepts_release_prerelease_and_build_versions(self) -> None:
        for version in (
            "0.0.0",
            "1.2.3",
            "0.0.0-internal.deadbeef",
            "1.2.3-alpha.1+build.01",
            "18446744073709551615.0.0",
        ):
            with self.subTest(version=version):
                self.assertEqual(parse_package_version(version), version)

    def test_rejects_versions_the_runtime_cannot_parse(self) -> None:
        for version in (
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "v1.2.3",
            "01.2.3",
            "1.02.3",
            "1.2.03",
            "1.2.3-",
            "1.2.3-alpha..1",
            "1.2.3-01",
            "1.2.3+",
            "1.2.3+build..1",
            "18446744073709551616.0.0",
        ):
            with self.subTest(version=version):
                with self.assertRaises(argparse.ArgumentTypeError):
                    parse_package_version(version)

    def test_fork_candidate_refuses_prebuilt_inputs_before_output(
        self,
    ) -> None:
        builder = Path(__file__).resolve().parents[1] / "build_codex_package.py"
        with tempfile.TemporaryDirectory() as temporary:
            package = Path(temporary) / "package"
            cases = [
                (
                    ["--entrypoint-bin", str(Path(temporary) / "unverified.exe")],
                    "source-built and pinned",
                )
            ]
            for extra, diagnostic in cases:
                result = subprocess.run(
                    [
                        sys.executable,
                        str(builder),
                        "--fork-base-commit",
                        "a" * 40,
                        "--package-dir",
                        str(package),
                        *extra,
                    ],
                    text=True,
                    capture_output=True,
                    check=False,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(diagnostic, result.stderr)
                self.assertFalse(package.exists())


if __name__ == "__main__":
    unittest.main()
