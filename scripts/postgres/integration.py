#!/usr/bin/env python3
"""Opt-in real Docker/PostgreSQL infrastructure qualification, not Codex qualification.

Creates two uniquely labeled disposable deployments under an explicit new root.
Down preserves volumes, credentials, backups and evidence, even after failures.
"""

import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import secrets
import subprocess
import sys

from docker_ops import compose, docker
from state import ServiceError, load, publish_json, state_path


def qualify(root, port):
    root = state_path(str(root))
    if root.exists():
        raise ServiceError("integration_root_must_be_new")
    # Test engine availability before generating any state or credentials.
    docker(["info", "--format", "{{.OSType}}"])
    root.mkdir(mode=0o700)
    suffix = secrets.token_hex(5)
    homes = [root / "source", root / "destination"]
    started = []
    report = {
        "format": 1,
        "started_at": datetime.now(timezone.utc).isoformat(),
        "scope": "postgresql_infrastructure_only",
        "codex_qualified": False,
        "docker_version": docker(
            ["version", "--format", "{{.Server.Version}} "]
        ).strip(),
        "compose_version": docker(["compose", "version", "--short"]).strip(),
        "status": "running",
        "steps": [],
        "volumes_retained": [],
    }

    def command(home, *args, expect_failure=False):
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
        if (result.returncode == 0) == expect_failure:
            raise ServiceError("unexpected_integration_command_outcome")
        return result.stdout.decode()

    def record(name):
        report["steps"].append({"name": name, "passed": True})
        publish_json(root / "qualification.json", report)

    def sql(home, text, role="postgres"):
        receipt = load(home)
        if role != "postgres":
            return compose(
                home,
                receipt,
                [
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
                    'source /opt/codex-pg/client.sh; client "$QUALIFICATION_ROLE"; '
                    'exec psql -X -qAt --no-password -v ON_ERROR_STOP=1 -c "$QUALIFICATION_SQL"',
                ],
            ).strip()
        return compose(
            home,
            receipt,
            [
                "exec",
                "-T",
                "--user",
                "postgres",
                "postgres",
                "psql",
                "-X",
                "-qAt",
                "--no-password",
                "-d",
                "codex",
                "-v",
                "ON_ERROR_STOP=1",
                "-c",
                text,
            ],
        ).strip()

    try:
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
            command(home, "smoke")
            record(f"deployment_{index}_tls_roles_and_authentication")
        source, destination = homes
        sql(
            source,
            "SET ROLE codex_owner; CREATE TABLE codex_storage.roundtrip(id bigint PRIMARY KEY, value text NOT NULL); "
            "INSERT INTO codex_storage.roundtrip VALUES (1, 'first'), (2, 'second')",
        )
        backup = json.loads(command(source, "backup"))
        archive = source / "backups" / (backup["backup_id"] + ".dump")
        if hashlib.sha256(archive.read_bytes()).hexdigest() != backup["sha256"]:
            raise ServiceError("backup_receipt_mismatch")
        record("schema_backup_and_checksum")
        command(source, "down")
        command(source, "up")
        if sql(source, "SELECT count(*) FROM codex_storage.roundtrip") != "2":
            raise ServiceError("source_restart_lost_data")
        sql(
            source,
            "SET ROLE codex_owner; INSERT INTO codex_storage.roundtrip VALUES (3, 'after-restart')",
        )
        latest = json.loads(command(source, "backup"))
        latest_archive = source / "backups" / (latest["backup_id"] + ".dump")
        record("restart_recreate_volume_retention_and_new_write")
        command(
            destination,
            "restore",
            "--archive",
            str(latest_archive),
            "--sha256",
            "0" * 64,
            "--confirm-empty-destination",
            expect_failure=True,
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
        if (
            sql(
                destination,
                "SELECT string_agg(value, ',' ORDER BY id) FROM codex_storage.roundtrip",
            )
            != "first,second,after-restart"
        ):
            raise ServiceError("restored_content_mismatch")
        if (
            sql(
                destination,
                "BEGIN; UPDATE codex_storage.roundtrip SET value='runtime-updated' WHERE id=1; "
                "INSERT INTO codex_storage.roundtrip VALUES (4, 'temporary'); "
                "DELETE FROM codex_storage.roundtrip WHERE id=4; "
                "SELECT value FROM codex_storage.roundtrip WHERE id=1; ROLLBACK",
                role="runtime",
            )
            != "runtime-updated"
        ):
            raise ServiceError("restored_runtime_privileges_missing")
        if (
            sql(
                destination,
                "SELECT count(*) FROM codex_storage.roundtrip",
                role="backup",
            )
            != "3"
        ):
            raise ServiceError("restored_backup_privileges_missing")
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
            expect_failure=True,
        )
        if sql(destination, "SELECT count(*) FROM codex_storage.roundtrip") != "3":
            raise ServiceError("occupied_destination_was_changed")
        record("occupied_destination_rejected_without_overwrite")
        command(destination, "stop")
        command(destination, "up")
        if sql(destination, "SELECT count(*) FROM codex_storage.roundtrip") != "3":
            raise ServiceError("destination_restart_lost_data")
        record("destination_restart_retained_data")
        report["status"] = "passed"
    except (ServiceError, OSError, ValueError, subprocess.TimeoutExpired):
        report["status"] = "failed"
    finally:
        for home in started:
            try:
                command(home, "down")
                receipt = load(home)
                docker(
                    ["volume", "inspect", receipt["volume"], "--format", "{{.Name}}"]
                )
                report["volumes_retained"].append(receipt["volume"])
            except (ServiceError, OSError, subprocess.TimeoutExpired):
                report["status"] = "failed"
                report.setdefault("cleanup_blockers", []).append(home.name)
        report["finished_at"] = datetime.now(timezone.utc).isoformat()
        publish_json(root / "qualification.json", report)
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
