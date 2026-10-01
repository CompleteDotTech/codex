#!/usr/bin/env python3
"""Operate only the receipt-bound PostgreSQL service; never change Codex settings."""

import argparse
import json
from pathlib import Path
import secrets
import sys

from docker_ops import (
    compose,
    engine,
    ensure_volume,
    inspect_owned,
    pin,
    restore,
    validate_restore_archive,
)
from certificates import check_expiry, renew
from qualification_checks import verify_endpoint
from state import ServiceError, initialize, load, operation_lock, state_path


class Parser(argparse.ArgumentParser):
    def error(self, message):
        # argparse's default can echo passwords accidentally pasted as arguments.
        raise ServiceError("invalid_arguments_use_help")


def main(argv=None):
    parser = Parser(description=__doc__)
    parser.add_argument(
        "--state", required=True, help="Absolute external state directory"
    )
    sub = parser.add_subparsers(dest="action", required=True, parser_class=Parser)
    init = sub.add_parser(
        "init", help="Generate isolated development credentials and TLS"
    )
    init.add_argument("--project", required=True)
    init.add_argument("--image", default="postgres:17.11-bookworm")
    init.add_argument("--port", type=int, default=55432)
    init.add_argument("--server-name", action="append", default=[])
    init.add_argument("--openssl", default="openssl")
    for action in ("pin", "config", "up", "status", "stop", "down", "backup", "smoke"):
        sub.add_parser(action)
    sub.add_parser(
        "renew-certificate", help="Renew the leaf certificate; run up to activate it"
    )
    recovery = sub.add_parser(
        "restore", help="Trusted schema backup into an empty destination only"
    )
    recovery.add_argument("--archive", type=Path, required=True)
    recovery.add_argument("--sha256", required=True)
    recovery.add_argument("--confirm-empty-destination", action="store_true")
    try:
        args = parser.parse_args(argv)
        path = state_path(args.state)
        if args.action == "init":
            receipt = initialize(
                path,
                args.project,
                args.image,
                args.port,
                args.server_name,
                args.openssl,
            )
            result = {
                "initialized": True,
                "image_pinned": receipt["image_digest"] is not None,
            }
        else:
            with operation_lock(path):
                receipt = load(path)
                if args.action == "renew-certificate":
                    renew(path, receipt)
                    result = {"certificate_renewed": True, "run_up_to_activate": True}
                elif args.action == "pin":
                    receipt = pin(path, receipt)
                    result = {
                        "image_pinned": True,
                        "image_digest": receipt["image_digest"],
                    }
                else:
                    engine(receipt)
                    inspect_owned(receipt)
                    if args.action == "restore":
                        validate_restore_archive(
                            args.archive, args.sha256, args.confirm_empty_destination
                        )
                    if args.action in ("up", "restore"):
                        check_expiry(path, receipt)
                        ensure_volume(receipt)
                    if args.action == "restore":
                        compose(
                            path,
                            receipt,
                            ["up", "-d", "--wait", "--wait-timeout", "120", "postgres"],
                        )
                        result = restore(
                            path,
                            receipt,
                            args.archive,
                            args.sha256,
                            args.confirm_empty_destination,
                        )
                    else:
                        commands = {
                            "up": [
                                "up",
                                "-d",
                                "--wait",
                                "--wait-timeout",
                                "120",
                                "postgres",
                            ],
                            "config": ["config", "--quiet"],
                            "status": ["ps", "--format", "json"],
                            "stop": ["stop", "postgres"],
                            "down": ["down"],
                            "backup": [
                                "run",
                                "--rm",
                                "--no-deps",
                                "-T",
                                "-e",
                                "BACKUP_ID=" + secrets.token_hex(16),
                                "backup",
                            ],
                            "smoke": [
                                "run",
                                "--rm",
                                "--no-deps",
                                "-T",
                                "-e",
                                "SMOKE_ID=" + secrets.token_hex(16),
                                "smoke",
                            ],
                        }
                        output = compose(path, receipt, commands[args.action])
                        if args.action == "up":
                            verify_endpoint(path)
                        result = (
                            output
                            if args.action in ("status", "backup", "smoke")
                            else {"action": args.action, "command_succeeded": True}
                        )
            if isinstance(result, str):
                print(result.strip())
                return 0
        print(json.dumps(dict(result, codex_backend_enabled=False)))
        return 0
    except ServiceError as error:
        print(
            json.dumps({"error": str(error), "codex_backend_enabled": False}),
            file=sys.stderr,
        )
        return 2
    except (OSError, ValueError, TypeError, KeyError):
        print(
            '{"error":"invalid_state_or_io_failure","codex_backend_enabled":false}',
            file=sys.stderr,
        )
        return 2


if __name__ == "__main__":
    sys.exit(main())
