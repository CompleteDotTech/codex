"""Real protected-metadata restore checks on two caller-owned isolated fixtures."""

import json

from qualification_checks import checked_backup, command, sql, verify_endpoint
from state import ServiceError, publish_json


def _cleanup_roles(destination):
    sql(
        destination,
        "DO $$ BEGIN "
        "IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'codex_restore_inherited') "
        "THEN REVOKE codex_restore_inherited FROM codex_runtime, codex_backup; DROP ROLE codex_restore_inherited; END IF; "
        "IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'codex_restore_assumable') "
        "THEN REVOKE codex_restore_assumable FROM codex_runtime, codex_backup; DROP ROLE codex_restore_assumable; END IF; "
        "IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'codex_restore_bridge') "
        "THEN REVOKE codex_restore_bridge FROM codex_backup; "
        "REVOKE codex_runtime FROM codex_restore_bridge; DROP ROLE codex_restore_bridge; END IF; "
        "IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'codex_restore_foreign') "
        "THEN DROP ROLE codex_restore_foreign; END IF; "
        "END $$",
    )


def qualify_restore_access(source, destination):
    """Reject drifted destination defaults, prove rollback, then retry safely."""
    failure = None
    owned_roles = []
    try:
        report = _qualify_restore_access(source, destination, owned_roles)
    except BaseException as error:
        failure = error
        raise
    finally:
        if owned_roles:
            try:
                _cleanup_roles(destination)
            except ServiceError:
                if failure is None:
                    raise
    report["safe_retry_passed"] = True
    publish_json(destination / "restore-qualification.json", report)
    return report


def _qualify_restore_access(source, destination, owned_roles):
    """Reject drifted destination defaults, prove rollback, then retry safely."""
    protected_rows = (
        "SELECT json_build_object('metadata', "
        "(SELECT json_agg(m ORDER BY singleton) FROM codex_storage.codex_schema_meta m), "
        "'history', (SELECT json_agg(h ORDER BY version) "
        "FROM codex_storage._codex_pg_migrations h))::text"
    )
    expected = json.loads(sql(source, protected_rows))
    if not expected["metadata"] or not expected["history"]:
        raise ServiceError("protected_restore_source_missing_metadata")
    sequence_name = "codex_storage.codex_restore_qualification_seq"
    if sql(source, f"SELECT to_regclass('{sequence_name}') IS NOT NULL") != "f":
        raise ServiceError("protected_restore_source_sequence_already_exists")
    sql(
        source,
        f"SET ROLE codex_owner; CREATE SEQUENCE {sequence_name}; RESET ROLE",
    )
    backup_failure = None
    try:
        backup, archive = checked_backup(source)
    except BaseException as error:
        backup_failure = error
        raise
    finally:
        try:
            sql(source, f"DROP SEQUENCE IF EXISTS {sequence_name}")
        except ServiceError:
            if backup_failure is None:
                raise
    command(destination, "up")
    verify_endpoint(destination)
    if (
        sql(
            destination,
            "SELECT count(*) FROM pg_class WHERE relnamespace = 'codex_storage'::regnamespace",
        )
        != "0"
    ):
        raise ServiceError("protected_restore_fixture_not_empty")
    if (
        sql(
            destination,
            "SELECT EXISTS (SELECT 1 FROM pg_roles WHERE rolname "
            "IN ('codex_restore_inherited', 'codex_restore_assumable', 'codex_restore_bridge', 'codex_restore_foreign'))",
        )
        != "f"
    ):
        raise ServiceError("protected_restore_fixture_roles_already_exist")
    sql(
        destination,
        "BEGIN; CREATE ROLE codex_restore_inherited; CREATE ROLE codex_restore_assumable; "
        "CREATE ROLE codex_restore_foreign; "
        "GRANT codex_restore_inherited TO codex_runtime WITH INHERIT TRUE, SET FALSE; "
        "GRANT codex_restore_assumable TO codex_runtime WITH INHERIT FALSE, SET TRUE; "
        "GRANT codex_restore_inherited TO codex_backup WITH INHERIT TRUE, SET FALSE; "
        "GRANT codex_restore_assumable TO codex_backup WITH INHERIT FALSE, SET TRUE; COMMIT",
    )
    owned_roles.append(True)
    if (
        sql(
            destination,
            "SELECT pg_has_role('codex_runtime','codex_restore_inherited','USAGE'), "
            "pg_has_role('codex_runtime','codex_restore_inherited','SET'), "
            "pg_has_role('codex_runtime','codex_restore_assumable','USAGE'), "
            "pg_has_role('codex_runtime','codex_restore_assumable','SET'), "
            "pg_has_role('codex_backup','codex_restore_inherited','USAGE'), "
            "pg_has_role('codex_backup','codex_restore_assumable','SET')",
        )
        != "t|f|f|t|t|t"
    ):
        raise ServiceError("protected_restore_fixture_memberships_incorrect")

    snapshot = (
        "SELECT json_build_object('namespace', "
        "(SELECT row_to_json(n) FROM pg_namespace n WHERE nspname='codex_storage'), "
        "'dependencies', (SELECT json_agg(d ORDER BY classid,objid,objsubid,deptype) "
        "FROM pg_depend d WHERE refclassid='pg_namespace'::regclass "
        "AND refobjid='codex_storage'::regnamespace), "
        "'defaults', (SELECT json_agg(a ORDER BY oid) FROM pg_default_acl a "
        "WHERE defaclrole='codex_owner'::regrole))::text"
    )
    restore_args = (
        "restore",
        "--archive",
        str(archive),
        "--sha256",
        backup["sha256"],
        "--confirm-empty-destination",
    )
    report = {"scope": "protected_metadata_restore_only", "steps": []}

    def reject_and_revert(name, setup, cleanup):
        sql(destination, setup)
        primary_failure = None
        try:
            before = json.loads(sql(destination, snapshot))
            command(
                destination,
                *restore_args,
                expected_error="restore_outcome_unconfirmed_inspect_destination",
            )
            if json.loads(sql(destination, snapshot)) != before:
                raise ServiceError("unsafe_restore_changed_empty_destination")
        except Exception as error:
            primary_failure = error
            raise
        finally:
            try:
                sql(destination, cleanup)
            except Exception:
                if primary_failure is None:
                    raise
        report["steps"].append({"name": name, "rejected_and_rolled_back": True})
        publish_json(destination / "restore-qualification.json", report)

    for name, grantee, privilege in (
        ("public_metadata_write", "PUBLIC", "UPDATE"),
        ("inherited_metadata_write", "codex_restore_inherited", "UPDATE"),
        ("set_role_metadata_write", "codex_restore_assumable", "UPDATE"),
        ("public_history_read", "PUBLIC", "SELECT"),
        ("backup_metadata_write", "codex_backup", "UPDATE"),
        ("backup_history_write", "codex_backup", "UPDATE"),
        ("backup_inherited_metadata_write", "codex_restore_inherited", "UPDATE"),
        ("backup_set_role_metadata_write", "codex_restore_assumable", "UPDATE"),
        ("foreign_metadata_write", "codex_restore_foreign", "UPDATE"),
    ):
        # Global ACLs survive the guard's DROP SCHEMA and affect restored tables.
        reject_and_revert(
            name,
            "ALTER DEFAULT PRIVILEGES FOR ROLE codex_owner "
            f"GRANT {privilege} ON TABLES TO {grantee}",
            "ALTER DEFAULT PRIVILEGES FOR ROLE codex_owner "
            f"REVOKE {privilege} ON TABLES FROM {grantee}",
        )

    for name, grantee in (
        ("runtime_sequence_write", "codex_runtime"),
        ("public_sequence_write", "PUBLIC"),
        ("backup_sequence_write", "codex_backup"),
        ("backup_inherited_sequence_write", "codex_restore_inherited"),
        ("backup_set_role_sequence_write", "codex_restore_assumable"),
    ):
        reject_and_revert(
            name,
            "ALTER DEFAULT PRIVILEGES FOR ROLE codex_owner "
            f"GRANT UPDATE ON SEQUENCES TO {grantee}",
            "ALTER DEFAULT PRIVILEGES FOR ROLE codex_owner "
            f"REVOKE UPDATE ON SEQUENCES FROM {grantee}",
        )

    for name, grantee in (
        ("public_schema_create", "PUBLIC"),
        ("runtime_schema_create", "codex_runtime"),
        ("backup_schema_create", "codex_backup"),
        ("inherited_schema_create", "codex_restore_inherited"),
        ("set_role_schema_create", "codex_restore_assumable"),
    ):
        reject_and_revert(
            name,
            "ALTER DEFAULT PRIVILEGES FOR ROLE codex_owner "
            f"GRANT CREATE ON SCHEMAS TO {grantee}",
            "ALTER DEFAULT PRIVILEGES FOR ROLE codex_owner "
            f"REVOKE CREATE ON SCHEMAS FROM {grantee}",
        )

    reject_and_revert(
        "backup_runtime_role_escalation",
        "GRANT codex_runtime TO codex_backup WITH INHERIT FALSE, SET TRUE",
        "REVOKE codex_runtime FROM codex_backup",
    )
    reject_and_revert(
        "backup_assumable_runtime_role_escalation",
        "BEGIN; CREATE ROLE codex_restore_bridge; "
        "GRANT codex_restore_bridge TO codex_backup WITH INHERIT FALSE, SET TRUE; "
        "GRANT codex_runtime TO codex_restore_bridge WITH INHERIT TRUE, SET FALSE; COMMIT",
        "BEGIN; REVOKE codex_restore_bridge FROM codex_backup; "
        "REVOKE codex_runtime FROM codex_restore_bridge; "
        "DROP ROLE codex_restore_bridge; COMMIT",
    )
    reject_and_revert(
        "public_function_execute_default",
        "ALTER DEFAULT PRIVILEGES FOR ROLE codex_owner GRANT EXECUTE ON FUNCTIONS TO PUBLIC",
        "ALTER DEFAULT PRIVILEGES FOR ROLE codex_owner REVOKE EXECUTE ON FUNCTIONS FROM PUBLIC",
    )
    reject_and_revert(
        "backup_global_sequence_update_default",
        "ALTER DEFAULT PRIVILEGES FOR ROLE codex_owner GRANT UPDATE ON SEQUENCES TO codex_backup",
        "ALTER DEFAULT PRIVILEGES FOR ROLE codex_owner REVOKE UPDATE ON SEQUENCES FROM codex_backup",
    )

    command(destination, *restore_args)
    verify_endpoint(destination)
    sql(destination, f"DROP SEQUENCE IF EXISTS {sequence_name}")
    schema_create_probe = (
        "SELECT EXISTS (SELECT 1 FROM pg_roles WHERE "
        "(pg_has_role('codex_runtime', oid, 'USAGE') "
        "OR pg_has_role('codex_runtime', oid, 'SET')) "
        "AND has_schema_privilege(oid, 'codex_storage', 'CREATE'))"
    )
    for grantee in ("PUBLIC", "codex_restore_inherited", "codex_restore_assumable"):
        sql(destination, f"GRANT CREATE ON SCHEMA codex_storage TO {grantee}")
        primary_failure = None
        try:
            if sql(destination, schema_create_probe) != "t":
                raise ServiceError("schema_create_effective_privilege_not_observed")
        except Exception as error:
            primary_failure = error
            raise
        finally:
            try:
                sql(
                    destination, f"REVOKE CREATE ON SCHEMA codex_storage FROM {grantee}"
                )
            except Exception:
                if primary_failure is None:
                    raise
    if json.loads(sql(destination, protected_rows)) != expected:
        raise ServiceError("protected_restore_content_mismatch")
    if (
        json.loads(
            sql(
                destination,
                "SELECT json_agg(m ORDER BY singleton) "
                "FROM codex_storage.codex_schema_meta m",
                role="runtime",
            )
        )
        != expected["metadata"]
    ):
        raise ServiceError("protected_restore_metadata_not_readable")
    if (
        json.loads(
            sql(
                destination,
                "SELECT json_agg(h ORDER BY version) "
                "FROM codex_storage._codex_pg_migrations h",
                role="backup",
            )
        )
        != expected["history"]
    ):
        raise ServiceError("protected_restore_backup_history_not_readable")
    for statement in (
        "UPDATE codex_storage.codex_schema_meta SET format_version=99",
        "SELECT version FROM codex_storage._codex_pg_migrations",
        "UPDATE codex_storage._codex_pg_migrations SET success=FALSE",
        "SET ROLE codex_restore_assumable; UPDATE codex_storage.codex_schema_meta SET format_version=99",
    ):
        sql(destination, statement, role="runtime", expected_sqlstate="42501")
    return report
