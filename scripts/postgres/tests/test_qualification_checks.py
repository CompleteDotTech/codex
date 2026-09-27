"""Qualification failure-path tests; real TLS sockets but no Docker claims."""

import hashlib
import json
from pathlib import Path
import socket
import ssl
import subprocess
import sys
import tempfile
import threading
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import qualification_checks as checks
from state import ServiceError, certificate_files, server_names


class QualificationChecksTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name)

    def test_negative_command_requires_exact_service_error(self):
        expected = {"error": "backup_checksum_mismatch", "codex_backend_enabled": False}
        result = subprocess.CompletedProcess([], 2, b"", json.dumps(expected).encode())
        with patch("qualification_checks.subprocess.run", return_value=result):
            self.assertEqual(
                checks.command(self.home, "restore", expected_error=expected["error"]),
                "",
            )
        for code, diagnostic in (
            (0, expected),
            (1, expected),
            (2, dict(expected, error="command_failed")),
            (2, dict(expected, codex_backend_enabled=True)),
        ):
            with (
                self.subTest(code=code, diagnostic=diagnostic),
                patch(
                    "qualification_checks.subprocess.run",
                    return_value=subprocess.CompletedProcess(
                        [], code, b"", json.dumps(diagnostic).encode()
                    ),
                ),
            ):
                with self.assertRaisesRegex(
                    ServiceError, "unexpected_integration_command_outcome"
                ):
                    checks.command(
                        self.home, "restore", expected_error=expected["error"]
                    )

    def test_backup_requires_matching_durable_receipt(self):
        backups = self.home / "backups"
        backups.mkdir()
        backup_id = "a" * 32
        archive = backups / (backup_id + ".dump")
        archive.write_bytes(b"fixture backup")
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        receipt_file = archive.with_suffix(".json")
        durable = {
            "format": 1,
            "scope": "codex_storage_schema_only",
            "instance": "instance",
            "sha256": digest,
            "bytes": archive.stat().st_size,
            "activation_permitted": False,
        }
        output = {"backup_id": backup_id, "sha256": digest, "scope": durable["scope"]}
        with (
            patch("qualification_checks.command", return_value=json.dumps(output)),
            patch("qualification_checks.load", return_value={"instance": "instance"}),
        ):
            with self.assertRaises(OSError):
                checks.checked_backup(self.home)
            receipt_file.write_text("not json")
            with self.assertRaises(ValueError):
                checks.checked_backup(self.home)
            for field, bad in (
                ("format", True),
                ("scope", "other"),
                ("instance", "other"),
                ("sha256", "0" * 64),
                ("bytes", 0),
                ("activation_permitted", True),
            ):
                with self.subTest(field=field):
                    receipt_file.write_text(json.dumps(dict(durable, **{field: bad})))
                    with self.assertRaisesRegex(
                        ServiceError, "backup_receipt_mismatch"
                    ):
                        checks.checked_backup(self.home)
            receipt_file.write_text(json.dumps(durable))
            self.assertEqual(checks.checked_backup(self.home), (output, archive))
            archive.write_bytes(b"fixture damage")
            with self.assertRaisesRegex(ServiceError, "backup_receipt_mismatch"):
                checks.checked_backup(self.home)

    def test_negative_sql_requires_permission_sqlstate_and_rolls_back(self):
        with (
            patch("qualification_checks.load", return_value={}),
            patch("qualification_checks.compose") as compose,
        ):
            for response in ("00000", "08006", "", "42501\nextra"):
                compose.return_value = response
                with (
                    self.subTest(response=response),
                    self.assertRaisesRegex(
                        ServiceError, "unexpected_qualification_sqlstate"
                    ),
                ):
                    checks.sql(
                        self.home,
                        "DELETE FROM codex_storage.roundtrip",
                        role="backup",
                        expected_sqlstate="42501",
                    )
            compose.return_value = "42501\n"
            self.assertEqual(
                checks.sql(
                    self.home,
                    "DELETE FROM codex_storage.roundtrip",
                    role="backup",
                    expected_sqlstate="42501",
                ),
                "42501",
            )
            script = next(
                arg
                for arg in compose.call_args.args[2]
                if arg.startswith("QUALIFICATION_SQL=")
            )
            self.assertIn(
                "BEGIN;\nDELETE FROM codex_storage.roundtrip;\n\\echo :SQLSTATE\nROLLBACK;",
                script,
            )
            compose.side_effect = ServiceError("network_failed")
            with self.assertRaisesRegex(ServiceError, "network_failed"):
                checks.sql(
                    self.home,
                    "DELETE FROM codex_storage.roundtrip",
                    role="backup",
                    expected_sqlstate="42501",
                )


class EndpointTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory()
        cls.home = Path(cls.temp.name)
        (cls.home / "secrets").mkdir()
        certificate_files(cls.home / "secrets", server_names([]), "openssl")
        subprocess.run(
            [
                "openssl",
                "x509",
                "-req",
                "-in",
                "server.csr",
                "-CA",
                "ca.crt",
                "-CAkey",
                "ca.key",
                "-set_serial",
                "2",
                "-days",
                "1",
                "-extfile",
                "server.ext",
                "-out",
                "renewed.crt",
            ],
            cwd=cls.home / "secrets",
            check=True,
            capture_output=True,
        )

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    def test_public_wrong_and_missing_bindings_rejected_before_connect(self):
        for bindings in (
            {},
            {"5432/tcp": [{"HostIp": "0.0.0.0", "HostPort": "55432"}]},
            {"5432/tcp": [{"HostIp": "127.0.0.1", "HostPort": "55433"}]},
        ):
            with (
                self.subTest(bindings=bindings),
                patch("qualification_checks.load", return_value={"port": 55432}),
                patch("qualification_checks.compose", return_value="a" * 64),
                patch("qualification_checks.docker", return_value=json.dumps(bindings)),
                patch("qualification_checks.socket.create_connection") as connect,
            ):
                with self.assertRaisesRegex(
                    ServiceError, "unexpected_postgres_published_endpoint"
                ):
                    checks.verify_endpoint(self.home)
                connect.assert_not_called()

    def test_endpoint_requires_active_leaf_even_when_old_leaf_is_still_valid(self):
        for served, active, rejected in (
            ("server.crt", None, False),
            ("server.crt", {"file": "renewed.crt"}, True),
            ("renewed.crt", {"file": "renewed.crt"}, False),
        ):
            with self.subTest(served=served, active=active):
                self.check_endpoint(served, active, rejected)

    def check_endpoint(self, served, active, rejected):
        listener = socket.socket()
        listener.bind(("127.0.0.1", 0))
        listener.listen(1)
        listener.settimeout(5)
        port = listener.getsockname()[1]
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        # This probe closes after TLS; avoid post-handshake TLS 1.3 ticket writes.
        context.maximum_version = ssl.TLSVersion.TLSv1_2
        context.load_cert_chain(
            str(self.home / "secrets" / served), str(self.home / "secrets/server.key")
        )
        requests = []
        errors = []

        def serve():
            try:
                with listener, listener.accept()[0] as client:
                    client.settimeout(5)
                    request = b""
                    while len(request) < 8:
                        chunk = client.recv(8 - len(request))
                        if not chunk:
                            raise RuntimeError("truncated SSLRequest")
                        request += chunk
                    requests.append(request)
                    client.sendall(b"S")
                    with context.wrap_socket(client, server_side=True):
                        pass
            except BaseException as error:
                errors.append(error)

        thread = threading.Thread(target=serve, daemon=True)
        thread.start()
        receipt = {"port": port, "active_certificate": active}
        try:
            with (
                patch("qualification_checks.load", return_value=receipt),
                patch("qualification_checks.compose", return_value="a" * 64),
                patch(
                    "qualification_checks.docker",
                    return_value=json.dumps(
                        {"5432/tcp": [{"HostIp": "127.0.0.1", "HostPort": str(port)}]}
                    ),
                ),
            ):
                if rejected:
                    with self.assertRaisesRegex(
                        ServiceError, "published_endpoint_certificate_not_active"
                    ):
                        checks.verify_endpoint(self.home)
                else:
                    checks.verify_endpoint(self.home)
        finally:
            thread.join(timeout=6)
            listener.close()
        self.assertFalse(thread.is_alive())
        self.assertEqual(errors, [])
        self.assertEqual(requests, [bytes.fromhex("0000000804d2162f")])
