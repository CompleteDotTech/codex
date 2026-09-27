"""Bounded certificate names and initial development certificate generation."""

import contextlib
import ipaddress
import os
import re
import secrets
import shutil

from state_io import ServiceError, run, sync_directory, write_new
from state_permissions import private_directory
from posix_io import read_private

MAX_SERVER_NAMES = 32
MAX_SAN_BYTES = 4096
DEFAULT_SERVER_NAMES = ("DNS:postgres", "DNS:localhost", "IP:127.0.0.1", "IP:::1")


def server_names(extra):
    if type(extra) not in (list, tuple) or len(extra) > MAX_SERVER_NAMES:
        raise ServiceError("too_many_or_invalid_certificate_names")
    names = list(DEFAULT_SERVER_NAMES)
    for value in extra:
        if type(value) is not str or not 0 < len(value) <= 253 or "%" in value:
            raise ServiceError("invalid_certificate_name")
        try:
            item = "IP:" + str(ipaddress.ip_address(value))
        except ValueError:
            if not all(
                re.fullmatch(r"[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?", part)
                for part in value.split(".")
            ):
                raise ServiceError("invalid_certificate_name") from None
            item = "DNS:" + value.lower()
        if item not in names:
            names.append(item)
        if (
            len(names) > MAX_SERVER_NAMES
            or len(",".join(names).encode("ascii")) > MAX_SAN_BYTES
        ):
            raise ServiceError("certificate_names_too_large")
    return names


def validate_server_names(names):
    """Require the exact normalized SAN list that initialization writes."""
    if (
        type(names) is not list
        or not len(DEFAULT_SERVER_NAMES) <= len(names) <= MAX_SERVER_NAMES
        or any(
            type(name) is not str or not name.startswith(("DNS:", "IP:"))
            for name in names
        )
    ):
        raise ServiceError("invalid_certificate_names")
    if server_names([name.split(":", 1)[1] for name in names]) != names:
        raise ServiceError("invalid_certificate_names")


def certificate_files(directory, names, openssl):
    validate_server_names(names)
    outputs = (
        "ca.key",
        "ca.crt",
        "server.key",
        "server.csr",
        "server.ext",
        "server.crt",
    )
    scope_factory = contextlib.nullcontext
    if os.name == "nt":
        from windows_state import pinned_paths

        scope_factory = pinned_paths
    with scope_factory() as parent:
        if parent is not None:
            parent.validate(directory, directory=True)
        if any(os.path.lexists(directory / name) for name in outputs):
            raise ServiceError("certificate_outputs_already_exist")
        staged = directory / (".certificate-stage-" + secrets.token_hex(16))
        private_directory(staged)
        try:
            with scope_factory() as scope:
                if scope is not None:
                    scope.validate(staged, directory=True)
                _generate_certificate_files(staged, names, openssl)
                contents = {
                    name: scope.read(staged / name, 16384)
                    if scope is not None
                    else read_private(
                        staged / name,
                        16384,
                        invalid_type="insecure_certificate_file",
                        insecure_permissions="insecure_certificate_file",
                    )
                    for name in outputs
                }
                if any(len(data) > 16384 for data in contents.values()):
                    raise ServiceError("certificate_file_too_large")
                published = []
                try:
                    for name, data in contents.items():
                        write_new(directory / name, data)
                        published.append(directory / name)
                    sync_directory(directory)
                except BaseException:
                    for item in reversed(published):
                        item.unlink()
                    sync_directory(directory)
                    raise
        finally:
            shutil.rmtree(staged)


def _generate_certificate_files(directory, names, openssl):
    # OpenSSL truncates these existing files, retaining their private Windows DACLs.
    if os.name == "nt":
        for name in ("ca.key", "ca.crt", "server.key", "server.csr", "server.crt"):
            write_new(directory / name, b"")
    # The local CA is for isolated development. Never mount its private key.
    run(
        [
            openssl,
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-sha256",
            "-keyout",
            str(directory / "ca.key"),
            "-out",
            str(directory / "ca.crt"),
            "-days",
            "3650",
            "-subj",
            "/CN=Codex-PostgreSQL-Development-CA",
            "-addext",
            "basicConstraints=critical,CA:TRUE",
            "-addext",
            "keyUsage=critical,keyCertSign,cRLSign",
        ]
    )
    run(
        [
            openssl,
            "req",
            "-new",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-sha256",
            "-keyout",
            str(directory / "server.key"),
            "-out",
            str(directory / "server.csr"),
            "-subj",
            "/CN=postgres",
        ]
    )
    extension = (
        "basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\n"
        "extendedKeyUsage=serverAuth\nsubjectAltName=" + ",".join(names) + "\n"
    )
    write_new(directory / "server.ext", extension.encode())
    run(
        [
            openssl,
            "x509",
            "-req",
            "-sha256",
            "-in",
            str(directory / "server.csr"),
            "-CA",
            str(directory / "ca.crt"),
            "-CAkey",
            str(directory / "ca.key"),
            "-set_serial",
            str(secrets.randbits(128) + 1),
            "-days",
            "90",
            "-extfile",
            str(directory / "server.ext"),
            "-out",
            str(directory / "server.crt"),
        ]
    )
    for item in directory.iterdir():
        item.chmod(0o600)
        # Windows FlushFileBuffers requires a writable handle; preserve contents.
        with item.open("r+b") as stream:
            os.fsync(stream.fileno())
    sync_directory(directory)
