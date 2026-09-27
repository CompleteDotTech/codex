"""Renew development leaf certificates without replacing credentials or the CA."""

import hashlib
import os
import re
import secrets
import stat

from state import ServiceError, publish_json, run, sync_directory, write_new


def certificate_path(path, receipt):
    active = receipt.get("active_certificate")
    if active is None:
        return path / "secrets/server.crt"
    if (
        not isinstance(active, dict)
        or set(active) != {"file", "sha256"}
        or not isinstance(active["file"], str)
        or not re.fullmatch(r"server-[a-f0-9]{32}\.crt", active["file"])
        or not isinstance(active["sha256"], str)
        or not re.fullmatch(r"[a-f0-9]{64}", active["sha256"])
    ):
        raise ServiceError("invalid_active_certificate")
    file = path / "secrets" / active["file"]
    info = file.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_size > 16384:
        raise ServiceError("invalid_active_certificate")
    if os.name == "nt":
        from state import _validate_windows_permissions

        _validate_windows_permissions([file])
    elif info.st_mode & 0o077:
        raise ServiceError("insecure_secret_file")
    if hashlib.sha256(file.read_bytes()).hexdigest() != active["sha256"]:
        raise ServiceError("active_certificate_changed_or_corrupt")
    return file


def openssl_program(receipt):
    program = receipt.get("openssl", "openssl")
    if (
        not isinstance(program, str)
        or not program
        or len(program) > 4096
        or "\0" in program
    ):
        raise ServiceError("invalid_openssl_program")
    return program


def check_expiry(path, receipt):
    try:
        run(
            [
                openssl_program(receipt),
                "x509",
                "-checkend",
                "604800",
                "-noout",
                "-in",
                str(certificate_path(path, receipt)),
            ]
        )
    except ServiceError:
        raise ServiceError("certificate_check_failed_run_renew_certificate") from None


def renew(path, receipt):
    """Caller holds the operation lock and has authenticated the current receipt.

    Publish a new immutable leaf first, then atomically point the receipt at it.
    Interrupted attempts retain the old active leaf and any incomplete candidate.
    """
    program = openssl_program(receipt)
    directory = path / "secrets"
    # A leaf must not outlive its signing CA. CA replacement is a separate operation.
    try:
        run(
            [
                program,
                "x509",
                "-checkend",
                "7776000",
                "-noout",
                "-in",
                str(directory / "ca.crt"),
            ]
        )
    except ServiceError:
        raise ServiceError("ca_check_failed_manual_ca_replacement_required") from None
    file = directory / ("server-" + secrets.token_hex(16) + ".crt")
    write_new(file, b"")
    run(
        [
            program,
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
            str(file),
        ]
    )
    file.chmod(0o600)
    with file.open("r+b") as stream:
        os.fsync(stream.fileno())
    sync_directory(directory)
    updated = dict(
        receipt,
        active_certificate={
            "file": file.name,
            "sha256": hashlib.sha256(file.read_bytes()).hexdigest(),
        },
    )
    certificate_path(path, updated)
    check_expiry(path, updated)
    for name in receipt["server_names"]:
        kind, value = name.split(":", 1)
        run(
            [
                program,
                "verify",
                "-CAfile",
                str(directory / "ca.crt"),
                "-verify_ip" if kind == "IP" else "-verify_hostname",
                value,
                str(file),
            ]
        )
    key = run([program, "pkey", "-in", str(directory / "server.key"), "-pubout"])
    public = run([program, "x509", "-in", str(file), "-pubkey", "-noout"])
    if key != public:
        raise ServiceError("certificate_key_mismatch")
    publish_json(path / "receipt.json", updated)
    return updated
