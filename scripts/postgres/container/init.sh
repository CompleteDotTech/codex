#!/usr/bin/env bash
# This script is SOURCED during first initialization by the official entrypoint.
# No dataset tables or Codex migrations are installed here.
set -Eeuo pipefail
psql -X -q --no-password --username postgres --dbname codex \
    --set ON_ERROR_STOP=1 --single-transaction --file /opt/codex-pg/roles.sql
# Commit happens before the durable marker. A crash in between fails closed.
printf '%s\n' "$CODEX_PG_INSTANCE" > "$PGDATA/.codex-service-identity.pending"
sync -f "$PGDATA/.codex-service-identity.pending"
mv "$PGDATA/.codex-service-identity.pending" "$PGDATA/.codex-service-identity"
sync -f "$PGDATA"
