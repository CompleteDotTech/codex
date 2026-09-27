import hashlib
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest

from .test_manifest import json_bytes, make_bundle

SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "verify_storage_bundle.py"


class CliTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = pathlib.Path(self.temp.name)
        manifest, payload, policy = make_bundle()
        self.files = {
            "manifest": json_bytes(manifest),
            "payload": payload,
            "inventory": json_bytes(policy),
        }
        for name, data in self.files.items():
            (self.home / name).write_bytes(data)
        self.args = [
            arg for name in self.files for arg in ("--" + name, str(self.home / name))
        ]
        self.args += [
            "--expected-manifest-sha256",
            hashlib.sha256(self.files["manifest"]).hexdigest(),
        ]

    def run_cli(self, args):
        return subprocess.run(
            [sys.executable, str(SCRIPT), *args],
            cwd=self.home,
            capture_output=True,
            text=True,
            check=False,
            timeout=10,
        )

    def test_real_process_verifies_without_mutating_inputs(self):
        before = {
            p.name: (p.stat().st_mtime_ns, p.read_bytes()) for p in self.home.iterdir()
        }
        result = self.run_cli(self.args)
        after = {
            p.name: (p.stat().st_mtime_ns, p.read_bytes()) for p in self.home.iterdir()
        }
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")
        self.assertEqual(before, after)
        self.assertEqual(json.loads(result.stdout)["status"], "bundle_verified")
        self.assertFalse(json.loads(result.stdout)["activation_permitted"])

    def test_missing_file_error_redacts_private_paths(self):
        secret = "PRIVATE_PATH_SENTINEL"
        args = self.args.copy()
        args[1] = str(self.home / secret)
        result = self.run_cli(args)
        self.assertEqual(result.returncode, 3)
        self.assertEqual(
            json.loads(result.stdout),
            {
                "status": "rejected",
                "code": "input_unavailable",
                "activation_permitted": False,
            },
        )
        self.assertNotIn(secret, result.stdout + result.stderr)

    def test_unknown_arguments_do_not_echo_credentials(self):
        secret = "postgresql://owner:PRIVATE_SECRET@host/db"
        result = self.run_cli([*self.args, "--password", secret])
        self.assertEqual(result.returncode, 2)
        self.assertNotIn(secret, result.stdout + result.stderr)
        self.assertEqual(json.loads(result.stdout)["code"], "invalid_arguments")

    def test_corrupt_payload_is_rejected_in_real_process(self):
        (self.home / "payload").write_bytes(self.files["payload"][:-1])
        result = self.run_cli(self.args)
        self.assertEqual(result.returncode, 2)
        self.assertEqual(json.loads(result.stdout)["status"], "rejected")
        self.assertEqual(result.stderr, "")

    def test_unknown_manifest_field_does_not_echo_payload(self):
        secret = "PRIVATE_PROVIDER_TOKEN"
        manifest = json.loads(self.files["manifest"])
        manifest["credential"] = secret
        raw = json_bytes(manifest)
        (self.home / "manifest").write_bytes(raw)
        args = self.args.copy()
        args[-1] = hashlib.sha256(raw).hexdigest()
        result = self.run_cli(args)
        self.assertEqual(result.returncode, 2)
        self.assertNotIn(secret, result.stdout + result.stderr)
        self.assertEqual(json.loads(result.stdout)["code"], "invalid_fields")

    def test_wrong_manifest_binding_never_reports_success(self):
        args = self.args.copy()
        args[-1] = "0" * 64
        result = self.run_cli(args)
        self.assertEqual(result.returncode, 2)
        self.assertEqual(json.loads(result.stdout)["code"], "manifest_digest_mismatch")

    @unittest.skipUnless(hasattr(os, "mkfifo"), "POSIX named-pipe behavior")
    def test_named_pipe_is_rejected_without_blocking(self):
        pipe = self.home / "pipe"
        os.mkfifo(pipe)
        args = self.args.copy()
        args[1] = str(pipe)
        result = self.run_cli(args)
        self.assertEqual(result.returncode, 2)
        self.assertEqual(json.loads(result.stdout)["code"], "input_not_regular_file")

    def test_oversized_input_is_rejected_before_parsing(self):
        (self.home / "manifest").write_bytes(b"x" * ((1 << 20) + 1))
        result = self.run_cli(self.args)
        self.assertEqual(result.returncode, 2)
        self.assertEqual(json.loads(result.stdout)["code"], "input_too_large")
