#!/usr/bin/env bash
set -Eeuo pipefail
source /opt/codex-pg/client.sh
client backup
[[ ${BACKUP_ID:-} =~ ^[a-f0-9]{32}$ ]] || exit 64
[[ ${CODEX_PG_INSTANCE:-} =~ ^[a-f0-9]{32}$ ]] || exit 64
[[ ${CODEX_PG_UID:-} =~ ^[0-9]+$ && ${CODEX_PG_GID:-} =~ ^[0-9]+$ ]] || exit 64
base="/backups/$BACKUP_ID"
[[ ! -e $base.dump && ! -e $base.json && ! -e $base.partial ]] || exit 73
# An interrupted attempt leaves a clearly incomplete .partial file, never a receipt.
set -o noclobber
exec 3> "$base.partial"
if ! pg_dump --no-password --format=custom --schema=codex_storage \
    --no-owner --no-privileges --lock-wait-timeout=5s >&3 2>/tmp/dump-error; then
    echo 'PostgreSQL schema backup failed; partial artifact retained' >&2
    exit 1
fi
exec 3>&-
pg_restore --list "$base.partial" >/dev/null 2>>/tmp/dump-error || exit 1
digest=$(sha256sum "$base.partial"); digest=${digest%% *}
bytes=$(stat -c %s "$base.partial")
if (( bytes > 134217728 )); then
    echo 'PostgreSQL schema backup exceeds the restore size limit; partial artifact retained' >&2
    exit 1
fi
sync -f "$base.partial"
# Hard-link publication fails on any name conflict without overwriting data.
ln "$base.partial" "$base.dump"
rm "$base.partial"
printf '{"format":1,"scope":"codex_storage_schema_only","instance":"%s","sha256":"%s","bytes":%s,"activation_permitted":false}\n' \
    "$CODEX_PG_INSTANCE" "$digest" "$bytes" > "$base.json.partial"
sync -f "$base.json.partial"
ln "$base.json.partial" "$base.json"
rm "$base.json.partial"
chown "$CODEX_PG_UID:$CODEX_PG_GID" "$base.dump" "$base.json"
sync -f /backups
printf '{"backup_id":"%s","sha256":"%s","scope":"codex_storage_schema_only"}\n' "$BACKUP_ID" "$digest"
