"""Real OpenSSL renewal, including interruption and an already expired leaf."""

import hashlib
import os
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import certificates
import state
import test_state


class CertificateTests(unittest.TestCase):
    setUpClass = classmethod(test_state.StateTests.setUpClass.__func__)
    tearDownClass = classmethod(test_state.StateTests.tearDownClass.__func__)
    setUp = test_state.StateTests.setUp

    def test_renewal_preserves_identity_ca_credentials_and_key(self):
        before = state.load(self.home)
        after = certificates.renew(self.home, before)
        self.assertEqual(
            {
                k: v
                for k, v in after.items()
                if k not in ("active_certificate", "format")
            },
            {k: v for k, v in before.items() if k != "format"},
        )
        self.assertEqual(after["format"], 2)
        self.assertEqual(state.load(self.home), after)
        certificates.check_expiry(self.home, after)
        self.assertNotEqual(
            certificates.certificate_path(self.home, after),
            self.home / "secrets/server.crt",
        )
        again = certificates.renew(self.home, after)
        self.assertNotEqual(again["active_certificate"], after["active_certificate"])
        self.assertTrue(certificates.certificate_path(self.home, after).is_file())

    def test_failed_publish_keeps_previous_receipt_and_leaf(self):
        before = state.load(self.home)
        with patch.object(
            certificates, "publish_json", side_effect=OSError("disk full")
        ):
            with self.assertRaises(OSError):
                certificates.renew(self.home, before)
        self.assertEqual(state.load(self.home), before)
        certificates.check_expiry(self.home, before)
        self.assertEqual(len(list((self.home / "secrets").glob("server-*.crt"))), 1)

    def test_failed_signing_does_not_publish_an_empty_leaf(self):
        before = state.load(self.home)
        real_run = certificates.run

        def fail_signing(argv):
            if argv[1:3] == ["x509", "-req"]:
                raise state.ServiceError("command_failed")
            return real_run(argv)

        with patch.object(certificates, "run", side_effect=fail_signing):
            with self.assertRaisesRegex(state.ServiceError, "command_failed"):
                certificates.renew(self.home, before)
        self.assertEqual(state.load(self.home), before)
        self.assertEqual(list((self.home / "secrets").glob("server-*.crt")), [])

    def test_expired_leaf_can_be_renewed_without_reinitialization(self):
        before = state.load(self.home)
        directory = self.home / "secrets"
        # Explicit historical dates work across OpenSSL 3.x; negative -days does not.
        (self.home / "index").write_text("")
        (self.home / "serial").write_text("02\n")
        config = self.home / "expired.cnf"
        config.write_text("""[ca]
default_ca = issuer
[issuer]
database = $ENV::TEST_CA_ROOT/index
new_certs_dir = $ENV::TEST_CA_ROOT
serial = $ENV::TEST_CA_ROOT/serial
certificate = $ENV::TEST_CA_ROOT/secrets/ca.crt
private_key = $ENV::TEST_CA_ROOT/secrets/ca.key
default_md = sha256
policy = names
[names]
commonName = supplied
""")
        state.run(
            [
                before["openssl"],
                "ca",
                "-batch",
                "-config",
                str(config),
                "-in",
                str(directory / "server.csr"),
                "-notext",
                "-startdate",
                "20000101000000Z",
                "-enddate",
                "20010101000000Z",
                "-extfile",
                str(directory / "server.ext"),
                "-out",
                str(directory / "server.crt"),
            ],
            env=dict(os.environ, TEST_CA_ROOT=self.home.as_posix()),
        )
        before["file_hashes"]["server.crt"] = hashlib.sha256(
            (directory / "server.crt").read_bytes()
        ).hexdigest()
        state.publish_json(self.home / "receipt.json", before)
        with self.assertRaisesRegex(state.ServiceError, "renew_certificate"):
            certificates.check_expiry(self.home, before)
        after = certificates.renew(self.home, state.load(self.home))
        certificates.check_expiry(self.home, after)
        self.assertEqual(after["file_hashes"], before["file_hashes"])

    def test_corrupt_active_certificate_is_rejected_on_load(self):
        receipt = certificates.renew(self.home, state.load(self.home))
        certificates.certificate_path(self.home, receipt).write_bytes(b"changed")
        with self.assertRaisesRegex(state.ServiceError, "changed_or_corrupt"):
            state.load(self.home)

    def test_invalid_active_certificate_cannot_select_another_file(self):
        receipt = state.load(self.home)
        for active in ["bad", {}, {"file": "../ca.key", "sha256": "0" * 64}]:
            with self.subTest(active=active):
                state.publish_json(
                    self.home / "receipt.json", dict(receipt, active_certificate=active)
                )
                with self.assertRaisesRegex(
                    state.ServiceError, "invalid_active_certificate"
                ):
                    state.load(self.home)

    def test_legacy_receipt_adds_selected_openssl_without_reinitialization(self):
        before = state.load(self.home)
        legacy = dict(before)
        del legacy["openssl"]
        state.publish_json(self.home / "receipt.json", legacy)
        after = state.initialize(
            self.home,
            before["project"],
            before["image_tag"],
            before["port"],
            ["db.example.test"],
            before["openssl"],
        )
        self.assertEqual(after, before)
        self.assertEqual(state.load(self.home), before)

    def test_format_one_cannot_select_a_renewed_leaf(self):
        before = state.load(self.home)
        renewed = certificates.renew(self.home, before)
        state.publish_json(self.home / "receipt.json", dict(renewed, format=1))
        with self.assertRaisesRegex(state.ServiceError, "invalid_active_certificate"):
            state.load(self.home)

    def test_stored_openssl_is_used_for_later_checks(self):
        receipt = dict(state.load(self.home), openssl="custom executable")
        with patch.object(certificates, "run") as run:
            certificates.check_expiry(self.home, receipt)
        self.assertEqual(run.call_args.args[0][0], "custom executable")

    def test_invalid_saved_executable_is_rejected(self):
        receipt = state.load(self.home)
        for program in [None, [], "", "bad\0executable", "x" * 4097]:
            with self.subTest(program=program):
                state.publish_json(
                    self.home / "receipt.json", dict(receipt, openssl=program)
                )
                with self.assertRaisesRegex(
                    state.ServiceError, "invalid_openssl_program"
                ):
                    state.load(self.home)
