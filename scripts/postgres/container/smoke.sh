#!/usr/bin/env bash
set -Eeuo pipefail
source /opt/codex-pg/client.sh
[[ ${SMOKE_ID:-} =~ ^[a-f0-9]{32}$ ]] || exit 64
table="probe_$SMOKE_ID"
query() { psql -X -qAt --no-password -v ON_ERROR_STOP=1 -c "$1"; }
client migrator
query "SET ROLE codex_owner; CREATE TABLE codex_storage.$table (id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, payload bytea NOT NULL)" >/dev/null
cleanup() {
    local result=$?
    client migrator
    query "SET ROLE codex_owner; DROP TABLE codex_storage.$table" >/dev/null 2>&1 || result=1
    exit "$result"
}
trap cleanup EXIT
client runtime
query "INSERT INTO codex_storage.$table(payload) VALUES(decode('00ff31','hex'))" >/dev/null
[[ $(query "SELECT encode(payload,'hex') FROM codex_storage.$table") == 00ff31 ]]
[[ $(query 'SELECT ssl FROM pg_stat_ssl WHERE pid=pg_backend_pid()') == t ]]
if query 'SET ROLE codex_owner' >/dev/null 2>&1; then exit 1; fi
if query 'CREATE TABLE codex_storage.forbidden_probe(id int)' >/dev/null 2>&1; then exit 1; fi
if PGSSLMODE=disable query 'SELECT 1' >/dev/null 2>&1; then exit 1; fi
address=$(getent ahostsv4 postgres | awk 'NR == 1 {print $1}')
[[ -n $address ]]
if PGHOST=not-in-certificate.invalid PGHOSTADDR="$address" query 'SELECT 1' >/dev/null 2>/tmp/tls-rejection; then exit 1; fi
grep -q 'does not match host name' /tmp/tls-rejection
printf 'postgres:5432:codex:codex_runtime:%064d\n' 0 > "$PGPASSFILE"
if query 'SELECT 1' >/dev/null 2>&1; then exit 1; fi
client runtime
if PGOPTIONS='-c statement_timeout=50' query 'SELECT pg_sleep(2)' >/dev/null 2>&1; then exit 1; fi
client backup
[[ $(query "SELECT count(*) FROM codex_storage.$table") == 1 ]]
if query "DELETE FROM codex_storage.$table" >/dev/null 2>&1; then exit 1; fi
printf '{"infrastructure_smoke":true,"codex_runtime_tested":false}\n'
