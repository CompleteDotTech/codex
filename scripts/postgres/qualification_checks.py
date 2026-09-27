"""Verified subprocess, backup and TLS probes shared by qualification steps."""

import hashlib
import json
from pathlib import Path
import re
import socket
import ssl
import struct
import subprocess
import sys

from docker_ops import compose, docker
from state import ServiceError, load


def command(home, *args, expected_error=None):
    result = subprocess.run(
        [
            sys.executable,
            str(Path(__file__).with_name("manage.py")),
            "--state",
            str(home),
            *args,
        ],
        capture_output=True,
        timeout=420,
    )
    if expected_error is not None:
        expected = {"error": expected_error, "codex_backend_enabled": False}
        if result.returncode != 2 or json.loads(result.stderr) != expected:
            raise ServiceError("unexpected_integration_command_outcome")
    elif result.returncode:
        raise ServiceError("unexpected_integration_command_outcome")
    return result.stdout.decode()


def sql(home, text, role="postgres", expected_sqlstate=None):
    text = text.rstrip("; \n") + ";\n"
    if expected_sqlstate is not None:
        # Roll back even if an unexpected grant allows the forbidden mutation.
        text = (
            "\\set ON_ERROR_STOP off\nBEGIN;\n" + text + "\\echo :SQLSTATE\nROLLBACK;\n"
        )
    receipt = load(home)
    psql = (
        "printf '%s\\n' \"$QUALIFICATION_SQL\" | "
        "psql -X -qAt --no-password -v ON_ERROR_STOP=1 -d codex"
    )
    if role == "postgres":
        args = [
            "exec",
            "-T",
            "--user",
            "postgres",
            "-e",
            "QUALIFICATION_SQL=" + text,
            "postgres",
            "/bin/bash",
            "-c",
            "set -euo pipefail; " + psql,
        ]
    else:
        args = [
            "run",
            "--rm",
            "--no-deps",
            "-T",
            "--entrypoint",
            "/bin/bash",
            "-e",
            "QUALIFICATION_SQL=" + text,
            "-e",
            "QUALIFICATION_ROLE=" + role,
            "smoke",
            "-c",
            'source /opt/codex-pg/client.sh; client "$QUALIFICATION_ROLE"; ' + psql,
        ]
    output = compose(home, receipt, args).strip()
    if expected_sqlstate is not None and output != expected_sqlstate:
        raise ServiceError("unexpected_qualification_sqlstate")
    return output


def checked_backup(home):
    backup = json.loads(command(home, "backup"))
    if (
        not isinstance(backup, dict)
        or not isinstance(backup.get("backup_id"), str)
        or not re.fullmatch(r"[a-f0-9]{32}", backup["backup_id"])
    ):
        raise ServiceError("invalid_backup_identity")
    archive = home / "backups" / (backup["backup_id"] + ".dump")
    receipt_path = archive.with_suffix(".json")
    if receipt_path.stat().st_size > 65536:
        raise ServiceError("backup_receipt_mismatch")
    durable = json.loads(receipt_path.read_bytes())
    digest = hashlib.sha256()
    with archive.open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            digest.update(chunk)
    expected = {
        "format": 1,
        "scope": "codex_storage_schema_only",
        "instance": load(home)["instance"],
        "sha256": digest.hexdigest(),
        "bytes": archive.stat().st_size,
        "activation_permitted": False,
    }
    if (
        durable != expected
        or type(durable["format"]) is not int
        or type(durable["bytes"]) is not int
        or durable["activation_permitted"] is not False
        or backup
        != {
            "backup_id": backup["backup_id"],
            "sha256": expected["sha256"],
            "scope": expected["scope"],
        }
    ):
        raise ServiceError("backup_receipt_mismatch")
    return backup, archive


def verify_endpoint(home):
    receipt = load(home)
    container = compose(home, receipt, ["ps", "--quiet", "postgres"]).strip()
    if not re.fullmatch(r"[a-f0-9]{12,64}", container):
        raise ServiceError("unexpected_postgres_container_identity")
    bindings = json.loads(
        docker(["inspect", container, "--format", "{{json .NetworkSettings.Ports}} "])
    )
    expected = {"5432/tcp": [{"HostIp": "127.0.0.1", "HostPort": str(receipt["port"])}]}
    if bindings != expected:
        raise ServiceError("unexpected_postgres_published_endpoint")
    leaf = (receipt.get("active_certificate") or {"file": "server.crt"})["file"]
    expected_der = ssl.PEM_cert_to_DER_cert((home / "secrets" / leaf).read_text())
    context = ssl.create_default_context(cafile=str(home / "secrets/ca.crt"))
    with socket.create_connection(
        ("127.0.0.1", receipt["port"]), timeout=5
    ) as connection:
        # PostgreSQL SSLRequest: read exactly the one-byte reply before TLS.
        connection.sendall(struct.pack("!II", 8, 80877103))
        if connection.recv(1) != b"S":
            raise ServiceError("published_endpoint_refused_tls")
        with context.wrap_socket(connection, server_hostname="localhost") as secured:
            if secured.getpeercert(binary_form=True) != expected_der:
                raise ServiceError("published_endpoint_certificate_not_active")
