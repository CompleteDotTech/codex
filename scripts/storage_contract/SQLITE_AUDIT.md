# Offline SQLite artifact audit contract

Partial issue #2 tooling, not a production storage adapter or migration path.
The Codex SQLite default and Rust runtime are unchanged. No PostgreSQL version,
installation route, or release is qualified by this module.

## Inputs and authority

`audit_sqlite_snapshot.py` consumes one **already captured, operator-owned,
standalone SQLite backup**, an independent expected SHA-256, and independent
trusted policy. It does not establish how the capture was produced or whether
it is current. Never supply a live main database file: hashing that file alone
can omit acknowledged WAL data. The CLI rejects `-wal`, `-shm`, and `-journal`
sidecars before and after inspection. This is a diagnostic guard, not an
exclusive-access protocol or proof against old/unknown writers.

```sh
python scripts/audit_sqlite_snapshot.py \
  --snapshot isolated-capture/backup.sqlite \
  --policy trusted/schema-policy.json \
  --expected-sha256 TRUSTED_CAPTURE_DIGEST
```

Policy is version 1 with exactly `version`, `schema_sha256`, and `tables` keys.
`tables` maps every actual table, including `sqlite_sequence` or SQLx metadata
when present, to `migrate`, `retain`, or `regenerate`. Unknown, omitted, and
nonexistent table entries fail. A missing file, zero-byte input, and valid empty
SQLite database are distinct. No table's treatment is inferred from its name.
An edited policy is not authenticated by this utility: trust it independently.

Input bytes are streamed into a private temporary file and authenticated before
SQLite opens it. SQLite reads that private copy with `mode=ro&immutable=1`;
no SQLite connection is opened on the caller's original path. The source file
is not written. Temporary copies are removed on ordinary success and exceptions;
an abrupt operating-system/process failure may leave a private audit temporary
folder, but no backend activation or installation change can have occurred.

## Verification and fingerprints

The schema digest includes every `sqlite_schema` object in `type,name` order.
Each row `(type,name,tbl_name,sql)` is encoded with the existing typed record
format and an eight-byte, big-endian ordinal starting at one. SHA-256 consumes
`CDTX-sqlite-schema-v1\0`, then each record's eight-byte length and record bytes,
then the object count as eight bytes. The policy must match that exact digest.
Whitespace/DDL differences can change it; it is not semantic SQL equivalence.

`integrity_check` and `foreign_key_check` must pass. Views, virtual tables,
generated/hidden columns, and non-ASCII SQL identifiers are currently refused.
The authorizer allows only selected reads/functions/inspection pragmas; schema
checks requiring other functions may be refused. These limits must be resolved
by reviewed adapters, never by silently skipping objects.

Every migrated table receives a **SQLite typed-row multiset fingerprint**.
Its values use `encode_row` with constant key `01`. SQLite sorts those BLOB
encodings without numeric/collation coercion, retaining duplicates. SHA-256
consumes `CDTX-sqlite-rows-v1\0`, the four-byte table-name length, ASCII table
name, each record's eight-byte length and record bytes, and the eight-byte row
count. All row fields participate, including queue order and sequence counters.
Identical counts with changed payloads therefore change the fingerprint.

This digest is **not** `manifest.py`'s portable-key domain fingerprint. Do not
place it in a manifest's `logical_sha256`; backend-independent key mapping,
timestamp units and relational semantics remain adapter responsibilities.

Retained/regenerated tables report row counts and `logical_sha256: null`, not
verified copied data. Their values are not canonicalized or emitted. Successful
output says `snapshot_audited`, `scope: single_sqlite_backup_artifact`, and
`activation_permitted: false`. It does not prove whole-dataset completeness,
regeneration, secret-free records, an upstream-compatible export or freshness.

## Bounds and errors

The CLI uses a 64 MiB artifact limit, 250,000 total rows, 1,024 schema objects,
10 million SQLite VM steps, and 128 MiB cumulative encoded-row budget. Records
remain capped at 1 MiB. SQLite sorts can spill into its own temporary files;
I/O failure fails the audit, never selecting a writable fallback. These are
bounded developer audit limits, not full-feature large-migration support.
The library exposes validated `AuditLimits`; the CLI does not increase them.

On Python versions with `Connection.setlimit`, SQLite row/SQL limits are also
set. Python 3.10 lacks that API; the artifact and encoder limits still apply,
but its runtime is not qualified by a 3.10 syntax check. Exit codes are 0 for
successful artifact auditing, 2 for contract rejection, and 3 for input/workspace
I/O failure. Error output contains fixed codes, not exception messages or paths.

## Evidence scope

`fixtures/PROVENANCE.json` identifies the pinned queue migrations and board
schema excerpt. Complete queue asset bytes reproduce their Git blob IDs.
Board SQL is the extracted `SCHEMA` string, not a complete Rust source blob.
SQL fixtures exercise revision backfill, deletion changes, board tombstones,
opt-outs, uniqueness, reopen persistence, and retained generated-ID state.
Their application payload JSON is synthetic, not qualified against public APIs.

The process test commits a queue write to WAL and abruptly exits **only its
disposable writer child**. Raw WAL-backed input is refused. A real SQLite backup
includes the committed row and passes the read-only CLI in another process.
This is not a PostgreSQL network, cross-host Codex, mixed-version fencing, or
installation/uninstallation qualification test. Those release gates remain open.
