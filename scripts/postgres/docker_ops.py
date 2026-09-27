"""Docker operations restricted to one receipt-bound PostgreSQL deployment."""

import hashlib
import json
import os
from pathlib import Path
import re
import secrets

from state import ServiceError, publish_json, run

INSTANCE_LABEL = "com.completedottech.codex.pg.instance"


def docker(arguments):
    # Environment interpolation must not override the authenticated local receipt.
    env = {
        k: v
        for k, v in os.environ.items()
        if not k.startswith(("COMPOSE_", "CODEX_PG_"))
    }
    return run(["docker", *arguments], env=env, timeout=300)


def engine(receipt, *, allow_unbound=False):
    if docker(["info", "--format", "{{.OSType}} "]).strip() != "linux":
        raise ServiceError("linux_containers_required")
    identity = docker(["info", "--format", "{{.ID}} "]).strip()
    if not re.fullmatch(r"[A-Za-z0-9:._-]{1,128}", identity):
        raise ServiceError("docker_engine_identity_missing")
    if not allow_unbound and receipt.get("engine_id") != identity:
        raise ServiceError("docker_engine_changed")
    return identity


def pin(path, receipt):
    identity = engine(receipt, allow_unbound=receipt["image_digest"] is None)
    if receipt["image_digest"]:
        return receipt
    docker(["pull", "--quiet", receipt["image_tag"]])
    images = json.loads(
        docker(
            [
                "image",
                "inspect",
                receipt["image_tag"],
                "--format",
                "{{json .RepoDigests}} ",
            ]
        )
    )
    digests = sorted(
        {
            value.removeprefix("docker.io/library/")
            for value in images
            if re.fullmatch(
                r"(?:docker.io/library/)?postgres@sha256:[a-f0-9]{64}", value
            )
        }
    )
    if len(digests) != 1:
        raise ServiceError("image_digest_ambiguous")
    version = docker(
        [
            "run",
            "--rm",
            "--network",
            "none",
            "--entrypoint",
            "postgres",
            digests[0],
            "--version",
        ]
    )
    expected = receipt["image_tag"].split(":")[1].removesuffix("-bookworm")
    if not re.search(r"\(PostgreSQL\) " + re.escape(expected) + r"(?:\s|$)", version):
        raise ServiceError("image_version_mismatch")
    updated = dict(receipt, image_digest=digests[0], engine_id=identity)
    publish_json(path / "receipt.json", updated)
    return updated


def inspect_owned(receipt):
    project = receipt["project"]
    ids = docker(
        ["ps", "-a", "-q", "--filter", f"label=com.docker.compose.project={project}"]
    ).split()
    for identity in ids:
        labels = json.loads(
            docker(["inspect", identity, "--format", "{{json .Config.Labels}} "])
        )
        if labels.get(INSTANCE_LABEL) != receipt["instance"]:
            raise ServiceError("foreign_project_container")
    network = project + "_storage"
    networks = docker(
        ["network", "ls", "--filter", f"name=^{network}$", "--format", "{{.Name}} "]
    ).split()
    if network in networks:
        labels = json.loads(
            docker(["network", "inspect", network, "--format", "{{json .Labels}} "])
        )
        if not labels or labels.get(INSTANCE_LABEL) != receipt["instance"]:
            raise ServiceError("foreign_project_network")


def ensure_volume(receipt):
    name = receipt["volume"]
    found = docker(
        ["volume", "ls", "--filter", f"name=^{name}$", "--format", "{{.Name}} "]
    ).split()
    if name not in found:
        docker(
            [
                "volume",
                "create",
                "--label",
                f"{INSTANCE_LABEL}={receipt['instance']}",
                name,
            ]
        )
    # Recheck after create; Docker's create can return a concurrently created volume.
    labels = json.loads(
        docker(["volume", "inspect", name, "--format", "{{json .Labels}} "])
    )
    if not labels or labels.get(INSTANCE_LABEL) != receipt["instance"]:
        raise ServiceError("foreign_data_volume")


def compose(path, receipt, arguments):
    if not receipt["image_digest"]:
        raise ServiceError("image_not_pinned")
    # Regenerate disposable configuration from the protected receipt; no passwords.
    values = {
        "CODEX_PG_IMAGE": receipt["image_digest"],
        "CODEX_PG_INSTANCE": receipt["instance"],
        "CODEX_PG_VOLUME": receipt["volume"],
        "CODEX_PG_PORT": str(receipt["port"]),
        "CODEX_PG_STATE": path.as_posix(),
        "CODEX_PG_UID": str(os.getuid() if hasattr(os, "getuid") else 1000),
        "CODEX_PG_GID": str(os.getgid() if hasattr(os, "getgid") else 1000),
    }
    from state import write_new

    pending = path / ("compose.env.pending-" + secrets.token_hex(8))
    write_new(pending, "".join(f"{k}='{v}'\n" for k, v in values.items()).encode())
    os.replace(pending, path / "compose.env")
    file = Path(__file__).with_name("compose.yaml")
    return docker(
        [
            "compose",
            "--env-file",
            str(path / "compose.env"),
            "-f",
            str(file),
            "--project-name",
            receipt["project"],
            *arguments,
        ]
    )


def restore(path, receipt, archive, expected, confirmed):
    if not confirmed:
        raise ServiceError("explicit_empty_destination_confirmation_required")
    if not re.fullmatch(r"[a-f0-9]{64}", expected or ""):
        raise ServiceError("independent_backup_digest_required")
    if (
        archive.is_symlink()
        or not archive.is_file()
        or archive.stat().st_size > 134217728
    ):
        raise ServiceError("invalid_or_oversized_backup")
    if any(c in str(archive) for c in "\r\n,$\0"):
        raise ServiceError("unsupported_backup_mount_path")
    digest = hashlib.sha256()
    with archive.open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            digest.update(chunk)
    if digest.hexdigest() != expected:
        raise ServiceError("backup_checksum_mismatch")
    try:
        return compose(
            path,
            receipt,
            [
                "run",
                "--rm",
                "--no-deps",
                "-T",
                "--volume",
                f"{archive.resolve().as_posix()}:/restore/input.dump:ro",
                "-e",
                "CONFIRM_EMPTY_DESTINATION=yes",
                "-e",
                f"EXPECTED_SHA256={expected}",
                "restore",
            ],
        )
    except ServiceError:
        # A disconnected CLI cannot prove that the server rolled back a COMMIT.
        raise ServiceError("restore_outcome_unconfirmed_inspect_destination") from None
