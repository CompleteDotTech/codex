#!/usr/bin/env python3
"""Opt-in real Docker/PostgreSQL infrastructure qualification, not Codex qualification.

Creates two uniquely labeled disposable deployments under an explicit new root.
Down preserves volumes, credentials, backups and evidence, even after failures.
"""

import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import secrets
import subprocess
import sys

from docker_ops import docker
from qualification_checks import checked_backup, command, sql, verify_endpoint
from state import ServiceError, load, private_directory, publish_json, state_path


def qualify(root, port):
    root = state_path(str(root))
    if root.exists():
        raise ServiceError("integration_root_must_be_new")
    # Test engine availability before generating any state or credentials.
    if docker(["info", "--format", "{{.OSType}}"]).strip() != "linux":
        raise ServiceError("linux_containers_required")
    docker_version = docker(["version", "--format", "{{.Server.Version}} "]).strip()
    compose_version = docker(["compose", "version", "--short"]).strip()
    suffix = secrets.token_hex(5)
    homes = [root / "source", root / "destination"]
    started = []
    report = {
        "format": 1,
        "started_at": datetime.now(timezone.utc).isoformat(),
        "scope": "postgresql_infrastructure_only",
        "codex_qualified": False,
        "docker_version": docker_version,
        "compose_version": compose_version,
        "status": "running",
        "steps": [],
        "volumes_retained": [],
    }

    def record(name):
        report["steps"].append({"name": name, "passed": True})
        publish_json(root / "qualification.json", report)

    private_directory(root)
    try:
        publish_json(root / "qualification.json", report)
        for index, home in enumerate(homes):
            command(
                home,
                "init",
                "--project",
                f"codex-pg-{suffix}-{index}",
                "--port",
                str(port + index),
            )
            command(home, "pin")
            command(home, "config")
            # Keep failed up attempts in cleanup: timeout can leave a started server.
            started.append(home)
            command(home, "up")
            verify_endpoint(home)
            sql(
                home,
                "CREATE TABLE codex_storage.direct_migrator_probe(id bigint)",
                role="migrator",
                expected_sqlstate="42501",
            )
            command(home, "smoke")
            record(f"deployment_{index}_tls_roles_and_authentication")
        source, destination = homes
        sql(
            source,
            "SET ROLE codex_owner; CREATE TABLE codex_storage.roundtrip("
            "id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, value text NOT NULL); "
            "INSERT INTO codex_storage.roundtrip(value) VALUES ('first'), ('second')",
        )
        backup, archive = checked_backup(source)
        record("schema_backup_and_checksum")
        before_renewal = load(source)
        command(source, "renew-certificate")
        after_renewal = load(source)
        if (
            not after_renewal.get("active_certificate")
            or after_renewal["active_certificate"]
            == before_renewal.get("active_certificate")
            or any(
                after_renewal[key] != before_renewal[key]
                for key in ("instance", "volume", "file_hashes")
            )
        ):
            raise ServiceError("certificate_renewal_changed_deployment_identity")
        command(source, "up")
        verify_endpoint(source)
        command(source, "smoke")
        contents = (
            "SELECT json_agg(roundtrip ORDER BY id)::text FROM codex_storage.roundtrip"
        )
        expected = [{"id": 1, "value": "first"}, {"id": 2, "value": "second"}]
        if json.loads(sql(source, contents)) != expected:
            raise ServiceError("source_restart_lost_data")
        record("certificate_renewal_preserved_identity_credentials_and_data")
        command(source, "down")
        command(source, "up")
        verify_endpoint(source)
        if json.loads(sql(source, contents)) != expected:
            raise ServiceError("source_restart_lost_data")
        sql(
            source,
            "SET ROLE codex_owner; INSERT INTO codex_storage.roundtrip(value) VALUES ('after-restart')",
        )
        expected.append({"id": 3, "value": "after-restart"})
        latest, latest_archive = checked_backup(source)
        record("restart_recreate_volume_retention_and_new_write")
        command(
            destination,
            "restore",
            "--archive",
            str(latest_archive),
            "--sha256",
            "0" * 64,
            "--confirm-empty-destination",
            expected_error="backup_checksum_mismatch",
        )
        record("corrupt_digest_rejected")
        command(
            destination,
            "restore",
            "--archive",
            str(latest_archive),
            "--sha256",
            latest["sha256"],
            "--confirm-empty-destination",
        )
        if json.loads(sql(destination, contents)) != expected:
            raise ServiceError("restored_content_mismatch")
        if (
            sql(
                destination,
                "BEGIN; UPDATE codex_storage.roundtrip SET value='runtime-updated' WHERE id=1; "
                "INSERT INTO codex_storage.roundtrip(value) VALUES ('temporary') RETURNING id; "
                "DELETE FROM codex_storage.roundtrip WHERE id=4; "
                "SELECT value FROM codex_storage.roundtrip WHERE id=1; ROLLBACK",
                role="runtime",
            )
            != "4\nruntime-updated"
        ):
            raise ServiceError("restored_runtime_privileges_missing")
        if json.loads(sql(destination, contents, role="backup")) != expected:
            raise ServiceError("restored_backup_privileges_missing")
        for statement in (
            "UPDATE codex_storage.roundtrip SET value='forbidden' WHERE id=1",
            "DELETE FROM codex_storage.roundtrip WHERE id=1",
            "INSERT INTO codex_storage.roundtrip(id,value) OVERRIDING SYSTEM VALUE VALUES (1000,'forbidden')",
        ):
            sql(destination, statement, role="backup", expected_sqlstate="42501")
        command(destination, "smoke")
        record("current_backup_restored_with_runtime_privileges")
        command(
            destination,
            "restore",
            "--archive",
            str(archive),
            "--sha256",
            backup["sha256"],
            "--confirm-empty-destination",
            expected_error="restore_destination_not_empty",
        )
        if json.loads(sql(destination, contents)) != expected:
            raise ServiceError("occupied_destination_was_changed")
        record("occupied_destination_rejected_without_overwrite")
        command(destination, "stop")
        command(destination, "up")
        verify_endpoint(destination)
        if json.loads(sql(destination, contents)) != expected:
            raise ServiceError("destination_restart_lost_data")
        record("destination_restart_retained_data")
        report["status"] = "passed"
    except (ServiceError, OSError, ValueError, subprocess.TimeoutExpired):
        report["status"] = "failed"
    finally:
        interrupt = None
        if report["status"] == "running":
            report["status"] = "failed"
        try:
            for home in started:
                try:
                    command(home, "down")
                    receipt = load(home)
                    docker(
                        [
                            "volume",
                            "inspect",
                            receipt["volume"],
                            "--format",
                            "{{.Name}}",
                        ]
                    )
                    report["volumes_retained"].append(receipt["volume"])
                except KeyboardInterrupt as error:
                    if interrupt is None:
                        interrupt = error
                    report["status"] = "failed"
                    report.setdefault("cleanup_blockers", []).append(home.name)
                except (ServiceError, OSError, subprocess.TimeoutExpired):
                    report["status"] = "failed"
                    report.setdefault("cleanup_blockers", []).append(home.name)
        finally:
            if sys.exc_info()[0] is not None:
                report["status"] = "failed"
            report["finished_at"] = datetime.now(timezone.utc).isoformat()
            publish_json(root / "qualification.json", report)
            if interrupt is not None:
                raise interrupt
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--port-base", type=int, default=55432)
    args = parser.parse_args()
    try:
        if not 1024 <= args.port_base < 65535:
            raise ServiceError("invalid_port_base")
        result = qualify(args.root, args.port_base)
        print(json.dumps(result, indent=2))
        sys.exit(0 if result["status"] == "passed" else 1)
    except (ServiceError, OSError):
        print(
            '{"status":"not_run","reason":"execution_preflight_failed","codex_qualified":false}'
        )
        sys.exit(2)
