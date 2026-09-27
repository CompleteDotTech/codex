"""Docker invocation contract tests; doubles are NOT real database evidence."""

import hashlib
import json
import os
import stat
from pathlib import Path
import tempfile
from types import SimpleNamespace
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import docker_ops as ops
from state import ServiceError, private_directory


class DockerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name) / "state"
        private_directory(self.home)
        self.receipt = {
            "format": 1,
            "project": "codex-pg-unit",
            "instance": "a" * 32,
            "volume": "codex-pg-unit-pgdata",
            "image_tag": "postgres:17.11-bookworm",
            "image_digest": "postgres@sha256:" + "b" * 64,
            "engine_id": "engine-one",
            "port": 55432,
        }
        self.calls = []

    def fake(self, args):
        self.calls.append(args)
        if args[:2] == ["info", "--format"]:
            return "linux" if "OSType" in args[-1] else "engine-one"
        if args[:2] == ["image", "inspect"]:
            return json.dumps([self.receipt["image_digest"]])
        if args[0] == "run":
            return "postgres (PostgreSQL) 17.11 (Debian)\n"
        if args[:2] == ["volume", "inspect"]:
            return json.dumps({ops.INSTANCE_LABEL: self.receipt["instance"]})
        if args[:2] == ["network", "inspect"] or args[0] == "inspect":
            return json.dumps({ops.INSTANCE_LABEL: self.receipt["instance"]})
        return ""

    def test_engine_switch_is_rejected(self):
        with patch.object(ops, "docker", side_effect=self.fake):
            with self.assertRaisesRegex(ServiceError, "docker_engine_changed"):
                ops.engine(dict(self.receipt, engine_id="different"))

    def test_windows_container_engine_is_rejected(self):
        with (
            patch.object(ops, "docker", return_value="windows"),
            self.assertRaisesRegex(ServiceError, "linux_containers_required"),
        ):
            ops.engine(self.receipt)

    def test_pin_records_resolved_digest_and_engine(self):
        with patch.object(ops, "docker", side_effect=self.fake):
            result = ops.pin(self.home, dict(self.receipt, image_digest=None))
        self.assertEqual(result, self.receipt)
        self.assertEqual(
            json.loads((self.home / "receipt.json").read_text()), self.receipt
        )
        self.assertIn(["pull", "--quiet", "postgres:17.11-bookworm"], self.calls)
        self.assertIn("--network", next(c for c in self.calls if c[0] == "run"))

    def test_repeat_pin_does_not_pull_or_change_receipt(self):
        with patch.object(ops, "docker", side_effect=self.fake):
            self.assertEqual(ops.pin(self.home, self.receipt), self.receipt)
        self.assertFalse((self.home / "receipt.json").exists())
        self.assertFalse(any(c[0] in ("pull", "run") for c in self.calls))

    def test_failed_pull_does_not_publish_digest(self):
        def fail(args):
            if args[0] == "pull":
                raise ServiceError("command_failed")
            return self.fake(args)

        with (
            patch.object(ops, "docker", side_effect=fail),
            self.assertRaises(ServiceError),
        ):
            ops.pin(self.home, dict(self.receipt, image_digest=None))
        self.assertFalse((self.home / "receipt.json").exists())

    def test_wrong_image_version_does_not_publish_digest(self):
        def wrong(args):
            return "postgres (PostgreSQL) 18.6" if args[0] == "run" else self.fake(args)

        with (
            patch.object(ops, "docker", side_effect=wrong),
            self.assertRaisesRegex(ServiceError, "image_version_mismatch"),
        ):
            ops.pin(self.home, dict(self.receipt, image_digest=None))
        self.assertFalse((self.home / "receipt.json").exists())

    def test_multiple_or_foreign_image_digests_are_refused(self):
        for values in [
            ["evil@sha256:" + "b" * 64],
            [self.receipt["image_digest"], "postgres@sha256:" + "c" * 64],
        ]:

            def bad(args):
                return (
                    json.dumps(values)
                    if args[:2] == ["image", "inspect"]
                    else self.fake(args)
                )

            with (
                patch.object(ops, "docker", side_effect=bad),
                self.assertRaisesRegex(ServiceError, "image_digest_ambiguous"),
            ):
                ops.pin(self.home, dict(self.receipt, image_digest=None))

    def test_new_volume_is_labeled_and_rechecked(self):
        with patch.object(ops, "docker", side_effect=self.fake):
            ops.ensure_volume(self.receipt)
        self.assertEqual(
            [x[:2] for x in self.calls],
            [["volume", "ls"], ["volume", "create"], ["volume", "inspect"]],
        )

    def test_existing_unowned_volume_is_not_modified(self):
        def foreign(args):
            if args[:2] == ["volume", "ls"]:
                self.calls.append(args)
                return self.receipt["volume"]
            if args[:2] == ["volume", "inspect"]:
                return json.dumps({ops.INSTANCE_LABEL: "different"})
            return self.fake(args)

        with (
            patch.object(ops, "docker", side_effect=foreign),
            self.assertRaisesRegex(ServiceError, "foreign_data_volume"),
        ):
            ops.ensure_volume(self.receipt)
        self.assertFalse(any(c[:2] == ["volume", "create"] for c in self.calls))

    def test_volume_create_race_is_checked(self):
        def race(args):
            return "{}" if args[:2] == ["volume", "inspect"] else self.fake(args)

        with (
            patch.object(ops, "docker", side_effect=race),
            self.assertRaisesRegex(ServiceError, "foreign_data_volume"),
        ):
            ops.ensure_volume(self.receipt)

    def test_other_project_containers_block_operations(self):
        def foreign(args):
            if args[0] == "ps":
                return "container-one"
            if args[0] == "inspect":
                return "{}"
            return self.fake(args)

        with (
            patch.object(ops, "docker", side_effect=foreign),
            self.assertRaisesRegex(ServiceError, "foreign_project_container"),
        ):
            ops.inspect_owned(self.receipt)

    def test_foreign_network_blocks_operations(self):
        def foreign(args):
            if args[:2] == ["network", "ls"]:
                return "codex-pg-unit_storage"
            if args[:2] == ["network", "inspect"]:
                return "null"
            return self.fake(args)

        with (
            patch.object(ops, "docker", side_effect=foreign),
            self.assertRaisesRegex(ServiceError, "foreign_project_network"),
        ):
            ops.inspect_owned(self.receipt)

    def test_compose_is_pinned_and_uses_external_nonsecret_environment(self):
        with patch.object(ops, "docker", side_effect=self.fake):
            ops.compose(self.home, self.receipt, ["down"])
        command = self.calls[-1]
        self.assertEqual(command[-3:], ["--project-name", "codex-pg-unit", "down"])
        self.assertNotIn("--volumes", command)
        text = (self.home / "compose.env").read_text()
        self.assertIn(self.receipt["image_digest"], text)
        self.assertNotIn("PASSWORD", text)
        self.assertIn(self.home.as_posix(), text)

    def test_unpinned_compose_is_rejected_before_docker(self):
        with (
            patch.object(ops, "docker") as execute,
            self.assertRaisesRegex(ServiceError, "image_not_pinned"),
        ):
            ops.compose(self.home, dict(self.receipt, image_digest=None), ["up"])
        execute.assert_not_called()

    def test_ambient_compose_and_pg_overrides_removed(self):
        variables = {
            "PATH": "path",
            "DOCKER_HOST": "host",
            "COMPOSE_FILE": "foreign",
            "CODEX_PG_IMAGE": "bad",
        }
        with (
            patch.object(ops.os, "environ", variables),
            patch.object(ops, "run", return_value="") as execute,
        ):
            ops.docker(["version"])
        self.assertEqual(
            execute.call_args.kwargs["env"], {"PATH": "path", "DOCKER_HOST": "host"}
        )

    def test_restore_needs_explicit_confirmation(self):
        with (
            patch.object(ops, "compose") as execute,
            self.assertRaisesRegex(ServiceError, "confirmation_required"),
        ):
            ops.restore(self.home, self.receipt, self.home / "none", "a" * 64, False)
        execute.assert_not_called()

    def test_corrupt_backup_never_connects(self):
        file = self.home / "input.dump"
        file.write_bytes(b"corrupt")
        with (
            patch.object(ops, "compose") as execute,
            self.assertRaisesRegex(ServiceError, "backup_checksum_mismatch"),
        ):
            ops.restore(self.home, self.receipt, file, "a" * 64, True)
        execute.assert_not_called()

    def test_backup_growth_is_rejected_while_hashing(self):
        file = self.home / "input.dump"
        file.write_bytes(b"growth")
        with (
            patch.object(ops, "MAX_BACKUP_BYTES", 4),
            patch.object(
                ops.os,
                "fstat",
                return_value=SimpleNamespace(st_mode=stat.S_IFREG, st_size=0),
            ),
            patch.object(ops, "compose") as execute,
            self.assertRaisesRegex(ServiceError, "invalid_or_oversized_backup"),
        ):
            ops.restore(self.home, self.receipt, file, "a" * 64, True)
        execute.assert_not_called()

    @unittest.skipUnless(hasattr(os, "mkfifo"), "POSIX FIFO required")
    def test_fifo_archive_is_rejected_without_waiting_for_a_writer(self):
        file = self.home / "input.dump"
        os.mkfifo(file)
        with (
            patch.object(Path, "is_file", return_value=True),
            self.assertRaisesRegex(ServiceError, "invalid_or_oversized_backup"),
        ):
            ops.validate_restore_archive(file, "a" * 64, True)

    def test_unpinned_restore_reports_pin_required_before_compose(self):
        file = self.home / "input.dump"
        file.write_bytes(b"fixture archive")
        digest = hashlib.sha256(file.read_bytes()).hexdigest()
        with (
            patch.object(ops, "compose") as execute,
            self.assertRaisesRegex(ServiceError, "image_not_pinned"),
        ):
            ops.restore(
                self.home, dict(self.receipt, image_digest=None), file, digest, True
            )
        execute.assert_not_called()

    def test_validated_backup_mount_is_read_only(self):
        file = self.home / "input.dump"
        file.write_bytes(b"fixture archive, not a real pg_dump")
        digest = hashlib.sha256(file.read_bytes()).hexdigest()
        with patch.object(ops, "compose", return_value="OK") as execute:
            self.assertEqual(
                ops.restore(self.home, self.receipt, file, digest, True), "OK"
            )
        argv = execute.call_args.args[2]
        self.assertIn(file.resolve().as_posix() + ":/restore/input.dump:ro", argv)
        self.assertIn("--no-deps", argv)
        self.assertIn("EXPECTED_SHA256=" + digest, argv)
        self.assertEqual(execute.call_args.kwargs["timeout"], 3600)

    def test_failed_remote_restore_does_not_claim_rollback(self):
        file = self.home / "input.dump"
        file.write_bytes(b"fixture archive")
        digest = hashlib.sha256(file.read_bytes()).hexdigest()
        with patch.object(ops, "compose", side_effect=ServiceError("command_failed")):
            with self.assertRaisesRegex(
                ServiceError, "restore_outcome_unconfirmed_inspect_destination"
            ):
                ops.restore(self.home, self.receipt, file, digest, True)

    def test_confirmed_guard_refusal_has_a_distinct_error(self):
        file = self.home / "input.dump"
        file.write_bytes(b"fixture archive")
        digest = hashlib.sha256(file.read_bytes()).hexdigest()
        with patch.object(
            ops, "compose", return_value='{"error":"restore_destination_not_empty"}\n'
        ):
            with self.assertRaisesRegex(
                ServiceError, "^restore_destination_not_empty$"
            ):
                ops.restore(self.home, self.receipt, file, digest, True)
