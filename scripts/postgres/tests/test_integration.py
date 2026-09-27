"""Qualification orchestration failure paths; no Docker evidence implied."""

import contextlib
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import integration
from state import ServiceError


class QualificationReportTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / "run"

    def test_failed_preflight_does_not_consume_root(self):
        for responses in (
            [ServiceError("no_engine")],
            ["linux", ServiceError("no_version")],
            ["linux", "version", ServiceError("no_compose")],
        ):
            with (
                self.subTest(responses=responses),
                patch("integration.docker", side_effect=responses),
            ):
                root = self.root
                with self.assertRaises(ServiceError):
                    integration.qualify(root, 55432)
                self.assertFalse(root.exists())

    def test_interrupt_and_unexpected_exception_publish_terminal_failure(self):
        for error in (KeyboardInterrupt(), KeyError("missing")):
            with self.subTest(error=type(error)), tempfile.TemporaryDirectory() as temp:
                root = Path(temp) / "run"
                with (
                    patch(
                        "integration.docker",
                        side_effect=["linux", "version", "compose"],
                    ),
                    patch("integration.command", side_effect=error),
                    self.assertRaises(type(error)),
                ):
                    integration.qualify(root, 55432)
                report = json.loads((root / "qualification.json").read_bytes())
                self.assertEqual(report["status"], "failed")
                self.assertIn("finished_at", report)

    def test_final_content_corruption_and_cleanup_interrupt_fail(self):
        for fault in ("none", "corrupt_final", "cleanup_interrupt"):
            with (
                self.subTest(fault=fault),
                tempfile.TemporaryDirectory() as temp,
                contextlib.ExitStack() as stack,
            ):
                root = Path(temp) / "run"
                renewed = False
                final_restart = False
                destination_reads = 0
                source_running = False
                source_stops = 0

                def command(home, action, *args, **kwargs):
                    nonlocal renewed, final_restart, source_running, source_stops
                    if home.name == "source":
                        if action == "up":
                            source_running = True
                        elif action == "down":
                            source_running = False
                            source_stops += 1
                    if action == "renew-certificate":
                        self.assertTrue(source_running)
                        renewed = True
                    if home.name == "destination" and action == "stop":
                        final_restart = True
                    if (
                        fault == "cleanup_interrupt"
                        and final_restart
                        and action == "down"
                    ):
                        raise KeyboardInterrupt()
                    return ""

                def query(home, text, **kwargs):
                    nonlocal destination_reads
                    if "json_agg" in text:
                        rows = [
                            {"id": 1, "value": "first"},
                            {"id": 2, "value": "second"},
                        ]
                        if home.name == "destination":
                            rows.append({"id": 3, "value": "after-restart"})
                            destination_reads += 1
                            if fault == "corrupt_final" and final_restart:
                                rows[0]["value"] = "damaged"
                        return json.dumps(rows)
                    return "4\nruntime-updated" if "RETURNING id" in text else ""

                stack.enter_context(
                    patch(
                        "integration.docker",
                        side_effect=lambda args: (
                            "linux" if args[0] == "info" else "version"
                        ),
                    )
                )
                stack.enter_context(patch("integration.command", side_effect=command))
                stack.enter_context(
                    patch(
                        "integration.load",
                        side_effect=lambda home: {
                            "instance": "instance",
                            "volume": home.name,
                            "file_hashes": {"ca.crt": "same"},
                            "active_certificate": "renewed" if renewed else None,
                        },
                    )
                )
                stack.enter_context(
                    patch(
                        "integration.checked_backup",
                        return_value=({"sha256": "a" * 64}, root / "backup.dump"),
                    )
                )
                stack.enter_context(patch("integration.sql", side_effect=query))
                stack.enter_context(patch("integration.verify_endpoint"))
                if fault == "cleanup_interrupt":
                    with self.assertRaises(KeyboardInterrupt):
                        integration.qualify(root, 55432)
                else:
                    integration.qualify(root, 55432)
                report = json.loads((root / "qualification.json").read_bytes())
                self.assertEqual(
                    report["status"], "passed" if fault == "none" else "failed"
                )
                self.assertTrue(renewed)
                self.assertGreaterEqual(source_stops, 2)
                self.assertEqual(destination_reads, 4)
                self.assertIn("finished_at", report)
