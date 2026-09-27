"""Protected, external state for the development PostgreSQL service, not Codex."""

import base64
import contextlib
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import re
import secrets
import socket
import stat
import subprocess


class ServiceError(Exception):
    """A fixed public diagnostic code; never include secret subprocess output."""


def run(argv, *, env=None, timeout=120, discard_output=False):
    try:
        result = subprocess.run(
            argv, env=env, capture_output=True, timeout=timeout, check=False
        )
    except (OSError, subprocess.TimeoutExpired):
        raise ServiceError("command_unavailable_or_timed_out") from None
    if result.returncode:
        raise ServiceError("command_failed")
    # Native Windows status output may use a legacy code page. Decode only
    # machine-readable output that callers actually consume.
    return "" if discard_output else result.stdout.decode("utf-8")


def write_new(path, data):
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
    fd = os.open(path, flags, 0o600)
    with os.fdopen(fd, "wb") as stream:
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())


def publish_json(path, data):
    pending = path.with_name(path.name + ".pending-" + secrets.token_hex(8))
    write_new(pending, (json.dumps(data, sort_keys=True, indent=2) + "\n").encode())
    os.replace(pending, path)
    sync_directory(path.parent)


def sync_directory(path):
    if os.name != "nt":
        fd = os.open(path, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
        try:
            os.fsync(fd)
        finally:
            os.close(fd)


def private_directory(path):
    path.mkdir(mode=0o700)
    if os.name == "nt":
        # Emit only the ASCII SID; whoami also emits a localized account name.
        sid = run(
            [
                "powershell.exe",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "[System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value",
            ]
        ).strip()
        if not re.fullmatch(r"S-1-[0-9-]+", sid):
            raise ServiceError("windows_identity_unresolved")
        run(
            [
                "icacls",
                str(path),
                "/inheritance:r",
                "/grant:r",
                f"*{sid}:(OI)(CI)F",
                "*S-1-5-18:(OI)(CI)F",
            ],
            discard_output=True,
        )
        _validate_windows_permissions([path])


def _validate_windows_permissions(paths):
    # Read native SIDs/ACEs rather than parsing localized icacls output. Paths are
    # passed as data, never interpolated into executable PowerShell text.
    script = """
$ErrorActionPreference = 'Stop'
$trusted = @([System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value,
             'S-1-5-18', 'S-1-5-32-544')
# OWNER RIGHTS grants apply only to the trusted owner verified below.
$grantees = $trusted + 'S-1-3-4'
foreach ($path in (ConvertFrom-Json $env:CODEX_PG_ACL_PATHS)) {
    $attributes = [System.IO.File]::GetAttributes($path)
    if ($attributes -band [System.IO.FileAttributes]::ReparsePoint) { exit 1 }
    if ($attributes -band [System.IO.FileAttributes]::Directory) {
        $acl = [System.IO.Directory]::GetAccessControl($path)
    } else {
        $acl = [System.IO.File]::GetAccessControl($path)
    }
    $descriptor = [System.Security.AccessControl.RawSecurityDescriptor]::new(
        $acl.GetSecurityDescriptorBinaryForm(), 0)
    if ($null -eq $descriptor.DiscretionaryAcl -or
        $descriptor.Owner.Value -notin $trusted) { exit 1 }
    foreach ($ace in $descriptor.DiscretionaryAcl) {
        if ($ace -isnot [System.Security.AccessControl.CommonAce] -or
            $ace.IsCallback) { exit 1 }
        if ($ace.AceQualifier -eq 'AccessAllowed' -and $ace.AccessMask -ne 0 -and
            $ace.SecurityIdentifier.Value -notin $grantees) { exit 1 }
        if ($ace.AceQualifier -notin @('AccessAllowed', 'AccessDenied')) { exit 1 }
    }
}
"""
    encoded = base64.b64encode(script.encode("utf-16le")).decode("ascii")
    env = dict(os.environ, CODEX_PG_ACL_PATHS=json.dumps([str(path) for path in paths]))
    try:
        run(
            [
                "powershell.exe",
                "-NoProfile",
                "-NonInteractive",
                "-EncodedCommand",
                encoded,
            ],
            env=env,
            discard_output=True,
        )
    except ServiceError:
        raise ServiceError(
            "insecure_or_unverifiable_windows_state_permissions"
        ) from None


def state_path(value):
    path = Path(value).expanduser()
    if not path.is_absolute() or any(c in str(path) for c in "\r\n'$\0"):
        raise ServiceError("invalid_state_path")
    # Reject symlink traversal instead of silently writing a different destination.
    if any(p.is_symlink() for p in (path, *path.parents)):
        raise ServiceError("symlink_state_path")
    path = path.resolve()
    source_root = Path(__file__).resolve().parents[2]
    if path == source_root or source_root in path.parents:
        raise ServiceError("state_must_be_outside_source_tree")
    return path


def server_names(extra):
    names = ["DNS:postgres", "DNS:localhost", "IP:127.0.0.1", "IP:::1"]
    for value in extra:
        try:
            item = "IP:" + str(ipaddress.ip_address(value))
        except ValueError:
            if len(value) > 253 or not all(
                re.fullmatch(r"[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?", x)
                for x in value.split(".")
            ):
                raise ServiceError("invalid_certificate_name") from None
            item = "DNS:" + value.lower()
        if item not in names:
            names.append(item)
    return names


def certificate_files(directory, names, openssl):
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


def initialize(path, project, image, port, names, openssl="openssl"):
    if not re.fullmatch(r"codex-pg-[a-z0-9][a-z0-9-]{0,39}", project):
        raise ServiceError("invalid_project")
    if not re.fullmatch(r"postgres:17\.[0-9]+-bookworm", image):
        raise ServiceError("only_postgres_17_bookworm_supported_here")
    if not 1024 <= port <= 65535:
        raise ServiceError("invalid_port")
    sans = server_names(names)
    if path.exists():
        saved = load(path)
        expected = (project, image, port, sans)
        if (
            saved["project"],
            saved["image_tag"],
            saved["port"],
            saved["server_names"],
        ) != expected:
            raise ServiceError("existing_state_conflict")
        return saved
    # Partial initialization is retained and rejected, never regenerated over lost credentials.
    private_directory(path)
    private_directory(path / "secrets")
    private_directory(path / "backups")
    for role in ("admin", "runtime", "migrator", "backup"):
        write_new(
            path / "secrets" / (role + ".password"), secrets.token_hex(32).encode()
        )
    certificate_files(path / "secrets", sans, openssl)
    receipt = {
        "format": 1,
        "project": project,
        "instance": secrets.token_hex(16),
        "image_tag": image,
        "image_digest": None,
        "port": port,
        "volume": project + "-pgdata",
        "server_names": sans,
        "file_hashes": {
            p.name: hashlib.sha256(p.read_bytes()).hexdigest()
            for p in (path / "secrets").iterdir()
        },
    }
    publish_json(path / "receipt.json", receipt)
    return receipt


def load(path):
    required = {
        "admin.password",
        "runtime.password",
        "migrator.password",
        "backup.password",
        "ca.key",
        "ca.crt",
        "server.key",
        "server.crt",
        "server.csr",
        "server.ext",
    }
    try:
        for folder in (path, path / "secrets", path / "backups"):
            mode = folder.lstat().st_mode
            if not stat.S_ISDIR(mode) or (os.name != "nt" and mode & 0o077):
                raise ServiceError("insecure_state_directory")
        receipt_file = path / "receipt.json"
        mode = receipt_file.lstat().st_mode
        if not stat.S_ISREG(mode) or receipt_file.stat().st_size > 65536:
            raise ServiceError("invalid_receipt")
        if os.name == "nt":
            _validate_windows_permissions(
                [
                    path,
                    path / "secrets",
                    path / "backups",
                    receipt_file,
                    *(path / "secrets" / name for name in sorted(required)),
                ]
            )
        receipt = json.loads((path / "receipt.json").read_text())
        if (
            receipt["format"] != 1
            or not re.fullmatch(r"[a-f0-9]{32}", receipt["instance"])
            or not re.fullmatch(r"codex-pg-[a-z0-9][a-z0-9-]{0,39}", receipt["project"])
            or receipt["volume"] != receipt["project"] + "-pgdata"
            or not re.fullmatch(r"postgres:17\.[0-9]+-bookworm", receipt["image_tag"])
            or type(receipt["port"]) is not int
            or not 1024 <= receipt["port"] <= 65535
        ):
            raise ServiceError("invalid_receipt")
        if set(receipt["file_hashes"]) != required:
            raise ServiceError("invalid_receipt_inventory")
        for name, expected in receipt["file_hashes"].items():
            file = path / "secrets" / name
            mode = file.lstat().st_mode
            if not stat.S_ISREG(mode) or (os.name != "nt" and mode & 0o077):
                raise ServiceError("insecure_secret_file")
            if (
                file.stat().st_size > 16384
                or hashlib.sha256(file.read_bytes()).hexdigest() != expected
            ):
                raise ServiceError("secret_changed_or_corrupt")
        digest = receipt["image_digest"]
        if digest is not None and not re.fullmatch(
            r"postgres@sha256:[a-f0-9]{64}", digest
        ):
            raise ServiceError("invalid_image_digest")
    except (OSError, ValueError, KeyError, TypeError):
        raise ServiceError("incomplete_or_invalid_state") from None
    return receipt


@contextlib.contextmanager
def operation_lock(path):
    lock = path / ".operation.lock"
    payload = json.dumps(
        {
            "pid": os.getpid(),
            "host": socket.gethostname(),
            "token": secrets.token_hex(16),
        }
    ).encode()
    try:
        write_new(lock, payload)
    except FileExistsError:
        raise ServiceError(
            "operation_locked_inspect_owner_before_manual_recovery"
        ) from None
    try:
        yield
    finally:
        mode = lock.lstat().st_mode
        if (
            not stat.S_ISREG(mode)
            or lock.stat().st_size != len(payload)
            or lock.read_bytes() != payload
        ):
            raise ServiceError("lock_ownership_changed_preserved")
        lock.unlink()
