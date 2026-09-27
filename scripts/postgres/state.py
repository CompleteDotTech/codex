"""Protected, external state for the development PostgreSQL service, not Codex."""

import hashlib
import json
import os
import re
import secrets
import stat

from state_io import MAX_RECEIPT_BYTES
from state_tls import validate_server_names

from state_io import ServiceError as ServiceError
from state_io import operation_lock as operation_lock
from state_io import publish_json as publish_json
from state_io import run as run
from state_io import state_path as state_path
from state_io import sync_directory as sync_directory
from state_io import write_new as write_new
from state_permissions import private_directory as private_directory
from state_permissions import (
    _validate_windows_permissions as _validate_windows_permissions,
)
from state_tls import certificate_files as certificate_files
from state_tls import server_names as server_names


def initialize(path, project, image, port, names, openssl="openssl"):
    if type(project) is not str or not re.fullmatch(
        r"codex-pg-[a-z0-9][a-z0-9-]{0,39}", project
    ):
        raise ServiceError("invalid_project")
    if (
        type(image) is not str
        or len(image) > 64
        or not re.fullmatch(r"postgres:17\.[0-9]+-bookworm", image)
    ):
        raise ServiceError("only_postgres_17_bookworm_supported_here")
    if type(port) is not int or not 1024 <= port <= 65535:
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
        if not stat.S_ISREG(mode) or receipt_file.stat().st_size > MAX_RECEIPT_BYTES:
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
        with receipt_file.open("rb") as stream:
            encoded = stream.read(MAX_RECEIPT_BYTES + 1)
        if len(encoded) > MAX_RECEIPT_BYTES:
            raise ServiceError("invalid_receipt")
        receipt = json.loads(encoded)
        if (
            type(receipt["format"]) is not int
            or receipt["format"] != 1
            or not re.fullmatch(r"[a-f0-9]{32}", receipt["instance"])
            or not re.fullmatch(r"codex-pg-[a-z0-9][a-z0-9-]{0,39}", receipt["project"])
            or receipt["volume"] != receipt["project"] + "-pgdata"
            or len(receipt["image_tag"]) > 64
            or not re.fullmatch(r"postgres:17\.[0-9]+-bookworm", receipt["image_tag"])
            or type(receipt["port"]) is not int
            or not 1024 <= receipt["port"] <= 65535
        ):
            raise ServiceError("invalid_receipt")
        validate_server_names(receipt["server_names"])
        if (
            type(receipt["file_hashes"]) is not dict
            or set(receipt["file_hashes"]) != required
        ):
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
