#!/usr/bin/env bash
# Source only. Credentials never enter command arguments or printed diagnostics.
set -Eeuo pipefail
umask 077
client() {
    case "$1" in runtime|migrator|backup) ;; *) return 64;; esac
    local secret
    secret=$(cat "/run/secrets/$1.password")
    [[ $secret =~ ^[a-f0-9]{64}$ ]] || return 65
    export PGUSER="codex_$1" PGPASSFILE="/tmp/$1.pgpass"
    printf 'postgres:5432:codex:%s:%s\n' "$PGUSER" "$secret" > "$PGPASSFILE"
    chmod 0600 "$PGPASSFILE"
    unset secret PGPASSWORD PGHOSTADDR PGSERVICE PGSERVICEFILE
    export PGHOST=postgres PGPORT=5432 PGDATABASE=codex PGCONNECT_TIMEOUT=5
    export PGSSLMODE=verify-full PGSSLROOTCERT=/run/secrets/ca.crt
}
