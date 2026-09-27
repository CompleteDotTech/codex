#!/usr/bin/env bash
set -Eeuo pipefail
[[ -f "$PGDATA/.codex-service-identity" ]] || exit 1
[[ $(cat "$PGDATA/.codex-service-identity") == "$CODEX_PG_INSTANCE" ]] || exit 1
export PGSSLMODE=verify-full PGSSLROOTCERT=/run/codex-pg/ca.crt
export PGPASSFILE=/run/codex-pg/health.pgpass PGCONNECT_TIMEOUT=3
unset PGPASSWORD PGHOSTADDR PGSERVICE PGSERVICEFILE
exec gosu postgres psql -X -qAt --no-password -h localhost -U codex_runtime -d codex \
    -v ON_ERROR_STOP=1 -c 'SELECT 1' >/dev/null 2>&1
