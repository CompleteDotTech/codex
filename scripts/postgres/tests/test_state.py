"""Offline tests: real OpenSSL and filesystem; no PostgreSQL assertions."""

import os
from pathlib import Path
import shutil
import sys
import tempfile
import unittest

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
            for item in self.seed.rglob("*"):
                if item.is_file():
                    state.write_new(
                        self.home / item.relative_to(self.seed), item.read_bytes()
                    )
        else:
            shutil.copytree(self.seed, self.home)

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

    def test_scoped_ipv6_is_rejected_before_creating_state(self):
        path = Path(self.temp.name) / "new"
        with self.assertRaises(state.ServiceError):
            state.initialize(
                path,
                "codex-pg-unit",
                "postgres:17.11-bookworm",
                55432,
                ["fe80::1%eth0"],
            )
        self.assertFalse(path.exists())

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

    def test_non_integer_or_invalid_ports_are_rejected_before_creating_state(self):
        path = Path(self.temp.name) / "new"
        for port in (55432.0, True, None, "55432", 1023, 65536):
            with (
                self.subTest(port=port),
                self.assertRaisesRegex(state.ServiceError, "^invalid_port$"),
            ):
                state.initialize(
                    path, "codex-pg-unit", "postgres:17.11-bookworm", port, []
                )
            self.assertFalse(path.exists())

    def test_oversized_names_or_image_are_rejected_before_creating_state(self):
        path = Path(self.temp.name) / "new"
        for names, image in (
            ([f"db{i}.test" for i in range(400)], "postgres:17.11-bookworm"),
            (
                [f"n{i}." + ".".join(["a" * 60] * 3) for i in range(28)],
                "postgres:17.11-bookworm",
            ),
            ([], "postgres:17." + "1" * 70000 + "-bookworm"),
        ):
            with self.subTest(count=len(names)), self.assertRaises(state.ServiceError):
                state.initialize(path, "codex-pg-unit", image, 55432, names)
            self.assertFalse(path.exists())

    def test_missing_or_noncanonical_receipt_names_are_rejected(self):
        original = state.load(self.home)
        for names in (
            None,
            "DNS:localhost",
            [],
            original["server_names"][::-1],
            [*original["server_names"], "DNS:DB.example.test"],
        ):
            receipt = dict(original, server_names=names)
            state.publish_json(self.home / "receipt.json", receipt)
            with self.subTest(names=names), self.assertRaises(state.ServiceError):
                state.load(self.home)
            with self.assertRaises(state.ServiceError):
                state.initialize(
                    self.home,
                    "codex-pg-unit",
                    "postgres:17.11-bookworm",
                    55432,
                    ["db.example.test"],
                )
        del original["server_names"]
        state.publish_json(self.home / "receipt.json", original)
        with self.assertRaises(state.ServiceError):
            state.load(self.home)


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

    def test_insecure_receipt_permissions_are_refused(self):
        receipt = self.home / "receipt.json"
        original = receipt.read_bytes()
        for mode in (0o644, 0o660, 0o666):
            receipt.chmod(mode)
            with (
                self.subTest(mode=oct(mode)),
                self.assertRaisesRegex(state.ServiceError, "insecure_receipt_file"),
            ):
                state.load(self.home)
            self.assertEqual(original, receipt.read_bytes())

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
