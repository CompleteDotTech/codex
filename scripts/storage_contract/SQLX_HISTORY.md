# SQLx history contract (offline draft, issue #2)

`audit_history` checks migration bookkeeping in a separately authenticated,
size-bounded, private standalone SQLite backup. It opens no path, executes no
migration SQL, does not repair the input, and cannot approve activation. Supply
an independently authenticated **complete** expected history for the selected
source version. Never generate that trust input from the incoming database.

## Source anchors

Codex source revision: `395c622d693cf8ec1c4769cf71bc4a949da3e017`.
`codex-rs/Cargo.toml` pins SQLx, SQLx SQLite and migration macros to `=0.9.0`.
`codex-rs/state/src/migrations.rs` (blob
`efbb76cc09483a47a029cc942faa20214043f279`) keeps `ignore_missing: true` for
ordinary runtime access and repairs the historical recency version collision.
Neither behavior establishes safe migration capture or old-writer fencing.

SQLx 0.9.0's authoritative implementation is:
- https://github.com/launchbadge/sqlx/blob/v0.9.0/sqlx-sqlite/src/migrate.rs
- https://github.com/launchbadge/sqlx/blob/v0.9.0/sqlx-core/src/migrate/migration.rs

Its checksum is SHA-384 over exact SQL bytes, preserving UTF-8 BOM and CRLF.
Application SQL and successful bookkeeping commit together, initially with
`execution_time = -1`; timing is written after commit. A crash may leave -1 in a
committed row. `skip()` also writes successful bookkeeping without executing the
application SQL. Consequently **matching bookkeeping never proves that the
schema or data exists**. Descriptions, wall-clock installation times and elapsed
nanoseconds are not portable domain identity or ordering cursors.

## Validation and refusal

The helper recognizes seven SQLx store names, not seven fully qualified schemas.
The independently initialized message board is explicitly outside this format.
Absent optional stores need a separate inventory disposition; an empty/missing
history is never fabricated as a successful capture. Unknown versions, missing
versions, changed checksums, dirty status, wrong metadata types and unknown table
layouts fail closed. Unlike normal Codex runtime, unknown future history is not
ignored. No incoming SQL, description or path is included in diagnostics.

`main` qualification prevents a temporary table/view from shadowing bookkeeping.
The query returns at most 1,025 bounded rows; only exact 48-byte BLOB checksums
and integer version/status/timing values enter Python. Descriptions and timestamp
values are not materialized. The caller retains connection/transaction ownership
and must enforce the existing snapshot authentication, file/query resource
budgets and writer exclusion; this helper does not make a live store coherent.

When primary version 38 has the exact pinned version-39 recency checksum and 39
is absent, the result is `legacy_recency_repair_required`, not an UPDATE. Repair
must occur under the separately authorized native recovery/capture protocol.
The fixture verifies original migration blobs
`74ae0435f83536a17abf24b663d3be9264aebd2c` and
`ccbf79f05fab905ed655f35c215f55a66f3c0f2a` before using those SQL bytes.

## Evidence boundary

Run `python -m unittest discover -s scripts/storage_contract -t scripts
-p test_sqlx_history.py -v` as one command from the repository root. Tests cover
source-derived SQLite metadata, exact recency SQL behavior, and a real disposable
Python/SQLite child-process commit/crash/read-only check. The two-version
bookkeeping fixture is deliberately **not** a complete Codex state database;
its subprocess emulates SQLx's documented shape but does not execute SQLx.

Integration with the complete source catalog, SQLx-created database captures,
legacy split-store data, canonical files/artifacts, namespace and writer fencing,
real PostgreSQL, exact upstream-binary compatibility, install/update/uninstall,
and all of #2's remaining contract/inventory criteria remain separate gates.
This helper changes no selected backend, model-visible context, Rust API, schema,
Bazel dependency or CI requirement. It does not complete #2 or any later issue.
