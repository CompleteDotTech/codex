"""Protected, external state for the development PostgreSQL service, not Codex."""

import contextlib
import hashlib
import json
import os
import re
import secrets

from programs import resolve_program
from state_io import MAX_RECEIPT_BYTES
from posix_state import validate_directory
from posix_io import read_private
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


REQUIRED_FILES = {
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
        with operation_lock(path):
            saved = load(path)
            expected = (project, image, port, sans)
            if (
                saved["project"],
                saved["image_tag"],
                saved["port"],
                saved["server_names"],
            ) != expected:
                raise ServiceError("existing_state_conflict")
            if "openssl" not in saved:
                try:
                    program = resolve_program(openssl)
                except ServiceError:
                    raise ServiceError("openssl_unavailable") from None
                saved = dict(saved, openssl=program)
                publish_json(path / "receipt.json", saved)
            return saved
    try:
        openssl = resolve_program(openssl)
    except ServiceError:
        raise ServiceError("openssl_unavailable") from None
    # Partial initialization is retained and rejected, never regenerated over lost credentials.
    private_directory(path)
    sync_directory(path.parent)
    with contextlib.ExitStack() as stack:
        scope = None
        if os.name == "nt":
            from windows_state import pinned_paths

            scope = stack.enter_context(pinned_paths())
            scope.validate(path, directory=True)
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
            "openssl": openssl,
            "file_hashes": {
                name: hashlib.sha256(
                    scope.read(path / "secrets" / name, 16384)
                    if scope is not None
                    else read_private(
                        path / "secrets" / name,
                        16384,
                        invalid_type="insecure_secret_file",
                        insecure_permissions="insecure_secret_file",
                    )
                ).hexdigest()
                for name in sorted(REQUIRED_FILES)
            },
        }
        publish_json(path / "receipt.json", receipt)
        return receipt


def load(path):
    try:
        with contextlib.ExitStack() as stack:
            scope = None
            if os.name == "nt":
                from windows_state import pinned_paths

                scope = stack.enter_context(pinned_paths())
            for folder in (path, path / "secrets", path / "backups"):
                if scope is not None:
                    scope.validate(folder, directory=True)
                else:
                    validate_directory(folder)
            receipt_file = path / "receipt.json"
            if scope is not None:
                encoded = scope.read(receipt_file, MAX_RECEIPT_BYTES)
            else:
                encoded = read_private(
                    receipt_file,
                    MAX_RECEIPT_BYTES,
                    invalid_type="invalid_receipt",
                    insecure_permissions="insecure_receipt_file",
                )
            if len(encoded) > MAX_RECEIPT_BYTES:
                raise ServiceError("invalid_receipt")
            receipt = json.loads(encoded)
            if (
                type(receipt["format"]) is not int
                or receipt["format"] not in (1, 2)
                or not re.fullmatch(r"[a-f0-9]{32}", receipt["instance"])
                or not re.fullmatch(
                    r"codex-pg-[a-z0-9][a-z0-9-]{0,39}", receipt["project"]
                )
                or receipt["volume"] != receipt["project"] + "-pgdata"
                or len(receipt["image_tag"]) > 64
                or not re.fullmatch(
                    r"postgres:17\.[0-9]+-bookworm", receipt["image_tag"]
                )
                or type(receipt["port"]) is not int
                or not 1024 <= receipt["port"] <= 65535
            ):
                raise ServiceError("invalid_receipt")
            validate_server_names(receipt["server_names"])
            if (
                type(receipt["file_hashes"]) is not dict
                or set(receipt["file_hashes"]) != REQUIRED_FILES
            ):
                raise ServiceError("invalid_receipt_inventory")
            for name, expected in receipt["file_hashes"].items():
                file = path / "secrets" / name
                if scope is not None:
                    content = scope.read(file, 16384)
                else:
                    content = read_private(
                        file,
                        16384,
                        invalid_type="insecure_secret_file",
                        insecure_permissions="insecure_secret_file",
                    )
                if (
                    len(content) > 16384
                    or hashlib.sha256(content).hexdigest() != expected
                ):
                    raise ServiceError("secret_changed_or_corrupt")
            digest = receipt["image_digest"]
            if digest is not None and not re.fullmatch(
                r"postgres@sha256:[a-f0-9]{64}", digest
            ):
                raise ServiceError("invalid_image_digest")
            from certificates import certificate_path, openssl_program

            certificate_path(path, receipt)
            openssl_program(receipt)
    except (OSError, ValueError, KeyError, TypeError):
        raise ServiceError("incomplete_or_invalid_state") from None
    return receipt
