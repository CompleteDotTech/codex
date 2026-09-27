#!/usr/bin/env bash
set -Eeuo pipefail
umask 077
fail() { echo "PostgreSQL service preflight failed: $1" >&2; exit 1; }
[[ ${CODEX_PG_INSTANCE:-} =~ ^[a-f0-9]{32}$ ]] || fail instance
[[ ${PGDATA:-} == /var/lib/postgresql/data/pgdata ]] || fail data_path
# Do not start a partially initialized or foreign cluster on a subsequent restart.
if [[ -e "$PGDATA/PG_VERSION" ]]; then
    [[ $(cat "$PGDATA/PG_VERSION") == 17 ]] || fail major_version
    [[ -f "$PGDATA/.codex-service-identity" ]] || fail incomplete_bootstrap
    [[ $(cat "$PGDATA/.codex-service-identity") == "$CODEX_PG_INSTANCE" ]] || fail foreign_cluster
fi
# File-backed Compose secrets do not implement portable uid/mode remapping.
# Copy into tmpfs with the actual postgres UID, rather than weakening key modes.
for name in admin.password runtime.password migrator.password backup.password server.key server.crt ca.crt; do
    [[ -f /run/secrets/$name && ! -L /run/secrets/$name ]] || fail secret_file
    install -o postgres -g postgres -m 0600 "/run/secrets/$name" "/run/codex-pg/$name"
done
for role in admin runtime migrator backup; do
    [[ $(cat "/run/codex-pg/$role.password") =~ ^[a-f0-9]{64}$ ]] || fail credential_format
done
printf 'localhost:5432:codex:codex_runtime:%s\n' "$(cat /run/codex-pg/runtime.password)" > /run/codex-pg/health.pgpass
chown postgres:postgres /run/codex-pg/health.pgpass
chmod 0600 /run/codex-pg/health.pgpass
# init.sh must be sourced by the official entrypoint; its file mode is 0644.
exec /usr/local/bin/docker-entrypoint.sh postgres -c config_file=/opt/codex-pg/postgresql.conf
