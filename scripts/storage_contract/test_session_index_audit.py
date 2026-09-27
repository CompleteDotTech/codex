"""Behavior and real-process checks using disposable captures, never live homes."""

import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

from . import session_index_audit as audit
from .records import ContractError

THREAD = "01990000-0000-7000-8000-000000000001"
SCRIPT = Path(__file__).resolve().parents[1] / "audit_session_index.py"


def record(name, timestamp="unknown", **changes):
    entry = {"id": THREAD, "thread_name": name, "updated_at": timestamp}
    entry.update(changes)
    return (json.dumps(entry, ensure_ascii=False) + "\n").encode()


def verify(data, expected=None):
    return audit.verify_session_index(
        io.BytesIO(data),
        expected_sha256=expected or hashlib.sha256(data).hexdigest(),
    )


class SessionIndexAuditTests(unittest.TestCase):
    def test_repeated_names_clears_and_nonmonotonic_timestamps(self):
        rows = [record("before", "2030"), record("", "1999"), record("  after  ")]
        data = b"\n" + b"".join(rows) + b" \t\r\n"
        self.assertEqual(
            verify(data),
            {
                "format_version": 1,
                "status": "verified",
                "scope": "session_index_snapshot_structure_and_bytes",
                "activation_permitted": False,
                "raw_sha256": hashlib.sha256(data).hexdigest(),
                "source_bytes": len(data),
                "physical_lines": 5,
                "name_records": 3,
                "blank_lines": 2,
                "empty_name_records": 1,
                "largest_line_bytes": max(map(len, rows)),
            },
        )

    def test_whitespace_unicode_nul_and_unknown_timestamp_are_retained(self):
        data = record(" \u00a0\u2603\x00e\u0301 ", "unknown") + record(" \t ")
        result = verify(data)
        self.assertEqual((result["name_records"], result["empty_name_records"]), (2, 0))
        self.assertEqual(result["raw_sha256"], hashlib.sha256(data).hexdigest())

    def test_empty_capture_is_distinct_from_missing_input(self):
        result = verify(b"")
        self.assertEqual((result["source_bytes"], result["physical_lines"]), (0, 0))
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(OSError):
                audit.audit_session_index_snapshot(
                    str(Path(directory) / "missing-index"), expected_sha256="0" * 64
                )

    def test_order_is_not_reduced_to_final_names_or_counts(self):
        old = record("one") + record("two") + record("")
        new = record("two") + record("one") + record("")
        with self.assertRaisesRegex(ContractError, "source_digest_mismatch"):
            verify(new, hashlib.sha256(old).hexdigest())

    def test_equal_count_corruption_is_detected(self):
        with self.assertRaisesRegex(ContractError, "source_digest_mismatch"):
            verify(record("new"), hashlib.sha256(record("old")).hexdigest())

    def test_equivalent_json_is_not_silently_reserialized(self):
        old = record("\u2603")
        for new in (
            old.replace(b": ", b":"),
            old.replace(b"\n", b"\r\n"),
            old.replace("\u2603".encode(), b"\\u2603"),
        ):
            with self.subTest(new=new):
                self.assertEqual(json.loads(old), json.loads(new))
                with self.assertRaisesRegex(ContractError, "source_digest_mismatch"):
                    verify(new, hashlib.sha256(old).hexdigest())

    def test_unknown_fields_types_and_id_encodings_fail_closed(self):
        cases = [
            record("ok", extra="unsupported"),
            record(None),
            record("ok", id=3),
            record("ok", updated_at=False),
            record("ok", id="not-a-uuid"),
            record("ok", id=THREAD.replace("-", "")),
            record("ok", id=THREAD + " "),
            record("ok", id="A" + THREAD[1:]),
            b"[]\n",
            b"{}\n",
        ]
        for data in cases:
            with self.subTest(data=data), self.assertRaises(ContractError):
                verify(data)

    def test_malformed_lines_are_not_skipped_between_good_records(self):
        for bad in (
            b"broken\n",
            b"\xff\n",
            b'{"thread_name":NaN}\n',
            b'{"id":"x","id":"y"}\n',
            b"{" * 2000 + b"\n",
        ):
            with self.subTest(bad=bad), self.assertRaises(ContractError):
                verify(record("good") + bad + record("good"))

    def test_unpaired_surrogate_is_not_valid_rust_string_data(self):
        data = record("ok").replace(b'"ok"', b'"\\ud800"')
        with self.assertRaisesRegex(ContractError, "index_invalid_text"):
            verify(data)

    def test_partial_final_line_and_whitespace_tail_are_rejected(self):
        for data in (record("ok")[:-1], record("ok") + b" ", b"\r"):
            with self.subTest(data=data):
                with self.assertRaisesRegex(ContractError, "index_incomplete_line"):
                    verify(data)

    def test_exact_line_and_capture_limits(self):
        data = record("ok")
        with patch.object(audit, "MAX_INDEX_LINE_BYTES", len(data)):
            self.assertEqual(verify(data)["name_records"], 1)
            with self.assertRaisesRegex(ContractError, "index_line_too_large"):
                verify(data[:-1] + b" \n")
        with patch.object(audit, "MAX_INDEX_BYTES", len(data)):
            self.assertEqual(verify(data)["name_records"], 1)
            with self.assertRaisesRegex(ContractError, "index_too_large"):
                verify(data + b"\n")

    def test_actual_one_mebibyte_line_boundary(self):
        data = record("x" * (audit.MAX_INDEX_LINE_BYTES - len(record(""))))
        self.assertEqual(verify(data)["largest_line_bytes"], audit.MAX_INDEX_LINE_BYTES)
        with self.assertRaisesRegex(ContractError, "index_line_too_large"):
            verify(data[:-1] + b" \n")

    def test_reads_are_bounded_and_do_not_require_seek(self):
        class Guarded(io.BytesIO):
            def readline(self, size=-1):
                if not 0 < size <= audit.MAX_INDEX_LINE_BYTES + 1:
                    raise AssertionError("unbounded read")
                return super().readline(size)

            def read(self, size=-1):
                raise AssertionError("bulk read")

            def seek(self, *args):
                raise AssertionError("seek required")

        data = record("ok") * 2000
        result = audit.verify_session_index(
            Guarded(data), expected_sha256=hashlib.sha256(data).hexdigest()
        )
        self.assertEqual(result["name_records"], 2000)

    def test_invalid_expected_digest_is_rejected_before_read(self):
        for digest in (None, "", "G" * 64, "0" * 63, "0" * 65):
            with self.subTest(digest=digest), self.assertRaises(ContractError):
                audit.verify_session_index(None, expected_sha256=digest)


class SessionIndexCommandTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.capture = self.root / "private-path-token.jsonl"

    def run_command(self, *args):
        result = subprocess.run(
            [sys.executable, str(SCRIPT), *args],
            cwd=self.root,
            text=True,
            capture_output=True,
            timeout=10,
        )
        self.assertEqual(result.stderr, "")
        self.assertNotIn("private-path-token", result.stdout)
        self.assertNotIn("private-payload-token", result.stdout)
        return result.returncode, json.loads(result.stdout)

    def test_real_cli_is_read_only_and_redacted(self):
        data = record("private-payload-token") + record("")
        self.capture.write_bytes(data)
        sentinel = self.root / "unrelated"
        sentinel.write_text("retain me")
        code, result = self.run_command(
            "--snapshot",
            str(self.capture),
            "--expected-sha256",
            hashlib.sha256(data).hexdigest(),
        )
        self.assertEqual(
            (code, result["name_records"], result["activation_permitted"]),
            (0, 2, False),
        )
        self.assertEqual(self.capture.read_bytes(), data)
        self.assertEqual(sentinel.read_text(), "retain me")
        self.assertEqual(set(self.root.iterdir()), {self.capture, sentinel})

    def test_real_cli_equal_count_corruption_and_missing_input(self):
        self.capture.write_bytes(record("private-payload-token"))
        code, result = self.run_command(
            "--snapshot", str(self.capture), "--expected-sha256", "0" * 64
        )
        self.assertEqual((code, result["code"]), (2, "source_digest_mismatch"))
        self.capture.unlink()
        code, result = self.run_command(
            "--snapshot", str(self.capture), "--expected-sha256", "0" * 64
        )
        self.assertEqual((code, result["code"]), (3, "input_unavailable"))

    def test_cli_argument_errors_are_redacted(self):
        code, result = self.run_command("--private-payload-token", "private-path-token")
        self.assertEqual((code, result["code"]), (2, "invalid_arguments"))

    def test_directory_is_rejected_without_opening(self):
        code, result = self.run_command(
            "--snapshot", str(self.root), "--expected-sha256", "0" * 64
        )
        self.assertEqual((code, result["code"]), (2, "input_not_regular_file"))

    @unittest.skipUnless(hasattr(os, "mkfifo"), "POSIX FIFO creation unavailable")
    def test_fifo_is_rejected_without_waiting_for_a_writer(self):
        os.mkfifo(self.capture)
        code, result = self.run_command(
            "--snapshot", str(self.capture), "--expected-sha256", "0" * 64
        )
        self.assertEqual((code, result["code"]), (2, "input_not_regular_file"))

    def test_oversize_regular_capture_is_rejected_before_verification(self):
        self.capture.write_bytes(record("ok"))
        with patch.object(audit, "MAX_INDEX_BYTES", 1):
            with patch.object(audit, "verify_session_index") as verifier:
                with self.assertRaisesRegex(ContractError, "input_too_large"):
                    audit.audit_session_index_snapshot(
                        str(self.capture), expected_sha256="0" * 64
                    )
                verifier.assert_not_called()

    def test_metadata_change_before_return_is_rejected(self):
        data = record("ok")
        self.capture.write_bytes(data)
        original = audit.verify_session_index

        def changed_after_read(stream, **kwargs):
            result = original(stream, **kwargs)
            with self.capture.open("ab") as destination:
                destination.write(b"\n")
            return result

        with patch.object(audit, "verify_session_index", changed_after_read):
            with self.assertRaisesRegex(ContractError, "source_changed"):
                audit.audit_session_index_snapshot(
                    str(self.capture), expected_sha256=hashlib.sha256(data).hexdigest()
                )

    def test_real_process_capture_with_incomplete_tail_is_refused(self):
        data = record("ok") + record("private-payload-token")[:-1]
        writer = subprocess.run(
            [
                sys.executable,
                "-c",
                "import os,sys; p=sys.argv[1]; "
                "f=open(p,'wb'); f.write(sys.stdin.buffer.read()); f.flush(); "
                "os.fsync(f.fileno()); os._exit(0)",
                str(self.capture),
            ],
            input=data,
            timeout=10,
            capture_output=True,
        )
        self.assertEqual(writer.returncode, 0)
        code, result = self.run_command(
            "--snapshot",
            str(self.capture),
            "--expected-sha256",
            hashlib.sha256(data).hexdigest(),
        )
        self.assertEqual((code, result["code"]), (2, "index_incomplete_line"))
        self.assertEqual(self.capture.read_bytes(), data)
