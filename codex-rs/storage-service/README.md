# Operating remote storage

This crate is the control plane behind `codex storage`, the app-server `storage/*` methods and
the TUI `/storage` menu. This guide is for the person who runs the PostgreSQL side and the host
that owns the Codex home.

## What is stored where

- **Local mode** keeps history in SQLite databases and rollout JSONL files under the Codex
  home. Nothing in this guide changes that mode.
- **Remote mode** keeps the same history in one PostgreSQL dataset: threads, projects, sections,
  attachments, spawn edges, rollout lines, goals, queued messages, logs, generated memories,
  the agent message board and external-import history.
- The host's authority records (`storage-identity.json` and `storage-activation.json` in the
  Codex home) say which backend is authoritative. A saved profile in `config.toml` is only a
  proposal until a verified migration is activated.

Not migrated, by policy: device-local data (remote control enrollments, backfill state), dynamic
tool rows, thread artifacts and memory file content. They stay on the host that owns them.
Rollout files are written into the destination as canonical lines; every thread is stored with
the legacy history contract, and a forked thread carries its full logical history.

## Provisioning PostgreSQL

Use PostgreSQL 17.11 with TLS. The client is qualified against exactly that server version and
refuses any other (the refusal surfaces as `connectionFailed`). Codex connects with certificate and name verification
(`verify-full`) and requires an explicit certificate authority file, so the server certificate
must carry the name or address you put in the profile.

Create three logins that never share a password:

| Role | Used for | Privileges |
| --- | --- | --- |
| `codex_migrator` | `codex storage initialize` only | member of the schema owner role |
| `codex_runtime` | every ordinary read and write | data access on the namespace tables only |
| `codex_backup` | backups | `SELECT` on the namespace |

`scripts/postgres/container/roles.sql` is the reference. The owner role owns the schema, the
runtime role has `USAGE` on it, and both set `search_path = pg_catalog, <schema>`. The runtime
role cannot create objects, cannot change the schema and cannot delete the append-only records
(project idempotency keys, queue revisions, board tombstones, external-import history).

A **named namespace** keeps several datasets in one database: the schema is
`codex_storage_<stem>` and the roles are `codex_<stem>_owner`, `codex_<stem>_migrator` and
`codex_<stem>_runtime`. The default namespace is `codex_storage`.

## Saving the profile

Put the proposal in a trusted configuration layer (the user `config.toml` or a managed layer,
never a project or session layer):

```toml
[storage_candidate]
backend = "remote_postgres"
endpoint = "db.internal"
port = 5432
database = "codex"
namespace = "codex_storage"
connect_timeout_seconds = 5
pool_acquire_timeout_seconds = 20
max_connections = 8

[storage_candidate.credential]
source = "keyring"
id = "team-postgres-runtime"

[storage_candidate.migrator_credential]
source = "keyring"
id = "team-postgres-owner"

[storage_candidate.tls]
ca_certificate = "/etc/codex/db-ca.pem"
```

Credentials are referenced, never written into the file. Store a secret with
`codex storage credential set <id>` and pipe it on standard input; it is never accepted as an
argument and never printed. An environment variable source
(`source = "environment"`, `variable = "NAME"`) also works for headless hosts.

## The usual journey

1. `codex storage check` tests the profile with the runtime login and says where it stopped:
   `profile`, `credential`, `connect`, `schema`, `dataset` or `ready`.
2. `codex storage initialize` creates or upgrades the tables with the schema-owner credential.
   Run it before any client of a newer Codex connects. Clients refuse a schema older or newer
   than the one they write, with `schemaNeedsUpgrade` or `schemaTooNew`.
3. `codex storage plan migrate` previews what would move and lists every blocker. The plan id
   changes whenever anything behind it changes, so a confirmation never starts work against a
   different world.
4. Stop every other Codex process on the host, then
   `codex storage start migrate --plan-id <id> --yes --writers-stopped`. The copy is
   checkpointed per domain and resumes after a crash. The destination stays closed to other
   writers while it runs, and the copy is verified domain by domain by digest.
5. `codex storage activate <operation-id>` makes the verified copy authoritative. The home
   records its intent, PostgreSQL publishes the new generation, and the home's records flip last.
   Add `--activate` to step 4 to do both.
6. Restart Codex on the host. It connects before it opens any local database, proves the dataset
   is the one the home published, and refuses to fall back to local files if it cannot.

The source files are never modified or deleted by a migration.

## Joining, returning, cancelling

- **Join an existing dataset** from a second host with `codex storage plan attach`, then
  `start attach --dataset-id <id>` using the dataset id from the preview. Nothing is copied and
  the host's own history stays where it is, unused. A host that already belongs to a dataset
  cannot join another.
- **Return to local files** with `plan return` and `start return`. The dataset is exported into a
  staging directory while it is closed to writers, verified, and swapped in on activation.
  Everything it replaces is moved to `storage-backups/<operation>-before-return/` with a
  manifest of file digests. The retired dataset stays in PostgreSQL, refuses writes, and any host
  still pointing at it is told so (`datasetRetired`).
- **Cancel** an operation that was not activated with `codex storage cancel`. The home stays
  authoritative and nothing is deleted. A cancelled migration leaves a partial copy closed in the
  destination, where the same source resumes it.
- **Recover** after a crash with `codex storage recover`. It reads both sides: a destination that
  already published rolls the home forward, one that did not rolls it back.

## Backups, upgrades, rotation, outages

- **Backup and restore**: back up the namespace schema with the backup role (`pg_dump -n
  codex_storage`). Restore into an empty database or namespace, run `codex storage initialize`,
  and compare the restored dataset id and generation with `codex storage status --probe` before
  pointing a host at it. A restored dataset with an older generation is reported as a mismatch,
  not used silently.
- **Schema upgrades** are explicit: `initialize` with the schema-owner credential. An upgrade is
  transactional and records the new format; clients that write an older format
  stop with `schemaNeedsUpgrade` until the host upgrades.
- **Credential rotation**: change the password in PostgreSQL and in the keyring entry. Credentials
  are resolved afresh for every connection, so running hosts pick the new one on reconnect.
- **Outages**: with PostgreSQL unreachable, a remote host reports a retryable
  `connectionFailed` or `connectionTimedOut` and writes nothing locally. Commits whose result was
  lost are reconciled by reading the destination back, not repeated blindly.
- **Rollback**: returning to local files is a data operation and uses `plan return`. Restoring an
  older binary does not roll data back: a binary that cannot read the current schema refuses to
  start rather than opening stale local files.

## Limits

- Workspace paths (`cwd`, project roots) are copied exactly as recorded. Rollout file locations
  are relocated to the layout the recorder uses in the destination home.
- The SQLite home must be the Codex home; other layouts are reported as
  `sqliteHomeDiffersFromCodexHome`.
- Storage changes are available to the host's own clients. The app-server refuses them over
  WebSocket (`storageAdminRequired`); reads are open to every client.
- Real packaged-artifact installation, update and uninstall flows, multi-OS qualification and
  performance budgets are tracked separately and are not covered by this crate.
