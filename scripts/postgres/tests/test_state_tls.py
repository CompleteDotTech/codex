"""Real OpenSSL verifies generated SANs, key pairing, and bounded name inputs."""

from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import state
import state_tls


class StateTlsTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory()
        cls.directory = Path(cls.temp.name) / "certificates"
        state.private_directory(cls.directory)
        state.certificate_files(
            cls.directory, state.server_names(["db.example.test"]), "openssl"
        )

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

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
                    str(self.directory / "ca.crt"),
                    option,
                    host,
                    str(self.directory / "server.crt"),
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
                str(self.directory / "ca.crt"),
                "-verify_hostname",
                "untrusted.test",
                str(self.directory / "server.crt"),
            ],
            capture_output=True,
        )
        self.assertNotEqual(result.returncode, 0)

    def test_actual_server_key_matches_certificate(self):
        key = state.run(
            ["openssl", "pkey", "-in", str(self.directory / "server.key"), "-pubout"]
        )
        cert = state.run(
            [
                "openssl",
                "x509",
                "-in",
                str(self.directory / "server.crt"),
                "-pubkey",
                "-noout",
            ]
        )
        self.assertEqual(key, cert)

    def test_hostnames_and_ip_names_are_normalized(self):
        names = state.server_names(["DB.example", "2001:db8::1", "db.example"])
        self.assertEqual(names[-2:], ["DNS:db.example", "IP:2001:db8::1"])

    def test_name_count_and_total_bytes_are_bounded(self):
        for names in (
            [f"db{i}.test" for i in range(33)],
            [f"n{i}." + ".".join(["a" * 60] * 3) for i in range(28)],
        ):
            with self.subTest(count=len(names)), self.assertRaises(state.ServiceError):
                state.server_names(names)

    def test_non_string_or_non_collection_names_are_rejected(self):
        for names in (None, "localhost", [True], [55432], [None]):
            with self.subTest(names=names), self.assertRaises(state.ServiceError):
                state.server_names(names)

    def test_receipt_names_require_canonical_types_order_and_prefixes(self):
        canonical = state.server_names(["db.example"])
        invalid = [
            None,
            "DNS:localhost",
            [],
            canonical[:-2],
            canonical + canonical[-1:],
            canonical[::-1],
            [*canonical[:-1], "DNS:DB.example"],
            [*canonical[:-1], "IP:db.example"],
            [*canonical[:-1], 123],
        ]
        for names in invalid:
            with self.subTest(names=names), self.assertRaises(state.ServiceError):
                state_tls.validate_server_names(names)
        state_tls.validate_server_names(canonical)

    def test_invalid_names_do_not_create_certificate_files(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary) / "certificates"
            state.private_directory(directory)
            with self.assertRaises(state.ServiceError):
                state.certificate_files(directory, ["DNS:injected\nname"], "openssl")
            self.assertEqual(list(directory.iterdir()), [])

    def test_large_accepted_san_list_keeps_generated_files_loadable(self):
        names = state.server_names(
            [f"n{i}." + ".".join(["a" * 60] * 3) for i in range(20)]
        )
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary) / "certificates"
            state.private_directory(directory)
            state.certificate_files(directory, names, "openssl")
            self.assertTrue(
                all(path.stat().st_size <= 16384 for path in directory.iterdir())
            )

    def test_scoped_ipv6_names_are_rejected_before_creating_files(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary) / "certificates"
            state.private_directory(directory)
            for address in ("fe80::1%eth0", "fe80::1%1"):
                with (
                    self.subTest(address=address),
                    self.assertRaises(state.ServiceError),
                ):
                    state.certificate_files(
                        directory, state.server_names([address]), "openssl"
                    )
                self.assertEqual(list(directory.iterdir()), [])

    def test_repeated_generation_preserves_every_existing_certificate(self):
        before = {path.name: path.read_bytes() for path in self.directory.iterdir()}
        with self.assertRaisesRegex(
            state.ServiceError, "certificate_outputs_already_exist"
        ):
            state.certificate_files(self.directory, state.server_names([]), "openssl")
        self.assertEqual(
            before, {path.name: path.read_bytes() for path in self.directory.iterdir()}
        )

    def test_failed_generation_cleans_staged_files_without_publishing(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary) / "certificates"
            state.private_directory(directory)
            original_run = state_tls.run

            def fail_leaf_signature(arguments):
                if "x509" in arguments:
                    raise state.ServiceError("command_failed")
                return original_run(arguments)

            with patch("state_tls.run", side_effect=fail_leaf_signature):
                with self.assertRaisesRegex(state.ServiceError, "command_failed"):
                    state.certificate_files(
                        directory, state.server_names([]), "openssl"
                    )
            self.assertEqual(list(directory.iterdir()), [])

    def test_failed_publication_removes_only_its_new_certificate_outputs(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary) / "certificates"
            state.private_directory(directory)
            write = state_tls.write_new

            def fail_ca_certificate(path, content):
                if path == directory / "ca.crt":
                    raise OSError("disk failure")
                return write(path, content)

            with patch("state_tls.write_new", side_effect=fail_ca_certificate):
                with self.assertRaises(OSError):
                    state.certificate_files(
                        directory, state.server_names([]), "openssl"
                    )
            self.assertEqual(list(directory.iterdir()), [])
            state.certificate_files(directory, state.server_names([]), "openssl")
            self.assertTrue((directory / "server.crt").is_file())
