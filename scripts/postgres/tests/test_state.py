"""Offline tests: real OpenSSL and filesystem; no PostgreSQL assertions."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import state


class StateTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.root = tempfile.TemporaryDirectory()
        cls.seed = Path(cls.root.name) / "seed"
        state.initialize(
            cls.seed,
            "codex-pg-unit",
            "postgres:17.11-bookworm",
            55432,
            ["db.example.test"],
        )

    @classmethod
    def tearDownClass(cls):
        cls.root.cleanup()

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name) / "state"
        if os.name == "nt":
            # copytree preserves POSIX modes, but not protected Windows ACLs.
            for path in (self.home, self.home / "secrets", self.home / "backups"):
                state.private_directory(path)
        shutil.copytree(self.seed, self.home, dirs_exist_ok=True)

    def test_repeat_init_preserves_every_secret(self):
        before = {p.name: p.read_bytes() for p in (self.home / "secrets").iterdir()}
        state.initialize(
            self.home,
            "codex-pg-unit",
            "postgres:17.11-bookworm",
            55432,
            ["db.example.test"],
        )
        self.assertEqual(
            before, {p.name: p.read_bytes() for p in (self.home / "secrets").iterdir()}
        )

    def test_reconfiguration_does_not_overwrite_state(self):
        original = (self.home / "receipt.json").read_bytes()
        with self.assertRaisesRegex(state.ServiceError, "existing_state_conflict"):
            state.initialize(
                self.home, "codex-pg-other", "postgres:17.11-bookworm", 55432, []
            )
        self.assertEqual(original, (self.home / "receipt.json").read_bytes())

    def test_interrupted_initialization_does_not_regenerate_credentials(self):
        (self.home / "receipt.json").unlink()
        before = (self.home / "secrets/runtime.password").read_bytes()
        with self.assertRaises(state.ServiceError):
            state.initialize(
                self.home, "codex-pg-unit", "postgres:17.11-bookworm", 55432, []
            )
        self.assertEqual(before, (self.home / "secrets/runtime.password").read_bytes())

    def test_unsupported_major_rejected_before_creating_files(self):
        path = Path(self.temp.name) / "new"
        with self.assertRaises(state.ServiceError):
            state.initialize(path, "codex-pg-unit", "postgres:18.6-bookworm", 55432, [])
        self.assertFalse(path.exists())

    def test_certificate_argument_injection_rejected(self):
        for name in [
            "db\nDNS:evil",
            "*.example.test",
            "db,IP:8.8.8.8",
            "; touch /tmp/no",
            "-x",
        ]:
            with self.subTest(name=name), self.assertRaises(state.ServiceError):
                state.server_names([name])

    def test_certificate_validates_service_localhost_ip_and_extra_name(self):
        for option, host in [
            ("-verify_hostname", "postgres"),
            ("-verify_hostname", "localhost"),
            ("-verify_hostname", "db.example.test"),
            ("-verify_ip", "127.0.0.1"),
            ("-verify_ip", "::1"),
        ]:
            result = subprocess.run(
                [
                    "openssl",
                    "verify",
                    "-CAfile",
                    str(self.home / "secrets/ca.crt"),
                    option,
                    host,
                    str(self.home / "secrets/server.crt"),
                ],
                capture_output=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr.decode())

    def test_certificate_rejects_wrong_host(self):
        result = subprocess.run(
            [
                "openssl",
                "verify",
                "-CAfile",
                str(self.home / "secrets/ca.crt"),
                "-verify_hostname",
                "untrusted.test",
                str(self.home / "secrets/server.crt"),
            ],
            capture_output=True,
        )
        self.assertNotEqual(result.returncode, 0)

    def test_actual_server_key_matches_certificate(self):
        key = state.run(
            ["openssl", "pkey", "-in", str(self.home / "secrets/server.key"), "-pubout"]
        )
        cert = state.run(
            [
                "openssl",
                "x509",
                "-in",
                str(self.home / "secrets/server.crt"),
                "-pubkey",
                "-noout",
            ]
        )
        self.assertEqual(key, cert)

    def test_secret_changes_detected(self):
        (self.home / "secrets/runtime.password").write_bytes(b"0" * 64)
        with self.assertRaisesRegex(state.ServiceError, "secret_changed"):
            state.load(self.home)

    def test_missing_secret_rejected(self):
        (self.home / "secrets/backup.password").unlink()
        with self.assertRaises(state.ServiceError):
            state.load(self.home)

    def test_unknown_receipt_file_not_read(self):
        receipt = state.load(self.home)
        receipt["file_hashes"]["../../outside"] = "0" * 64
        state.publish_json(self.home / "receipt.json", receipt)
        with self.assertRaisesRegex(state.ServiceError, "invalid_receipt_inventory"):
            state.load(self.home)

    def test_damaged_receipt_rejected(self):
        (self.home / "receipt.json").write_text('{"format":1}')
        with self.assertRaises(state.ServiceError):
            state.load(self.home)

    def test_locked_operation_does_not_delete_other_owners_lock(self):
        marker = self.home / ".operation.lock"
        marker.write_text("another owner")
        with self.assertRaisesRegex(state.ServiceError, "operation_locked"):
            with state.operation_lock(self.home):
                self.fail("lock unexpectedly acquired")
        self.assertEqual(marker.read_text(), "another owner")

    def test_operation_lock_released_after_own_failure(self):
        with self.assertRaises(RuntimeError):
            with state.operation_lock(self.home):
                raise RuntimeError("operation interrupted")
        self.assertFalse((self.home / ".operation.lock").exists())

    def test_lock_owner_change_preserves_replacement(self):
        with self.assertRaisesRegex(state.ServiceError, "lock_ownership_changed"):
            with state.operation_lock(self.home):
                (self.home / ".operation.lock").write_text("replacement owner")
        self.assertEqual(
            (self.home / ".operation.lock").read_text(), "replacement owner"
        )

    def test_write_new_never_overwrites(self):
        file = self.home / "protected"
        state.write_new(file, b"old")
        with self.assertRaises(FileExistsError):
            state.write_new(file, b"new")
        self.assertEqual(file.read_bytes(), b"old")

    def test_failure_before_receipt_replace_preserves_old(self):
        before = (self.home / "receipt.json").read_bytes()
        with (
            patch("state.os.replace", side_effect=OSError("disk failure")),
            self.assertRaises(OSError),
        ):
            state.publish_json(self.home / "receipt.json", {"new": True})
        self.assertEqual((self.home / "receipt.json").read_bytes(), before)

    def test_state_inside_source_tree_is_refused(self):
        target = Path(state.__file__).parent / "live"
        with self.assertRaisesRegex(state.ServiceError, "outside_source"):
            state.state_path(str(target))

    def test_hostnames_and_ip_names_are_normalized(self):
        names = state.server_names(["DB.example", "2001:db8::1", "db.example"])
        self.assertEqual(names[-2:], ["DNS:db.example", "IP:2001:db8::1"])

    def test_openssl_errors_are_redacted(self):
        with (
            patch("state.subprocess.run", side_effect=OSError("secret content")),
            self.assertRaisesRegex(
                state.ServiceError, "^command_unavailable_or_timed_out$"
            ),
        ):
            state.run(["missing-program"])

    def test_discarded_native_output_does_not_require_utf8(self):
        result = subprocess.CompletedProcess(["native-command"], 0, b"path-\xe9", b"")
        with patch("state.subprocess.run", return_value=result):
            self.assertEqual(state.run(["native-command"], discard_output=True), "")
            with self.assertRaises(UnicodeDecodeError):
                state.run(["machine-readable-command"])


@unittest.skipIf(
    os.name == "nt", "POSIX modes/FIFOs; native ACL coverage is in test_state_windows"
)
class PosixPermissionsTests(unittest.TestCase):
    setUpClass = classmethod(StateTests.setUpClass.__func__)
    tearDownClass = classmethod(StateTests.tearDownClass.__func__)
    setUp = StateTests.setUp

    # Run these only in the advertised offline Linux qualification command.
    # Windows ACL tests require a separate Windows runner; not silently counted.
    def test_secret_symlink_is_rejected_without_reading_target(self):
        file = self.home / "secrets/runtime.password"
        file.unlink()
        file.symlink_to("ca.key")
        with self.assertRaisesRegex(state.ServiceError, "insecure_secret_file"):
            state.load(self.home)

    def test_insecure_permissions_are_refused(self):
        (self.home / "secrets/runtime.password").chmod(0o644)
        with self.assertRaisesRegex(state.ServiceError, "insecure_secret_file"):
            state.load(self.home)

    def test_fifo_receipt_is_refused_without_blocking(self):
        file = self.home / "receipt.json"
        file.unlink()
        os.mkfifo(file)
        with self.assertRaisesRegex(state.ServiceError, "invalid_receipt"):
            state.load(self.home)

    def test_symlinked_state_subdirectory_is_rejected(self):
        saved = self.home / "secret-real"
        (self.home / "secrets").rename(saved)
        (self.home / "secrets").symlink_to(saved, target_is_directory=True)
        with self.assertRaisesRegex(state.ServiceError, "insecure_state_directory"):
            state.load(self.home)
