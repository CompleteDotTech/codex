#!/usr/bin/env bash
set -Eeuo pipefail
source /opt/codex-pg/client.sh
client migrator
[[ ${CONFIRM_EMPTY_DESTINATION:-} == yes ]] || exit 64
[[ ${EXPECTED_SHA256:-} =~ ^[a-f0-9]{64}$ ]] || exit 64
[[ -f /restore/input.dump && ! -L /restore/input.dump ]] || exit 65
# Authenticate the private copy actually consumed, not a mutable original path.
head -c 134217729 /restore/input.dump > /tmp/input.dump
[[ $(stat -c %s /tmp/input.dump) -le 134217728 ]] || exit 65
actual=$(sha256sum /tmp/input.dump); actual=${actual%% *}
[[ $actual == "$EXPECTED_SHA256" ]] || { echo 'Backup checksum mismatch' >&2; exit 65; }
# Materialize all SQL before connecting: a truncated archive cannot commit a prefix.
# SQL expansion must fit the tool container's 512 MiB tmpfs; otherwise no mutation.
if ! pg_restore --exit-on-error --no-owner --no-privileges --no-tablespaces \
    --file=/tmp/restore.sql /tmp/input.dump 2>/tmp/restore-error; then
    echo 'Backup cannot be decoded' >&2; exit 65
fi
export PGOPTIONS='-c statement_timeout=0 -c lock_timeout=5000 -c idle_in_transaction_session_timeout=60000'
if ! psql -X -q --no-password -v ON_ERROR_STOP=1 --single-transaction \
    -f /opt/codex-pg/restore-guard.sql -f /tmp/restore.sql \
    -f /opt/codex-pg/restore-access.sql >/tmp/restore-out 2>/tmp/restore-error; then
    echo 'Restore did not report success; commit outcome may be uncertain. Inspect destination before retry' >&2
    exit 1
fi
printf '{"schema_restored":true,"codex_compatibility_verified":false,"activation_permitted":false}\n'
