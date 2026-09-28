#!/usr/bin/env python3
"""Provision opt-in, receipt-bound roles for a two-schema PostgreSQL test."""

import argparse
import hashlib
import json
import os
import re
import secrets
import subprocess

from docker_ops import compose, engine, inspect_owned
from state import ServiceError, load, operation_lock, state_path, write_new


def _psql(container, sql):
    environment = {
        key: value
        for key, value in os.environ.items()
        if not key.startswith(("COMPOSE_", "CODEX_PG_"))
    }
    try:
        result = subprocess.run(
            [
                "docker",
                "exec",
                "-i",
                "--user",
                "postgres",
                container,
                "psql",
                "-X",
                "-qAt",
                "--no-password",
                "-v",
                "ON_ERROR_STOP=1",
                "--single-transaction",
                "-d",
                "codex",
            ],
            input=sql.encode(),
            capture_output=True,
            env=environment,
            timeout=30,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        raise ServiceError("isolation_fixture_command_unavailable") from None
    if result.returncode:
        # SQL carries generated credentials; never echo command or server output.
        raise ServiceError("isolation_fixture_sql_failed")
    return result.stdout.decode("utf-8").strip()


def provision(path):
    with operation_lock(path):
        receipt = load(path)
        engine(receipt)
        inspect_owned(receipt)
        sidecar = path / "isolation-fixture.json"
        migrator_file = path / "secrets/isolation_migrator.password"
        runtime_file = path / "secrets/isolation_runtime.password"
        if any(item.exists() for item in (sidecar, migrator_file, runtime_file)):
            raise ServiceError("isolation_fixture_must_be_new")
        container = compose(path, receipt, ["ps", "--quiet", "postgres"]).strip()
        if not re.fullmatch(r"[a-f0-9]{12,64}", container):
            raise ServiceError("isolation_fixture_server_missing")
        existing = _psql(
            container,
            "SELECT COUNT(*) FROM pg_roles WHERE rolname IN "
            "('codex_isolation_owner','codex_isolation_migrator','codex_isolation_runtime'); "
            "SELECT COUNT(*) FROM pg_namespace WHERE nspname = 'codex_storage_isolation';",
        )
        if existing != "0\n0":
            raise ServiceError("isolation_fixture_roles_or_schema_exist")

        migrator_password = secrets.token_hex(32)
        runtime_password = secrets.token_hex(32)
        write_new(migrator_file, migrator_password.encode())
        write_new(runtime_file, runtime_password.encode())
        _psql(
            container,
            "CREATE ROLE codex_isolation_owner NOLOGIN NOSUPERUSER NOCREATEDB "
            "NOCREATEROLE NOREPLICATION NOBYPASSRLS;\n"
            "CREATE ROLE codex_isolation_migrator LOGIN NOINHERIT NOSUPERUSER "
            "NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS PASSWORD '"
            + migrator_password
            + "';\n"
            "CREATE ROLE codex_isolation_runtime LOGIN NOINHERIT NOSUPERUSER "
            "NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS PASSWORD '"
            + runtime_password
            + "';\n"
            "GRANT codex_isolation_owner TO codex_isolation_migrator "
            "WITH INHERIT FALSE, SET TRUE;\n"
            "GRANT CONNECT ON DATABASE codex TO codex_isolation_migrator, "
            "codex_isolation_runtime;\n"
            "CREATE SCHEMA codex_storage_isolation AUTHORIZATION codex_isolation_owner;\n"
            "GRANT USAGE ON SCHEMA codex_storage_isolation TO codex_isolation_runtime;\n"
            "ALTER DEFAULT PRIVILEGES FOR ROLE codex_isolation_owner "
            "IN SCHEMA codex_storage_isolation GRANT SELECT, INSERT, UPDATE, DELETE "
            "ON TABLES TO codex_isolation_runtime;\n"
            "ALTER DEFAULT PRIVILEGES FOR ROLE codex_isolation_owner "
            "REVOKE EXECUTE ON FUNCTIONS FROM PUBLIC;\n",
        )
        # A crash after SQL commit leaves roles without this sidecar and fails closed.
        record = {
            "format": 1,
            "instance": receipt["instance"],
            "schema": "codex_storage_isolation",
            "migrator_sha256": hashlib.sha256(migrator_password.encode()).hexdigest(),
            "runtime_sha256": hashlib.sha256(runtime_password.encode()).hexdigest(),
            "activation_permitted": False,
        }
        write_new(sidecar, (json.dumps(record, sort_keys=True) + "\n").encode())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--state", required=True)
    args = parser.parse_args()
    try:
        provision(state_path(args.state))
    except ServiceError as error:
        parser.exit(2, json.dumps({"error": str(error)}) + "\n")
    print(json.dumps({"provisioned": True, "activation_permitted": False}))


if __name__ == "__main__":
    main()
