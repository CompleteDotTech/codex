"""Hostile archive fixtures with reversible changes to an isolated source."""

from contextlib import contextmanager

from qualification_checks import checked_backup, sql
from qualification_mutations import temporary_sql
from state import ServiceError


@contextmanager
def mutated_archive(source, setup, cleanup):
    """Capture committed source changes and always restore the caller's fixture."""
    with temporary_sql(
        source,
        f"SET LOCAL ROLE codex_owner; {setup}",
        f"SET LOCAL ROLE codex_owner; {cleanup}",
    ):
        yield checked_backup(source)


def qualify_archive_shapes(source, reject):
    """Exercise archive metadata guards, then an ordinary-table-only archive."""
    if (
        sql(
            source,
            "SELECT EXISTS (SELECT 1 FROM pg_class WHERE "
            "relnamespace = 'codex_storage'::regnamespace AND "
            "relname IN ('restore_shape_probe', 'restore_shape_parent', 'restore_shape_child')) "
            "OR to_regnamespace('codex_restore_shape') IS NOT NULL "
            "OR to_regtype('codex_storage.restore_shape_type') IS NOT NULL "
            "OR EXISTS (SELECT 1 FROM pg_constraint WHERE "
            "connamespace = 'codex_storage'::regnamespace AND conname IN ('restore_shape_extra', 'restore_shape_probe')) "
            "OR to_regprocedure('codex_storage.restore_shape_trigger()') IS NOT NULL",
        )
        != "f"
    ):
        raise ServiceError("protected_restore_source_probe_already_exists")

    tables = {
        "codex_schema_meta": (
            "singleton",
            "singleton boolean, format_version integer, min_reader_version integer, min_writer_version integer",
        ),
        "_codex_pg_migrations": (
            "version",
            "version bigint, description text, installed_on timestamptz, success boolean, checksum bytea, execution_time bigint",
        ),
    }
    for table, (key, columns) in tables.items():
        relation = f"codex_storage.{table}"
        for name, setup, cleanup in (
            (
                "missing_peer",
                f"ALTER TABLE {relation} RENAME TO restore_shape_probe",
                f"ALTER TABLE codex_storage.restore_shape_probe RENAME TO {table}",
            ),
            (
                "unlogged",
                f"ALTER TABLE {relation} SET UNLOGGED",
                f"ALTER TABLE {relation} SET LOGGED",
            ),
            (
                "typed_table",
                f"CREATE TYPE codex_storage.restore_shape_type AS ({columns}); "
                f"ALTER TABLE {relation} OF codex_storage.restore_shape_type",
                f"ALTER TABLE {relation} NOT OF; DROP TYPE codex_storage.restore_shape_type",
            ),
            (
                "missing_primary_key",
                f"ALTER TABLE {relation} DROP CONSTRAINT {table}_pkey",
                f"ALTER TABLE {relation} ADD CONSTRAINT {table}_pkey PRIMARY KEY ({key})",
            ),
            (
                "renamed_primary_key",
                f"ALTER TABLE {relation} RENAME CONSTRAINT {table}_pkey TO restore_shape_probe",
                f"ALTER TABLE {relation} RENAME CONSTRAINT restore_shape_probe TO {table}_pkey",
            ),
            (
                "inherits",
                "CREATE TABLE codex_storage.restore_shape_parent (); "
                f"ALTER TABLE {relation} INHERIT codex_storage.restore_shape_parent",
                f"ALTER TABLE {relation} NO INHERIT codex_storage.restore_shape_parent; "
                "DROP TABLE codex_storage.restore_shape_parent",
            ),
            (
                "inherited_by",
                f"CREATE TABLE codex_storage.restore_shape_child () INHERITS ({relation})",
                "DROP TABLE codex_storage.restore_shape_child",
            ),
            (
                "trigger",
                "CREATE FUNCTION codex_storage.restore_shape_trigger() RETURNS trigger "
                "LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$; "
                f"CREATE TRIGGER restore_shape_trigger BEFORE UPDATE ON {relation} "
                "FOR EACH ROW EXECUTE FUNCTION codex_storage.restore_shape_trigger()",
                f"DROP TRIGGER restore_shape_trigger ON {relation}; "
                "DROP FUNCTION codex_storage.restore_shape_trigger()",
            ),
            (
                "rule",
                f"CREATE RULE restore_shape_rule AS ON UPDATE TO {relation} DO INSTEAD NOTHING",
                f"DROP RULE restore_shape_rule ON {relation}",
            ),
        ):
            with mutated_archive(source, setup, cleanup) as captured:
                reject(f"{table}_{name}", "SELECT 1", "SELECT 1", captured)

    for table, name, definition in (
        ("_codex_pg_migrations", "history_extra_check", "CHECK (version > 0)"),
        ("_codex_pg_migrations", "history_extra_unique", "UNIQUE (version)"),
        ("codex_schema_meta", "metadata_extra_check", "CHECK (format_version <= 1)"),
    ):
        with mutated_archive(
            source,
            f"ALTER TABLE codex_storage.{table} ADD CONSTRAINT restore_shape_extra {definition}",
            f"ALTER TABLE codex_storage.{table} DROP CONSTRAINT restore_shape_extra",
        ) as captured:
            reject(name, "SELECT 1", "SELECT 1", captured)

    for field in ("format_version", "min_reader_version", "min_writer_version"):
        stored = sql(source, f"SELECT {field} FROM codex_storage.codex_schema_meta")
        with mutated_archive(
            source,
            f"UPDATE codex_storage.codex_schema_meta SET {field}=99",
            f"UPDATE codex_storage.codex_schema_meta SET {field}={int(stored)}",
        ) as captured:
            reject(f"incompatible_{field}", "SELECT 1", "SELECT 1", captured)

    with mutated_archive(
        source,
        "ALTER TABLE codex_storage.codex_schema_meta DROP CONSTRAINT codex_schema_meta_singleton_check",
        "ALTER TABLE codex_storage.codex_schema_meta ADD CONSTRAINT codex_schema_meta_singleton_check CHECK (singleton)",
    ) as captured:
        reject("missing_singleton_check", "SELECT 1", "SELECT 1", captured)

    with mutated_archive(
        source,
        "ALTER TABLE codex_storage.codex_schema_meta RENAME CONSTRAINT codex_schema_meta_singleton_check TO restore_shape_extra",
        "ALTER TABLE codex_storage.codex_schema_meta RENAME CONSTRAINT restore_shape_extra TO codex_schema_meta_singleton_check",
    ) as captured:
        reject("renamed_singleton_check", "SELECT 1", "SELECT 1", captured)

    # Move the two protected tables out of the captured schema temporarily so
    # these archives prove ordinary-table checks do not depend on metadata.
    with mutated_archive(
        source,
        "CREATE SCHEMA codex_restore_shape AUTHORIZATION codex_owner; "
        "ALTER TABLE codex_storage.codex_schema_meta SET SCHEMA codex_restore_shape; "
        "ALTER TABLE codex_storage._codex_pg_migrations SET SCHEMA codex_restore_shape; "
        "CREATE TABLE codex_storage.restore_shape_probe (id bigint PRIMARY KEY)",
        "DROP TABLE codex_storage.restore_shape_probe; "
        "ALTER TABLE codex_restore_shape.codex_schema_meta SET SCHEMA codex_storage; "
        "ALTER TABLE codex_restore_shape._codex_pg_migrations SET SCHEMA codex_storage; "
        "DROP SCHEMA codex_restore_shape",
    ) as captured:
        for grantee in (
            "PUBLIC",
            "codex_backup",
            "codex_restore_inherited",
            "codex_restore_assumable",
        ):
            for privilege in ("INSERT", "UPDATE", "DELETE"):
                reject(
                    f"ordinary_table_{grantee}_{privilege}",
                    "ALTER DEFAULT PRIVILEGES FOR ROLE codex_owner "
                    f"GRANT {privilege} ON TABLES TO {grantee}",
                    "ALTER DEFAULT PRIVILEGES FOR ROLE codex_owner "
                    f"REVOKE {privilege} ON TABLES FROM {grantee}",
                    captured,
                )
